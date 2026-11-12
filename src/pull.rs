//! `rwv pull` — fetch, fast-forward the project branch, then update.
//!
//! A branch that is behind its upstream takes it in, one that is even or ahead
//! has nothing to take, and one that has diverged is refused with nothing
//! changed, unless `--merge-upstream` merges it or `--reset-to-upstream` moves
//! it to the upstream, keeping a backup. The members are then aligned to the
//! lock the pulled commit brought.

use crate::activate::ActivateOptions;
use crate::diagnosis::{judge_edits, Report, State};
use crate::fetch::{fetch_repos, FetchedRepoOutput};
use crate::manifest::{LockFile, Manifest, ProjectName, RepoEntry, RepoKey, RepoOperand, RepoPath};
use crate::member_selection::MemberSelection;
use crate::merge_upstream::{Flags, Op};
use crate::op_state::{OpVerb, TakeInRecord};
use crate::parallel::OutputMode;
use crate::placement::PlacementOutput;
use crate::project_merge::{ProjectMerge, Sides};
use crate::refusal::RefusalKind;
use crate::selector::RepoFilter;
use crate::skipped::SkippedEffect;
use crate::update::{update_members, MemberOutcomeOutput};
use crate::vcs::{
    project_vcs, vcs_for, AttachedRef, HeadAttachment, RawRefName, ResolvedRevisionId, Vcs,
};
use crate::workspace::{project_dir, project_rel_path, Checkout, Resolution, WorkspaceContext};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Schema URL embedded in `rwv pull --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/pull.json`.
pub const PULL_SCHEMA_URL: &str = crate::schema_url::schema_url!("pull");

/// What taking in the project branch came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TakeIn {
    /// The branch was behind its upstream and now is at it.
    Advanced,
    /// The branch is at its upstream.
    Even,
    /// The branch holds commits its upstream lacks, and nothing it has not.
    Ahead,
    /// The branch has no upstream, or its upstream is not on the remote.
    NoUpstream,
    /// The project repo is on no branch.
    Detached,
    /// The branch had diverged and was merged with its upstream.
    Merged,
    /// The branch was moved to its upstream, its own tip kept in a backup.
    Reset,
}

/// A member whose pin the pulled lock moved backward, or to a commit that does
/// not descend from the old one.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RePinnedOutput {
    pub path: String,
    pub from: String,
    pub to: String,
}

/// How the pulled `rwv.toml` changed a member's entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EntryChange {
    Added,
    Removed,
    Changed,
}

/// A member entry the pulled `rwv.toml` added, removed or changed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemberChangeOutput {
    pub path: String,
    pub change: EntryChange,
    /// For a changed entry, each field that differs as `<field>: <was> -> <now>`.
    pub fields: Vec<String>,
}

/// What `rwv pull` did to the project branch.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProjectTakeInOutput {
    /// The project repo's path, `projects/<name>`.
    pub path: String,
    /// The branch the project repo is on. Absent when it is detached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub result: TakeIn,
    /// The branch's tip before. Present for `advanced`, `merged` and `reset`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The branch's tip after. Present for `advanced`, `merged` and `reset`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The backup holding the tip a `reset` left.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    /// Members the pulled lock re-pinned backward.
    pub re_pinned: Vec<RePinnedOutput>,
    /// Member entries the pulled `rwv.toml` added, removed or changed.
    pub member_changes: Vec<MemberChangeOutput>,
}

/// Top-level envelope for `rwv pull --json`.
///
/// The update's records are the envelope's own, as `rwv update --json` has
/// them.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PullJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    /// The pruning fetch of each repo, as `rwv fetch --json` reports it.
    pub fetched: Vec<FetchedRepoOutput>,
    /// What taking in the project branch did.
    pub project: ProjectTakeInOutput,
    /// The update's per-member outcomes.
    pub outcomes: Vec<MemberOutcomeOutput>,
    /// Each member the update left where it was, judged.
    pub judged: Vec<crate::diagnosis::JudgedOutput>,
    /// How the files of each checkout the update moved or created were
    /// placed. Empty, not absent, when it placed none.
    pub placements: Vec<PlacementOutput>,
    /// Resolved workspace coordinates (weave root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// The materialization side effects the update skipped, each naming the
    /// command that performs it. Empty, not absent, when it skipped none.
    #[serde(default)]
    pub skipped: Vec<SkippedEffect>,
}

impl PullJsonOutput {
    fn print(&self) -> anyhow::Result<()> {
        let out = serde_json::to_string_pretty(self).context("failed to serialize pull output")?;
        outln!("{out}");
        Ok(())
    }
}

