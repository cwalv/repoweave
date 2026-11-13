//! The project merge: the merge of two project histories, with the member
//! merges and the relock it needs.
//!
//! The merge is computed as a tree first where the backend can
//! ([`crate::vcs::Vcs::tree_merges`]), and in the checkout, stopped before its
//! commit, where it cannot. Either way the merged `rwv.toml` is read before
//! anything moves, the member set is taken from it, the members merge, the
//! checkouts the result drops are pruned once every member has merged, and the
//! project commit is written last, carrying the relock.

use crate::activate::ActivateOptions;
use crate::lock_merge::{merge_pins, Held, Parent, PinFinding, PinMerge, Reach};
use crate::manifest::{LockFile, Manifest, ProjectName, RepoEntry, RepoPath, Role};
use crate::op_state::{self, RefMove};
use crate::refusal::RefusalKind;
use crate::vcs::DerivedContentPolicy;
use crate::vcs::{
    project_vcs, vcs_for, CommitId, RawRefName, RefMerge, ResolvedRevisionId, TreeMerge, Vcs,
    VcsError,
};
use crate::workspace::{project_dir, WorkspaceContext};
use anyhow::Context;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The two commits a merge joins. `first` is the first parent of the commit it
/// writes.
#[derive(Debug, Clone)]
pub(crate) struct Sides {
    pub(crate) first: CommitId,
    pub(crate) second: CommitId,
}

/// A member a project merge merges before the project: the first side's pin
/// goes into the member's checkout.
pub(crate) struct MemberMerge {
    pub(crate) path: RepoPath,
    pub(crate) entry: RepoEntry,
    pub(crate) first: CommitId,
}

/// The `owned` and `fork` members of `manifest`, the merged entries, whose
/// pin both sides changed since the base under one declared branch.
fn both_changed(
    manifest: &Manifest,
    held: (Option<&Held>, &Held, &Held),
    merged_declared: &BTreeMap<RepoPath, String>,
) -> Vec<MemberMerge> {
    let (base, first, second) = held;
    manifest
        .iter_entries()
        .filter(|(_, entry)| matches!(entry.role, Role::Owned | Role::Fork))
        .filter_map(|(path, entry)| {
            let (theirs, ours) = (first.pins.get(path)?, second.pins.get(path)?);
            let was = base.and_then(|held| held.pins.get(path));
            let one_branch = first.declared.contains_key(path)
                && first.declared.get(path) == second.declared.get(path)
                && first.declared.get(path) == merged_declared.get(path);
            let changed_apart = theirs != ours && Some(theirs) != was && Some(ours) != was;
            (changed_apart && one_branch).then(|| MemberMerge {
                path: path.clone(),
                entry: entry.clone(),
                first: theirs.clone(),
            })
        })
        .collect()
}

/// The members a merge of `sides` would merge, read without touching the
/// checkout; `None` where the backend cannot merge trees or the merged
/// `rwv.toml` does not come out clean.
pub(crate) fn member_merges_planned(
    vcs: &dyn Vcs,
    root: &Path,
    dir: &Path,
    sides: &Sides,
) -> anyhow::Result<Option<(Manifest, Vec<MemberMerge>)>> {
    let Some(trees) = vcs.tree_merges() else {
        return Ok(None);
    };
    let tree = match trees.merge_tree(dir, &sides.first, &sides.second)? {
        TreeMerge::Clean(tree) => tree,
        TreeMerge::Conflicted { tree, paths }
            if paths.iter().all(|path| path == LockFile::FILE_NAME) =>
        {
            tree
        }
        TreeMerge::Conflicted { .. } => return Ok(None),
    };
    let Some(text) = trees.file_in_tree(dir, &tree, Manifest::FILE_NAME)? else {
        return Ok(None);
    };
    let Ok(manifest) = Manifest::from_toml_str(&text) else {
        return Ok(None);
    };
    let base = vcs
        .merge_bases(dir, &sides.first, &sides.second)?
        .into_iter()
        .next();
    let reach = MemberReach {
        root,
        manifest: &manifest,
    };
    let held = |commit: &CommitId| crate::unmerged::held_at(vcs, dir, commit, &reach);
    let base_held = base.as_ref().map(held);
    let (first, second) = (held(&sides.first), held(&sides.second));
    let merged_declared: BTreeMap<RepoPath, String> = manifest
        .iter_entries()
        .map(|(path, entry)| {
            (
                path.clone(),
                entry.branch.local_counterpart().as_str().to_owned(),
            )
        })
        .collect();
    let merging = both_changed(
        &manifest,
        (base_held.as_ref(), &first, &second),
        &merged_declared,
    );
    Ok(Some((manifest, merging)))
}

