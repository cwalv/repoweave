pub mod cargo_workspace;
pub mod gita;
pub mod go_work;
pub mod merge;
pub mod npm_workspaces;
pub mod pnpm_workspaces;
pub mod static_files;
pub mod uv_workspace;
pub mod vscode_workspace;

use crate::integration::Integration;
use crate::tool_program::ToolProgram;

pub use cargo_workspace::CargoWorkspace;
pub use gita::Gita;
pub use go_work::GoWork;
pub use npm_workspaces::NpmWorkspaces;
pub use pnpm_workspaces::PnpmWorkspaces;
pub use static_files::StaticFiles;
pub use uv_workspace::UvWorkspace;
pub use vscode_workspace::VscodeWorkspace;

/// Returns all built-in integrations.
pub fn builtin_integrations() -> Vec<Box<dyn Integration>> {
    vec![
        Box::new(NpmWorkspaces),
        Box::new(PnpmWorkspaces),
        Box::new(GoWork),
        Box::new(UvWorkspace),
        Box::new(CargoWorkspace),
        Box::new(Gita),
        Box::new(VscodeWorkspace),
        Box::new(StaticFiles),
    ]
}

/// The `tool-missing` finding for the tool `integration` declares, when it is
/// not on `PATH`. `consequence` follows the sentence, for an integration that
/// carries on without the tool.
pub(crate) fn tool_missing_issue<I: crate::integration::DrivesTool>(
    integration: &I,
    consequence: &str,
) -> Option<crate::integration::Issue> {
    if ToolProgram::of(I::TOOL).is_some() {
        return None;
    }
    Some(crate::integration::Issue {
        source: crate::integration::IssueSource::integration(integration.name()),
        severity: crate::integration::Severity::Warning,
        message: format!("{} is not on PATH{consequence}", I::TOOL).into(),
        kind: crate::integration::IssueKind::ToolMissing,
        safe_to_fix: true,
    })
}

/// The working directory an ecosystem tool can stand in: the Windows
/// verbatim (`\\?\`) spelling a workspace root read from a record can carry
/// is a spelling many tools' own relative joins cannot survive — a `/`-separated
/// component under a verbatim prefix is an invalid path, so a tool spawned
/// there fails to find its own config (cargo's `.cargo/config.toml`
/// discovery is the measured case). `dunce::simplified` drops the prefix
/// only where Windows itself accepts the short form and is the identity
/// everywhere else — the same strip the git-argv seam applies.
pub(crate) fn subprocess_cwd(root: &std::path::Path) -> &std::path::Path {
    dunce::simplified(root)
}

/// Run an install hook's tool from the weave root `root`, or `Ok(None)` without
/// spawning anything when `tool` is not on `PATH`.
///
/// The lookup comes first because a spawn error cannot say which input was
/// missing: a working directory that does not exist fails with the same
/// not-found kind as a program that does not.
///
/// The tool's stdout goes to rwv's stderr: a verb that runs hooks may owe its
/// own stdout to a machine — `--json`, or the bare path `workweave create
/// --hook-mode` prints for the agent runtime that reads it.
pub(crate) fn run_install_tool(
    tool: crate::integration::Tool,
    args: &[&str],
    root: &std::path::Path,
) -> anyhow::Result<Option<std::process::ExitStatus>> {
    use anyhow::Context;
    let Some(program) = ToolProgram::of(tool) else {
        return Ok(None);
    };
    program
        .command()
        .args(args)
        .current_dir(subprocess_cwd(root))
        .stdout(std::io::stderr())
        .status()
        .map(Some)
        .with_context(|| format!("failed to run {program}"))
}
