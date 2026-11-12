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
            let tip = vcs.commit_merge(dir, &first, &second, message)?;
            journal(&tip)?;
            return Ok(Joined::Merged(tip));
        }
    }
    let attached = match vcs.head_attachment(dir)? {
        crate::vcs::HeadAttachment::Attached(attached) if !as_refs => Some(attached),
        _ => None,
    };
    let Some(attached) = attached else {
        return merge_member_as_refs(workspace, repo, entry, dir, source, order, message);
    };
    let head = vcs.head_revision(dir)?;
    let source_rev = ResolvedRevisionId::from(source.clone());
    if vcs.is_ancestor(dir, &source_rev, &head)? {
        return Ok(Joined::Nothing);
    }
    if vcs.is_ancestor(dir, &head, &source_rev)? {
        vcs.advance_attached_ref(&attached, &source_rev)?;
        journal(source)?;
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
    let paths = vcs.start_merge(dir, source)?;
    if !paths.is_empty() {
        return Err(MemberStop::Conflict(Conflicted {
            repo: repo.to_string(),
            checkout: dir.to_path_buf(),
            paths,
        }));
    }
    let (first, second) = parents(order, head.id(), source);
    let tip = vcs.commit_merge(dir, &first, &second, message)?;
    journal(&tip)?;
    Ok(Joined::Merged(tip))
}

fn parents(order: Order, head: &CommitId, source: &CommitId) -> (CommitId, CommitId) {
    match order {
        Order::SourceFirst => (source.clone(), head.clone()),
        Order::HeadFirst => (head.clone(), source.clone()),
    }
}

fn merge_member_as_refs(
    workspace: &Path,
    repo: &RepoPath,
    entry: &RepoEntry,
    dir: &Path,
    source: &CommitId,
    order: Order,
    message: &str,
) -> Result<Joined, MemberStop> {
    let vcs = vcs_for(entry.vcs_type);
    let branch = RawRefName::new(entry.branch.local_counterpart().as_str());
    let refuse = |why: String| MemberStop::Refused(anyhow::anyhow!("{repo}: {why}"));
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
    if vcs.is_ancestor(dir, &tip, &source_rev)? {
        vcs.advance_branch_ref(dir, &branch, &at, source)?;
        moved(source)?;
        return Ok(Joined::Moved(source.clone()));
    }
    let (first, second) = parents(order, &at, source);
    match vcs.merge_as_refs(dir, &first, &second, message)? {
        RefMerge::Merged(commit) => {
            vcs.advance_branch_ref(dir, &branch, &at, &commit)?;
            moved(&commit)?;
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

fn setter(field: &str, member: &RepoPath, value: &str) -> Option<String> {
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
}

/// One project merge: what it joins and the workspace it runs in.
pub(crate) struct ProjectMerge<'a> {
    pub(crate) ctx: &'a WorkspaceContext,
    pub(crate) project: &'a ProjectName,
    pub(crate) verb: &'static str,
    pub(crate) resume: &'static str,
    pub(crate) jobs: usize,
    pub(crate) opts: ActivateOptions,
    pub(crate) message: String,
}

impl ProjectMerge<'_> {
    pub(crate) fn run(&self, sides: &Sides) -> anyhow::Result<CommitId> {
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
        let manifest = Manifest::from_toml_str(&text)
            .context("the merged rwv.toml does not parse; resolve it, then continue")?;

        let held = |commit: &CommitId| crate::unmerged::held_at(vcs, &dir, commit);
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

        let member_merges = self.merge_members(
            vcs,
            &dir,
            &manifest,
            (base_held.as_ref(), &first_held, &second_held),
            &merged_declared,
            sides,
            base.as_ref(),
        )?;

        let before = read_manifest_at(vcs, &dir, &sides.second);
        if let Some(before) = &before {
            for (path, entry) in before.iter_entries() {
                if !manifest.contains_repo(path) {
                    crate::sync::prune_dropped_repo(
                        vcs_for(entry.vcs_type).as_ref(),
                        self.ctx,
                        path,
                    )?;
                }
            }
        }

        if matches!(merged, Merged::Tree(_)) {
            let paths = vcs.start_merge(&dir, &sides.first)?;
            anyhow::ensure!(
                paths.iter().all(|path| path == LockFile::FILE_NAME),
                "the merge computed as clean conflicts in {}",
                paths.join(", ")
            );
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

        let commit = vcs.commit_merge(&dir, &sides.first, &sides.second, &self.message)?;
        op_state::journal_tip(
            self.ctx.active_path(),
            crate::manifest::RepoKey::Project,
            &commit,
        )?;
        Ok(commit)
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
                self.resume,
                &Conflicted {
                    repo: repo.to_owned(),
                    checkout: dir.to_path_buf(),
                    paths,
                },
                vcs,
                &setters,
            )
        };
        if vcs.merge_in_flight(dir)? == Some(sides.first.clone()) {
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
        let paths: Vec<String> = vcs
            .start_merge(dir, &sides.first)?
            .into_iter()
            .filter(|path| path != LockFile::FILE_NAME)
            .collect();
        if paths.is_empty() {
            Ok(Merged::Index)
        } else {
            Err(stop(paths))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn merge_members(
        &self,
        vcs: &dyn Vcs,
        dir: &Path,
        manifest: &Manifest,
        held: (Option<&Held>, &Held, &Held),
        merged_declared: &BTreeMap<RepoPath, String>,
        sides: &Sides,
        base: Option<&CommitId>,
    ) -> anyhow::Result<BTreeMap<RepoPath, CommitId>> {
        let (base_held, first, second) = held;
        let mut merged = BTreeMap::new();
        for (path, entry) in manifest.iter_entries() {
            if !matches!(entry.role, Role::Owned | Role::Fork) {
                continue;
            }
            let (Some(theirs), Some(ours)) = (first.pins.get(path), second.pins.get(path)) else {
                continue;
            };
            let was = base_held.and_then(|held| held.pins.get(path));
            let one_branch = first.declared.contains_key(path)
                && first.declared.get(path) == second.declared.get(path)
                && first.declared.get(path) == merged_declared.get(path);
            if theirs == ours || Some(theirs) == was || Some(ours) == was || !one_branch {
                continue;
            }
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
            ) {
                Ok(Joined::Merged(tip)) => {
                    merged.insert(path.clone(), tip);
                }
                Ok(Joined::Moved(_) | Joined::Nothing) => {}
                Err(MemberStop::Conflict(conflicted)) => {
                    return Err(conflict_stop(
                        self.verb,
                        self.resume,
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