/// Where the merged files are read from.
enum Merged {
    Tree(crate::vcs::TreeId),
    Index,
}

impl Merged {
    fn file(&self, vcs: &dyn Vcs, dir: &Path, path: &str) -> anyhow::Result<Option<String>> {
        match self {
            Self::Tree(tree) => {
                let trees = vcs
                    .tree_merges()
                    .context("a merged tree is read where the backend merges trees")?;
                Ok(trees.file_in_tree(dir, tree, path)?)
            }
            Self::Index => Ok(vcs.staged_file(dir, path)?),
        }
    }
}

/// A merge stopped on conflicts in `repo`, whose paths are listed.
pub(crate) struct Conflicted {
    pub(crate) repo: String,
    pub(crate) checkout: PathBuf,
    pub(crate) paths: Vec<String>,
}

/// What a member merge came to.
pub(crate) enum Joined {
    Nothing,
    Moved(CommitId),
    Merged(CommitId),
}

impl Joined {
    pub(crate) fn tip(&self) -> Option<&CommitId> {
        match self {
            Self::Nothing => None,
            Self::Moved(tip) | Self::Merged(tip) => Some(tip),
        }
    }
}

/// Which parent a merge's source is.
#[derive(Clone, Copy)]
pub(crate) enum Order {
    SourceFirst,
    HeadFirst,
    /// The source second, and a merge commit even where the branch could
    /// fast-forward to it: a merge naming another branch than the one the
    /// branch lands into.
    HeadFirstAlwaysMerge,
}

/// A refusal of a conflict where no checkout can hold it.
pub(crate) enum MemberStop {
    Conflict(Conflicted),
    Refused(anyhow::Error),
}

impl From<anyhow::Error> for MemberStop {
    fn from(error: anyhow::Error) -> Self {
        Self::Refused(error)
    }
}

impl From<VcsError> for MemberStop {
    fn from(error: VcsError) -> Self {
        Self::Refused(error.into())
    }
}

/// Merge `source` into the member `repo`, in its checkout where it has the
/// branch attached, else as refs on the member's expected branch where
/// nothing has that checked out. Idempotent: a merge already staged is
/// finished, one already made is left.
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_member(
    as_refs: bool,
    workspace: &Path,
    repo: &RepoPath,
    entry: &RepoEntry,
    dir: &Path,
    source: &CommitId,
    order: Order,
    message: &str,
    derived: DerivedContentPolicy,
) -> Result<Joined, MemberStop> {
    let vcs = vcs_for(entry.vcs_type);
    let journal = |tip: &CommitId| {
        op_state::journal_tip(
            workspace,
            crate::manifest::RepoKey::Member(repo.clone()),
            tip,
        )
    };
    if let Some(in_flight) = vcs.merge_in_flight(dir)? {
        if in_flight == *source {
            let paths = vcs.unmerged_paths(dir)?;
            if !paths.is_empty() {
                return Err(MemberStop::Conflict(Conflicted {
                    repo: repo.to_string(),
                    checkout: dir.to_path_buf(),
                    paths,
                }));
            }
            let head = vcs.head_revision(dir)?.id().clone();
            let (first, second) = parents(order, &head, source);
            let tip = crate::repin::commit_merge(
                vcs.as_ref(),
                dir,
                (&first, &second),
                message,
                None,
                &crate::repin::Naming::Unnamed,
            )?;
            journal(&tip)?;
            return Ok(Joined::Merged(tip));
        }
    }
    let attached = match vcs.head_attachment(dir)? {
        crate::vcs::HeadAttachment::Attached(attached) if !as_refs => Some(attached),
        _ => None,
    };
    let Some(attached) = attached else {
        let branch = RawRefName::new(entry.branch.local_counterpart().as_str());
        return merge_branch_as_refs(
            workspace,
            repo,
            vcs.as_ref(),
            dir,
            &branch,
            source,
            order,
            message,
            derived,
        );
    };
    let head = vcs.head_revision(dir)?;
    let source_rev = ResolvedRevisionId::from(source.clone());
    if vcs.is_ancestor(dir, &source_rev, &head)? {
        return Ok(Joined::Nothing);
    }
    if !matches!(order, Order::HeadFirstAlwaysMerge) && vcs.is_ancestor(dir, &head, &source_rev)? {
        journal(source)?;
        vcs.advance_attached_ref(&attached, &source_rev)?;
        return Ok(Joined::Moved(source.clone()));
    }
    let tracked = vcs.tracked_dirty_file_names(dir)?;
    if let Some(edits) = crate::diagnosis::judge_edits(vcs.as_ref(), repo, dir, tracked, None) {
        let mut report = crate::diagnosis::Report::default();
        report.add(edits);
        return Err(MemberStop::Refused(
            report.into_result().expect_err("a judged edit refuses"),
        ));
    }
    op_state::journal_merging(
        workspace,
        crate::manifest::RepoKey::Member(repo.clone()),
        source,
    )?;
    let paths = vcs.start_merge(dir, source, derived)?;
    if !paths.is_empty() {
        return Err(MemberStop::Conflict(Conflicted {
            repo: repo.to_string(),
            checkout: dir.to_path_buf(),
            paths,
        }));
    }
    let (first, second) = parents(order, head.id(), source);
    let tip = crate::repin::commit_merge(
        vcs.as_ref(),
        dir,
        (&first, &second),
        message,
        None,
        &crate::repin::Naming::Unnamed,
    )?;
    journal(&tip)?;
    Ok(Joined::Merged(tip))
}