/// The manifest and lock of the project at `project_dir`, as checked out.
struct Recorded {
    manifest: Manifest,
    lock: Option<LockFile>,
}

impl Recorded {
    fn read(project_dir: &Path) -> anyhow::Result<Self> {
        let manifest_path = project_dir.join(Manifest::FILE_NAME);
        let manifest = Manifest::from_path(&manifest_path)
            .with_context(|| format!("failed to read manifest from {}", manifest_path.display()))?;
        let lock_path = LockFile::path_in(project_dir);
        let lock = if lock_path.exists() {
            Some(
                LockFile::from_state(&lock_path)
                    .with_context(|| format!("failed to read lock file at {lock_path}"))?,
            )
        } else {
            None
        };
        Ok(Self { manifest, lock })
    }
}

/// Run `rwv pull`: the pruning fetch of every repo, a fast-forward of the
/// project branch, then `rwv update`, which `selection` narrows.
///
/// The fetch moves no branch; the take-in moves the project branch and
/// nothing else; the update aligns the members to the lock the project
/// branch now holds. With `flags`, a diverged project branch is merged or
/// reset under the op record, and `resume` finishes the one a conflict
/// stopped.
pub fn run_pull(
    ctx: &WorkspaceContext,
    selection: &MemberSelection,
    jobs: usize,
    output: OutputMode,
    activation: ActivateOptions,
    take: Take,
) -> anyhow::Result<()> {
    let resumed = match &take {
        Take::Continue => Some(Op::resume(ctx, OpVerb::Pull)?),
        Take::Fresh(_) => {
            crate::op_state::check_no_op_in_progress(&[ctx.active_path()])?;
            None
        }
    };
    let fetched = match &resumed {
        Some(_) => None,
        None => {
            let everything = MemberSelection::new(RepoFilter::parse(&[], &[])?, false);
            let fetched = fetch_repos(ctx, &everything, jobs, output)?;
            if fetched.unconfirmed > 0 {
                crate::refuse!(
                    RefusalKind::PartialRunAborted,
                    "pull could not complete the fetch of {} repo(s) out of {}; nothing was \
                     taken in. `rwv update` aligns the members to the lock as it stands",
                    fetched.unconfirmed,
                    fetched.records.len()
                );
            }
            Some(fetched)
        }
    };
    let project_name = match &ctx.checkout {
        Checkout::Primary { .. } => ctx.require_active_project_on_disk()?.clone(),
        Checkout::Workweave { project, .. } => project.clone(),
    };
    crate::branch::require_recorded(ctx)?;
    let how = match (&take, resumed) {
        (Take::Fresh(flags), None) => How {
            flags: flags.clone(),
            resumed: None,
            activation,
            jobs,
        },
        (_, Some((op, project, record))) => {
            if project != project_name {
                return Err(crate::untokened!(
                    Owed,
                    "the op was started for project `{project}`, not `{project_name}`"
                ));
            }
            How {
                flags: Flags::default().recorded(&record),
                resumed: Some((op, record)),
                activation,
                jobs,
            }
        }
        (Take::Continue, None) => unreachable!("a continue resumes an op"),
    };
    let project = take_in_project(ctx, &project_name, output, &how)?;

    let run = update_members(ctx, selection, jobs, output, activation)?;
    if output.is_json() {
        PullJsonOutput {
            schema: PULL_SCHEMA_URL.to_owned(),
            fetched: fetched.map(|fetched| fetched.records).unwrap_or_default(),
            project,
            outcomes: run.envelope.outcomes,
            judged: run.envelope.judged,
            placements: run.envelope.placements,
            resolution: Some(ctx.resolution()),
            skipped: run.envelope.skipped,
        }
        .print()?;
    }
    run.tail
}

/// What `pull` was asked to do for a diverged branch.
pub enum Take {
    Fresh(Flags),
    Continue,
}

struct How {
    flags: Flags,
    resumed: Option<(Op, TakeInRecord)>,
    activation: ActivateOptions,
    jobs: usize,
}

/// Every repo a merge or reset may move: the project repo and each member
/// checked out here.
fn savepoint_repos(ctx: &WorkspaceContext, project: &ProjectName) -> Vec<(PathBuf, Box<dyn Vcs>)> {
    let dir = project_dir(ctx.active_path(), project);
    let mut repos: Vec<(PathBuf, Box<dyn Vcs>)> = vec![(dir.clone(), project_vcs())];
    if let Ok(manifest) = Manifest::from_path(&dir.join(Manifest::FILE_NAME)) {
        for (path, entry) in manifest.iter_entries() {
            let member = ctx.active_path().join(path.as_path());
            if member.exists() {
                repos.push((member, vcs_for(entry.vcs_type)));
            }
        }
    }
    repos
}

