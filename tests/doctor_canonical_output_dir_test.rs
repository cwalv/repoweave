//! Regression tests: `rwv doctor` bound the integrations'
//! `output_dir` to the weave root, where a project's managed and generated
//! files appear only as surfacing symlinks — and only for the **active**
//! project. Activation binds it to `projects/<project>/`, where those files
//! actually live.
//!
//! For a file that exists under the active project both views name the same
//! inode, which is why the split stayed invisible. It is not invisible
//! otherwise, and the damage is not only cosmetic:
//!
//! 1. **A missing file is named at a path that does not exist**, so the same
//!    finding was reported under two different paths depending on which verb
//!    produced it.
//! 2. **A non-active project's verify read the active project's file.** Every
//!    managed file except vscode's has a project-independent name
//!    (`Cargo.toml`, `go.work`, `package.json`, ...), so under `doctor --all`
//!    the root view hands project B the inode belonging to project A. That
//!    produced a DRIFT finding for a file that was in fact MISSING, and —
//!    where the sibling file happened to satisfy the check — no finding at all
//!    for content that was genuinely absent.
//!
//! The fix derives `output_dir` inside `WorkspaceSession::context_base` rather
//! than accepting it from the caller, so no verb can bind the root view. These
//! tests drive the shipped binary end to end: the defect lived in the seam
//! between doctor's context construction and each integration's read hook,
//! which a unit test on either side alone cannot see.

use std::path::{Path, PathBuf};