fn parents(order: Order, head: &CommitId, source: &CommitId) -> (CommitId, CommitId) {
    match order {
        Order::SourceFirst => (source.clone(), head.clone()),
        Order::HeadFirst | Order::HeadFirstAlwaysMerge => (head.clone(), source.clone()),
    }
}

/// Merge `source` into the member branch `branch`, which no checkout has
/// attached, without touching a working tree.
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_branch_as_refs(
    workspace: &Path,
    repo: &RepoPath,
    vcs: &dyn Vcs,
    dir: &Path,
    branch: &RawRefName,
    source: &CommitId,
    order: Order,
    message: &str,
    derived: DerivedContentPolicy,
) -> Result<Joined, MemberStop> {
    let branch = branch.clone();
    let refuse = |why: String| MemberStop::Refused(crate::untokened!(Owed, "{repo}: {why}"));
    let Some(tip) = vcs.resolve_local_branch_tip(dir, &branch)? else {
        return Err(refuse(format!(
            "no checkout has a branch attached and '{branch}' does not exist here, so there is \
             nothing to merge into"
        )));
    };
    if !vcs.checkouts_of_branch(dir, &branch)?.is_empty() {
        return Err(refuse(format!(
            "'{branch}' is checked out in another workspace; run the merge there"
        )));
    }
    let source_rev = ResolvedRevisionId::from(source.clone());
    if vcs.is_ancestor(dir, &source_rev, &tip)? {
        return Ok(Joined::Nothing);
    }
    let at = tip.id().clone();
    let moved = |to: &CommitId| -> Result<(), MemberStop> {
        op_state::journal_ref_move(
            workspace,
            RefMove {
                repo: crate::manifest::RepoKey::Member(repo.clone()),
                branch: branch.as_str().to_owned(),
                from: at.clone(),
                to: to.clone(),
            },
        )?;
        Ok(())
    };
    if !matches!(order, Order::HeadFirstAlwaysMerge) && vcs.is_ancestor(dir, &tip, &source_rev)? {
        moved(source)?;
        vcs.advance_branch_ref(dir, &branch, &at, source)?;
        return Ok(Joined::Moved(source.clone()));
    }
    let (first, second) = parents(order, &at, source);
    match vcs.merge_as_refs(dir, &first, &second, message, derived, source)? {
        RefMerge::Merged(commit) => {
            moved(&commit)?;
            vcs.advance_branch_ref(dir, &branch, &at, &commit)?;
            Ok(Joined::Merged(commit))
        }
        RefMerge::Conflicted { paths } => Err(refuse(format!(
            "'{branch}' is checked out nowhere and the merge conflicts in {}; a conflict needs a \
             workspace to stop in, so run it where '{branch}' is checked out",
            paths.join(", ")
        ))),
    }
}

