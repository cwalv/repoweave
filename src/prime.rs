//! `rwv prime` — emit structured workspace context for agent system prompts.
//!
//! Prints markdown describing the current repoweave workspace: root path,
//! active project, checkout kind (primary/workweave), repository table with
//! roles, enabled integrations, key commands, and directory layout.
//!
//! Silent (exit 0, no output) when not inside a repoweave workspace.

use crate::manifest::{Manifest, ProjectName, RepoPath};
use crate::workspace::{Checkout, WorkspaceContext};

/// Run `rwv prime` against an already-resolved workspace context.
///
/// Returns `Ok(())` unconditionally. When `ctx` is `None` (the origin dir
/// resolved to no workspace), prints nothing unless `no_suppress` is true —
/// in which case an orientation overview is emitted so agents can orient
/// themselves without per-workspace details.
///
/// Resolution happens in `main`; this handler never touches the process
/// cwd on its own.
pub fn prime(ctx: Option<&WorkspaceContext>, no_suppress: bool) -> anyhow::Result<()> {
    let Some(ctx) = ctx else {
        if no_suppress {
            print!("{}", render_overview());
        }
        return Ok(());
    };

    let output = render_context(ctx);
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
///
/// The content is maintained as a committed markdown file:
/// `docs/reference/prime/overview.md` (rendered from
/// `docs/reference/prime/templates/overview.md.tmpl` by `cargo run --bin
/// generate-explain`). This function simply embeds it at compile time.
pub fn render_overview() -> String {
    include_str!("../docs/reference/prime/overview.md").to_string()
}

/// Render the full markdown context string.
pub fn render_context(ctx: &WorkspaceContext) -> String {
    let mut out = String::new();

    out.push_str("# repoweave workspace\n\n");

    // -- Location ---------------------------------------------------------------
    let project: Option<&ProjectName> = match &ctx.checkout {
        Checkout::Primary { project } => {
            out.push_str(&format!(
                "- **Weave**: `{}`\n",
                ctx.primary_path().display()
            ));
            project.as_ref()
        }
        Checkout::Workweave {
            name: _,
            dir,
            project,
        } => {
            out.push_str(&format!("- **Workweave**: `{}`\n", dir.display()));
            out.push_str(&format!(
                "- **Weave**: `{}`\n",
                ctx.primary_path().display()
            ));
            Some(project)
        }
    };

    if let Some(p) = project {
        out.push_str(&format!("- **Project**: `{}`\n", p.as_str()));
    }

    // -- Repository table -------------------------------------------------------
    if let Some(p) = project {
        let manifest_path = ctx
            .primary_path()
            .join("projects")
            .join(p.as_str())
            .join("rwv.yaml");
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
        "| `rwv workweave PROJECT create NAME` | Create a workweave (worktree-based workspace) |\n",
    );
    out.push_str("| `rwv add URL [--role ROLE]` | Add a repo to the active project |\n");
    out.push_str("| `rwv remove PATH` | Remove a repo from the active project |\n");
    out.push_str("| `rwv lock` | Snapshot repo versions to rwv.lock |\n");
    out.push_str("| `rwv doctor` | Run convention enforcement |\n");
    out.push_str("| `rwv fetch SOURCE` | Clone a project and its repos |\n");

    // -- Directory layout -------------------------------------------------------
    render_directory_layout(&mut out, ctx, project);

    // -- Agent integration surfaces ---------------------------------------------
    out.push_str("\n## Agent integration surfaces\n\n");
    out.push_str("- **Structured output:** `rwv status --json`, `rwv doctor --json`, `rwv sync --json`. Array-of-records with `path` + `absolute_path` identifiers; per-record `kind` for failure discrimination.\n");
    out.push_str("- **Per-verb reflection:** `rwv explain <verb>` returns a markdown bundle (purpose, invocation, output description with JSON Schema). Use when scripting against an unfamiliar verb.\n");
    out.push_str("- **Schemas:** committed at `docs/reference/schemas/<verb>.json`. Each `--json` output embeds a `$schema` URL pointing here.\n");

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
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            rp.as_str(),
            entry.role.as_str(),
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
    out.push_str(&format!("{}/\n", ctx.primary_path().display()));

    // List registry dirs
    let registries = ["github", "gitlab", "bitbucket"];
    for reg in &registries {
        let reg_path = ctx.primary_path().join(reg);
        if reg_path.is_dir() {
            out.push_str(&format!("  {reg}/           # {reg} repos\n"));
        }
    }

    // Projects dir
    let projects_dir = ctx.primary_path().join("projects");
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
    use std::path::{Path, PathBuf};

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
        // Simulate the dispatch-time behaviour: origin dir has no workspace,
        // so main passes None to prime; the handler stays quiet.
        prime(None, false).unwrap();
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
        // Agent integration surfaces.
        assert!(overview.contains("Agent integration surfaces"));
        assert!(overview.contains("rwv status --json"));
        assert!(overview.contains("rwv doctor --json"));
        assert!(overview.contains("rwv sync --json"));
        assert!(overview.contains("rwv explain"));
        assert!(overview.contains("docs/reference/schemas"));
    }

    // -- render_overview is repoweave-only — no gc/city leakage ----------------

    #[test]
    fn render_overview_has_no_gc_or_city_references() {
        let overview = render_overview();
        // Mirrors the amendment grep from the prime revamp:
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
        // Bumped to 70 after the Agent integration surfaces section
        // landed, so trivial trims don't regress us silently.
        assert!(
            lines >= 70,
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
    role: owned
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
        assert!(output.contains("owned"));
        assert!(output.contains("github/acme/client"));
        assert!(output.contains("fork"));
        assert!(output.contains("## Integrations"));
        assert!(output.contains("- cargo"));
        assert!(output.contains("## Key commands"));
        assert!(output.contains("## Directory layout"));
        // Agent integration surfaces.
        assert!(output.contains("## Agent integration surfaces"));
        assert!(output.contains("rwv status --json"));
        assert!(output.contains("rwv doctor --json"));
        assert!(output.contains("rwv sync --json"));
        assert!(output.contains("rwv explain"));
        assert!(output.contains("docs/reference/schemas"));
    }

    // -- Key commands table names only real verbs -----------------------------

    /// A token that names an argument placeholder (`PROJECT`, `NAME`, ...) or
    /// a flag (`--role`, `[--role`), rather than a subcommand.
    fn is_placeholder_or_flag(token: &str) -> bool {
        if token.starts_with('-') || token.starts_with('[') {
            return true;
        }
        let core = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        !core.is_empty() && core.chars().all(|c| c.is_ascii_uppercase())
    }

    /// Every verb the "Key commands" table names must be a real subcommand.
    /// The table is a hand-curated onboarding subset (not the full `rwv
    /// explain` registry — its rows carry usage syntax and short blurbs a
    /// mechanical listing would either omit or bloat), so this does not
    /// regenerate it; it walks the *rendered* rows against `Cli::command()`
    /// instead of a second hand-typed verb list, so a stale row is what fails.
    #[test]
    fn key_commands_table_names_only_real_verbs() {
        use clap::CommandFactory;

        let tmp = tempfile::tempdir().unwrap();
        let root = make_test_workspace(tmp.path(), "ws");
        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let output = render_context(&ctx);

        let section_start = output
            .find("## Key commands")
            .expect("Key commands section missing from render_context output");
        let section = &output[section_start..];
        let section_end = section[1..]
            .find("\n## ")
            .map(|i| i + 1)
            .unwrap_or(section.len());
        let section = &section[..section_end];

        let cli_cmd = crate::cli::Cli::command();
        let mut rows_checked = 0;
        for line in section.lines() {
            let Some(cmd_str) = line
                .strip_prefix("| `")
                .and_then(|rest| rest.split_once('`'))
                .map(|(cmd, _)| cmd)
            else {
                continue;
            };
            rows_checked += 1;

            let mut tokens = cmd_str.split_whitespace();
            assert_eq!(
                tokens.next(),
                Some("rwv"),
                "Key commands row `{cmd_str}` doesn't start with `rwv`"
            );
            let mut current = &cli_cmd;
            for token in tokens {
                if is_placeholder_or_flag(token) {
                    continue;
                }
                current = current.find_subcommand(token).unwrap_or_else(|| {
                    panic!(
                        "Key commands table names `{cmd_str}`, whose `{token}` is not a real rwv subcommand"
                    )
                });
            }
        }
        assert!(
            rows_checked >= 5,
            "parsed only {rows_checked} Key commands rows — parser regression, not a thin table"
        );
    }

    // -- render_context in workweave ------------------------------------------

    #[test]
    fn render_context_workweave() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_test_workspace(tmp.path(), "ws");
        let workweave_dir = tmp.path().join("ws--hotfix");
        std::fs::create_dir_all(&workweave_dir).unwrap();

        // Write the .rwv-workweave marker so resolve() recognizes this as a
        // workweave (marker-less resolution was removed).
        let primary_canon = root.canonicalize().unwrap();
        crate::workspace::WorkweaveMarker {
            primary: primary_canon.clone(),
            project: crate::manifest::ProjectName::new("ws").unwrap(),
            parent: primary_canon,
        }
        .write(&workweave_dir)
        .unwrap();

        write_manifest(
            &root,
            "ws",
            r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: owned
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
