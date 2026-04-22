//! `rwv prime` — emit structured workspace context for agent system prompts.
//!
//! Prints markdown describing the current repoweave workspace: root path,
//! active project, workspace location (weave/workweave), repository table with roles,
//! enabled integrations, key commands, and directory layout.
//!
//! Silent (exit 0, no output) when not inside a repoweave workspace.

use crate::manifest::{Manifest, ProjectName, RepoPath, Role};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use std::path::Path;

/// Run `rwv prime` from the given working directory.
///
/// Returns `Ok(())` unconditionally. Prints nothing if not in a workspace,
/// unless `no_suppress` is true — in which case an orientation overview is
/// emitted instead so agents can orient themselves without per-workspace details.
pub fn prime(cwd: &Path, no_suppress: bool) -> anyhow::Result<()> {
    let ctx = match WorkspaceContext::resolve(cwd, None) {
        Ok(ctx) => ctx,
        Err(_) => {
            if no_suppress {
                print!("{}", render_overview());
            }
            return Ok(());
        }
    };

    let output = render_context(&ctx);
    print!("{output}");
    Ok(())
}

/// Render an orientation block for when CWD is not inside any weave or workweave.
///
/// Covers concept definitions (weave / workweave / rig), common agent pitfalls,
/// and a quick command reference. Intended for `--no-suppress` callers such as
/// session-start hooks running from a gc city directory.
pub fn render_overview() -> String {
    let mut out = String::new();

    out.push_str("# repoweave: orientation\n\n");
    out.push_str("> CWD is not inside a weave or workweave. ");
    out.push_str("No per-workspace details are available — this is not an error.\n\n");

    out.push_str("## Concepts\n\n");
    out.push_str("**Weave** — a directory that weaves multiple repository *threads* into a single workspace *fabric*. ");
    out.push_str("It contains repositories cloned under `{registry}/{owner}/{repo}/` and projects under `projects/{name}/`. ");
    out.push_str("Ecosystem workspace files and symlinks are ephemeral (regenerated on `rwv activate`); ");
    out.push_str("repos and projects are the persistent state. ");
    out.push_str("A weave is analogous to a `go.work` or Cargo `[workspace]`, with lock-based reproducibility and multi-ecosystem support.\n\n");

    out.push_str("**Workweave** — an ephemeral, isolated copy of a weave (the multi-repo equivalent of `git worktree`). ");
    out.push_str("Workweaves enable isolated parallel work or review across multiple repos without affecting the primary weave. ");
    out.push_str("Created with `rwv workweave PROJECT create NAME`; deleted with `rwv workweave PROJECT delete NAME`.\n\n");

    out.push_str("**Rig** — a session configuration in a Gas City `city.toml` that pairs a shell environment with a ");
    out.push_str("session provider (e.g. tmux or cloudcli) and optional integrations. ");
    out.push_str("Rigs are how gc agents get launched in an isolated session with the right CWD, environment, and capabilities.\n\n");

    out.push_str("## Common pitfalls\n\n");
    out.push_str("- Do not assume code lives in the city (gc) directory — the weave and workweave are separate from the city CWD.\n");
    out.push_str("- Do not confuse a *weave* (the primary workspace root) with a *workweave* (an isolated working copy).\n");
    out.push_str("- Do not `cd` into arbitrary paths before repo-scoped commands; run them from inside the weave or workweave directory.\n");
    out.push_str("- `rwv prime` without `--no-suppress` is intentionally silent outside a weave; absence of output is not an error.\n\n");

    out.push_str("## Essential commands\n\n");
    out.push_str("Run `rwv --help` for the full command reference.\n\n");
    out.push_str("| Command | Description |\n");
    out.push_str("|---------|-------------|\n");
    out.push_str("| `rwv` | Show workspace context |\n");
    out.push_str("| `rwv prime [--no-suppress]` | Emit structured context; `--no-suppress` always emits, even outside a weave |\n");
    out.push_str("| `rwv workweave PROJECT NAME` | Create a workweave (worktree-based workspace) |\n");
    out.push_str("| `rwv fetch SOURCE` | Clone a project and its repos |\n");
    out.push_str("| `rwv resolve` | Print effective root path |\n");
    out.push_str("| `rwv sync SOURCE` | Align CWD workspace with another workspace's committed rwv.lock |\n");
    out.push_str("| `rwv abort` | Restore CWD workspace to its pre-sync state |\n");
    out.push_str("| `rwv doctor --locked` | Zero exit iff every repo's tip matches its rwv.lock entry |\n");
    out.push_str("| `rwv status` | Show per-repo state of the CWD workspace |\n");

    out
}