/// What a conflicted `rwv.toml` entry needs: the setter of each field the two
/// sides changed differently, spelled with each side's value.
pub(crate) fn entry_setters(
    vcs: &dyn Vcs,
    dir: &Path,
    base: Option<&CommitId>,
    sides: &Sides,
) -> Vec<String> {
    let read = |commit: &CommitId| -> Option<Manifest> {
        let text = vcs
            .read_file_at_revision(
                dir,
                &ResolvedRevisionId::from(commit.clone()),
                Path::new(Manifest::FILE_NAME),
            )
            .ok()?;
        Manifest::from_toml_str(&text).ok()
    };
    let (Some(base), Some(theirs), Some(ours)) =
        (base.and_then(read), read(&sides.first), read(&sides.second))
    else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if theirs.history != ours.history
        && theirs.history != base.history
        && ours.history != base.history
    {
        lines.push("rwv.toml top-level history differs".to_owned());
        for (side, value) in [("ours", ours.history), ("theirs", theirs.history)] {
            lines.push(format!("  keep {side}: {}", history_setter(value, None)));
        }
    }
    for (path, was) in base.iter_entries() {
        let (Some(theirs), Some(ours)) = (theirs.get_entry(path), ours.get_entry(path)) else {
            continue;
        };
        let theirs_fields = crate::entry_report::fields_between(was, theirs);
        for ours_field in crate::entry_report::fields_between(was, ours) {
            let Some(theirs_field) = theirs_fields.iter().find(|f| f.name == ours_field.name)
            else {
                continue;
            };
            if theirs_field.now == ours_field.now {
                continue;
            }
            lines.push(format!(
                "rwv.toml entry {path}: {} differs",
                ours_field.name
            ));
            for (side, value) in [("ours", &ours_field.now), ("theirs", &theirs_field.now)] {
                if let Some(setter) = setter(ours_field.name, path, value) {
                    lines.push(format!("  keep {side}: {setter}"));
                }
            }
        }
    }
    lines
}

fn history_setter(value: Option<crate::manifest::History>, member: Option<&RepoPath>) -> String {
    let repo = member.map_or(String::new(), |member| format!(" --repo {member}"));
    match value {
        Some(history) => format!("rwv branch set-history {history}{repo}"),
        None => format!("rwv branch set-history --unset{repo}"),
    }
}

fn setter(field: &str, member: &RepoPath, value: &str) -> Option<String> {
    if field == "history" {
        return Some(history_setter(value.parse().ok(), Some(member)));
    }
    let verb = match field {
        "branch" => "set-branch",
        "url" => "set-url",
        "role" => "set-role",
        "recurse-submodules" => "set-recurse-submodules",
        _ => return None,
    };
    Some(format!("rwv member {verb} {member} {value}"))
}

/// The refusal that stops an operation on a conflict: what conflicted where,
/// the backend's hint up to staging, each conflicted entry's setter, and the
/// two ways out.
pub(crate) fn conflict_stop(
    verb: &str,
    resume: &str,
    conflicted: &Conflicted,
    vcs: &dyn Vcs,
    setters: &[String],
) -> anyhow::Error {
    let mut text = format!(
        "rwv {verb}: merge conflict in {} ({}) at {}.\n{}\n",
        conflicted.repo,
        conflicted.paths.join(", "),
        conflicted.checkout.display(),
        vcs.conflict_resolution_hint(crate::vcs::ConflictOp::Rebase),
    );
    for line in setters {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str(&format!(
        "then run `{resume}`, or `rwv abort` to put everything back as it was"
    ));
    crate::refusal::refusal(RefusalKind::OpParked, text)
}

struct MemberReach<'a> {
    root: &'a Path,
    manifest: &'a Manifest,
}

impl Reach for MemberReach<'_> {
    fn reaches(&self, member: &RepoPath, ancestor: &CommitId, of: &CommitId) -> Option<bool> {
        let entry = self.manifest.get_entry(member)?;
        vcs_for(entry.vcs_type)
            .is_ancestor(
                &self.root.join(member.as_path()),
                &ResolvedRevisionId::from(ancestor.clone()),
                &ResolvedRevisionId::from(of.clone()),
            )
            .ok()
    }

    fn named(&self, member: &RepoPath, name: &str) -> Option<CommitId> {
        let entry = self.manifest.get_entry(member)?;
        Some(
            vcs_for(entry.vcs_type)
                .resolve_revision(&self.root.join(member.as_path()), name)
                .ok()?
                .id()
                .clone(),
        )
    }
}

/// One project merge: what it joins and the workspace it runs in.
pub(crate) struct ProjectMerge<'a> {
    pub(crate) ctx: &'a WorkspaceContext,
    pub(crate) project: &'a ProjectName,
    pub(crate) verb: &'static str,
    pub(crate) resume: String,
    pub(crate) jobs: usize,
    pub(crate) opts: ActivateOptions,
    pub(crate) message: String,
    /// Which side the checkout holds: the other is merged in.
    pub(crate) head: Parent,
    pub(crate) members: MemberMerging,
    /// The member rebases this operation made: the merge records each one's
    /// move of the pin the checkout's side held, by name.
    pub(crate) rebases: Vec<op_state::Rebasing>,
}

