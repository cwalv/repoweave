//! CLI-level tests for the **member-incompatibility** category.
//!
//! `Ownership::DefaultOnly` says any on-disk value is the operator's choice and
//! `verify()` reports CLEAN. That is right for a *preference* and wrong for an
//! *incompatibility*: a `go.work` pinned below what the members' `go.mod` files
//! declare does not build, and rwv can see it. The category reports that second
//! fact without touching rule 5.
//!
//! Two surfacings, one predicate:
//!
//! - `rwv doctor` — the standing observation arm.
//! - `rwv update` — the verb that *causes* the breach (advancing members raises
//!   the requirement above an existing pin), reporting at the moment of
//!   causation. Not a refusal: the update stays valid and exits zero.
//!
//! Design: `docs/repoweave/integration-ownership/member-incompatibility.md`.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

mod common;

fn rwv() -> Command {
    common::rwv()
}

/// `rwv` with `go` removed from `PATH`.
///
/// The go-work integration has two activate paths, and this pins the `update`
/// arm on the hand-edit fallback — the path the integration documents as
/// mandatory ("`go` is not on PATH in CI / typical test environments"), and the
/// one that runs on any machine. `update_reports_breach_on_the_go_tool_path`
/// pins the same arm on the tool path.
///
/// Both are needed because the two paths reach a below-members pin by different
/// code: the fallback defers to `merge_activate`'s DefaultOnly rule, while the
/// tool path has to actively undo the raise `go work use` performs. Before
/// that undo existed the tool path could not hold a pin at
/// all, which is why this helper was the *only* way to exercise the arm.
/// `doctor` authors nothing and needs no such pinning.
fn rwv_without_go() -> Command {
    let mut cmd = common::rwv();
    cmd.env("PATH", go_free_bin());
    cmd
}

/// A directory holding a symlink to `git` and nothing else — the `PATH` the
/// helper above hands to `rwv`.
///
/// Stated as an allow-list because the subtractive spelling is wrong in a way
/// that hides. Dropping every `PATH` entry that contains a `go` also drops
/// `git` wherever the two are installed into one bin directory, as they are on
/// the CI runner: every subprocess then fails naming the wrong tool (`git fetch
/// failed to spawn`), and whether the suite passes at all depends on the
/// operator's Go install layout. A directory is not the unit the intent is
/// about.
///
/// A test under this helper that needs a tool beyond `git` fails loudly and
/// says which — add it here.
fn go_free_bin() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        // Not a `TempDir`: one held in a `static` never drops, so it would
        // leave a directory behind on every run.
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("go-free-bin");
        std::fs::create_dir_all(&dir).expect("shim bin directory should be creatable");

        let git = which("git").expect("git must be on PATH to run these tests");
        let link = dir.join("git");
        match std::os::unix::fs::symlink(&git, &link) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => panic!("linking git into {}: {e}", dir.display()),
        }
        dir
    })
    .clone()
}