/// Take in the project repo's branch from its upstream: fast-forward it where
/// it is behind, and merge or reset it where it has diverged and the flags
/// say so; report what the commits it took in changed.
fn take_in_project(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    output: OutputMode,
    how: &How,
) -> anyhow::Result<ProjectTakeInOutput> {
    let dir = project_dir(ctx.active_path(), project);
    let path = project_rel_path(project);
    let vcs = project_vcs();
    let say = |line: String| {
        if output == OutputMode::Text {
            outln!("rwv pull: {line}");
        }
    };
    let mut taken = ProjectTakeInOutput {
        path: path.clone(),
        branch: None,
        result: TakeIn::Detached,
        from: None,
        to: None,
        backup: None,
        re_pinned: Vec::new(),
        member_changes: Vec::new(),
    };

    let attached = match vcs.head_attachment(&dir)? {
        HeadAttachment::Attached(attached) => attached,
        HeadAttachment::Detached(_) | HeadAttachment::Unborn(_) => {
            say(format!("{path}: on no branch, so nothing to take in"));
            return Ok(taken);
        }
    };
    taken.branch = Some(attached.to_string());
    let recorded_upstream = how
        .resumed
        .as_ref()
        .and_then(|(_, record)| record.upstream_tips.get(&RepoKey::Project))
        .map(|tip| ResolvedRevisionId::from(tip.clone()));
    let upstream = match recorded_upstream {
        Some(tip) => Some(tip),
        None => vcs.upstream_tip(&attached)?,
    };
    let Some(upstream) = upstream else {
        taken.result = TakeIn::NoUpstream;
        say(format!("{path}: '{attached}' has no upstream to take in"));
        return Ok(taken);
    };
    let head = vcs.head_revision(&dir)?;
    let operand = RepoOperand::Project(project.clone());
    for named in &how.flags.reset_to_upstream {
        if *named != operand {
            crate::refuse!(
                RefusalKind::InapplicableFlag,
                "`rwv pull --reset-to-upstream` resets the project repo, `{operand}`; `{named}` \
                 is reset by `rwv update --remote --reset-to-upstream {named}`"
            );
        }
    }
    let contains_upstream = vcs.is_ancestor(&dir, &upstream, &head)?;
    let reset = how.flags.resets(&operand) && head != upstream;
    if head == upstream && !reset {
        taken.result = TakeIn::Even;
        say(format!("{path}: '{attached}' is at its upstream"));
        return Ok(taken);
    }
    let diverged = !contains_upstream && !vcs.is_ancestor(&dir, &head, &upstream)?;
    if contains_upstream && !reset && how.resumed.is_none() {
        taken.result = TakeIn::Ahead;
        say(format!(
            "{path}: '{attached}' is ahead of its upstream, so nothing to take in"
        ));
        return Ok(taken);
    }
    if reset {
        return reset_project(
            ctx, project, &dir, &attached, &upstream, &head, how, taken, &say,
        );
    }
    if diverged || (contains_upstream && how.resumed.is_some()) {
        if !how.flags.merge_upstream {
            let (ahead, behind) = vcs.ahead_behind(&dir, upstream.id(), head.id());
            let mut judging = State::PushBranchDiverged
                .of(&path)
                .project_repo()
                .checkout(&dir)
                .bind("<repo>", &path)
                .subject(format!("{ahead} ahead, {behind} behind"));
            let url_remote = vcs.project_url_remote(&dir).ok().flatten();
            if !crate::push_branch::upstream_is_push_branch(
                vcs.as_ref(),
                &dir,
                &RawRefName::new(attached.to_string()),
                url_remote.as_ref(),
            ) {
                judging = judging.only(&[crate::diagnosis::ChoiceId::C2]);
            }
            return Err(judging.judge(vcs.as_ref()).refused());
        }
        return merge_project(
            ctx, project, &dir, &attached, &upstream, &head, how, taken, &say,
        );
    }

    let mut report = Report::default();
    crate::switch::ProjectMove {
        project,
        dir: &dir,
        detached: false,
        verb: "pull",
        refused_files: &[Manifest::FILE_NAME, LockFile::FILE_NAME],
        destination: Some(&upstream),
    }
    .judge(vcs.as_ref(), &mut report)?;
    report.into_result()?;

    let before = Recorded::read(&dir)?;
    vcs.advance_attached_ref(&attached, &upstream)?;
    crate::lock::report_left_edits(vcs.as_ref(), project, &dir, &BTreeSet::new())?;
    let after = Recorded::read(&dir)?;
    taken.result = TakeIn::Advanced;
    taken.from = Some(head.id().as_str().to_owned());
    taken.to = Some(upstream.id().as_str().to_owned());
    say(format!(
        "{path}: took '{attached}' from {} to {}",
        head.id().short(),
        upstream.id().short()
    ));

    taken.re_pinned = re_pinned(ctx, &before, &after);
    for moved in &taken.re_pinned {
        say(format!(
            "re-pinned {}: {} -> {}",
            moved.path,
            crate::vcs::short_revision(&moved.from),
            crate::vcs::short_revision(&moved.to)
        ));
    }
    taken.member_changes = member_changes(&before.manifest, &after.manifest);
    for change in &taken.member_changes {
        say(describe_change(change));
    }
    Ok(taken)
}