/// How a project merge settles its members.
pub(crate) enum MemberMerging {
    /// Merge each member whose pin both sides changed under one branch, the
    /// lineage rule judging each where the branch is a feature.
    BothChanged(Option<crate::feature_sync::FeatureLineage>),
    /// The members were merged before the project: their tips.
    Done(BTreeMap<RepoPath, CommitId>),
}

impl Sides {
    fn incoming(&self, head: Parent) -> &CommitId {
        match head {
            Parent::First => &self.second,
            Parent::Second => &self.first,
        }
    }

    fn held(&self, head: Parent) -> &CommitId {
        match head {
            Parent::First => &self.first,
            Parent::Second => &self.second,
        }
    }
}

impl ProjectMerge<'_> {
    pub(crate) fn run(
        &self,
        sides: &Sides,
    ) -> anyhow::Result<(CommitId, Vec<crate::sync::MemberOutput>)> {
        let dir = project_dir(self.ctx.active_path(), self.project);
        let vcs = project_vcs();
        let vcs = vcs.as_ref();
        let repo = crate::workspace::project_rel_path(self.project);
        let base = vcs
            .merge_bases(&dir, &sides.first, &sides.second)?
            .into_iter()
            .next();
        let merged = self.read_merged(vcs, &dir, sides, &repo, base.as_ref())?;
        let text = merged
            .file(vcs, &dir, Manifest::FILE_NAME)?
            .context("the merged project holds no rwv.toml")?;
        let manifest = match Manifest::from_toml_str(&text) {
            Ok(manifest) => manifest,
            Err(error) => {
                if matches!(merged, Merged::Tree(_)) {
                    op_state::journal_merging(
                        self.ctx.active_path(),
                        crate::manifest::RepoKey::Project,
                        sides.incoming(self.head),
                    )?;
                    vcs.start_merge(
                        &dir,
                        sides.incoming(self.head),
                        DerivedContentPolicy::keep_incoming_side(),
                    )?;
                }
                return Err(unparseable_result(
                    vcs,
                    &repo,
                    &dir,
                    (&self.resume, Produced::Merge),
                    &error,
                ));
            }
        };

        let reach = MemberReach {
            root: self.ctx.active_path(),
            manifest: &manifest,
        };
        let held = |commit: &CommitId| crate::unmerged::held_at(vcs, &dir, commit, &reach);
        let base_held = base.as_ref().map(held);
        let (first_held, second_held) = (held(&sides.first), held(&sides.second));
        let merged_declared: BTreeMap<RepoPath, String> = manifest
            .iter_entries()
            .map(|(path, entry)| {
                (
                    path.clone(),
                    entry.branch.local_counterpart().as_str().to_owned(),
                )
            })
            .collect();

        let member_merges = match &self.members {
            MemberMerging::BothChanged(lineage) => {
                let merging = both_changed(
                    &manifest,
                    (base_held.as_ref(), &first_held, &second_held),
                    &merged_declared,
                );
                if let Some(lineage) = lineage {
                    let mut report = crate::diagnosis::Report::default();
                    lineage.judge_member_merges(
                        self.ctx.active_path(),
                        &manifest,
                        &merging,
                        &repo,
                        &mut report,
                    )?;
                    report.into_result()?;
                }
                self.merge_members(vcs, &dir, &merging, sides, base.as_ref())?
            }
            MemberMerging::Done(tips) => tips.clone(),
        };

        let before = read_manifest_at(vcs, &dir, sides.held(self.head));
        let mut pruning = Vec::new();
        if let Some(before) = &before {
            for (path, entry) in before.iter_entries() {
                if !manifest.contains_repo(path) {
                    let checkout = self.ctx.active_path().join(path.as_path());
                    pruning.push(
                        match crate::sync::prune_dropped_repo(
                            vcs_for(entry.vcs_type).as_ref(),
                            self.ctx,
                            path,
                        ) {
                            Ok(()) => crate::sync::MemberOutput::pruned(path, &checkout),
                            Err(e) => crate::sync::MemberOutput::prune_refused(path, &checkout, &e),
                        },
                    );
                }
            }
        }

        if matches!(merged, Merged::Tree(_)) {
            op_state::journal_merging(
                self.ctx.active_path(),
                crate::manifest::RepoKey::Project,
                sides.incoming(self.head),
            )?;
            let paths = vcs.start_merge(
                &dir,
                sides.incoming(self.head),
                DerivedContentPolicy::keep_incoming_side(),
            )?;
            if !paths.iter().all(|path| path == LockFile::FILE_NAME) {
                return Err(crate::untokened!(
                    InternalInvariant,
                    "the merge computed as clean conflicts in {}",
                    paths.join(", ")
                ));
            }
        }

        let reach = MemberReach {
            root: self.ctx.active_path(),
            manifest: &manifest,
        };
        let pins = merge_pins(
            &PinMerge {
                base: base_held.as_ref(),
                first: &first_held,
                second: &second_held,
                merged_declared: &merged_declared,
                member_merges: &member_merges,
            },
            &reach,
        );
        self.report(&pins.findings);
        let resolved: Vec<(RepoPath, ResolvedRevisionId)> = pins
            .pins
            .iter()
            .map(|(path, pin)| (path.clone(), ResolvedRevisionId::from(pin.clone())))
            .collect();
        let lock = crate::lock::with_pins(
            &manifest,
            None,
            resolved.iter().map(|(path, pin)| (path, pin)),
        );
        crate::lock::write_lock_in(&lock, &dir)?;
        vcs.stage_paths(&dir, &[LockFile::FILE_NAME])?;

        self.settle(vcs, &dir, &manifest)?;

        let ours = match self.head {
            Parent::First => &first_held,
            Parent::Second => &second_held,
        };
        let jumps = crate::remap::rebase_jumps(
            self.ctx.active_path(),
            &manifest,
            &self.rebases,
            &ours.pins,
            &pins.pins,
            self.ctx.active_path(),
        )?;
        let commit = crate::repin::commit_merge_annotated(
            vcs,
            &dir,
            (&sides.first, &sides.second),
            &self.message,
            Some(&lock),
            &crate::repin::Naming::Unnamed,
            jumps.iter().map(crate::repin::Repin::annotation).collect(),
        )?;
        op_state::journal_tip(
            self.ctx.active_path(),
            crate::manifest::RepoKey::Project,
            &commit,
        )?;
        Ok((commit, pruning))
    }

    fn read_merged(
        &self,
        vcs: &dyn Vcs,
        dir: &Path,
        sides: &Sides,
        repo: &str,
        base: Option<&CommitId>,
    ) -> anyhow::Result<Merged> {
        let stop = |paths: Vec<String>| {
            let setters = entry_setters(vcs, dir, base, sides);
            conflict_stop(
                self.verb,
                &self.resume,
                &Conflicted {
                    repo: repo.to_owned(),
                    checkout: dir.to_path_buf(),
                    paths,
                },
                vcs,
                &setters,
            )
        };
        if vcs.merge_in_flight(dir)? == Some(sides.incoming(self.head).clone()) {
            let unmerged: Vec<String> = vcs
                .unmerged_paths(dir)?
                .into_iter()
                .filter(|path| path != LockFile::FILE_NAME)
                .collect();
            if unmerged.is_empty() {
                return Ok(Merged::Index);
            }
            return Err(stop(unmerged));
        }
        let tracked = vcs.tracked_dirty_file_names(dir)?;
        if let Some(edits) = crate::diagnosis::judge_edits(vcs, repo, dir, tracked, None) {
            let mut report = crate::diagnosis::Report::default();
            report.add(edits);
            report.into_result()?;
        }
        if let Some(trees) = vcs.tree_merges() {
            match trees.merge_tree(dir, &sides.first, &sides.second)? {
                TreeMerge::Clean(tree) => return Ok(Merged::Tree(tree)),
                TreeMerge::Conflicted { tree, paths }
                    if paths.iter().all(|path| path == LockFile::FILE_NAME) =>
                {
                    return Ok(Merged::Tree(tree))
                }
                TreeMerge::Conflicted { .. } => {}
            }
        }
        op_state::journal_merging(
            self.ctx.active_path(),
            crate::manifest::RepoKey::Project,
            sides.incoming(self.head),
        )?;
        let paths: Vec<String> = vcs
            .start_merge(
                dir,
                sides.incoming(self.head),
                DerivedContentPolicy::keep_incoming_side(),
            )?
            .into_iter()
            .filter(|path| path != LockFile::FILE_NAME)
            .collect();
        if paths.is_empty() {
            Ok(Merged::Index)
        } else {
            Err(stop(paths))
        }
    }

    fn merge_members(
        &self,
        vcs: &dyn Vcs,
        dir: &Path,
        merging: &[MemberMerge],
        sides: &Sides,
        base: Option<&CommitId>,
    ) -> anyhow::Result<BTreeMap<RepoPath, CommitId>> {
        let mut merged = BTreeMap::new();
        for MemberMerge {
            path,
            entry,
            first: theirs,
        } in merging
        {
            let member_dir = self.ctx.active_path().join(path.as_path());
            if !member_dir.exists() {
                continue;
            }
            let message = format!("Merge the pin of {path} taken from {}", sides.first.short());
            match merge_member(
                false,
                self.ctx.active_path(),
                path,
                entry,
                &member_dir,
                theirs,
                Order::HeadFirst,
                &message,
                DerivedContentPolicy::keep_incoming_side(),
            ) {
                Ok(Joined::Merged(tip)) => {
                    merged.insert(path.clone(), tip);
                }
                Ok(Joined::Moved(_) | Joined::Nothing) => {}
                Err(MemberStop::Conflict(conflicted)) => {
                    return Err(conflict_stop(
                        self.verb,
                        &self.resume,
                        &conflicted,
                        vcs_for(entry.vcs_type).as_ref(),
                        &entry_setters(vcs, dir, base, sides),
                    ));
                }
                Err(MemberStop::Refused(error)) => return Err(error),
            }
        }
        Ok(merged)
    }

    fn report(&self, findings: &[PinFinding]) {
        for finding in findings {
            match finding {
                PinFinding::Unmerged {
                    member,
                    first,
                    second,
                } => eprintln!(
                    "rwv {}: {member}: both sides moved the pin under one branch and neither \
                     contains the other ({} and {}); the pin of the first parent is kept, and \
                     `rwv push` refuses until a commit holds both",
                    self.verb,
                    first.short(),
                    second.short()
                ),
                PinFinding::DeclaredDiffer {
                    member,
                    kept,
                    kept_pin,
                    other_pin,
                    ..
                } => eprintln!(
                    "rwv {}: {member}: the parents declare it differently; kept the pin {} of \
                     the {} parent, which the other parent's {} does not reach",
                    self.verb,
                    kept_pin.short(),
                    match kept {
                        Parent::First => "first",
                        Parent::Second => "second",
                    },
                    other_pin.short()
                ),
            }
        }
    }

    fn settle(&self, vcs: &dyn Vcs, dir: &Path, manifest: &Manifest) -> anyhow::Result<()> {
        let root = self.ctx.active_path();
        let missing: Vec<String> = manifest
            .iter_entries()
            .filter(|(path, _)| !root.join(path.as_path()).exists())
            .map(|(path, _)| path.to_string())
            .collect();
        if !missing.is_empty() {
            let selection = crate::member_selection::MemberSelection::new(
                crate::selector::RepoFilter::parse(&[], &missing)?,
                false,
            );
            crate::update::update_members(
                self.ctx,
                &selection,
                self.jobs,
                crate::parallel::OutputMode::Text,
                self.opts,
            )?
            .tail?;
        }
        crate::add_remove::settle_manifest(self.ctx, self.project, None, &[], self.opts)?;
        let authored = crate::activate::owned_paths(root, self.project, manifest);
        let dirty = vcs.dirty_file_names(dir)?;
        let carried: Vec<&str> = dirty
            .iter()
            .map(String::as_str)
            .filter(|path| authored.contains(*path))
            .collect();
        if !carried.is_empty() {
            vcs.stage_paths(dir, &carried)?;
        }
        Ok(())
    }
}

