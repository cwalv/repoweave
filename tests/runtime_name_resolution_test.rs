//! A workweave's runtime name comes from the primary-side registry entry that
//! records its directory, never from the directory's own basename.
//!
//! Two arms, and both fixtures are built by the shipped binary rather than by
//! hand where that is possible at all:
//!
//!   1. **Drifted basename.** `workweave create --dir` places a registered
//!      workweave at a directory whose basename spells a different name than
//!      the registry records. Runtime works, on the recorded name — including
//!      the ephemeral ref a later `rwv add` mints from inside it.
//!   2. **Unregistered directory.** A marker-bearing directory no entry names
//!      has no name at all. Verbs that act on the identity refuse and name the
//!      repair; verbs that only report proceed and say what they found.
//!
//! Restore the basename parse in `by_marker` and arm 1 fails on the name it
//! reports and on the ref it mints. Let the absence fall back to the basename
//! and arm 2 fails on both halves at once — the refusals stop refusing and the
//! reports stop reporting.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process;

mod common;

use common::src_scan::{production_lines, SourceLine};

fn rwv() -> Command {
    common::rwv()
}

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

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// A workspace with one project (`web-app`) and one manifest repo.
fn make_workspace(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nversion = \"main\"\nrole = \"owned\"\n",
            repo = common::url_path(&repo_path)
        ),
    )
    .unwrap();
    ws
}

fn index_path(ws: &Path) -> PathBuf {
    ws.join("projects/web-app/.rwv-workweave-index")
}

fn read_index(ws: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(index_path(ws)).expect("index should exist after create");
    serde_json::from_str(&raw).expect("index should parse")
}

/// Drop `name`'s placement entry, leaving the directory on disk with its
/// marker and no record naming it. This is the state
/// `WorkweaveTreeIntegrityKind::UnregisteredWorkweave` reports and
/// `rwv doctor --fix` repairs.
fn deregister(ws: &Path, name: &str) {
    let mut index = read_index(ws);
    let removed = index
        .get_mut("workweaves")
        .and_then(|w| w.as_object_mut())
        .and_then(|w| w.remove(name));
    assert!(
        removed.is_some(),
        "fixture: `{name}` must be recorded before this test removes it"
    );
    std::fs::write(index_path(ws), serde_json::to_string(&index).unwrap()).unwrap();
}

/// The `resolution` block of `rwv status --json`.
fn status_resolution(dir: &Path) -> serde_json::Value {
    let out = rwv()
        .args(["status", "--json"])
        .current_dir(dir)
        .output()
        .expect("status should run");
    assert!(
        out.status.success(),
        "status --json must succeed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("status --json should parse");
    parsed
        .get("resolution")
        .cloned()
        .expect("status --json carries a resolution block")
}

fn branch_names(repo: &Path) -> Vec<String> {
    let out = common::git()
        .args([
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/heads/",
        ])
        .current_dir(repo)
        .output()
        .expect("git should be available");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Create workweave `feat` in the project's own container, where its basename
/// and the record agree. This is what `deregister` then strips the record from,
/// so the unregistered arm turns on the missing entry alone and not on any
/// second oddity of placement.
fn make_registered_workweave(ws: &Path) -> PathBuf {
    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .current_dir(ws)
        .assert()
        .success();
    let dir = read_index(ws)["workweaves"]["feat"]
        .as_str()
        .map(PathBuf::from)
        .expect("fixture: create must record a placement");
    assert!(dir.join(".rwv-workweave").exists());
    dir
}

/// Create workweave `feat` at a directory whose basename says `drifted`.
/// The placement override is the shipped way to produce this divergence, so
/// the fixture is rwv's own output and not a hand-written record.
fn make_drifted_workweave(ws: &Path, container: &Path) -> PathBuf {
    let dir = container.join("web-app--drifted");
    rwv()
        .args([
            "workweave",
            "web-app",
            "create",
            "feat",
            "--dir",
            dir.to_str().unwrap(),
        ])
        .current_dir(ws)
        .assert()
        .success();
    assert!(
        dir.join(".rwv-workweave").exists(),
        "fixture: the workweave must exist at the overridden placement"
    );
    assert_eq!(
        read_index(ws)["workweaves"]["feat"].as_str().map(Path::new),
        Some(dir.canonicalize().unwrap().as_path()),
        "fixture: the registry must record `feat` at the drifted directory"
    );
    dir
}

// ---------------------------------------------------------------------------
// Arm 1 — a basename that disagrees with the record
// ---------------------------------------------------------------------------

/// The reported identity is the one the registry holds. The basename's own
/// name half (`drifted`) is discovery; it never reaches a consumer.
#[test]
fn drifted_basename_reports_the_recorded_name() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let container = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&container).unwrap();

    let dir = make_drifted_workweave(&ws, &container);

    let resolution = status_resolution(&dir);
    assert_eq!(
        resolution.get("workweave").and_then(|v| v.as_str()),
        Some("web-app--feat"),
        "the identity must be the recorded name, not the basename's: {resolution}"
    );
}

