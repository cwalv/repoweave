//! `sync-to`'s ff-advance decline fallback must report git's own account of
//! the refusal, not just rwv's context sentence wrapped around it.
//!
//! Drives a real primary + workweave sharing one repo store via
//! `git worktree add`, so a fast-forward attempted from `sync-to` against the
//! target is a real git operation rather than a hand-built error.

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
/// initial server-repo SHA. `sync-to` treats CWD (the workweave in these
/// tests) as the side already advanced, and the named argument (primary) as
/// the target it fast-forwards.
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

/// `sync-to`'s advance-target phase (`run_advance_target`, `ff_advance_repo`)
/// is a ff-advance path with its own decline fallback: when
/// `classify_untracked_collision` declines, `ff_advance_repo` wraps the raw
/// `VcsError` with `.context("fast-forward advance failed in target")`, and
/// the human-render reporting site prints the alternate `{:#}` form of that
/// error so the whole chain reaches the operator, not just the context
/// sentence around it.
///
/// The obstruction here is real but shaped so the classifier's own
/// independent computation misses it: the incoming commit adds `foo/bar.txt`
/// (a diff `--diff-filter=A` entry), but the untracked file already occupying
/// that name in the target's working tree is `foo` itself — a different path
/// string, so `ls-files --others -- foo/bar.txt` (the classifier's positive
/// check) reports nothing even though `git merge --ff-only` refuses natively
/// and names `foo` in its own stderr.
#[test]
fn sync_to_ff_advance_decline_path_carries_gits_stderr() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared(tmp.path());

    // CWD (ww) advances with a commit that adds a new directory+file; this is
    // the commit sync-to will try to fast-forward primary onto.
    std::fs::create_dir_all(ww.server_dir.join("foo")).unwrap();
    std::fs::write(ww.server_dir.join("foo/bar.txt"), "hi\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "foo/bar.txt"]);
    common::git_in(&ww.server_dir, &["commit", "-m", "add foo/bar.txt"]);
    let ww_server_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    common::fixture_lock(
        &ww.project_dir,
        &[(SERVER_PATH, SERVER_URL, &ww_server_tip)],
    );
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: add foo/bar.txt"]);

    // primary (the sync-to TARGET) has an untracked plain file named `foo`
    // occupying the path the incoming directory needs.
    std::fs::write(primary.server_dir.join("foo"), "blocking\n").unwrap();

    let assert = common::rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(
        !assert.status.success(),
        "the untracked file must block the advance:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&assert.stdout),
        String::from_utf8_lossy(&assert.stderr)
    );
    let stderr = String::from_utf8_lossy(&assert.stderr).into_owned();

    assert!(
        stderr.contains("ff-advance failed")
            && stderr.contains("fast-forward advance failed in target"),
        "expected rwv's own context sentence in stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("would be overwritten by merge") && stderr.contains("foo"),
        "git's own account of the refusal must reach the operator, not just rwv's \
         context sentence around it:\n{stderr}"
    );
}

/// The other reporting site: once every manifest repo lands, `run_advance_target`
/// ff-advances the project repo itself through the same `ff_advance_repo` /
/// decline-fallback shape. Same obstruction pattern, applied to the project
/// repo instead of a manifest repo, so the `(project): ff-advance failed`
/// site is driven independently of the manifest-repo site above.
#[test]
fn sync_to_ff_advance_decline_path_carries_gits_stderr_for_the_project_repo() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared(tmp.path());

    // Manifest repo advances cleanly — nothing blocks it, so `any_ff_failure`
    // stays false and control reaches the project-repo advance below.
    std::fs::write(ww.server_dir.join("ww.txt"), "workweave\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "ww.txt"]);
    common::git_in(&ww.server_dir, &["commit", "-m", "ww: advance"]);
    let ww_server_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    common::fixture_lock(
        &ww.project_dir,
        &[(SERVER_PATH, SERVER_URL, &ww_server_tip)],
    );
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    // The project repo's own next commit adds a new directory+file — this is
    // what sync-to will try to fast-forward primary's project repo onto.
    std::fs::create_dir_all(ww.project_dir.join("newdir")).unwrap();
    std::fs::write(ww.project_dir.join("newdir/newfile.txt"), "hi\n").unwrap();
    common::git_in(&ww.project_dir, &["add", "newdir/newfile.txt"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "add newdir/newfile.txt"]);

    // primary's project repo has an untracked plain file named `newdir`
    // occupying the path the incoming directory needs.
    std::fs::write(primary.project_dir.join("newdir"), "blocking\n").unwrap();

    let assert = common::rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(
        !assert.status.success(),
        "the untracked file must block the project-repo advance:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&assert.stdout),
        String::from_utf8_lossy(&assert.stderr)
    );
    let stderr = String::from_utf8_lossy(&assert.stderr).into_owned();

    assert!(
        stderr.contains("(project): ff-advance failed")
            && stderr.contains("fast-forward advance failed in target"),
        "expected rwv's own context sentence in stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("would be overwritten by merge") && stderr.contains("newdir"),
        "git's own account of the refusal must reach the operator, not just rwv's \
         context sentence around it:\n{stderr}"
    );
}
