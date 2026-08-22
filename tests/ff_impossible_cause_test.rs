//! `ff-impossible` must carry the typed cause available at its failure point.
//!
//! Drives a real primary + workweave sharing one repo store via
//! `git worktree add`, so a fast-forward attempted from the workweave against
//! the primary's lock is a real git operation rather than a hand-built error.

use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

const SERVER_URL: &str = "https://github.com/example/server.git";
const SERVER_PATH: &str = "github/example/server";

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), manifest_toml).unwrap();
}

/// Build a primary workspace + a workweave that shares its repos via
/// `git worktree add`. Returns `(primary, workweave)`, both at the same
/// initial server-repo SHA.
fn make_shared(parent: &Path) -> (Workspace, Workspace) {
    let primary = parent.join("primary");
    std::fs::create_dir_all(primary.join("github/example")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let primary_server = primary.join(SERVER_PATH);
    let sha = init_repo(&primary_server);

    let primary_project = primary.join("projects/web-app");
    init_repo(&primary_project);
    std::fs::write(
        primary_project.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    write_manifest(&primary_project, &[(SERVER_PATH, SERVER_URL)]);
    common::fixture_lock(&primary_project, &[(SERVER_PATH, SERVER_URL, &sha)]);
    common::git_in(
        &primary_project,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&primary_project, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    let ww = parent.join("ww");
    std::fs::create_dir_all(ww.join("github/example")).unwrap();
    std::fs::create_dir_all(ww.join("projects")).unwrap();

    let ww_server = ww.join(SERVER_PATH);
    common::git_in(
        &primary_server,
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/main",
        ],
    );

    let ww_project = ww.join("projects/web-app");
    common::git_in(
        &primary_project,
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );
    std::fs::write(ww.join(".rwv-active"), "web-app\n").unwrap();

    (
        Workspace {
            root: primary,
            project_dir: primary_project,
            server_dir: primary_server,
        },
        Workspace {
            root: ww,
            project_dir: ww_project,
            server_dir: ww_server,
        },
    )
}

/// Advance `primary`'s server repo by one commit that writes `arriving.txt`,
/// and re-lock `primary`'s project onto it. The workweave's server repo is
/// left untouched, so its HEAD stays a strict ancestor of the new tip and a
/// fast-forward is the strategy sync will choose.
fn advance_primary_with_arriving_file(primary: &Workspace) -> String {
    std::fs::write(primary.server_dir.join("arriving.txt"), "arriving\n").unwrap();
    common::git_in(&primary.server_dir, &["add", "arriving.txt"]);
    common::git_in(&primary.server_dir, &["commit", "-m", "add arriving.txt"]);
    let new_sha = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &new_sha)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: arriving"]);
    new_sha
}

/// `rwv sync <primary>` run from `ww`, in `--json` mode, as one parsed
/// envelope's sole outcome.
fn run_sync_json_single_outcome(primary: &Workspace, ww: &Workspace) -> Value {
    let output = common::rwv()
        .args(["sync", &primary.root.to_string_lossy(), "--json"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope must parse ({e}):\n{stdout}"));
    let outcomes = parsed["outcomes"]
        .as_array()
        .unwrap_or_else(|| panic!("envelope must carry outcomes:\n{stdout}"));
    assert_eq!(
        outcomes.len(),
        1,
        "the manifest names one repo, so one outcome is owed:\n{stdout}"
    );
    outcomes[0].clone()
}

/// `classify_untracked_collision` re-mints a raw `merge --ff-only` failure as
/// `VcsError::UntrackedCollision` at the VCS seam (`src/git.rs`) when an
/// untracked file blocks a path the incoming tree writes. `apply_strategy`'s
/// `Ff` arm threads that typed cause through to `--json` output alongside its
/// own composed advice message, so a consumer can branch on `cause.kind`
/// (`VcsErrorOutput`'s own doc comment) instead of parsing `message`.
///
/// Drives the untracked-collision condition specifically, not just any
/// ff-impossible failure: a diverged tip or a dirty tracked file both reach
/// `apply_strategy`'s `Ff` arm too, but neither exercises the re-minted
/// `UntrackedCollision` this test is about.
#[test]
fn ff_impossible_untracked_collision_carries_its_cause() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared(tmp.path());

    advance_primary_with_arriving_file(&primary);

    // The workweave's own working tree has an untracked file sitting exactly
    // where the incoming fast-forward would write one.
    std::fs::write(ww.server_dir.join("arriving.txt"), "mine, untracked\n").unwrap();

    let outcome = run_sync_json_single_outcome(&primary, &ww);

    assert_eq!(outcome["path"], Value::from(SERVER_PATH), "\n{outcome:#}");
    assert_eq!(outcome["kind"], Value::from("failed"), "\n{outcome:#}");

    let failure = outcome
        .get("failure")
        .unwrap_or_else(|| panic!("failed outcome must carry an inner `failure`:\n{outcome:#}"));
    assert_eq!(
        failure["kind"],
        Value::from("ff-impossible"),
        "\n{failure:#}"
    );

    // Both the composed advice message and the typed cause must survive
    // together; losing either half is a regression.
    let message = failure["message"]
        .as_str()
        .unwrap_or_else(|| panic!("failure must carry a message:\n{failure:#}"));
    assert!(
        message.contains("cannot fast-forward") && message.contains("--strategy rebase"),
        "the composed advice message must survive alongside the new cause: {message:?}"
    );

    let cause = failure.get("cause").unwrap_or_else(|| {
        panic!(
            "ff-impossible must carry cause.kind for a consumer told to branch on it \
             instead of parsing message; got no cause at all:\n{failure:#}"
        )
    });
    assert_eq!(
        cause["kind"],
        Value::from("untracked-collision"),
        "\n{cause:#}"
    );
    let paths = cause["paths"]
        .as_array()
        .unwrap_or_else(|| panic!("untracked-collision cause must carry paths:\n{cause:#}"));
    assert_eq!(
        paths,
        &[Value::from("arriving.txt")],
        "the re-minted cause must name the obstructing file:\n{cause:#}"
    );
}

/// A diverged tip also reaches `apply_strategy`'s `Ff` arm and also fails
/// ff-impossible, but classification declines (no `UntrackedCollision`) and
/// the raw `CommandFailed` becomes the cause instead. `cause` must still be
/// populated on this arm too — every `StrategyError` this arm can produce now
/// carries one.
#[test]
fn ff_impossible_command_failed_also_carries_its_cause() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared(tmp.path());

    advance_primary_with_arriving_file(&primary);

    // Diverge the workweave's server repo with a commit primary does not
    // have, so the incoming fast-forward is refused for a reason no amount
    // of moving files resolves — the classifier's guard declines and the
    // raw `CommandFailed` stands.
    std::fs::write(ww.server_dir.join("ww-only.txt"), "ww\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "ww-only.txt"]);
    common::git_in(&ww.server_dir, &["commit", "-m", "ww diverges"]);

    let outcome = common::rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--json",
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&outcome.stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope must parse ({e}):\n{stdout}"));
    let failed = &parsed["outcomes"][0];
    assert_eq!(failed["kind"], Value::from("failed"), "\n{stdout}");

    let failure = &failed["failure"];
    assert_eq!(failure["kind"], Value::from("ff-impossible"), "\n{stdout}");

    let cause = failure.get("cause").unwrap_or_else(|| {
        panic!("ff-impossible must carry a cause even on the decline path:\n{stdout}")
    });
    assert_eq!(cause["kind"], Value::from("command-failed"), "\n{stdout}");
}