fn read_manifest_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<Manifest> {
    let text = vcs
        .read_file_at_revision(
            dir,
            &ResolvedRevisionId::from(commit.clone()),
            Path::new(Manifest::FILE_NAME),
        )
        .ok()?;
    Manifest::from_toml_str(&text).ok()
}

/// What left the project repo's unparseable `rwv.toml`.
#[derive(Clone, Copy)]
pub(crate) enum Produced {
    Merge,
    Replay,
}

impl Produced {
    fn says(self) -> &'static str {
        match self {
            Self::Merge => {
                "the merge is in progress, its rwv.toml staged as the two sides merged it as \
                 text; a staged rwv.toml that parses is committed in the merge"
            }
            Self::Replay => {
                "the replay finished and the replayed tip's rwv.toml does not parse; a staged \
                 rwv.toml that parses becomes part of that commit"
            }
        }
    }
}

/// The stop where the `rwv.toml` a merge or replay of the project repo at
/// `dir` produced does not parse: what is there, the parser's error, then
/// staging a fix, the operation's continue, and abort.
pub(crate) fn unparseable_result(
    vcs: &dyn Vcs,
    repo: impl std::fmt::Display,
    dir: &Path,
    (resume, produced): (&str, Produced),
    error: &anyhow::Error,
) -> anyhow::Error {
    let file = dir.join(Manifest::FILE_NAME).display().to_string();
    let parse_error = format!("{error:#}");
    crate::diagnosis::State::UnparseableProjectMidOperation
        .of(repo)
        .project_repo()
        .subject(file)
        .listing(std::iter::once(produced.says()).chain(parse_error.lines()))
        .bind_list("<paths>", [Manifest::FILE_NAME])
        .bind_list("<resume>", resume.split_whitespace())
        .checkout(dir)
        .judge(vcs)
        .refused()
}