mod common;

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// One member repo per ecosystem, so every integration's detection gate opens
/// and every `verify()` has something to say. Two rust crates: the projects
/// below claim one each, which is what makes the active project's `Cargo.toml`
/// *wrong* for the inactive one rather than merely misfiled.
fn make_members(ws: &Path) {
    for name in ["rust-lib", "rust-lib-2"] {
        let rust = ws.join("github/acme").join(name);
        std::fs::create_dir_all(rust.join("src")).unwrap();
        std::fs::write(
            rust.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        std::fs::write(rust.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        git_init_with_commit(&rust);
    }

    let go = ws.join("github/acme/go-svc");
    std::fs::create_dir_all(&go).unwrap();
    std::fs::write(go.join("go.mod"), "module acme/go-svc\n\ngo 1.21\n").unwrap();
    std::fs::write(go.join("main.go"), "package main\n\nfunc main() {}\n").unwrap();
    git_init_with_commit(&go);

    let node = ws.join("github/acme/node-pkg");
    std::fs::create_dir_all(&node).unwrap();
    std::fs::write(
        node.join("package.json"),
        "{\n  \"name\": \"node-pkg\",\n  \"version\": \"0.1.0\"\n}\n",
    )
    .unwrap();
    git_init_with_commit(&node);

    let py = ws.join("github/acme/py-pkg");
    std::fs::create_dir_all(&py).unwrap();
    std::fs::write(
        py.join("pyproject.toml"),
        "[project]\nname = \"py-pkg\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    git_init_with_commit(&py);
}

/// Every integration that has a read hook, enabled, over one member repo per
/// ecosystem. `pnpm-workspaces` and `static-files` are default-disabled, so
/// they are switched on explicitly. `{rust_crate}` is the one thing the two
/// projects disagree about.
const MANIFEST_TEMPLATE: &str = "[repositories.\"github/acme/{rust_crate}\"]\ntype = \"git\"\nurl = \"https://github.com/acme/{rust_crate}.git\"\nversion = \"main\"\nrole = \"owned\"\n\n[repositories.\"github/acme/go-svc\"]\ntype = \"git\"\nurl = \"https://github.com/acme/go-svc.git\"\nversion = \"main\"\nrole = \"owned\"\n\n[repositories.\"github/acme/node-pkg\"]\ntype = \"git\"\nurl = \"https://github.com/acme/node-pkg.git\"\nversion = \"main\"\nrole = \"owned\"\n\n[repositories.\"github/acme/py-pkg\"]\ntype = \"git\"\nurl = \"https://github.com/acme/py-pkg.git\"\nversion = \"main\"\nrole = \"owned\"\n\n[integrations.pnpm-workspaces]\nenabled = true\n\n[integrations.static-files]\nenabled = true\nfiles = [\"shared-tooling.json\"]\n";

fn manifest_for(rust_crate: &str) -> String {
    MANIFEST_TEMPLATE.replace("{rust_crate}", rust_crate)
}

struct Fixture {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
}

impl Fixture {
    fn rwv(&self, args: &[&str]) -> String {
        let output = common::rwv()
            .args(args)
            .current_dir(&self.ws)
            .output()
            .expect("rwv should run");
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

/// A weave with two projects over overlapping members. `alpha` is activated
/// (so the weave root carries alpha's surfacing symlinks and alpha's files
/// exist); `beta` never is, so every file beta owns is genuinely absent from
/// `projects/beta/` while a same-named file of alpha's sits at the root.
///
/// The two claim different rust crates and identical everything else, so the
/// root view is wrong for beta in both directions at once: alpha's
/// `Cargo.toml` *contradicts* beta's config, and alpha's `go.work` /
/// `package.json` / `pnpm-workspace.yaml` / `pyproject.toml` *satisfy* it.
fn two_project_fixture() -> Fixture {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    make_members(&ws);

    for (project, rust_crate) in [("alpha", "rust-lib"), ("beta", "rust-lib-2")] {
        let dir = ws.join("projects").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rwv.toml"), manifest_for(rust_crate)).unwrap();
        git_init_with_commit(&dir);
    }
    // Only alpha gets the declared static file, so beta's copy is missing
    // while a file of that name is reachable through the root view.
    std::fs::write(
        ws.join("projects/alpha/shared-tooling.json"),
        "{ \"alpha\": true }\n",
    )
    .unwrap();

    std::fs::write(ws.join(".rwv-active"), "alpha\n").unwrap();

    let fixture = Fixture {
        _tmp: tmp,
        ws: ws.clone(),
    };
    // Author alpha's content and surface it. `doctor --fix` runs the intent
    // write path for the active project only; beta is left untouched.
    fixture.rwv(&["doctor", "--fix"]);
    assert!(
        ws.join("projects/alpha/Cargo.toml").exists(),
        "fixture: alpha's managed Cargo.toml should have been authored"
    );
    assert!(
        ws.join("Cargo.toml").symlink_metadata().is_ok(),
        "fixture: the weave root should carry alpha's surfacing symlink — \
         without it the root view names nothing and the test proves nothing"
    );
    assert!(
        !ws.join("projects/beta/Cargo.toml").exists(),
        "fixture: beta must have no authored content"
    );
    fixture
}

/// Every `(integration, file)` pair whose read hook resolves a path through
/// `output_dir`, with the state doctor should report for the inactive project.
///
/// `.cargo/config.toml` is absent: it only joins `managed_files()` when the
/// patch surface is configured to emit patches, and its read hook resolves the
/// same `output_dir` join as the rest — it is covered by the
/// no-finding-names-the-root-view assertion rather than by a per-file arm.
const EXPECTED_MISSING: &[(&str, &str)] = &[
    ("cargo-workspace", "Cargo.toml"),
    ("cargo-workspace", "Cargo.lock"),
    ("go-work", "go.work"),
    ("npm-workspaces", "package.json"),
    ("pnpm-workspaces", "pnpm-workspace.yaml"),
    ("uv-workspace", "pyproject.toml"),
    ("vscode-workspace", "beta.code-workspace"),
];

#[test]
fn every_integration_names_the_canonical_path_for_an_inactive_project() {
    let f = two_project_fixture();
    let report = f.rwv(&["doctor", "--all"]);

    for (integration, file) in EXPECTED_MISSING {
        let canonical = f.ws.join("projects/beta").join(file);
        let expected = format!(
            "{integration} managed file missing: {}; run rwv doctor --fix to regenerate",
            repoweave::path_spelling::operator_path(&canonical)
        );
        assert!(
            report.contains(&expected),
            "{integration} should report `{file}` missing at the canonical path.\n\
             expected line: {expected}\n\
             got:\n{report}"
        );
    }
}

#[test]
fn no_finding_names_the_weave_root_view_of_an_inactive_project() {
    let f = two_project_fixture();
    let report = f.rwv(&["doctor", "--all"]);

    // Every file rwv owns, including the ones with no per-file arm above.
    let owned = [
        "Cargo.toml",
        "Cargo.lock",
        ".cargo/config.toml",
        "go.work",
        "go.work.sum",
        "package.json",
        "package-lock.json",
        "pnpm-workspace.yaml",
        "pnpm-lock.yaml",
        "pyproject.toml",
        "uv.lock",
        "beta.code-workspace",
        "shared-tooling.json",
    ];
    for file in owned {
        // The Axis-1 surfacing pass legitimately names root paths — that axis
        // is *about* the root. Only the content findings are constrained here.
        let root_view = format!("managed file missing: {}", f.ws.join(file).display());
        assert!(
            !report.contains(&root_view),
            "a content finding named the weave-root view of `{file}`, which for an \
             inactive project is either a path that does not exist or another \
             project's file.\ngot:\n{report}"
        );
        let root_drift = format!("managed file has drift: {}", f.ws.join(file).display());
        assert!(
            !report.contains(&root_drift),
            "a content finding named the weave-root view of `{file}` as DRIFT — \
             that reads the active project's inode.\ngot:\n{report}"
        );
    }
}

#[test]
fn an_inactive_project_is_not_verified_against_the_active_project_s_file() {
    let f = two_project_fixture();
    let report = f.rwv(&["doctor", "--all"]);

    // Direction 1 — a sibling that CONTRADICTS the config. beta's Cargo.toml
    // is absent; read through the root view it resolved to alpha's, which is
    // present and marked but lists alpha's crate, so the finding came back as
    // DRIFT on a file beta does not own — wrong state and wrong file.
    assert!(
        !report.contains("cargo-workspace managed file has drift"),
        "beta's Cargo.toml is absent, so no DRIFT finding is possible for it; \
         a DRIFT here means the read resolved to alpha's file.\ngot:\n{report}"
    );

    // Direction 2 — a sibling that SATISFIES the config hides a genuinely
    // absent file. alpha and beta agree on the go/node/python members, so
    // alpha's generated files verify CLEAN against beta's config and beta's
    // own absent ones are never reported. Same for the declared static file,
    // which exists for alpha (and so at the root) but not for beta.
    assert!(
        report.contains("declared file 'shared-tooling.json' not found in project directory"),
        "static-files should report beta's declared file absent; reading it \
         through the root view finds alpha's copy and reports nothing.\ngot:\n{report}"
    );
    for (integration, file) in [
        ("go-work", "go.work"),
        ("npm-workspaces", "package.json"),
        ("pnpm-workspaces", "pnpm-workspace.yaml"),
        ("uv-workspace", "pyproject.toml"),
    ] {
        assert!(
            report.contains(&format!(
                "{integration} managed file missing: {}",
                repoweave::path_spelling::operator_path(&f.ws.join("projects/beta").join(file))
            )),
            "beta's `{file}` is absent and must be reported; alpha's copy \
             satisfies beta's config, so through the root view this finding \
             disappears entirely.\ngot:\n{report}"
        );
    }
}

/// The workweave arm of the same split, and the shape it was first reported
/// in: a file whose source is absent gets no surfacing symlink at all, so the
/// root view named a path that existed in neither view.
#[test]
fn doctor_in_a_workweave_names_the_canonical_path_for_an_absent_file() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    make_members(&ws);

    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), manifest_for("rust-lib")).unwrap();
    std::fs::write(project_dir.join("shared-tooling.json"), "{}\n").unwrap();
    // Generated content is regenerable, so it is not committed — which is why
    // a fresh workweave does not inherit it.
    std::fs::write(
        project_dir.join(".gitignore"),
        "/Cargo.lock\n/web-app.code-workspace\n",
    )
    .unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    let weaveroot = root.join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let rwv_in = |args: &[&str], cwd: &Path| -> String {
        let output = common::rwv()
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("rwv should run");
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };

    rwv_in(&["doctor", "--fix"], &ws);
    common::git_in(&project_dir, &["add", "-A"]);
    common::git_in(&project_dir, &["commit", "-m", "activate"]);

    let create = rwv_in(&["workweave", "web-app", "create", "agent-1"], &ws);
    let ww = weaveroot.join("web-app--agent-1");
    assert!(
        ww.join("projects/web-app/rwv.toml").exists(),
        "fixture: workweave create failed:\n{create}"
    );

    let canonical = ww.join("projects/web-app/web-app.code-workspace");
    assert!(
        !canonical.exists(),
        "fixture: the gitignored code-workspace must not have come across, \
         or there is no absent file to report"
    );
    assert!(
        ww.join("web-app.code-workspace")
            .symlink_metadata()
            .is_err(),
        "fixture: a workweave omits the surfacing symlink for an absent source, \
         which is what made the root view name a path that exists in neither view"
    );

    let report = rwv_in(&["doctor"], &ww);
    let expected = format!(
        "vscode-workspace managed file missing: {}; run rwv doctor --fix to regenerate",
        repoweave::path_spelling::operator_path(&canonical)
    );
    assert!(
        report.contains(&expected),
        "doctor in a workweave should name the canonical path.\n\
         expected line: {expected}\ngot:\n{report}"
    );
    assert!(
        !report.contains(&format!(
            "managed file missing: {}",
            repoweave::path_spelling::operator_path(&ww.join("web-app.code-workspace"))
        )),
        "doctor named the weave-root view, which does not exist here even as a \
         link.\ngot:\n{report}"
    );
}