/// Render the full markdown context string.
pub fn render_context(ctx: &WorkspaceContext) -> String {
    let mut out = String::new();

    out.push_str("# repoweave workspace\n\n");

    // -- Location ---------------------------------------------------------------
    let project: Option<&ProjectName> = match &ctx.location {
        WorkspaceLocation::Weave { project } => {
            out.push_str(&format!("- **Weave**: `{}`\n", ctx.root.display()));
            project.as_ref()
        }
        WorkspaceLocation::Workweave {
            name: _,
            dir,
            project,
        } => {
            out.push_str(&format!("- **Workweave**: `{}`\n", dir.display()));
            out.push_str(&format!("- **Weave**: `{}`\n", ctx.root.display()));
            Some(project)
        }
    };

    if let Some(p) = project {
        out.push_str(&format!("- **Project**: `{}`\n", p.as_str()));
    }

    // -- Repository table -------------------------------------------------------
    if let Some(p) = project {
        let manifest_path = ctx.root.join("projects").join(p.as_str()).join("rwv.yaml");
        if let Ok(manifest) = Manifest::from_path(&manifest_path) {
            out.push('\n');
            render_repo_table(&mut out, &manifest);
            render_integrations(&mut out, &manifest);
        }
    }

    // -- Key commands -----------------------------------------------------------
    out.push_str("\n## Key commands\n\n");
    out.push_str("| Command | Description |\n");
    out.push_str("|---------|-------------|\n");
    out.push_str("| `rwv` | Show workspace context |\n");
    out.push_str("| `rwv resolve` | Print effective root path |\n");
    out.push_str("| `rwv activate PROJECT` | Set active project, generate ecosystem configs |\n");
    out.push_str(
        "| `rwv workweave PROJECT NAME` | Create a workweave (worktree-based workspace) |\n",
    );
    out.push_str("| `rwv add URL [--role ROLE]` | Add a repo to the active project |\n");
    out.push_str("| `rwv remove PATH` | Remove a repo from the active project |\n");
    out.push_str("| `rwv lock` | Snapshot repo versions to rwv.lock |\n");
    out.push_str("| `rwv doctor` | Run convention enforcement |\n");
    out.push_str("| `rwv fetch SOURCE` | Clone a project and its repos |\n");

    // -- Directory layout -------------------------------------------------------
    render_directory_layout(&mut out, ctx, project);

    out
}

/// Render the repository table from the manifest.
fn render_repo_table(out: &mut String, manifest: &Manifest) {
    if manifest.repositories.is_empty() {
        return;
    }

    out.push_str("## Repositories\n\n");
    out.push_str("| Path | Role | Branch | URL |\n");
    out.push_str("|------|------|--------|-----|\n");

    let mut repos: Vec<(&RepoPath, _)> = manifest.repositories.iter().collect();
    repos.sort_by_key(|(rp, _)| rp.as_str().to_string());

    for (rp, entry) in repos {
        let role_str = match entry.role {
            Role::Primary => "primary",
            Role::Fork => "fork",
            Role::Dependency => "dependency",
            Role::Reference => "reference",
        };
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            rp.as_str(),
            role_str,
            entry.version,
            entry.url
        ));
    }
}

/// Render enabled integrations.
fn render_integrations(out: &mut String, manifest: &Manifest) {
    if manifest.integrations.is_empty() {
        return;
    }

    let enabled: Vec<&String> = manifest
        .integrations
        .iter()
        .filter(|(_, cfg)| cfg.enabled().unwrap_or(true))
        .map(|(name, _)| name)
        .collect();

    if enabled.is_empty() {
        return;
    }

    out.push_str("\n## Integrations\n\n");
    for name in &enabled {
        out.push_str(&format!("- {name}\n"));
    }
}