/// Settle the `rwv.toml` the replay of the project repo at `dir` left at its
/// tip. Where it does not parse, a staged `rwv.toml` that does becomes part
/// of the replayed tip; without one the operation stops on
/// [`unparseable_result`].
pub(crate) fn settle_replayed_manifest(
    vcs: &dyn Vcs,
    repo: impl std::fmt::Display,
    dir: &Path,
    workspace: &Path,
    verb: op_state::OpVerb,
) -> anyhow::Result<()> {
    let head = vcs.head_revision(dir)?;
    let Ok(at_head) = vcs.read_file_at_revision(dir, &head, Path::new(Manifest::FILE_NAME)) else {
        return Ok(());
    };
    let Err(at_head_error) = Manifest::from_toml_str(&at_head) else {
        return Ok(());
    };
    let stop = |error: &anyhow::Error| {
        unparseable_result(
            vcs,
            &repo,
            dir,
            (&op_state::resume_command(verb), Produced::Replay),
            error,
        )
    };
    let fix = match vcs
        .staged_file(dir, Manifest::FILE_NAME)?
        .filter(|staged| *staged != at_head)
    {
        None => return Err(stop(&at_head_error)),
        Some(fix) => match Manifest::from_toml_str(&fix) {
            Ok(_) => fix,
            Err(error) => return Err(stop(&error)),
        },
    };
    amend_replayed_tip(vcs, dir, workspace, head.id(), fix)
}