/// The ephemeral ref a verb mints from inside a drifted workweave carries the
/// recorded name. This is the failure the runtime-name doctrine exists to
/// prevent: a name taken from the directory mints a ref the registry's records
/// do not contain.
#[test]
fn drifted_basename_mints_the_recorded_name() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let container = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&container).unwrap();

    let dir = make_drifted_workweave(&ws, &container);

    // A second repo, added from inside the workweave: `rwv add` cuts a
    // worktree there on a freshly minted ephemeral ref. The source sits at
    // `<owner>/<repo>` because that is what the local-path derivation reads.
    let second = tmp.path().join("org").join("second");
    init_repo_with_commit(&second);
    rwv()
        .args(["add", &common::file_url(&second)])
        .current_dir(&dir)
        .assert()
        .success();

    let canonical_second = ws.join("local").join("org").join("second");
    assert!(
        canonical_second.exists(),
        "fixture: the canonical clone must land where the derivation names it"
    );
    let branches = branch_names(&canonical_second);
    assert!(
        branches.iter().any(|b| b == "web-app--feat"),
        "the minted ref must spell the recorded name; branches: {branches:?}"
    );
    assert!(
        !branches.iter().any(|b| b == "web-app--drifted"),
        "no ref may spell the directory's basename; branches: {branches:?}"
    );
}

// ---------------------------------------------------------------------------
// Arm 2 — a marker-bearing directory no record names
// ---------------------------------------------------------------------------

/// `rwv push` refuses from every workweave, so what an unregistered one
/// changes is how the refusal addresses it: by directory, since there is no
/// recorded name to quote.
#[test]
fn unregistered_workweave_is_addressed_by_directory_in_push_refusal() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let dir = make_registered_workweave(&ws);
    deregister(&ws, "feat");

    let out = rwv().args(["push"]).current_dir(&dir).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "push must refuse: {stderr}");
    assert!(
        stderr.contains(&format!("workweave at {}", dir.canonicalize().unwrap().display())),
        "the refusal must address the workweave by directory: {stderr}"
    );
}

/// A verb that acts on the workweave's identity refuses, and the refusal
/// carries the repair rather than a description of the problem.
#[test]
fn unregistered_workweave_refuses_an_identity_consuming_verb() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let dir = make_registered_workweave(&ws);
    deregister(&ws, "feat");

    let second = tmp.path().join("org").join("second");
    init_repo_with_commit(&second);
    let second_url = common::file_url(&second);
    let add_second = vec!["add", second_url.as_str()];
    for verb in [vec!["workweave", "web-app", "log"], add_second] {
        let out = rwv()
            .args(&verb)
            .current_dir(&dir)
            .output()
            .expect("verb should run");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !out.status.success(),
            "`rwv {}` must refuse an unregistered workweave: {stderr}",
            verb.join(" ")
        );
        assert!(
            stderr.contains("no entry in that project's workweave index records this directory"),
            "`rwv {}` must name the state: {stderr}",
            verb.join(" ")
        );
        assert!(
            stderr.contains("rwv doctor --fix"),
            "`rwv {}` must name the repair: {stderr}",
            verb.join(" ")
        );
    }
}

