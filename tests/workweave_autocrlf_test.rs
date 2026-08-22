//! Pins `rwv workweave create` against git's clean/smudge filter.
//!
//! Under `core.autocrlf = true` — Git for Windows' installer default, and
//! what CI's Windows runners ship — a fresh worktree checkout smudges LF
//! blobs to CRLF on disk while the source's working tree still holds the LF
//! bytes the fixture wrote. The two spellings are one content to git and two
//! byte strings to `std::fs::read`. Whether `rwv.toml` / `rwv.lock` carry
//! uncommitted changes is therefore git's question: an overlay driven by
//! byte inequality writes the source's line endings into a tree whose index
//! expects the filtered ones, and the fresh workweave is born with tracked
//! dirt that blocks `rwv lock --commit` and every later verb with a
//! clean-tree precondition.
//!
//! The cause is forced here with a `HOME` whose `.gitconfig` turns the
//! filter on, so the pin holds on every host rather than only where the
//! platform supplies the config.

use std::path::{Path, PathBuf};

mod common;

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let status = common::git()
        .env("HOME", home)
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn git_out(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = common::git()
        .env("HOME", home)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should be available");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}",
        dir.display()
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A weave whose project repo has `rwv.toml` + `rwv.lock` committed with LF,
/// under a `HOME` that smudges checkouts to CRLF.
fn smudging_weave(tmp: &Path) -> (PathBuf, PathBuf) {
    let home = tmp.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(".gitconfig"),
        "[core]\n\tautocrlf = true\n[user]\n\temail = test@test.com\n\tname = Test\n",
    )
    .unwrap();

    let ws = tmp.join("ws");
    let lib = ws.join("github/org/lib");
    std::fs::create_dir_all(&lib).unwrap();
    git(&home, &lib, &["init", "-q", "-b", "main"]);
    std::fs::write(lib.join("README.md"), "init\n").unwrap();
    git(&home, &lib, &["add", "-A"]);
    git(&home, &lib, &["commit", "-qm", "initial"]);
    let sha = git_out(&home, &lib, &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let app = ws.join("projects/app");
    std::fs::create_dir_all(&app).unwrap();
    git(&home, &app, &["init", "-q", "-b", "main"]);
    std::fs::write(app.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    std::fs::write(
        app.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/lib\"]\ntype = \"git\"\nurl = \"file://{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::url_path(&lib)
        ),
    )
    .unwrap();
    let lib_url = common::file_url(&lib);
    common::fixture_lock(&app, &[("github/org/lib", &lib_url, &sha)]);
    git(&home, &app, &["add", "-A"]);
    git(&home, &app, &["commit", "-qm", "lock: initial"]);

    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();
    (home, ws)
}

#[test]
fn a_clean_source_births_a_clean_workweave_under_the_eol_filter() {
    let tmp = common::tempdir().unwrap();
    let (home, ws) = smudging_weave(tmp.path());

    common::rwv()
        .env("HOME", &home)
        .args(["workweave", "app", "create", "ww1"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww = tmp.path().join(".workweaves/app--ww1");
    let status = git_out(&home, &ww.join("projects/app"), &["status", "--porcelain"]);
    assert!(
        status.is_empty(),
        "a workweave created from a clean source must be clean under the \
         same git config that checked it out; status reports:\n{status}"
    );

    common::rwv()
        .env("HOME", &home)
        .args(["lock", "--commit"])
        .current_dir(&ww)
        .assert()
        .success();
}

#[test]
fn a_genuinely_dirty_lock_is_still_captured_with_capture_dirty() {
    let tmp = common::tempdir().unwrap();
    let (home, ws) = smudging_weave(tmp.path());

    let app = ws.join("projects/app");
    // raw lock bytes: an append onto the committed lock, so it reads as
    // tracked-dirty for `--capture-dirty` to carry. The shared builder writes
    // a whole file for given content and cannot make an existing one dirty.
    let committed = std::fs::read(app.join("rwv.lock")).unwrap();
    let mut dirty = committed.clone();
    dirty.extend_from_slice(b"\n");
    std::fs::write(app.join("rwv.lock"), &dirty).unwrap();

    common::rwv()
        .env("HOME", &home)
        .args(["workweave", "app", "create", "ww1", "--capture-dirty"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_lock = tmp
        .path()
        .join(".workweaves/app--ww1/projects/app/rwv.lock");
    let captured = std::fs::read(ww_lock).unwrap();
    assert_eq!(
        captured, dirty,
        "the workweave must hold the source's dirty lock bytes verbatim"
    );
}
