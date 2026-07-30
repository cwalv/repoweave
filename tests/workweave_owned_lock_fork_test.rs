//! Regression tests: a workweave created from a workspace holding an
//! rwv-accepted `Cargo.lock` must hold that same lock.
//!
//! rwv-owned generated files are gitignore-eligible, so the project repo's
//! worktree checkout that `workweave create` makes arrives without one, and
//! the create-time activation suppresses install hooks — a fresh workweave
//! therefore had neither the lock nor the `.rwv-owned-digests` record of it.
//! The first `rwv doctor --fix` then produced one by running
//! `cargo generate-lockfile`, which resolves against the registry as of that
//! moment and discards any previous resolve, and re-stamped the digest to
//! match. Both workspaces read clean while holding different dependency sets:
//! the digest attests "nobody hand-edited this since rwv wrote it", never
//! "this is what the workspace this was forked from holds".
//!
//! Two arms off one fixture shape, so a fixture that stopped establishing its
//! precondition cannot pass both: with an attested lock at the source the
//! fork reproduces it byte for byte and create says nothing about a missing
//! file; with no lock at the source create still reports one missing.
//!
//! Deliberately cargo-free. The source lock is hand-authored and stamped with
//! the shipped digest helper, carrying a package no resolution of this
//! fixture's crates could produce. A real `cargo generate-lockfile` here
//! would resolve two path-dependency crates deterministically, so its output
//! would be byte-identical whether the fork copied it or regenerated it —
//! a test built on that passes without the behaviour it means to pin.

use std::path::{Path, PathBuf};
use std::process;

mod common;

/// A source-side lock holding a package no resolution of this fixture's two
/// path-dependency crates can produce, so "the fork holds these bytes" is
/// only satisfiable by carrying them.
const SOURCE_LOCK: &str = "\
version = 4

[[package]]
name = \"chatly-protocol\"
version = \"0.1.0\"