/// Render a concise directory layout.
fn render_directory_layout(
    out: &mut String,
    ctx: &WorkspaceContext,
    project: Option<&ProjectName>,
) {
    out.push_str("\n## Directory layout\n\n");
    out.push_str("```\n");
    out.push_str(&format!("{}/\n", ctx.root.display()));

    // List registry dirs
    let registries = ["github", "gitlab", "bitbucket"];
    for reg in &registries {
        let reg_path = ctx.root.join(reg);
        if reg_path.is_dir() {
            out.push_str(&format!("  {reg}/           # {reg} repos\n"));
        }
    }

    // Projects dir
    let projects_dir = ctx.root.join("projects");
    if projects_dir.is_dir() {
        out.push_str("  projects/\n");
        if let Ok(entries) = std::fs::read_dir(&projects_dir) {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            for name in &names {
                let marker = if project.map(|p| p.as_str()) == Some(name.as_str()) {
                    " (active)"
                } else {
                    ""
                };
                out.push_str(&format!("    {name}/{marker}\n"));
            }
        }
    }

    out.push_str("```\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_test_workspace(parent: &Path, name: &str) -> PathBuf {
        let root = parent.join(name);
        std::fs::create_dir_all(root.join("github")).unwrap();
        std::fs::create_dir_all(root.join("projects")).unwrap();
        root
    }

    fn write_manifest(root: &Path, project: &str, yaml: &str) {
        let dir = root.join("projects").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rwv.yaml"), yaml).unwrap();
    }

    // -- prime is silent outside workspace ------------------------------------

    #[test]
    fn prime_silent_outside_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        prime(tmp.path(), false).unwrap();
        // No panic, no error — just silent
    }

    // -- render_overview contains required sections ---------------------------

    #[test]
    fn render_overview_contains_concepts() {
        let overview = render_overview();
        assert!(overview.contains("repoweave: orientation"));
        assert!(overview.contains("CWD is not inside a weave"));
        assert!(overview.contains("**Weave**"));
        assert!(overview.contains("**Workweave**"));
        assert!(overview.contains("**Rig**"));
        assert!(overview.contains("Common pitfalls"));
        assert!(overview.contains("Essential commands"));
        assert!(overview.contains("rwv --help"));
    }

    // -- render_context in primary with project -------------------------------

    #[test]
    fn render_context_primary_with_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_test_workspace(tmp.path(), "ws");

        write_manifest(
            &root,
            "web-app",
            r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: primary
  github/acme/client:
    type: git
    url: https://github.com/acme/client.git
    version: develop
    role: fork
integrations:
  cargo:
    enabled: true
"#,
        );

        std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let output = render_context(&ctx);

        assert!(output.contains("# repoweave workspace"));
        assert!(output.contains("**Weave**"));
        assert!(output.contains("**Project**: `web-app`"));
        assert!(output.contains("## Repositories"));
        assert!(output.contains("github/acme/server"));
        assert!(output.contains("primary"));
        assert!(output.contains("github/acme/client"));
        assert!(output.contains("fork"));
        assert!(output.contains("## Integrations"));
        assert!(output.contains("- cargo"));
        assert!(output.contains("## Key commands"));
        assert!(output.contains("## Directory layout"));
    }

    // -- render_context in workweave ------------------------------------------

    #[test]
    fn render_context_workweave() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_test_workspace(tmp.path(), "ws");
        let workweave_dir = tmp.path().join("ws--hotfix");
        std::fs::create_dir_all(&workweave_dir).unwrap();

        write_manifest(
            &root,
            "ws",
            r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: primary
"#,
        );

        let ctx = WorkspaceContext::resolve(&workweave_dir, None).unwrap();
        let output = render_context(&ctx);

        assert!(output.contains("**Workweave**"));
        assert!(output.contains("**Project**: `ws`"));
        assert!(output.contains("## Repositories"));
    }

    // -- render_context with no project --------------------------------------

    #[test]
    fn render_context_no_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_test_workspace(tmp.path(), "ws");

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let output = render_context(&ctx);

        assert!(output.contains("# repoweave workspace"));
        assert!(output.contains("**Weave**"));
        assert!(!output.contains("**Project**"));
        assert!(!output.contains("## Repositories"));
    }

    // -- render_context with empty repositories --------------------------------

    #[test]
    fn render_context_empty_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_test_workspace(tmp.path(), "ws");

        write_manifest(&root, "minimal", "repositories: {}\n");
        std::fs::write(root.join(".rwv-active"), "minimal\n").unwrap();

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let output = render_context(&ctx);

        assert!(output.contains("**Project**: `minimal`"));
        // No repo table when empty
        assert!(!output.contains("## Repositories"));
    }

    // -- directory layout shows active marker ---------------------------------

    #[test]
    fn directory_layout_active_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_test_workspace(tmp.path(), "ws");
        std::fs::create_dir_all(root.join("projects").join("web-app")).unwrap();
        std::fs::create_dir_all(root.join("projects").join("mobile")).unwrap();
        std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let output = render_context(&ctx);

        assert!(output.contains("web-app/ (active)"));
        assert!(output.contains("mobile/\n"));
    }
}
