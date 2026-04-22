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
/// Covers concept definitions (weave / workweave / lock-and-sync), common agent
/// pitfalls, a typical multi-repo flow, and a command reference grouped so the
/// less self-evident sync-family commands carry a "when to use" note. Intended
/// for `--no-suppress` callers such as session-start hooks running outside a
/// repoweave workspace.
pub fn render_overview() -> String {
    let mut out = String::new();

    out.push_str("# repoweave: orientation\n\n");
    out.push_str("> CWD is not inside a weave or workweave. ");
    out.push_str("No per-workspace details are available — this is not an error.\n\n");

    out.push_str("## Concepts\n\n");
    out.push_str("**Weave** — a directory that weaves multiple repository *threads* into a single workspace *fabric*. ");
    out.push_str("Contains repositories cloned under `{registry}/{owner}/{repo}/` and projects under `projects/{name}/`. ");
    out.push_str(
        "Ecosystem workspace files and symlinks are ephemeral (regenerated on `rwv activate`); ",
    );
    out.push_str("repos and projects are the persistent state. ");
    out.push_str("Analogous to a `go.work` or Cargo `[workspace]`, with lock-based reproducibility and multi-ecosystem support.\n\n");

    out.push_str("**Workweave** — an ephemeral, isolated derivative of a weave (the multi-repo equivalent of `git worktree`). ");
    out.push_str("Each repo gets a worktree on its own ephemeral branch; ecosystem files and tool state (`node_modules/`, `.venv/`, `target/`) are per-workweave. ");
    out.push_str("Use for feature work, PR review, or per-agent isolation without disturbing the primary weave. ");
    out.push_str("Created with `rwv workweave PROJECT create NAME`; deleted with `rwv workweave PROJECT delete NAME`.\n\n");

    out.push_str("**Lock & sync** — every project owns an `rwv.lock` that pins each repo to an exact revision (tag name when HEAD is tagged, SHA otherwise). ");
    out.push_str("The lock is *load-bearing*, not a passive snapshot: `rwv sync <source>` aligns the CWD workspace with `<source>`'s committed lock. ");
    out.push_str("It is direction-neutral — `cd primary && rwv sync payments` brings a workweave's work home; `cd .workweaves/payments && rwv sync primary` catches the workweave up. ");
    out.push_str("Both sides must satisfy `rwv doctor --locked` first (bypass with `--force`); `rwv abort` rolls back via savepoint refs under `refs/rwv/pre-op/`. ");
    out.push_str("`sha256sum rwv.lock` is the project fingerprint — the multi-repo equivalent of `git rev-parse HEAD` on a monorepo.\n\n");

    out.push_str("## Common pitfalls\n\n");
    out.push_str("- Do not confuse a *weave* (the primary workspace root) with a *workweave* (a worktree-based isolated copy). They share an object store, but branches, locks, and tool state diverge.\n");
    out.push_str("- Do not `cd` into arbitrary paths before repo-scoped commands; `rwv` infers project and workspace from CWD. Use `rwv resolve` if you need the effective workspace root for scripting.\n");
    out.push_str("- Do not edit ecosystem workspace files (`package.json`, `go.work`, `Cargo.toml`) at the weave directory by hand — they are symlinks to generated files in `projects/{name}/` and get clobbered by the next `rwv activate`. Edit `rwv.yaml` instead.\n");
    out.push_str("- Do not run `rwv lock` with uncommitted changes — it errors by design (the lock would record HEAD, not your working tree). Commit first, or pass `--dirty` if you accept the divergence.\n");
    out.push_str("- Do not assume `rwv sync` has a one-true direction. The verb is direction-neutral; `<source>` is whichever workspace's committed lock you want to align against.\n");
    out.push_str("- `rwv prime` without `--no-suppress` is intentionally silent outside a weave; absence of output is not an error.\n\n");

    out.push_str("## Typical flow\n\n");
    out.push_str("Reproduce a project, work in isolation, land the result back in primary:\n\n");
    out.push_str("```\n");
    out.push_str(
        "rwv fetch <owner>/<project>             # clone project repo + every repo it lists\n",
    );
    out.push_str("rwv activate <project>                  # generate ecosystem workspace files; set active project\n");
    out.push_str("rwv workweave <project> create <name>   # spin up an isolated workspace for the feature/agent\n");
    out.push_str("# ... edit, test, commit across repos in .workweaves/<name>/ ...\n");
    out.push_str("rwv lock                                # snapshot revisions to rwv.lock\n");
    out.push_str("git -C projects/<project> commit -am 'lock: <name>'   # commit the lock in the project repo\n");
    out.push_str(
        "cd <primary> && rwv sync <name>         # land the workweave's lock back in primary\n",
    );
    out.push_str("rwv doctor                              # convention audit (orphans, stale locks, drift)\n");
    out.push_str("```\n\n");

    out.push_str("## Essential commands\n\n");
    out.push_str("Run `rwv --help` for the full command reference. Workspace and project are inferred from CWD unless overridden with `--project`.\n\n");
    out.push_str("| Command | Description |\n");
    out.push_str("|---------|-------------|\n");
    out.push_str("| `rwv` | Show workspace context |\n");
    out.push_str("| `rwv prime [--no-suppress]` | Emit structured context; `--no-suppress` always emits, even outside a weave |\n");
    out.push_str("| `rwv resolve` | Print effective workspace root path (handy for scripting: `cd $(rwv resolve)`) |\n");
    out.push_str("| `rwv fetch SOURCE [--locked\\|--frozen]` | Clone a project and every repo it lists; activate it |\n");
    out.push_str("| `rwv activate PROJECT` | Set active project; (re)generate ecosystem workspace files and symlinks |\n");
    out.push_str("| `rwv init PROJECT [--provider REG/OWNER]` | Create a new project directory with empty `rwv.yaml` |\n");
    out.push_str("| `rwv add URL [--role ROLE\\|--new]` | Clone and register a repo; `--new` initializes a brand-new repo at the canonical path |\n");
    out.push_str(
        "| `rwv remove PATH [--delete]` | Unregister a repo; `--delete` also removes the clone |\n",
    );
    out.push_str("| `rwv lock [--dirty]` | Snapshot repo revisions to the project's `rwv.lock`; runs integration lock hooks |\n");
    out.push_str(
        "| `rwv workweave PROJECT create NAME` | Spin up a worktree-based isolated workspace |\n",
    );
    out.push_str("| `rwv workweave PROJECT delete NAME` | Tear down a workweave (worktrees + ephemeral branches) |\n");
    out.push_str("| `rwv workweave PROJECT list` | List workweaves for a project |\n\n");

    out.push_str("### Sync family — when to use which\n\n");
    out.push_str("These four commands are easy to confuse — they cooperate around the lock-authoritative model.\n\n");
    out.push_str("| Command | When to use |\n");
    out.push_str("|---------|-------------|\n");
    out.push_str("| `rwv sync <source> [--strategy ff\\|rebase\\|merge] [--force]` | Align CWD workspace with `<source>`'s committed `rwv.lock`. Default `ff`; use `rebase` or `merge` when both sides advanced |\n");
    out.push_str("| `rwv abort` | Restore CWD workspace to its pre-sync state via savepoint refs; runs VCS-native abort for in-progress rebase/merge |\n");
    out.push_str("| `rwv status [--json]` | Show per-repo branch, tip, lock SHA, and relation (`ok`/`ahead`/`behind`/`diverged`/`no-lock`) without changing anything |\n");
    out.push_str("| `rwv doctor --locked` | Zero exit iff every repo's tip matches its lock entry — the precondition `rwv sync` enforces. Scriptable |\n");
    out.push_str("| `rwv doctor` | Full convention audit (orphans, dangling refs, stale locks, workweave drift, integration checks) |\n");

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
        assert!(overview.contains("**Lock & sync**"));
        assert!(!overview.contains("**Rig**"));
        assert!(overview.contains("Common pitfalls"));
        assert!(overview.contains("Typical flow"));
        assert!(overview.contains("Essential commands"));
        assert!(overview.contains("Sync family"));
        assert!(overview.contains("rwv --help"));
    }

    // -- render_overview is repoweave-only — no gc/city leakage ----------------

    #[test]
    fn render_overview_has_no_gc_or_city_references() {
        let overview = render_overview();
        // Mirrors the amendment grep from fo-rwv-prime-revamp:
        //   rwv prime --no-suppress | grep -iE 'rig|gas city|city ?\(gc\)|gc agents|gc session|gc.city'
        let lower = overview.to_ascii_lowercase();
        assert!(!lower.contains("rig"));
        assert!(!lower.contains("gas city"));
        assert!(!lower.contains("city (gc)"));
        assert!(!lower.contains("city(gc)"));
        assert!(!lower.contains("gc agents"));
        assert!(!lower.contains("gc session"));
        assert!(!lower.contains("gc.city"));
    }

    // -- render_overview density floor ----------------------------------------

    #[test]
    fn render_overview_is_meaningfully_dense() {
        let overview = render_overview();
        let lines = overview.lines().count();
        // v0.3.2 was ~32 lines; the amendment asked for noticeably richer.
        // Treat 60 as a soft floor so trivial trims don't regress us silently.
        assert!(
            lines >= 60,
            "render_overview shrank to {lines} lines; amendment requires noticeably denser than v0.3.2 (~32)"
        );
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
