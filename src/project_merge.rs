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

/// The refusal that stops an operation on a conflict: what conflicted where,
/// the backend's hint up to staging, each conflicted entry's setter, and the
/// two ways out.
/// `resume` is spelled to run against `workspace`, which owns the operation.
pub(crate) fn conflict_stop(
    verb: &str,
    workspace: &Path,
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
        "then run `{resume}`, or `{}` to put everything back as it was",
        crate::op_state::abort_command_at(workspace)
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
        let merged = self.read_merged(vcs, &dir, sides, &repo)?;
        let Some(text) = merged.file(vcs, &dir, Manifest::FILE_NAME)? else {
            return Err(unparseable_result(
                vcs,
                &repo,
                &dir,
                (&self.resume, Produced::Merge),
                &NO_MANIFEST_STAGED,
            ));
        };
        let manifest = match Manifest::from_toml_str(&text) {
            Ok(manifest) => manifest,
            Err(error) => {
                if matches!(merged, Merged::Tree(_)) {
                    refuse_untracked_in_the_way(
                        vcs,
                        &repo,
                        &dir,
                        (base.as_ref(), sides.incoming(self.head)),
                        &self.resume,
                    )?;
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
        if let Merged::Index = merged {
            refuse_unparseable_working_manifest(vcs, &repo, &dir, &self.resume, &text)?;
        }

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
                self.merge_members(vcs, &dir, &merging, sides)?
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
    ) -> anyhow::Result<Merged> {
        let stop = |paths: Vec<String>| {
            let setters = crate::manifest_conflict::stop_lines(vcs, dir, &self.resume);
            conflict_stop(
                self.verb,
                self.ctx.active_path(),
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
            let tree = match trees.merge_tree(dir, &sides.first, &sides.second)? {
                TreeMerge::Clean(tree) => Some(tree),
                TreeMerge::Conflicted { tree, paths }
                    if paths.iter().all(|path| path == LockFile::FILE_NAME) =>
                {
                    Some(tree)
                }
                TreeMerge::Conflicted { .. } => None,
            };
            if let Some(tree) = tree {
                let merged = Merged::Tree(tree);
                let held = sides.held(self.head);
                let incoming = sides.incoming(self.head);
                let text = merged
                    .file(vcs, dir, Manifest::FILE_NAME)?
                    .unwrap_or_default();
                let differs =
                    match crate::manifest_conflict::sides_to_check(vcs, dir, held, incoming)? {
                        Some(recorded) => {
                            crate::manifest_conflict::text_merge_differs(&recorded, &text)?
                        }
                        None => false,
                    };
                if !differs {
                    return Ok(merged);
                }
            }
        }
        op_state::journal_merging(
            self.ctx.active_path(),
            crate::manifest::RepoKey::Project,
            sides.incoming(self.head),
        )?;
        let mut paths: Vec<String> = vcs
            .start_merge(
                dir,
                sides.incoming(self.head),
                DerivedContentPolicy::keep_incoming_side(),
            )?
            .into_iter()
            .filter(|path| path != LockFile::FILE_NAME)
            .collect();
        if crate::manifest_conflict::check_merge_in_flight(vcs, dir)? {
            paths.push(Manifest::FILE_NAME.to_owned());
        }
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
                        self.ctx.active_path(),
                        &self.resume,
                        &conflicted,
                        vcs_for(entry.vcs_type).as_ref(),
                        &crate::manifest_conflict::stop_lines(vcs, dir, &self.resume),
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

/// Why a merge or replay whose `rwv.toml` was removed stops.
pub(crate) const NO_MANIFEST_STAGED: &str = "no rwv.toml is marked resolved";

/// What left the project repo's unparseable `rwv.toml`.
#[derive(Clone, Copy)]
pub(crate) enum Produced {
    Merge,
    Replay,
    ReplayDiffers,
    /// An edit of the working file over a resolved one that parses.
    WorkingEdit,
}

impl Produced {
    fn says(self) -> &'static str {
        match self {
            Self::Merge => {
                "the merge is in progress and no resolved rwv.toml parses; a resolved \
                 rwv.toml that parses is committed in the merge"
            }
            Self::Replay => {
                "the replay finished and the replayed tip's rwv.toml does not parse; a resolved \
                 rwv.toml that parses becomes part of that commit"
            }
            Self::ReplayDiffers => {
                "the replay finished and the replayed tip's rwv.toml parses, but differs from \
                 the merge of the values of the merge base, the commit replayed onto and the \
                 branch's tip before the replay"
            }
            Self::WorkingEdit => {
                "the resolved rwv.toml parses, but the working rwv.toml differs from the resolved \
                 one and does not parse; nothing is committed while it does not"
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
    resume: (&str, Produced),
    error: &dyn std::fmt::Display,
) -> anyhow::Error {
    unparseable_result_saying(vcs, repo, dir, resume, error, &[])
}

/// [`unparseable_result`] with `lines` said after what produced the file.
fn unparseable_result_saying(
    vcs: &dyn Vcs,
    repo: impl std::fmt::Display,
    dir: &Path,
    (resume, produced): (&str, Produced),
    error: &dyn std::fmt::Display,
    lines: &[String],
) -> anyhow::Error {
    use crate::diagnosis::ChoiceId as C;
    let file = dir.join(Manifest::FILE_NAME).display().to_string();
    let parse_error = format!("{error:#}");
    let settled_by = match produced {
        Produced::ReplayDiffers => [C::StageParsingManifest, C::ResumeWithManifest],
        _ => [C::WriteValueMerge, C::KeepTextMerge],
    };
    let judging = crate::diagnosis::State::UnparseableProjectMidOperation
        .of(repo)
        .project_repo()
        .subject(file)
        .listing(
            std::iter::once(produced.says())
                .chain(lines.iter().map(String::as_str))
                .chain(parse_error.lines()),
        )
        .bind_list("<paths>", [Manifest::FILE_NAME])
        .bind_list("<resume>", resume.split_whitespace())
        .bind_list("<abort>", ["rwv", "abort"])
        .checkout(dir)
        .without(&settled_by);
    match produced {
        Produced::ReplayDiffers => judging,
        _ => judging.without(&[C::StageOwnManifest]),
    }
    .judge(vcs)
    .refused()
}

/// Refuse to start, in the checkout at `dir`, the merge of `incoming`
/// whose base is `base`, where a path it writes stands there untracked: the
/// merge would refuse to overwrite it.
fn refuse_untracked_in_the_way(
    vcs: &dyn Vcs,
    repo: impl std::fmt::Display,
    dir: &Path,
    (base, incoming): (Option<&CommitId>, &CommitId),
    resume: &str,
) -> anyhow::Result<()> {
    let Some(base) = base else {
        return Ok(());
    };
    let mut in_the_way = Vec::new();
    for path in vcs.changed_paths_between(
        dir,
        &ResolvedRevisionId::from(base.clone()),
        &ResolvedRevisionId::from(incoming.clone()),
    )? {
        if dir.join(&path).exists() && !vcs.is_tracked(dir, Path::new(&path))? {
            in_the_way.push(path);
        }
    }
    if in_the_way.is_empty() {
        return Ok(());
    }
    crate::refuse!(
        RefusalKind::UntrackedCollision,
        "{repo}: the merge of {} would overwrite {}, untracked here; move {} aside, then run \
         `{resume}`",
        incoming.short(),
        in_the_way.join(", "),
        if in_the_way.len() == 1 { "it" } else { "them" },
    );
}

/// The stop where the working `rwv.toml` in the project repo at `dir`
/// differs from `staged`, which parses, and does not parse itself: the
/// operation would commit the staged file and then read the working one.
pub(crate) fn refuse_unparseable_working_manifest(
    vcs: &dyn Vcs,
    repo: impl std::fmt::Display,
    dir: &Path,
    resume: &str,
    staged: &str,
) -> anyhow::Result<()> {
    let Ok(working) = std::fs::read_to_string(dir.join(Manifest::FILE_NAME)) else {
        return Ok(());
    };
    if working == staged {
        return Ok(());
    }
    match Manifest::from_toml_str(&working) {
        Ok(_) => Ok(()),
        Err(error) => Err(unparseable_result(
            vcs,
            repo,
            dir,
            (resume, Produced::WorkingEdit),
            &error,
        )),
    }
}

/// Settle the `rwv.toml` the replay of the project repo at `dir` left at its
/// tip. Where it does not parse, a staged `rwv.toml` that does is returned,
/// for the re-map to write into its copy of the replayed tip; without one,
/// or where the checkout is off the tip the replay wrote, the operation stops
/// on [`unparseable_result`].
pub(crate) fn settle_replayed_manifest(
    vcs: &dyn Vcs,
    repo: impl std::fmt::Display,
    dir: &Path,
    workspace: &Path,
    verb: op_state::OpVerb,
) -> anyhow::Result<Option<String>> {
    let head = vcs.head_revision(dir)?;
    let at_head = manifest_text_at(vcs, dir, head.id());
    let unparsed = unparseable_at(vcs, dir, head.id());
    let differs = match unparsed {
        Some(_) => None,
        None => replayed_tip_differs(vcs, dir, workspace)?.map(|(_, differs)| differs),
    };
    let staged = vcs
        .staged_file(dir, Manifest::FILE_NAME)?
        .filter(|staged| Some(staged) != at_head.as_ref());
    let rebase = project_rebase(workspace)?;
    let settled = matches!(rebase, Some((_, true)));
    if unparsed.is_none() && differs.is_none() && !(staged.is_some() && settled) {
        return Ok(None);
    }
    let Some((rebase, _)) = rebase else {
        return Err(crate::untokened!(
            InternalInvariant,
            "{}: its tip {} is no replay this operation journaled",
            dir.display(),
            head.id().short()
        ));
    };
    let replayed = vcs.replayed(dir, &rebase.base, &rebase.from, &rebase.onto, head.id())?;
    let replayed_tip = replayed.as_ref().and_then(|replayed| {
        replayed
            .images
            .iter()
            .rev()
            .find_map(|(_, image)| image.clone())
    });
    let mut lines = differs.map(|differs| differs.lines).unwrap_or_default();
    let earlier = replayed.as_ref().map_or(0, |replayed| {
        replayed
            .written
            .iter()
            .filter(|commit| *commit != head.id() && unparseable_at(vcs, dir, commit).is_some())
            .count()
    });
    if earlier > 0 {
        lines.push(format!(
            "{earlier} earlier replayed {} the rwv.toml that does not parse, as the replay \
             wrote it: only the tip takes the resolved fix, and rwv's verbs that read a commit \
             by name refuse {}",
            if earlier == 1 {
                "commit keeps"
            } else {
                "commits keep"
            },
            if earlier == 1 { "it" } else { "them" },
        ));
    }
    let resume = op_state::resume_command(verb);
    let (produced, cause) = match &unparsed {
        Some((error, _)) => (Produced::Replay, format!("{error:#}")),
        None => (Produced::ReplayDiffers, String::new()),
    };
    let stop = |error: &dyn std::fmt::Display, lines: &[String]| {
        unparseable_result_saying(vcs, &repo, dir, (&resume, produced), error, lines)
    };
    let fix = match staged {
        None => return Err(stop(&cause, &lines)),
        Some(fix) => match Manifest::from_toml_str(&fix) {
            Ok(_) => fix,
            Err(error) => return Err(stop(&error, &lines)),
        },
    };
    refuse_unparseable_working_manifest(vcs, &repo, dir, &resume, &fix)?;
    let in_the_way = match vcs.head_attachment(dir)? {
        crate::vcs::HeadAttachment::Attached(on) if on.to_string() != rebase.branch => {
            Some(format!(
                "'{on}' is checked out, not '{}', the branch the replay wrote",
                rebase.branch
            ))
        }
        crate::vcs::HeadAttachment::Attached(_) => None,
        _ => Some(format!(
            "HEAD is detached, not on '{}', the branch the replay wrote",
            rebase.branch
        )),
    }
    .or_else(|| match &replayed_tip {
        Some(tip) if tip == head.id() => None,
        Some(tip) => Some(format!(
            "{} stands on the replayed tip {}, a commit made after the replay; only the \
             replayed tip takes the resolved rwv.toml, so move that commit off it to go on",
            head.id().short(),
            tip.short()
        )),
        None => Some(format!(
            "{} is not the tip the replay wrote; only that tip takes the resolved rwv.toml",
            head.id().short()
        )),
    });
    let in_the_way = match in_the_way {
        Some(line) => Some(line),
        None => vcs
            .tracked_dirty_file_names(dir)?
            .iter()
            .any(|path| path == LockFile::FILE_NAME)
            .then(|| {
                "rwv.lock has uncommitted edits, which the copy of the replayed tip that \
                 takes the resolved rwv.toml rewrites"
                    .to_owned()
            }),
    };
    if let Some(line) = in_the_way {
        lines.insert(0, line);
        return Err(stop(&cause, &lines));
    }
    Ok(Some(fix))
}

/// The project replay journaled in `workspace` whose tip, `HEAD` in `dir`
/// with no merge or replay in flight there, holds an `rwv.toml` that parses
/// and differs from the merge of the values, and that nobody settled.
pub(crate) fn replayed_tip_differs(
    vcs: &dyn Vcs,
    dir: &Path,
    workspace: &Path,
) -> anyhow::Result<Option<(op_state::Rebasing, crate::manifest_conflict::TipDiffers)>> {
    let Some((rebase, false)) = project_rebase(workspace)? else {
        return Ok(None);
    };
    if vcs.mid_op(dir).is_some() {
        return Ok(None);
    }
    let Some(tip) = manifest_text_at(vcs, dir, vcs.head_revision(dir)?.id()) else {
        return Ok(None);
    };
    let Some(sides) =
        crate::manifest_conflict::sides_to_check(vcs, dir, &rebase.onto, &rebase.from)?
    else {
        return Ok(None);
    };
    Ok(crate::manifest_conflict::check_tip(&sides, &tip)?.map(|differs| (rebase, differs)))
}

/// Where the project replay journaled in `workspace` stopped in `dir` with
/// `rwv.toml` unmerged, journal its tip settled: the operator resolves
/// `rwv.toml` there, and what they make of it is not judged again.
pub(crate) fn settled_by_hand(vcs: &dyn Vcs, dir: &Path, workspace: &Path) -> anyhow::Result<()> {
    let unmerged = vcs.rebase_in_flight(dir)?.is_some()
        && vcs
            .unmerged_paths(dir)?
            .iter()
            .any(|path| path == Manifest::FILE_NAME);
    match project_rebase(workspace)? {
        Some((rebase, _)) if unmerged => op_state::journal_tip_settled(workspace, rebase),
        _ => Ok(()),
    }
}

/// The project rebase the operation journaled in `workspace`, and whether
/// the operator settled its tip.
fn project_rebase(workspace: &Path) -> anyhow::Result<Option<(op_state::Rebasing, bool)>> {
    let Some(take_in) = op_state::read_owner(workspace)?.and_then(|owner| owner.take_in) else {
        return Ok(None);
    };
    Ok(take_in
        .rebases
        .into_iter()
        .find(|rebase| rebase.repo == crate::manifest::RepoKey::Project)
        .map(|rebase| {
            let settled = take_in.tip_settled.as_ref() == Some(&rebase);
            (rebase, settled)
        }))
}

fn manifest_text_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<String> {
    vcs.read_file_at_revision(
        dir,
        &ResolvedRevisionId::from(commit.clone()),
        Path::new(Manifest::FILE_NAME),
    )
    .ok()
}

/// The parser's error for the `rwv.toml` at `commit` in `dir`, and the file,
/// where it does not parse.
fn unparseable_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<(anyhow::Error, String)> {
    let text = manifest_text_at(vcs, dir, commit)?;
    Manifest::from_toml_str(&text)
        .err()
        .map(|error| (error, text))
}
