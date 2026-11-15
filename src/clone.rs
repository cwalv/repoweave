//! `rwv clone` — clone a project and its members into the workspace.
//!
//! The source must be a URL or a `<registry>/<owner>/<repo>` shorthand. A
//! plain path is refused: a local remote is spelled `file://`, and a project
//! already in the weave is brought up with `rwv update`.

use crate::activate::ActivateOptions;
use crate::diagnosis::{Judged, JudgedOutput, Report, State};
use crate::exit_status::CompletedWithRemainder;
use crate::member_selection::MemberSelection;
use crate::parallel::OutputMode;
use crate::placement::PlacementOutput;
use crate::refusal::RefusalKind;
use crate::registry;
use crate::skipped::SkippedEffect;
use crate::update::{bring_members, CloneDuty, MemberOutcomeOutput, MemberPin};
use crate::vcs::{project_vcs, vcs_for, PinOutcome};
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

/// Remove the project repo this clone just made at `project_dir`, which the
/// clone refused to start in when anything was there.
fn take_back_clone(project_dir: &Path) -> anyhow::Result<()> {
    std::fs::remove_dir_all(project_dir)
        .with_context(|| format!("failed to remove {}", project_dir.display()))
}

/// Resolve `source` to a clone URL.
///
/// Accepts full URLs (returned as-is) or `registry/owner/repo` shorthand
/// (resolved via the built-in registries to an HTTPS clone URL).
fn resolve_source(source: &str) -> anyhow::Result<registry::CloneUrl> {
    let parsed: crate::manifest::RepoUrl = source.parse()?;
    if parsed.is_plain_path() {
        return Err(registry::unresolvable_source(
            Some("rwv clone"),
            source,
            Some(
                "To bring up the members of a project already in this weave at \
                 projects/<name>/, drop SOURCE: `rwv update --project <name>`."
                    .to_owned(),
            ),
        ));
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
    init_manifest: bool,
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
    if init_manifest {
        crate::init::judge_commit_identity(project_vcs.as_ref(), workspace_root)?;
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

    let has_manifest = project_dir
        .join(crate::manifest::Manifest::FILE_NAME)
        .exists();
    match (has_manifest, init_manifest) {
        (true, false) => {}
        (true, true) => {
            take_back_clone(&project_dir)?;
            crate::refuse!(
                RefusalKind::InapplicableFlag,
                "`--init-manifest` writes the `rwv.toml` a repository lacks, and {source} has \
                 one. Nothing was left on disk. Run `rwv clone {source}`."
            );
        }
        (false, false) => {
            take_back_clone(&project_dir)?;
            crate::refuse!(
                RefusalKind::NotAProject,
                "{source} holds no `rwv.toml`, so it is not a project (an empty repository \
                 holds none). Nothing was left on disk. To make it one, run `rwv clone \
                 {source} --init-manifest`."
            );
        }
        (false, true) => {
            crate::init::make_cloned_repo_a_project(
                project_vcs.as_ref(),
                workspace_root,
                &name,
                &project_dir,
                source,
            )?;
        }
    }

    crate::branch::record_trunk(project_vcs.as_ref(), &name, &project_dir).with_context(|| {
        format!(
            "could not record the branch {} is on; once the cause is fixed, run \
                 `rwv branch set-mode <branch> declared` in it",
            project_dir.display()
        )
    })?;

    project_vcs
        .define_lock_driver(&project_dir)
        .with_context(|| {
            format!(
                "failed to define the lock merge driver in {}",
                project_dir.display()
            )
        })?;

    let run = match bring_members(
        "clone",
        &project_dir,
        workspace_root,
        selection,
        jobs,
        output,
        None,
        CloneDuty::Checkouts,
        true,
    ) {
        Ok(run) => run,
        Err(aborted) => {
            if let (true, Some(report)) = (output.is_json(), aborted.report) {
                let _ = report.into_clone_envelope(Vec::new()).print();
            }
            return Err(aborted.error);
        }
    };
    let left = judge_left(&run.pins, workspace_root);
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
fn judge_left(pins: &[MemberPin], workspace: &std::path::Path) -> Vec<Judged> {
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
                    .checkout(&workspace.join(pin.path.as_path()))
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
    fn resolve_source_resolves_registry_shorthand() {
        let url = resolve_source("github/cwalv/repoweave").unwrap();
        assert_eq!(url.to_string(), "https://github.com/cwalv/repoweave.git");
    }

    #[test]
    fn resolve_source_refuses_two_segments_naming_the_shorthand_and_file_url() {
        let refused = resolve_source("cwalv/repoweave").unwrap_err();
        assert_eq!(
            crate::refusal::kind_of(&refused),
            Some(RefusalKind::UnresolvableRepoSource)
        );
        let message = format!("{refused:#}");
        assert!(message.contains("`<registry>/<owner>/<repo>`"), "{message}");
        assert!(message.contains("github, gitlab, bitbucket"), "{message}");
        assert!(message.contains("file://"), "{message}");
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