[[package]]
name = \"chatly-server\"
version = \"0.1.0\"
dependencies = [\"chatly-protocol\"]

[[package]]
name = \"pinned-only-at-the-source\"
version = \"0.0.1\"
";

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

fn git_init_with_commit(dir: &Path) {
    git(&["init", "--initial-branch=main"], dir);
    git(&["config", "user.email", "test@test.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "-A"], dir);
    git(&["commit", "-m", "init"], dir);
}

struct Fixture {
    _tmp: tempfile::TempDir,
    source_project_dir: PathBuf,
    ww_dir: PathBuf,
    /// stdout+stderr of the `rwv workweave ... create` that made `ww_dir`.
    create_output: String,
}

impl Fixture {
    fn ww_canonical_lock(&self) -> PathBuf {
        self.ww_dir.join("projects/web-app/Cargo.lock")
    }

    fn ww_digest_state(&self) -> PathBuf {
        self.ww_dir.join("projects/web-app/.rwv-owned-digests")
    }

    fn source_lock(&self) -> PathBuf {
        self.source_project_dir.join("Cargo.lock")
    }

    fn rwv(&self, args: &[&str], cwd: &Path) -> String {
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
    }
}

/// Read the `Cargo.lock` entry out of a `.rwv-owned-digests` state file.
fn recorded_lock_digest(state_file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(state_file).ok()?;
    let map: std::collections::BTreeMap<String, String> = serde_json::from_str(&text).ok()?;
    map.get("Cargo.lock").cloned()
}

/// Pull the path out of a `<name> managed file missing: <path>; ...` line
/// naming `Cargo.lock`.
fn missing_lock_path(haystack: &str) -> Option<String> {
    haystack
        .lines()
        .filter_map(|line| line.split_once("managed file missing: "))
        .filter_map(|(_, rest)| rest.split_once(';'))
        .map(|(path, _)| path.trim().to_string())
        .find(|path| path.ends_with("Cargo.lock"))
}

/// Build a primary weave of two path-dependency crates plus a project repo
/// that gitignores its generated lock, optionally leave an rwv-accepted lock
/// in the project dir, then create a workweave off it.
fn fixture(source_lock: Option<&str>) -> Fixture {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    let protocol = ws.join("github/chatly/protocol");
    std::fs::create_dir_all(protocol.join("src")).unwrap();
    std::fs::write(
        protocol.join("Cargo.toml"),
        "[package]\nname = \"chatly-protocol\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        protocol.join("src/lib.rs"),
        "pub fn version() -> &'static str { \"1.0\" }\n",
    )
    .unwrap();
    git_init_with_commit(&protocol);

    let server = ws.join("github/chatly/server");
    std::fs::create_dir_all(server.join("src")).unwrap();
    std::fs::write(
        server.join("Cargo.toml"),
        "[package]\nname = \"chatly-server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nchatly-protocol = { path = \"../protocol\" }\n",
    )
    .unwrap();
    std::fs::write(
        server.join("src/main.rs"),
        "fn main() { println!(\"{}\", chatly_protocol::version()); }\n",
    )
    .unwrap();
    git_init_with_commit(&server);

    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.yaml"),
        "\
repositories:
  github/chatly/protocol:
    type: git
    url: https://github.com/chatly/protocol.git
    version: main
    role: owned
  github/chatly/server:
    type: git
    url: https://github.com/chatly/server.git
    version: main
    role: owned
",
    )
    .unwrap();
    std::fs::write(project_dir.join(".gitignore"), "/Cargo.lock\n").unwrap();
    git_init_with_commit(&project_dir);

    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    // Author the managed Cargo.toml with hooks suppressed: the source's lock
    // is this fixture's own, not whatever a resolver would produce here.
    let ctx = repoweave::workspace::WorkspaceContext::resolve(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "web-app",
        &ctx,
        repoweave::activate::ActivateOptions { no_install: true },
    )
    .expect("primary intent activation should succeed");
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "activate"], &project_dir);

    if let Some(content) = source_lock {
        let lock = project_dir.join("Cargo.lock");
        std::fs::write(&lock, content).unwrap();
        repoweave::integrations::merge::stamp_owned_digest(&lock, content.as_bytes())
            .expect("stamping the source lock should succeed");

        // The carry, not the worktree checkout, is the only route this lock
        // can reach a workweave by.
        let ignored = common::git()
            .args(["check-ignore", "-q", "Cargo.lock"])
            .current_dir(&project_dir)
            .status()
            .expect("git should be available");
        assert!(
            ignored.success(),
            "fixture: the source lock must be gitignored, or a workweave would \
             inherit it through git and prove nothing"
        );
    }

    let weaveroot = root.join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    let create_output = {
        let output = common::rwv()
            .args(["workweave", "web-app", "create", "agent-1"])
            .current_dir(&ws)
            .output()
            .expect("rwv workweave create should run");
        assert!(
            output.status.success(),
            "fixture: workweave create failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };

    Fixture {
        _tmp: tmp,
        source_project_dir: project_dir,
        ww_dir: weaveroot.join("web-app--agent-1"),
        create_output,
    }
}

/// Arm 1 — the fork holds the lock the workspace it forked from holds, and
/// the record travels with it.
#[test]
fn a_workweave_carries_the_attested_lock_of_the_workspace_it_forked_from() {
    let f = fixture(Some(SOURCE_LOCK));

    let source_digest = recorded_lock_digest(&f.source_project_dir.join(".rwv-owned-digests"))
        .expect("fixture: the source should record a digest for its lock");

    assert!(
        f.ww_canonical_lock().is_file(),
        "the workweave should hold a lock at {}.\ncreate output:\n{}",
        f.ww_canonical_lock().display(),
        f.create_output
    );
    assert_eq!(
        std::fs::read(f.ww_canonical_lock()).unwrap(),
        std::fs::read(f.source_lock()).unwrap(),
        "the workweave's lock should be the source's, byte for byte — a lock \
         resolved fresh in the workweave is a different dependency set than \
         the workspace it forked from"
    );
    assert_eq!(
        recorded_lock_digest(&f.ww_digest_state()).as_deref(),
        Some(source_digest.as_str()),
        "the carried lock should carry the source's recorded digest verbatim, so \
         both workspaces report the same verdict on the same bytes"
    );

    // Nothing is missing, so create has nothing to report and doctor has no
    // finding whose repair would re-resolve the lock.
    assert_eq!(
        missing_lock_path(&f.create_output),
        None,
        "create should not report a missing lock when it carried one.\noutput:\n{}",
        f.create_output
    );
    let doctor = f.rwv(&["doctor"], &f.ww_dir);
    assert_eq!(
        missing_lock_path(&doctor),
        None,
        "doctor should report no missing lock in the workweave.\noutput:\n{doctor}"
    );
    assert!(
        !doctor.contains("generated file has drift"),
        "the carried lock matches its carried digest, so doctor should report no \
         drift.\noutput:\n{doctor}"
    );

    let surfaced = f.ww_dir.join("Cargo.lock");
    assert_eq!(
        std::fs::read_link(&surfaced).ok(),
        Some(PathBuf::from("projects/web-app/Cargo.lock")),
        "the carried lock should be surfaced at the workweave root like any other \
         generated file, not left visible only in the project dir"
    );
}

/// Arm 2 — with nothing to carry, the missing-file finding still fires. The
/// carry closes the gap by filling it, never by silencing the report.
#[test]
fn a_workweave_forked_from_a_lockless_workspace_still_reports_the_missing_lock() {
    let f = fixture(None);

    assert!(
        !f.source_lock().exists(),
        "fixture: the source must have no lock for this arm to mean anything"
    );
    assert!(
        !f.ww_canonical_lock().exists(),
        "the workweave should have no lock either — there was nothing to carry"
    );
    assert!(
        missing_lock_path(&f.create_output).is_some(),
        "create should still report the missing lock.\noutput:\n{}",
        f.create_output
    );
}