fn describe_change(change: &MemberChangeOutput) -> String {
    match change.change {
        EntryChange::Added => format!("member-changed {}: added", change.path),
        EntryChange::Removed => format!("member-changed {}: removed", change.path),
        EntryChange::Changed => format!(
            "member-changed {}: {}",
            change.path,
            change.fields.join(", ")
        ),
    }
}

/// The members `after`'s lock pins at a commit that does not descend from the
/// one `before`'s pinned. A pin either clone cannot place is not reported:
/// nothing here can tell which way it moved.
fn re_pinned(ctx: &WorkspaceContext, before: &Recorded, after: &Recorded) -> Vec<RePinnedOutput> {
    let (Some(old), Some(new)) = (&before.lock, &after.lock) else {
        return Vec::new();
    };
    let mut moved = Vec::new();
    for (path, now) in new.iter_entries() {
        let Some(was) = old.get_entry(path) else {
            continue;
        };
        if was.version == now.version {
            continue;
        }
        let Some(entry) = after.manifest.get_entry(path) else {
            continue;
        };
        let vcs = vcs_for(entry.vcs_type);
        let dir = ctx.primary_path().join(path.as_path());
        let (Ok(from), Ok(to)) = (
            vcs.resolve_revision(&dir, was.version.as_str()),
            vcs.resolve_revision(&dir, now.version.as_str()),
        ) else {
            continue;
        };
        if !matches!(vcs.is_ancestor(&dir, &from, &to), Ok(true)) {
            moved.push(RePinnedOutput {
                path: path.to_string(),
                from: from.id().as_str().to_owned(),
                to: to.id().as_str().to_owned(),
            });
        }
    }
    moved
}

/// Each member entry `after` adds, removes or changes against `before`.
fn member_changes(before: &Manifest, after: &Manifest) -> Vec<MemberChangeOutput> {
    let paths: BTreeSet<&RepoPath> = before
        .iter_entries()
        .chain(after.iter_entries())
        .map(|(path, _)| path)
        .collect();
    paths
        .into_iter()
        .filter_map(
            |path| match (before.get_entry(path), after.get_entry(path)) {
                (None, Some(_)) => Some((path, EntryChange::Added, Vec::new())),
                (Some(_), None) => Some((path, EntryChange::Removed, Vec::new())),
                (Some(was), Some(now)) => {
                    let fields = differing_fields(was, now);
                    (!fields.is_empty()).then_some((path, EntryChange::Changed, fields))
                }
                (None, None) => None,
            },
        )
        .map(|(path, change, fields)| MemberChangeOutput {
            path: path.to_string(),
            change,
            fields,
        })
        .collect()
}

fn differing_fields(was: &RepoEntry, now: &RepoEntry) -> Vec<String> {
    let mut fields = Vec::new();
    let mut note = |name: &str, was: String, now: String| {
        if was != now {
            fields.push(format!("{name}: {was} -> {now}"));
        }
    };
    note("url", was.url.to_string(), now.url.to_string());
    note("branch", was.branch.to_string(), now.branch.to_string());
    note("role", was.role.to_string(), now.role.to_string());
    note(
        "recurse-submodules",
        bool::from(was.recurse_submodules).to_string(),
        bool::from(now.recurse_submodules).to_string(),
    );
    fields
}

