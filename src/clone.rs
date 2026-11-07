//! `rwv clone` — clone a project and its members into the workspace.
//!
//! The source must be a URL (full URL or `owner/repo` shorthand resolved via
//! the registry). A plain path is refused: a local remote is spelled
//! `file://`, and a project already in the weave is brought up with
//! `rwv update`.

use crate::activate::ActivateOptions;
use crate::diagnosis::{Judged, JudgedOutput, Report, State};
use crate::exit_status::CompletedWithRemainder;
use crate::manifest::LockFile;
use crate::member_selection::MemberSelection;
use crate::parallel::OutputMode;
use crate::placement::PlacementOutput;
use crate::refusal::RefusalKind;
use crate::registry;
use crate::skipped::SkippedEffect;
use crate::update::{bring_members, CloneDuty, MemberOutcomeOutput, MemberPin};
use crate::vcs::{project_vcs, vcs_for, PinOutcome, ReplayExclusionState};
use crate::workspace::{projects_dir, Resolution};
use anyhow::Context;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Schema URL embedded in `rwv clone --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/clone.json`.
pub const CLONE_SCHEMA_URL: &str = crate::schema_url::schema_url!("clone");

/// Top-level envelope for `rwv clone --json`.
///
/// Shape: `{ "$schema": "<url>", "outcomes": [...], "left": [...] }`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CloneJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub outcomes: Vec<MemberOutcomeOutput>,
    /// The members the clone completed without a commit to place them at: the
    /// lock has no entry for them (`pin` `none`), or pins a commit no ref in
    /// the clone reaches (`pin` `unreachable`). Empty, and the exit status
    /// `0`, when every member sits at its pin.
    pub left: Vec<JudgedOutput>,
    /// How the files of each checkout the clone created were placed. Empty,
    /// not absent, when it placed none.
    pub placements: Vec<PlacementOutput>,
    /// Resolved workspace coordinates (weave root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// The materialization side effects this clone skipped, each naming the
    /// command that performs it. Empty, not absent, when it skipped none.
    #[serde(default)]
    pub skipped: Vec<SkippedEffect>,
}

impl CloneJsonOutput {
    fn print(&self) -> anyhow::Result<()> {
        let out = serde_json::to_string_pretty(self).context("failed to serialize clone output")?;
        outln!("{out}");
        Ok(())
    }
}

/// Resolve `source` to a clone URL.
///
/// Accepts full URLs (returned as-is) or `owner/repo` / `registry/owner/repo`
/// shorthand (resolved via the built-in registries to an HTTPS clone URL).
fn resolve_source(source: &str) -> anyhow::Result<registry::CloneUrl> {
    let parsed: crate::manifest::RepoUrl = source.parse()?;
    if parsed.is_plain_path() {
        crate::refuse!(
            RefusalKind::UnresolvableRepoSource,
            "rwv clone: '{source}' is a plain path, which names no remote to clone the \
             project from. To clone from a local remote, spell it as a URL: \
             `rwv clone {file_url}`. To bring up the members of a project already in this \
             weave at projects/<name>/, drop SOURCE: `rwv update --project <name>`.",
            file_url = crate::path_spelling::file_url_suggestion(source),
        );
    }
    Ok(registry::resolve_to_clone_info(&parsed)?.url)
}