/// Make `fix`, the staged `rwv.toml`, part of `tip`, the commit the
/// project's replay journaled in `workspace` wrote last. The replay's record
/// names the amended commit before the branch moves to it, so the operation
/// owns the branch at either tip.
fn amend_replayed_tip(
    vcs: &dyn Vcs,
    dir: &Path,
    workspace: &Path,
    tip: &CommitId,
    fix: String,
) -> anyhow::Result<()> {
    let rebase = op_state::read_owner(workspace)?
        .and_then(|owner| owner.take_in)
        .and_then(|take_in| {
            take_in
                .rebases
                .into_iter()
                .find(|rebase| rebase.repo == crate::manifest::RepoKey::Project)
        })
        .ok_or_else(|| {
            crate::untokened!(
                InternalInvariant,
                "{}: its tip {} is no replay this operation journaled",
                dir.display(),
                tip.short()
            )
        })?;
    let replayed = vcs
        .replayed(dir, &rebase.base, &rebase.from, &rebase.onto, tip)?
        .filter(|replayed| replayed.written.last() == Some(tip))
        .ok_or_else(|| {
            crate::untokened!(
                InternalInvariant,
                "{}: no record of its replay describes its tip {}",
                dir.display(),
                tip.short()
            )
        })?;
    let crate::vcs::HeadAttachment::Attached(on) = vcs.head_attachment(dir)? else {
        return Err(crate::untokened!(
            EnvironmentPassthrough,
            "{}: left its branch '{}' after its replay",
            dir.display(),
            rebase.branch
        ));
    };
    let parent = vcs.parents(dir, tip)?.into_iter().next().ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "{}: the replayed tip {} has no parent",
            dir.display(),
            tip.short()
        )
    })?;
    let amended = vcs.copy_replayed(
        dir,
        tip,
        &parent,
        &[crate::vcs::FileChange {
            path: Manifest::FILE_NAME.to_owned(),
            content: Some(fix.into_bytes()),
        }],
        None,
    )?;
    let became = |commit: &CommitId| {
        if commit == tip {
            amended.clone()
        } else {
            commit.clone()
        }
    };
    let replayed = crate::vcs::Replayed {
        images: replayed
            .images
            .iter()
            .map(|(was, image)| (was.clone(), image.as_ref().map(became)))
            .collect(),
        written: replayed.written.iter().map(became).collect(),
    };
    vcs.record_replayed(dir, &rebase.from, &rebase.onto, &replayed)?;
    crate::test_pause::point("replayed-manifest-recorded");
    vcs.take_amended(&on, tip, &amended)?;
    crate::test_pause::point("replayed-manifest-amended");
    Ok(())
}
