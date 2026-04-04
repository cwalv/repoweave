//! `rwv prime` — emit structured workspace context for agent system prompts.
//!
//! Prints markdown describing the current repoweave workspace: root path,
//! active project, workspace/weave location, repository table with roles,
//! enabled integrations, key commands, and directory layout.
//!
//! Silent (exit 0, no output) when not inside a repoweave workspace.

use crate::manifest::{Manifest, ProjectName, RepoPath, Role};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use std::path::Path;

/// Run `rwv prime` from the given working directory.
///
/// Returns `Ok(())` unconditionally. Prints nothing if not in a workspace.
pub fn prime(cwd: &Path) -> anyhow::Result<()> {
    let ctx = match WorkspaceContext::resolve(cwd, None) {
        Ok(ctx) => ctx,
        Err(_) => return Ok(()), // silent outside workspace
    };

    let output = render_context(&ctx);
    print!("{output}");
    Ok(())
}

/// Render the full markdown context string.
pub fn render_context(ctx: &WorkspaceContext) -> String {
    let mut out = String::new();

    out.push_str("# repoweave workspace\n\n");

    // -- Location ---------------------------------------------------------------
    out.push_str(&format!("- **Root**: `{}`\n", ctx.root.display()));

    let project: Option<&ProjectName> = match &ctx.location {
        WorkspaceLocation::Primary { project } => {
            out.push_str("- **Location**: primary\n");
            project.as_ref()
        }
        WorkspaceLocation::Weave { name, dir, project } => {
            out.push_str(&format!(
                "- **Location**: weave `{}`\n- **Weave dir**: `{}`\n",
                name.as_str(),
                dir.display()
            ));
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
    out.push_str("| `rwv weave PROJECT NAME` | Create a weave (worktree-based workspace) |\n");
    out.push_str("| `rwv add URL [--role ROLE]` | Add a repo to the current weave |\n");
    out.push_str("| `rwv remove PATH` | Remove a repo from the current weave |\n");
    out.push_str("| `rwv lock` | Snapshot repo versions to rwv.lock |\n");
    out.push_str("| `rwv check` | Run convention enforcement |\n");
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
        .filter(|(_, cfg)| cfg.enabled.unwrap_or(true))
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
fn render_directory_layout(out: &mut String, ctx: &WorkspaceContext, project: Option<&ProjectName>) {
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
        // No workspace markers
        prime(tmp.path()).unwrap();
        // No panic, no error — just silent
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
        assert!(output.contains("**Root**"));
        assert!(output.contains("**Location**: primary"));
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

    // -- render_context in weave ----------------------------------------------

    #[test]
    fn render_context_weave() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_test_workspace(tmp.path(), "ws");
        let weave_dir = tmp.path().join("ws--hotfix");
        std::fs::create_dir_all(&weave_dir).unwrap();

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

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        let output = render_context(&ctx);

        assert!(output.contains("weave `hotfix`"));
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
        assert!(output.contains("**Location**: primary"));
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