/// Run `rwv clone`: clone the project source, then each member at the commit
/// its lock pins, activating the project when none is active.
///
/// `workspace_root` is the directory where repos and `projects/` live (CWD).
///
/// The lock is read and never written. A member it has no entry for, or whose
/// entry no ref in the clone reaches, is cloned at the tip of its declared
/// branch and listed as left: the run completes, and returns
/// [`CompletedWithRemainder`] once it has printed them.
///
/// `jobs` is the resolved worker count (post-[`crate::parallel::resolve_jobs`]).
/// `jobs == 1` runs serially with no prefix; `jobs > 1` runs the per-repo
/// clone/checkout loop on a bounded worker pool, prefixing each line with
/// the repo path. Project-level steps (cloning the project repo, activation)
/// happen serially.
///
/// Under `--json`, one `{ "$schema": ..., "outcomes": [...] }` envelope
/// reaches stdout after all repos complete, its records sorted by repo path.
pub fn run_clone(
    source: &str,
    workspace_root: &Path,
    selection: &MemberSelection,
    jobs: usize,
    output: OutputMode,
    activation: ActivateOptions,
) -> anyhow::Result<()> {
    let project_vcs = project_vcs();

    let url = resolve_source(source)?;
    let name = crate::manifest::ProjectName::new(registry::repo_name_from_source(url.as_str()))?;
    let projects_dir = projects_dir(workspace_root);
    std::fs::create_dir_all(&projects_dir).context("failed to create projects/ directory")?;
    if let Some(crate::workspace::RootObservation::Primary { pointer, .. }) =
        crate::workspace::observe_root(workspace_root)
    {
        pointer.selection()?;
    }
    let project_dir = crate::workspace::project_dir(workspace_root, &name);
    if project_dir.exists() {
        crate::refuse!(
            RefusalKind::ProjectDirOccupied,
            "cannot clone project '{name}': {}\n\
             Hint: if that is this project, `rwv update --project {name}` brings up its \
             members in place. To clone this one beside it, run `rwv clone {source}` in an \
             empty directory, which becomes a weave of its own.",
            crate::workspace::describe_existing(&project_dir)
        );
    }
    if output.is_json() {
        eprintln!("rwv clone: cloning project '{name}'");
    } else {
        outln!("rwv clone: cloning project '{name}'");
    }
    let url_remote = project_vcs
        .clone_repo(&url, &project_dir)
        .with_context(|| format!("failed to clone project source '{url}'"))?;
    project_vcs
        .record_project_url_remote(&project_dir, &url_remote)
        .with_context(|| {
            format!(
                "failed to record the url remote in {}",
                project_dir.display()
            )
        })?;

    crate::branch::record_trunk(project_vcs.as_ref(), &name, &project_dir).with_context(|| {
        format!(
            "could not record the branch {} is on; once the cause is fixed, run \
                 `rwv branch set-mode <branch> declared` in it",
            project_dir.display()
        )
    })?;

    // Unlike `init`/`init_adopt`, clone never calls `set_replay_exclusion` —
    // it clones a project that already exists, and whether `rwv.lock` is
    // declared derived is that project's own prior choice. Complete the
    // config only when the clone already carries the declaration; otherwise
    // there is no `rwv-ours` assignment for the driver to serve.
    let declares_rwv_ours = matches!(
        project_vcs
            .replay_exclusion_state(&project_dir, Path::new(LockFile::FILE_NAME))
            .with_context(|| format!(
                "failed to read .gitattributes in {}",
                project_dir.display()
            ))?,
        ReplayExclusionState::Current | ReplayExclusionState::LegacyAlongsideCurrent
    );
    if declares_rwv_ours {
        crate::git::plant_rwv_merge_driver_config(&project_dir)
            .context("failed to plant merge.rwv-ours config")?;
    }

    let run = match bring_members(
        "clone",
        &project_dir,
        workspace_root,
        selection,
        jobs,
        output,
        None,
        CloneDuty::Checkouts,
    ) {
        Ok(run) => run,
        Err(aborted) => {
            if let (true, Some(report)) = (output.is_json(), aborted.report) {
                let _ = report.into_clone_envelope(Vec::new()).print();
            }
            return Err(aborted.error);
        }
    };
    let left = judge_left(&run.pins);
    let mut report = run.report;
    crate::alignment::record(workspace_root, &name)?;

    let ctx = crate::workspace::WorkspaceContext::resolve_unbound(workspace_root)?;
    let active = match ctx.primary_identity() {
        Some(primary) => primary.pointer().selection()?.cloned(),
        None => None,
    };
    match active {
        Some(active) => {
            let skip = SkippedEffect::activation_while_active(&active, &name);
            if output.is_json() {
                eprintln!("{}", skip.line());
            } else {
                outln!("{}", skip.line());
            }
            report.skipped.push(skip);
        }
        None => {
            report
                .skipped
                .extend(crate::activate::activate_with_options(
                    &name, &ctx, activation,
                )?);
        }
    }

    let remainder = !left.is_empty();
    if output.is_json() {
        let left = left.iter().map(Judged::output).collect();
        report.into_clone_envelope(left).print()?;
    } else if remainder {
        let mut listed = Report::default();
        for judged in left {
            listed.add(judged);
        }
        outln!("{}", listed.render("clone").trim_end());
    }
    if remainder {
        return Err(CompletedWithRemainder.into());
    }
    Ok(())
}

/// Each member the clone placed without its pin, judged: `pin` `none` where
/// the lock has no entry, `unreachable` where no ref reaches the entry.
fn judge_left(pins: &[MemberPin]) -> Vec<Judged> {
    pins.iter()
        .filter_map(|pin| {
            let state = match pin.outcome {
                PinOutcome::Reached => return None,
                PinOutcome::None => State::PinNone,
                PinOutcome::Unreachable => State::PinUnreachable,
            };
            let vcs = vcs_for(pin.vcs_type);
            Some(
                state
                    .of(&pin.path)
                    .bind("<m>", &pin.path)
                    .judge(vcs.as_ref()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_source_passes_through_urls() {
        let url = "https://github.com/org/repo.git";
        assert_eq!(resolve_source(url).unwrap().to_string(), url);
    }

    #[test]
    fn resolve_source_passes_through_ssh_urls() {
        let url = "git@github.com:org/repo.git";
        assert_eq!(resolve_source(url).unwrap().to_string(), url);
    }

    #[test]
    fn resolve_source_passes_through_file_urls() {
        let url = format!("file://{}", std::env::temp_dir().join("repo.git").display());
        assert_eq!(resolve_source(&url).unwrap().to_string(), url);
    }

    #[test]
    fn resolve_source_resolves_two_part_shorthand() {
        let url = resolve_source("cwalv/repoweave").unwrap();
        assert_eq!(url.to_string(), "https://github.com/cwalv/repoweave.git");
    }

    #[test]
    fn resolve_source_resolves_three_part_shorthand() {
        let url = resolve_source("gitlab/org/proj").unwrap();
        assert_eq!(url.to_string(), "https://gitlab.com/org/proj.git");
    }

    #[test]
    fn resolve_source_rejects_invalid_shorthand() {
        assert!(resolve_source("not-a-valid-source").is_err());
    }

    #[test]
    fn resolve_source_rejects_four_part_shorthand() {
        assert!(resolve_source("a/b/c/d").is_err());
    }
}
