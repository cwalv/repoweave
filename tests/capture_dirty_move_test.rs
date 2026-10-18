//! `rwv workweave create --capture-dirty` MOVES the source's uncommitted
//! `rwv.toml` / `rwv.lock` edits into the new workweave: copied, then
//! discarded from the source, so the workweave can land back on it.
//!
//! The move's data-safety half is pinned here: nothing the copy does not hold
//! is ever discarded, and a copy that fails partway leaves the source exactly
//! as it was.

use std::path::{Path, PathBuf};

mod common;

const MARKER: &str = "# captured edit\n";

/// A weave whose active project `app`, made by `rwv init`, has no members and
/// its `rwv.toml`, `rwv.lock` and a `notes.md` committed. Returns the weave
/// root.
fn weave(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let (ok, report) = rwv(&ws, &["init", "app"]);
    assert!(ok, "fixture: init failed:\n{report}");
    let project = ws.join("projects/app");
    common::git_in(&project, &["config", "user.email", "test@test.com"]);
    common::git_in(&project, &["config", "user.name", "Test"]);
    std::fs::write(
        project.join("rwv.toml"),
        "[repositories]\n\n[integrations.vscode-workspace]\nenabled = false\n",
    )
    .unwrap();
    common::fixture_lock(&project, &[]);
    std::fs::write(project.join("notes.md"), "notes\n").unwrap();
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-q", "-m", "init"]);
    ws
}

fn append(path: &Path, text: &str) -> Vec<u8> {
    let mut bytes = std::fs::read(path).unwrap();
    bytes.extend_from_slice(text.as_bytes());
    std::fs::write(path, &bytes).unwrap();
    bytes
}

fn rwv(cwd: &Path, args: &[&str]) -> (bool, String) {
    let out = common::rwv()
        .args(args)
        .current_dir(cwd)
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

fn status(repo: &Path) -> String {
    common::git_in(repo, &["status", "--porcelain"])
}

/// The move, and the round trip it exists for: the edit lands back on the
/// source through `sync-to`, which refuses a source holding a second copy.
#[test]
fn capture_dirty_moves_the_edits_so_the_workweave_lands_back() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project = ws.join("projects/app");
    let edited = append(&project.join("rwv.toml"), MARKER);

    let (ok, report) = rwv(
        &ws,
        &["workweave", "app", "create", "seat", "--capture-dirty"],
    );
    assert!(ok, "create failed:\n{report}");
    let seat = tmp.path().join(".workweaves/app--seat");
    assert_eq!(
        std::fs::read(seat.join("projects/app/rwv.toml")).unwrap(),
        edited,
        "the workweave must hold the captured bytes"
    );
    assert_eq!(
        status(&project),
        "",
        "the source must give the captured edit up:\n{report}"
    );

    let seat_project = seat.join("projects/app");
    common::git_in(&seat_project, &["commit", "-q", "-am", "the captured edit"]);
    let (ok, report) = rwv(&seat, &["sync-to"]);
    assert!(
        ok,
        "the captured edit must land back on the source:\n{report}"
    );
    assert!(
        common::read_normalized(project.join("rwv.toml")).contains(MARKER),
        "the source holds the edit again, committed:\n{report}"
    );
    assert_eq!(status(&project), "");
}

/// Uncommitted state the capture does not cover, and a staged version its
/// working file does not show, stay in the source byte for byte.
#[test]
fn capture_dirty_never_discards_what_it_did_not_capture() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project = ws.join("projects/app");
    let notes = append(&project.join("notes.md"), "draft\n");
    std::fs::write(project.join("scratch.txt"), "untracked\n").unwrap();
    append(&project.join("rwv.toml"), "# staged\n");
    common::git_in(&project, &["add", "rwv.toml"]);
    let staged = common::git_in(&project, &["show", ":rwv.toml"]);
    let working = append(&project.join("rwv.toml"), MARKER);

    let (ok, report) = rwv(
        &ws,
        &["workweave", "app", "create", "seat", "--capture-dirty"],
    );
    assert!(ok, "create failed:\n{report}");

    assert_eq!(std::fs::read(project.join("notes.md")).unwrap(), notes);
    assert_eq!(
        std::fs::read_to_string(project.join("scratch.txt")).unwrap(),
        "untracked\n"
    );
    assert_eq!(std::fs::read(project.join("rwv.toml")).unwrap(), working);
    assert_eq!(
        common::git_in(&project, &["show", ":rwv.toml"]),
        staged,
        "the staged version, which the workweave did not receive, must survive"
    );
    assert!(
        report.contains("staged changes its working file does not show")
            && report.contains("notes.md")
            && report.contains("scratch.txt"),
        "what stays behind is named:\n{report}"
    );
}

/// A copy that fails partway discards nothing: the file that did copy keeps
/// its edit in the source too.
///
/// The failure is planted by the project repo's own `post-checkout` hook,
/// which `git worktree add` runs in the new workweave's project worktree: it
/// puts a directory where `rwv.lock` is, so the second of the two copies
/// cannot be written.
#[cfg(unix)]
#[test]
fn a_capture_that_fails_partway_leaves_the_source_untouched() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project = ws.join("projects/app");
    let hook = project.join(".git/hooks/post-checkout");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(
        &hook,
        "#!/bin/sh\n[ -f rwv.lock ] && rm rwv.lock && mkdir rwv.lock\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    let manifest = append(&project.join("rwv.toml"), MARKER);
    let lock = append(&project.join("rwv.lock"), "\n");
    let before = status(&project);

    let (ok, report) = rwv(
        &ws,
        &["workweave", "app", "create", "seat", "--capture-dirty"],
    );
    let seat_lock = tmp
        .path()
        .join(".workweaves/app--seat/projects/app/rwv.lock");
    assert!(
        ok && seat_lock.is_dir(),
        "fixture: create must have succeeded with the lock's copy blocked:\n{report}"
    );
    assert_eq!(
        std::fs::read(
            tmp.path()
                .join(".workweaves/app--seat/projects/app/rwv.toml")
        )
        .unwrap(),
        manifest,
        "fixture: the manifest's copy must have gone through, so the failure is partway"
    );

    assert_eq!(std::fs::read(project.join("rwv.toml")).unwrap(), manifest);
    assert_eq!(std::fs::read(project.join("rwv.lock")).unwrap(), lock);
    assert_eq!(status(&project), before, "the source must be untouched");
    assert!(
        report.contains("nothing was discarded"),
        "and say so:\n{report}"
    );
}
