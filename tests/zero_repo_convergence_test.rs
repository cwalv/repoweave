//! A project with no member repos converges to CLEAN, and stays there.
//!
//! vscode-workspace is the integration this is about: unlike the ecosystem
//! ports it detects **all** repos rather than a manifest file, so "nothing to
//! contribute" is not a state it can gate on the way cargo-workspace does. A
//! project whose membership is empty still gets a `.code-workspace`, and the
//! generated `files.exclude` set legitimately *changes* when the last member
//! goes — the registry directory becomes excludable — so the drift a `doctor`
//! reports in that moment is real rather than spurious.
//!
//! What has to be true is that the report is actionable: `doctor --fix`
//! resolves it, the next pass is silent, and the operator's own content comes
//! through untouched. That is a deliberate silence, and a silence is exactly
//! the claim that decays without anyone noticing — an integration that stopped
//! converging would show up as a warning nobody can clear, which trains an
//! operator to ignore warnings. Hence a test that fails, rather than an
//! absence.
//!
//! The `.code-workspace` is deliberately not gitignore-eligible and not
//! whole-deletable: rwv owns the `rwv.generated` object, the operator owns
//! `folders` and everything else. So convergence here must not be reached by
//! deleting the file.

use std::path::{Path, PathBuf};

mod common;

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

fn git_init_with_commit(dir: &Path) {
    git(&["init", "--initial-branch=main"], dir);
    git(&["config", "user.email", "test@test.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "-A"], dir);
    git(&["commit", "-m", "init"], dir);
}

struct Weave {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
}

impl Weave {
    fn code_workspace(&self) -> PathBuf {
        self.ws.join("projects/app/app.code-workspace")
    }

    fn manifest(&self) -> PathBuf {
        self.ws.join("projects/app/rwv.toml")
    }

    /// Run `rwv` and return (success, combined output).
    fn rwv(&self, args: &[&str]) -> (bool, String) {
        let out = common::rwv()
            .args(args)
            .current_dir(&self.ws)
            .output()
            .expect("rwv should run");
        (
            out.status.success(),
            format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    /// The vscode-workspace lines of a `rwv doctor` pass.
    fn vscode_findings(&self) -> Vec<String> {
        let (_, report) = self.rwv(&["doctor"]);
        report
            .lines()
            .filter(|l| l.contains("vscode-workspace"))
            .map(str::to_string)
            .collect()
    }
}

/// A weave whose project starts with one member repo and has it authored.
fn weave_with_one_member() -> Weave {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects/app")).unwrap();

    let member = ws.join("github/acme/lib");
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(member.join("src/lib.rs"), "").unwrap();
    git_init_with_commit(&member);

    std::fs::write(
        ws.join("projects/app/rwv.toml"),
        "[repositories.\"github/acme/lib\"]\ntype = \"git\"\nurl = \"https://github.com/acme/lib.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    git_init_with_commit(&ws.join("projects/app"));
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation should author the managed files");

    Weave { _tmp: tmp, ws }
}

/// Emptying a project's membership reports drift once, and one `--fix` ends it.
///
/// The first assertion is the non-vacuity guard: if emptying the manifest
/// stopped producing a finding at all, the rest of this test would pass while
/// measuring nothing.
#[test]
fn an_emptied_membership_converges_to_clean_in_one_fix() {
    let w = weave_with_one_member();
    std::fs::write(w.manifest(), "[repositories]\n").unwrap();

    let before = w.vscode_findings();
    assert!(
        before.iter().any(|f| f.contains("drift")),
        "fixture: emptying the membership should produce the drift this test is \
         about; got {before:#?}"
    );

    let (fixed, report) = w.rwv(&["doctor", "--fix"]);
    assert!(fixed, "`doctor --fix` should succeed:\n{report}");

    let after = w.vscode_findings();
    assert!(
        after.is_empty(),
        "a zero-member project must reach CLEAN — the warning `--fix` names is \
         the operator's only move, and one they cannot complete is one they \
         learn to ignore. Still reported:\n{after:#?}\n--fix said:\n{report}"
    );

    // Idempotent: a second pass neither reports nor rewrites.
    let settled = std::fs::read_to_string(w.code_workspace()).unwrap();
    let (fixed_again, report) = w.rwv(&["doctor", "--fix"]);
    assert!(fixed_again, "a second `--fix` should succeed:\n{report}");
    assert_eq!(
        std::fs::read_to_string(w.code_workspace()).unwrap(),
        settled,
        "the converged state must be a fixed point, not a file two passes \
         disagree about"
    );
    assert!(
        w.vscode_findings().is_empty(),
        "the second pass must stay silent"
    );
}

/// Convergence must not be reached by discarding what the operator wrote. The
/// `.code-workspace` is a hybrid, always-committed file: rwv owns the
/// `rwv.generated` object and nothing else in it.
#[test]
fn converging_a_zero_repo_project_keeps_operator_content() {
    let w = weave_with_one_member();

    let authored = std::fs::read_to_string(w.code_workspace()).unwrap();
    std::fs::write(
        w.code_workspace(),
        authored.replace(
            "\"settings\": {",
            "\"extensions\": { \"recommendations\": [\"rust-lang.rust-analyzer\"] },\n  \
             \"settings\": {\n    \"editor.tabSize\": 7,",
        ),
    )
    .unwrap();
    std::fs::write(w.manifest(), "[repositories]\n").unwrap();

    let (fixed, report) = w.rwv(&["doctor", "--fix"]);
    assert!(fixed, "`doctor --fix` should succeed:\n{report}");

    let converged = std::fs::read_to_string(w.code_workspace()).unwrap();
    assert!(
        w.code_workspace().is_file(),
        "convergence must not delete a file the operator co-owns"
    );
    assert!(
        converged.contains("rust-lang.rust-analyzer"),
        "the operator's extension recommendations must survive:\n{converged}"
    );
    assert!(
        converged.contains("\"editor.tabSize\": 7"),
        "the operator's settings must survive:\n{converged}"
    );
    assert!(
        w.vscode_findings().is_empty(),
        "and the project must still reach CLEAN"
    );
}
