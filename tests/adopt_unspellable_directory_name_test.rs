//! `rwv add --adopt` refuses a clone whose position under the weave root
//! passes through a directory name no member path can spell, names that
//! directory and why, and leaves the renaming to the operator.

#![cfg(unix)]

use nix::errno::Errno;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

mod common;

fn weave(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    let project = ws.join("projects/test-project");
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "--initial-branch=main"]);
    std::fs::write(project.join("rwv.toml"), "[repositories]\n").unwrap();
    std::fs::write(ws.join(".rwv-active"), "test-project\n").unwrap();
    ws
}

fn clone_into(tmp: &Path, dest: &Path) {
    let bare = tmp.join("remotes/lib.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    common::git_in(
        tmp,
        &["clone", &bare.to_string_lossy(), &dest.to_string_lossy()],
    );
}

fn adopt(ws: &Path, path: &Path) -> (bool, String) {
    let out = common::rwv()
        .arg("add")
        .arg("--adopt")
        .arg(path)
        .current_dir(ws)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, [token], "stderr was:\n{stderr}");
}

/// The refusal `dir` gives a directory whose name is not UTF-8, or `None`
/// where it holds one.
///
/// APFS refuses such a name with EILSEQ, and so does ZFS with `utf8only`. A
/// casefolded Linux directory with strict encoding refuses it with EINVAL,
/// which mkdir(2) documents as a final component the filesystem does not
/// permit; in a fresh, empty parent nothing else raises it. Any other refusal
/// is not an answer about the name, and fails here.
fn refusal_of_a_name_that_is_not_unicode(dir: &Path) -> Option<Errno> {
    let parent = dir.join("not-unicode-probe");
    std::fs::create_dir(&parent).unwrap();
    let probe = parent.join(std::ffi::OsStr::from_bytes(b"lib\xff"));
    let refusal = match std::fs::create_dir(&probe) {
        Ok(()) => {
            std::fs::remove_dir(&probe).unwrap();
            None
        }
        Err(e) => match e.raw_os_error().map(Errno::from_raw) {
            Some(errno @ (Errno::EILSEQ | Errno::EINVAL)) => Some(errno),
            _ => panic!("the probe was refused for a reason other than the name: {e}"),
        },
    };
    std::fs::remove_dir(&parent).unwrap();
    refusal
}

fn manifest_keys(ws: &Path) -> Vec<String> {
    let raw = std::fs::read_to_string(ws.join("projects/test-project/rwv.toml")).unwrap();
    let doc: toml::Table = raw.parse().unwrap();
    doc.get("repositories")
        .and_then(|r| r.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

fn manifest_unchanged(ws: &Path) {
    assert_eq!(
        std::fs::read_to_string(ws.join("projects/test-project/rwv.toml")).unwrap(),
        "[repositories]\n"
    );
}

#[test]
fn a_backslash_in_a_directory_name_refuses_naming_the_rename() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    clone_into(tmp.path(), &ws.join("github/o/a\\b"));

    let (ok, stderr) = adopt(&ws, Path::new("github/o/a\\b"));

    assert!(!ok, "stderr was:\n{stderr}");
    assert_routes_to(&stderr, "unspellable-directory-name");
    assert!(
        stderr.contains("'a\\b'") && stderr.contains("Rename the directory"),
        "the refusal names the directory and the rename; stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("holds a `\\`, which no member path can hold")
            && !stderr.contains("not Unicode"),
        "the refusal states the backslash as its reason; stderr was:\n{stderr}"
    );
    manifest_unchanged(&ws);

    let findings_before = common::doctor_findings(&ws);
    std::fs::rename(ws.join("github/o/a\\b"), ws.join("github/o/ab")).unwrap();
    let (ok, stderr) = adopt(&ws, Path::new("github/o/ab"));
    assert!(ok, "adopting by the new path:\n{stderr}");
    assert!(manifest_keys(&ws).contains(&"github/o/ab".to_owned()));
    common::assert_no_new_findings(&ws, &findings_before);
}

#[test]
fn a_directory_name_that_is_not_unicode_refuses_naming_the_rename() {
    let tmp = common::tempdir().unwrap();
    if let Some(errno) = refusal_of_a_name_that_is_not_unicode(tmp.path()) {
        common::report_skip(&format!(
            "this filesystem refuses a directory name that is not UTF-8 ({errno}), \
             so adopt's refusal of one is unexercised here"
        ));
        return;
    }
    let ws = weave(tmp.path());
    let spelled = ws.join("github/o/lib");
    clone_into(tmp.path(), &spelled);
    let unspellable = ws
        .join("github/o")
        .join(std::ffi::OsStr::from_bytes(b"lib\xff"));
    std::fs::rename(&spelled, &unspellable).unwrap();
    let link = tmp.path().join("to-the-clone");
    std::os::unix::fs::symlink(&unspellable, &link).unwrap();

    let (ok, stderr) = adopt(&ws, &link);

    assert!(!ok, "stderr was:\n{stderr}");
    assert_routes_to(&stderr, "unspellable-directory-name");
    assert!(
        stderr.contains(r#""lib\xFF""#) && stderr.contains("not Unicode"),
        "the refusal names the directory and why; stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("Rename the directory"),
        "stderr was:\n{stderr}"
    );
    manifest_unchanged(&ws);

    let findings_before = common::doctor_findings(&ws);
    std::fs::rename(&unspellable, &spelled).unwrap();
    let (ok, stderr) = adopt(&ws, Path::new("github/o/lib"));
    assert!(ok, "adopting by the new path:\n{stderr}");
    assert_eq!(manifest_keys(&ws), vec!["github/o/lib".to_owned()]);
    common::assert_no_new_findings(&ws, &findings_before);
}
