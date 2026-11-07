//! `rwv pull` — fetch, fast-forward the project branch, then update.
//!
//! The project branch moves only forward: a branch that is behind its
//! upstream takes it in, one that is even or ahead has nothing to take, and
//! one that has diverged is refused with nothing changed. The members are
//! then aligned to the lock the pulled commit brought.

use crate::activate::ActivateOptions;
use crate::cli::consent::DetachConsent;
use crate::diagnosis::{judge_edits, Report, State};
use crate::fetch::{fetch_repos, FetchedRepoOutput};
use crate::manifest::{LockFile, Manifest, ProjectName, RepoEntry, RepoPath};
use crate::member_selection::MemberSelection;
use crate::parallel::OutputMode;
use crate::placement::PlacementOutput;
use crate::refusal::RefusalKind;
use crate::selector::RepoFilter;
use crate::skipped::SkippedEffect;
use crate::update::{update_members, MemberOutcomeOutput};
use crate::vcs::{project_vcs, vcs_for, HeadAttachment};
use crate::workspace::{project_dir, project_rel_path, Checkout, Resolution, WorkspaceContext};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

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
    /// The branch's tip before. Present for `advanced`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The branch's tip after. Present for `advanced`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
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
/// The fetch moves no branch; the fast-forward moves the project branch and
/// nothing else; the update aligns the members to the lock the project
/// branch now holds.
pub fn run_pull(
    ctx: &WorkspaceContext,
    selection: &MemberSelection,
    detach_checkouts: Option<DetachConsent>,
    jobs: usize,
    output: OutputMode,
    activation: ActivateOptions,
) -> anyhow::Result<()> {
    let everything = MemberSelection::new(RepoFilter::parse(&[], &[])?, false);
    let fetched = fetch_repos(ctx, &everything, jobs, output)?;
    if fetched.unconfirmed > 0 {
        crate::refuse!(
            RefusalKind::PartialRunAborted,
            "pull could not complete the fetch of {} repo(s) out of {}; nothing was taken in. \
             `rwv update` aligns the members to the lock as it stands",
            fetched.unconfirmed,
            fetched.records.len()
        );
    }
    let project_name = match &ctx.checkout {
        Checkout::Primary { .. } => ctx.require_active_project_on_disk()?.clone(),
        Checkout::Workweave { project, .. } => project.clone(),
    };
    let project = take_in_project(ctx, &project_name, output)?;

    let run = update_members(ctx, selection, detach_checkouts, jobs, output, activation)?;
    if output.is_json() {
        PullJsonOutput {
            schema: PULL_SCHEMA_URL.to_owned(),
            fetched: fetched.records,
            project,
            outcomes: run.envelope.outcomes,
            placements: run.envelope.placements,
            resolution: Some(ctx.resolution()),
            skipped: run.envelope.skipped,
        }
        .print()?;
    }
    run.tail
}

/// Fast-forward the project repo's branch to its upstream when it is behind,
/// and report what the commits it took in changed.
fn take_in_project(
    ctx: &WorkspaceContext,
    project: &ProjectName,
    output: OutputMode,
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
    let Some(upstream) = vcs.upstream_tip(&attached)? else {
        taken.result = TakeIn::NoUpstream;
        say(format!("{path}: '{attached}' has no upstream to take in"));
        return Ok(taken);
    };
    let head = vcs.head_revision(&dir)?;
    if head == upstream {
        taken.result = TakeIn::Even;
        say(format!("{path}: '{attached}' is at its upstream"));
        return Ok(taken);
    }
    if vcs.is_ancestor(&dir, &upstream, &head)? {
        taken.result = TakeIn::Ahead;
        say(format!(
            "{path}: '{attached}' is ahead of its upstream, so nothing to take in"
        ));
        return Ok(taken);
    }
    if !vcs.is_ancestor(&dir, &head, &upstream)? {
        let (ahead, behind) = vcs.ahead_behind(&dir, upstream.id(), head.id());
        return Err(State::PushBranchDiverged
            .of(&path)
            .subject(format!("{ahead} ahead, {behind} behind"))
            .judge(vcs.as_ref())
            .refused());
    }

    let tracked = vcs.tracked_dirty_file_names(&dir)?;
    let mut report = Report::default();
    if let Some(edits) = judge_edits(vcs.as_ref(), &path, &dir, tracked, None) {
        report.add(edits);
    }
    report.into_result()?;

    let before = Recorded::read(&dir)?;
    vcs.advance_attached_ref(&attached, &upstream)?;
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