/// Absolute path of `name` on the ambient `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Whether the real `go` binary is on `PATH`.
///
/// A test gated on this pins the go-tool activate path, which *is* the `go`
/// binary — there is nothing to exercise without it, and a stub would pin the
/// stub. A machine without `go` runs the fallback path, which the
/// `rwv_without_go()` tests cover on every machine including that one.
fn go_is_installed() -> bool {
    std::process::Command::new("which")
        .arg("go")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Surface `<ws>/<file>` the way rwv does: a symlink with a **relative**
/// target. An absolute one is not recognised as owner-scoped by the surfacing
/// layer, which then refuses to replace it and warns.
fn surface(ws: &Path, project_name: &str, file: &str) {
    std::os::unix::fs::symlink(
        Path::new("projects").join(project_name).join(file),
        ws.join(file),
    )
    .unwrap();
}

fn git_run(cwd: &Path, args: &[&str]) -> String {
    let output = common::git()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should be available");
    assert!(
        output.status.success(),
        "git {:?} in {} failed: {}",
        args,
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn git_run_silent(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git command failed to start");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// The single line of `output` carrying the member-incompatibility finding.
/// Panics with the whole output when there is not exactly one — "reported
/// twice" and "not reported" are both failures worth seeing in full.
fn the_finding(output: &str) -> String {
    let hits: Vec<&str> = output
        .lines()
        .filter(|l| l.contains("member-incompatibility"))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one member-incompatibility finding; got {}:\n{output}",
        hits.len()
    );
    hits[0].to_string()
}

/// Assertions every surfacing of this category must satisfy, whichever verb
/// produced the line.
fn assert_finding_shape(line: &str, on_disk: &str, required: &str) {
    assert!(
        line.contains(on_disk) && line.contains(required),
        "finding must name the on-disk value `{on_disk}` and the requirement \
         `{required}`; got:\n{line}"
    );
    assert!(
        line.contains("raise"),
        "finding must offer raising the managed value; got:\n{line}"
    );
    assert!(
        line.contains("lower the requirement"),
        "finding must offer lowering the member requirement; got:\n{line}"
    );
    // `--fix` re-runs activate(), which by rule-5 contract refuses to overwrite
    // an existing DefaultOnly value. There is no automated repair to advertise.
    assert!(
        !line.contains("--fix"),
        "finding must never advertise --fix; got:\n{line}"
    );
}

// ===========================================================================
// doctor — the standing observation arm
// ===========================================================================

/// Workspace with one on-disk member and a hand-seeded `go.work`, ready for
/// `rwv doctor`. Mirrors the fixture in `cli_e2e_default_only_test.rs`.
fn make_doctor_workspace(
    parent: &Path,
    project_name: &str,
    go_work_version: &str,
    member_go_version: &str,
) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    let project_dir = ws.join("projects").join(project_name);
    std::fs::create_dir_all(&project_dir).unwrap();

    // Anchor every subprocess git call inside the sandbox.
    git_run_silent(&["init", "--initial-branch=main"], &ws);
    git_run_silent(&["config", "user.email", "test@test.com"], &ws);
    git_run_silent(&["config", "user.name", "Test"], &ws);

    let repo_path = "github/org/module-a";
    let member_dir = ws.join(repo_path);
    std::fs::create_dir_all(&member_dir).unwrap();
    std::fs::write(
        member_dir.join("go.mod"),
        format!("module github.com/org/module-a\n\ngo {member_go_version}\n"),
    )
    .unwrap();
    git_run_silent(&["init", "--initial-branch=main"], &member_dir);
    git_run_silent(&["config", "user.email", "test@test.com"], &member_dir);
    git_run_silent(&["config", "user.name", "Test"], &member_dir);
    git_run_silent(&["add", "."], &member_dir);
    git_run_silent(&["commit", "-m", "initial"], &member_dir);

    let manifest = format!(
        "repositories:\n  {repo_path}:\n    type: git\n    \
         url: https://github.com/org/module-a.git\n    version: main\n    role: owned\n"
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    // go.work carries the operator's pin plus the rwv marker, so the `use`
    // block is CLEAN and only the go-line is in question.
    let go_work =
        format!("go {go_work_version}\n\n// managed by repoweave\nuse (\n\t./{repo_path}\n)\n");
    std::fs::write(project_dir.join("go.work"), go_work).unwrap();
    surface(&ws, project_name, "go.work");

    git_run_silent(&["init", "--initial-branch=main"], &project_dir);
    git_run_silent(&["config", "user.email", "test@test.com"], &project_dir);
    git_run_silent(&["config", "user.name", "Test"], &project_dir);
    git_run_silent(&["add", "."], &project_dir);
    git_run_silent(&["commit", "-m", "init"], &project_dir);

    std::fs::write(ws.join(".rwv-active"), format!("{project_name}\n")).unwrap();
    ws
}

fn doctor_output(ws: &Path) -> String {
    let output = rwv()
        .args(["doctor"])
        .current_dir(ws)
        .output()
        .expect("rwv doctor should run");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn doctor_reports_go_work_pinned_below_members() {
    let tmp = common::tempdir().unwrap();
    let ws = make_doctor_workspace(tmp.path(), "go-pin-project", "1.21", "1.26");

    let combined = doctor_output(&ws);
    let line = the_finding(&combined);
    assert_finding_shape(&line, "1.21", "1.26");
    assert!(
        line.contains("go-work"),
        "finding must be attributed to the go-work integration; got:\n{line}"
    );
}

#[test]
fn doctor_silent_when_pin_meets_members() {
    let tmp = common::tempdir().unwrap();
    let ws = make_doctor_workspace(tmp.path(), "go-ok-project", "1.26", "1.26");

    let combined = doctor_output(&ws);
    assert!(
        !combined.contains("member-incompatibility"),
        "a pin at the members' requirement is compatible; got:\n{combined}"
    );
}

/// Rule-5 coexistence at the CLI: the same fixture that produces the
/// member-incompatibility finding must NOT produce a go.work drift finding.
/// The DefaultOnly go-line diverging from rwv's computed default is the
/// operator's business and stays CLEAN — the new category coexists with that
/// verdict rather than reinterpreting it.
#[test]
fn doctor_does_not_report_go_line_as_drift() {
    let tmp = common::tempdir().unwrap();
    let ws = make_doctor_workspace(tmp.path(), "go-coexist-project", "1.21", "1.26");

    let combined = doctor_output(&ws);
    // The finding is there …
    the_finding(&combined);
    // … and it is the ONLY thing go-work says about this file.
    assert!(
        !combined.contains("go-work managed file has drift"),
        "DefaultOnly go-line divergence must not be reported as drift; got:\n{combined}"
    );
}

// ===========================================================================
// update — the report at the moment of causation
// ===========================================================================

/// Workspace whose single member is cloned from a bare remote, so `rwv update`
/// can advance it to a tip that raises the member's `go.mod` requirement.
struct UpdateFixture {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    member_bare: PathBuf,
    project_dir: PathBuf,
}

fn init_bare_with_go_mod(bare: &Path, go_version: &str) {
    let parent = bare.parent().unwrap();
    git_run(
        parent,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    let seed = parent.join("__seed_member");
    git_run(
        parent,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    git_run(&seed, &["config", "user.email", "test@test.com"]);
    git_run(&seed, &["config", "user.name", "Test"]);
    std::fs::write(
        seed.join("go.mod"),
        format!("module github.com/org/module-a\n\ngo {go_version}\n"),
    )
    .unwrap();
    git_run(&seed, &["add", "."]);
    git_run(&seed, &["commit", "-m", "initial"]);
    git_run(&seed, &["push", "origin", "main"]);
}

/// Push a new tip that raises the member's declared go version.
fn advance_member_go_version(bare: &Path, go_version: &str) {
    let parent = bare.parent().unwrap();
    let work = parent.join("__adv_member");
    git_run(
        parent,
        &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
    );
    git_run(&work, &["config", "user.email", "test@test.com"]);
    git_run(&work, &["config", "user.name", "Test"]);
    std::fs::write(
        work.join("go.mod"),
        format!("module github.com/org/module-a\n\ngo {go_version}\n"),
    )
    .unwrap();
    git_run(&work, &["add", "."]);
    git_run(&work, &["commit", "-m", "raise go directive"]);
    git_run(&work, &["push", "origin", "main"]);
}

/// Build a workspace pinned at `go_work_version` whose member starts at
/// `member_go_version` — i.e. compatible before any advance.
fn build_update_fixture(
    project_name: &str,
    go_work_version: &str,
    member_go_version: &str,
) -> UpdateFixture {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

    let repo_path = "local/org/module-a";
    let member_bare = tmp.path().join("module-a.git");
    init_bare_with_go_mod(&member_bare, member_go_version);

    let canonical = workspace.join(repo_path);
    std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
    git_run(
        tmp.path(),
        &[
            "clone",
            "--origin",
            "origin",
            member_bare.to_str().unwrap(),
            canonical.to_str().unwrap(),
        ],
    );
    git_run(&canonical, &["config", "user.email", "test@test.com"]);
    git_run(&canonical, &["config", "user.name", "Test"]);
    let member_head = git_run(&canonical, &["rev-parse", "HEAD"]);

    // Project repo carrying the manifest, lock and go.work.
    let project_bare = tmp.path().join("project.git");
    git_run(
        tmp.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            project_bare.to_str().unwrap(),
        ],
    );
    let project_seed = tmp.path().join("__seed_project");
    git_run(
        tmp.path(),
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_seed.to_str().unwrap(),
        ],
    );
    git_run(&project_seed, &["config", "user.email", "test@test.com"]);
    git_run(&project_seed, &["config", "user.name", "Test"]);
    std::fs::write(project_seed.join("README"), "seed").unwrap();
    git_run(&project_seed, &["add", "."]);
    git_run(&project_seed, &["commit", "-m", "initial"]);
    git_run(&project_seed, &["push", "origin", "main"]);

    let project_dir = workspace.join("projects").join(project_name);
    git_run(
        tmp.path(),
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_dir.to_str().unwrap(),
        ],
    );
    git_run(&project_dir, &["config", "user.email", "test@test.com"]);
    git_run(&project_dir, &["config", "user.name", "Test"]);

    let bare_url = member_bare.to_str().unwrap();
    std::fs::write(
        project_dir.join("rwv.yaml"),
        format!(
            "repositories:\n  {repo_path}:\n    type: git\n    url: {bare_url}\n    \
             version: main\n    role: owned\n"
        ),
    )
    .unwrap();
    std::fs::write(
        project_dir.join("rwv.lock"),
        format!(
            "repositories:\n  {repo_path}:\n    type: git\n    url: {bare_url}\n    \
             version: {member_head}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        project_dir.join("go.work"),
        format!("go {go_work_version}\n\n// managed by repoweave\nuse (\n\t./{repo_path}\n)\n"),
    )
    .unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "manifest + lock + go.work"]);

    surface(&workspace, project_name, "go.work");
    std::fs::write(workspace.join(".rwv-active"), format!("{project_name}\n")).unwrap();

    UpdateFixture {
        _tmp: tmp,
        workspace,
        member_bare,
        project_dir,
    }
}

/// The motivating case: the pin was compatible when it was made, and the
/// update itself is what breaks it. `update` reports where the operator is
/// standing — and still succeeds, because the advance is valid.
#[test]
fn update_reports_breach_it_newly_created() {
    let fx = build_update_fixture("go-update-project", "1.21", "1.21");

    // Before the advance the workspace is compatible.
    advance_member_go_version(&fx.member_bare, "1.26");

    let output = rwv_without_go()
        .args(["update", "--dirty"])
        .current_dir(&fx.workspace)
        .output()
        .expect("rwv update should run");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        output.status.success(),
        "update must not refuse on a member incompatibility; got:\n{combined}"
    );

    let line = the_finding(&combined);
    assert_finding_shape(&line, "1.21", "1.26");

    // The pin itself is untouched: this is a report, not a repair. Rule 5
    // still owns the go-line, so the regeneration `update` just ran left it
    // exactly where the operator put it.
    let go_work = std::fs::read_to_string(fx.project_dir.join("go.work")).unwrap();
    assert!(
        go_work.contains("go 1.21"),
        "update must not rewrite the DefaultOnly go-line; got:\n{go_work}"
    );
}

/// The same arm on the **go-tool** activate path, i.e. on most machines.
///
/// `update` regenerates `go.work` through `activate()`, and with `go` on PATH
/// that runs `go work use`, which raises the go directive to the members'
/// strongest requirement. rwv restores the operator's pin afterwards;
/// if it stopped doing so, the breach would be silently *repaired*
/// into a raise and there would be nothing left to report — so this test fails
/// on the finding, not just on the pin.
///
/// Versions are one minor apart at 1.20/1.21 rather than the 1.21/1.26 used
/// above: `go work` downloads a toolchain whenever a version above the
/// installed one is demanded, and 1.21 is the oldest release that can do that,
/// so `installed >= 1.21` holds for every `go` this test can run under. The
/// PATH-filtered tests are free of that constraint because they never invoke
/// the tool.
#[test]
fn update_reports_breach_on_the_go_tool_path() {
    if !go_is_installed() {
        eprintln!(
            "skipping test: `go` is not on PATH, so the go-tool activate path is unreachable"
        );
        return;
    }

    let fx = build_update_fixture("go-update-tool-project", "1.20", "1.20");

    advance_member_go_version(&fx.member_bare, "1.21");

    let output = rwv()
        .args(["update", "--dirty"])
        .current_dir(&fx.workspace)
        .output()
        .expect("rwv update should run");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        output.status.success(),
        "update must not refuse on a member incompatibility; got:\n{combined}"
    );

    let line = the_finding(&combined);
    assert_finding_shape(&line, "1.20", "1.21");

    // The report is only meaningful because the pin survived the regeneration
    // that produced it. `go work use` would have raised it to 1.21.
    let go_work = std::fs::read_to_string(fx.project_dir.join("go.work")).unwrap();
    assert!(
        go_work.contains("go 1.20"),
        "the go-tool path must not rewrite the DefaultOnly go-line; got:\n{go_work}"
    );
}

#[test]
fn update_silent_when_advance_keeps_members_compatible() {
    let fx = build_update_fixture("go-update-ok-project", "1.26", "1.21");

    advance_member_go_version(&fx.member_bare, "1.24");

    let output = rwv_without_go()
        .args(["update", "--dirty"])
        .current_dir(&fx.workspace)
        .output()
        .expect("rwv update should run");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "update should succeed; got:\n{combined}"
    );
    assert!(
        !combined.contains("member-incompatibility"),
        "an advance that stays under the pin is not a breach; got:\n{combined}"
    );
}