#[allow(clippy::too_many_arguments)]
fn reset_project(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    dir: &Path,
    attached: &AttachedRef,
    upstream: &ResolvedRevisionId,
    head: &ResolvedRevisionId,
    how: &How,
    mut taken: ProjectTakeInOutput,
    say: &dyn Fn(String),
) -> anyhow::Result<ProjectTakeInOutput> {
    let vcs = project_vcs();
    let path = project_rel_path(project);
    let branch = RawRefName::new(attached.to_string());
    let tracked = vcs.tracked_dirty_file_names(dir)?;
    let mut report = Report::default();
    crate::merge_upstream::judge_reset(
        &path,
        dir,
        vcs.as_ref(),
        Some(upstream),
        &crate::merge_upstream::dependents_of(vcs.as_ref(), dir, attached.to_string().as_str())?,
        judge_edits(vcs.as_ref(), &path, dir, tracked, None),
        &mut report,
    );
    report.into_result()?;
    let repos = savepoint_repos(ctx, project);
    let op = match &how.resumed {
        Some(_) => None,
        None => Some(Op::begin(
            ctx.active_path(),
            project,
            OpVerb::Pull,
            how.flags.to_record(),
            &repos,
        )?),
    };
    let op = op.context("an op that reset the project repo has nothing left to resume")?;
    op.record_upstream(RepoKey::Project, upstream.id())?;
    let backup = crate::merge_upstream::reset_to_upstream(
        ctx.active_path(),
        RepoKey::Project,
        vcs.as_ref(),
        dir,
        &branch,
        Some(attached),
        upstream,
    )?;
    op.finish(&repos);
    taken.result = TakeIn::Reset;
    taken.from = Some(head.id().as_str().to_owned());
    taken.to = Some(upstream.id().as_str().to_owned());
    taken.backup = Some(backup.clone());
    say(format!(
        "{path}: reset '{attached}' from {} to its upstream {}, keeping {} in backup {backup}",
        head.id().short(),
        upstream.id().short(),
        head.id().short()
    ));
    Ok(taken)
}

#[allow(clippy::too_many_arguments)]
fn merge_project(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    dir: &Path,
    attached: &AttachedRef,
    upstream: &ResolvedRevisionId,
    head: &ResolvedRevisionId,
    how: &How,
    mut taken: ProjectTakeInOutput,
    say: &dyn Fn(String),
) -> anyhow::Result<ProjectTakeInOutput> {
    let vcs = project_vcs();
    let path = project_rel_path(project);
    let repos = savepoint_repos(ctx, project);
    let lineage = crate::feature_sync::feature_lineage(ctx, project)?;
    let sides = Sides {
        first: upstream.id().clone(),
        second: head.id().clone(),
    };
    let began;
    let op = match &how.resumed {
        Some((op, _)) => op,
        None => {
            let tracked = vcs.tracked_dirty_file_names(dir)?;
            let mut report = Report::default();
            report.extend(judge_edits(vcs.as_ref(), &path, dir, tracked, None));
            if let (Some(lineage), Some((merged, merging))) = (
                &lineage,
                crate::project_merge::member_merges_planned(vcs.as_ref(), dir, &sides)?,
            ) {
                lineage.judge_member_merges(
                    ctx.active_path(),
                    &merged,
                    &merging,
                    &path,
                    &mut report,
                )?;
            }
            report.into_result()?;
            began = Op::begin(
                ctx.active_path(),
                project,
                OpVerb::Pull,
                how.flags.to_record(),
                &repos,
            )?;
            &began
        }
    };
    if how.resumed.is_none() {
        op.record_upstream(RepoKey::Project, upstream.id())?;
    }
    let merged = if vcs.is_ancestor(dir, upstream, head)? {
        head.id().clone()
    } else {
        ProjectMerge {
            ctx,
            project,
            verb: "pull",
            resume: crate::op_state::resume_command(OpVerb::Pull),
            jobs: how.jobs,
            opts: how.activation,
            message: format!("Merge the upstream of {attached} into {attached}"),
            head: crate::lock_merge::Parent::Second,
            members: crate::project_merge::MemberMerging::BothChanged(lineage),
        }
        .run(&sides)
        .and_then(|(commit, pruning)| {
            crate::sync::fail_on_refused_prunes(&pruning)?;
            Ok(commit)
        })?
    };
    op.finish(&repos);
    taken.result = TakeIn::Merged;
    taken.from = Some(head.id().as_str().to_owned());
    taken.to = Some(merged.as_str().to_owned());
    say(format!(
        "{path}: merged '{attached}' with its upstream {}, into {}",
        upstream.id().short(),
        merged.short()
    ));
    Ok(taken)
}