/// The diagnostic exemption: the verbs an operator reaches for to understand
/// and repair the state must run in it. They report the absence rather than
/// inventing a name for it, and the machine surface omits the identity it
/// does not have.
#[test]
fn unregistered_workweave_lets_diagnostics_proceed_and_report() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let dir = make_registered_workweave(&ws);
    deregister(&ws, "feat");

    for verb in [vec!["status"], vec!["prime"]] {
        let out = rwv()
            .args(&verb)
            .current_dir(&dir)
            .output()
            .expect("verb should run");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "`rwv {}` must proceed in an unregistered workweave: {combined}",
            verb.join(" ")
        );
        assert!(
            combined.contains("no workweave index entry records this directory"),
            "`rwv {}` must report the absence: {combined}",
            verb.join(" ")
        );
    }

    // doctor is the verb that repairs this, so it above all must run.
    let doctor = rwv().args(["doctor"]).current_dir(&dir).output().unwrap();
    let doctor_out = format!(
        "{}{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    assert!(
        doctor_out.contains("present on disk but not recorded in `.rwv-workweave-index`"),
        "doctor must report the unregistered workweave: {doctor_out}"
    );

    let resolution = status_resolution(&dir);
    assert!(
        resolution.get("workweave").is_none(),
        "an unregistered workweave has no identity to report: {resolution}"
    );
}

/// `rwv doctor --fix` is what the refusals send the operator to, so the loop
/// has to close: after the fix the recorded name is back and the verbs that
/// refused proceed.
#[test]
fn doctor_fix_restores_the_recorded_name_the_refusals_named() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let dir = make_registered_workweave(&ws);
    deregister(&ws, "feat");

    // Not asserted successful: `--fix` reports this fixture's unrelated
    // findings too (no git repo at the project dir, so the merge-driver plant
    // fails). What is asserted is the repair the refusals above promise.
    let _ = rwv().args(["doctor", "--fix"]).current_dir(&ws).output();

    let resolution = status_resolution(&dir);
    let identity = resolution
        .get("workweave")
        .and_then(|v| v.as_str())
        .expect("the fix must restore a recorded identity");
    rwv()
        .args(["workweave", "web-app", "log"])
        .current_dir(&dir)
        .assert()
        .success();

    // Adoption names the orphan from its basename, which is what makes the
    // remedy honest about being a repair rather than a restoration: the name
    // that came back is the one the directory spells.
    assert_eq!(identity, "web-app--feat");
}

// ---------------------------------------------------------------------------
// The matcher itself, which no run on a non-folding filesystem can see
// ---------------------------------------------------------------------------

/// The body of the production function named `needle`, comments dropped.
fn function_body(file: &str, needle: &str) -> Vec<SourceLine> {
    let lines = production_lines();
    let start = lines
        .iter()
        .position(|l| l.file == file && l.text.contains(needle))
        .unwrap_or_else(|| panic!("`{needle}` must exist in {file}"));
    let mut body = Vec::new();
    for line in &lines[start..] {
        body.push(line.clone());
        if line.text == "}" && body.len() > 1 {
            break;
        }
    }
    assert!(
        body.len() >= 3 && body.last().expect("non-empty").text == "}",
        "the slicer must yield a whole body for `{needle}`, not {} lines ending `{}`",
        body.len(),
        body.last().map(|l| l.text.as_str()).unwrap_or("")
    );
    body
}

fn body_text(body: &[SourceLine]) -> String {
    body.iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A recorded path and the directory a resolution walked into are compared by
/// asking the filesystem which object each one is, not by comparing their
/// spellings. On a folding filesystem `canonicalize` hands back the spelling
/// it was asked with, so two spellings of one workweave compare unequal and
/// the entry recording it is never found.
///
/// This is a source pin because it is not observable at runtime here: on a
/// case-sensitive filesystem `canonicalize` resolves every alias, so the two
/// matchers agree on every directory a test can build. Reduce `same_directory`
/// to the canonicalized-path comparison and the whole suite stays green.
#[test]
fn the_registry_match_reads_filesystem_identity() {
    let matcher = body_text(&function_body(
        "workweave_index.rs",
        "fn same_directory(",
    ));
    assert!(
        matcher.contains(".dev()") && matcher.contains(".ino()"),
        "the match must read filesystem identity: {matcher}"
    );

    let lookup = body_text(&function_body("workweave.rs", "fn workweave_name_for_path("));
    assert!(
        lookup.contains("same_directory("),
        "the inverse lookup must compare through that one matcher: {lookup}"
    );
    assert!(
        !lookup.contains("canonicalize"),
        "the inverse lookup must not compare paths itself: {lookup}"
    );
}
