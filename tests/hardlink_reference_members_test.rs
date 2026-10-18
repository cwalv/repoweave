//! A `reference` member's files are shared between seats by hard link where
//! the backend declares that an update in one workspace leaves another's file
//! alone, and where nothing better can be placed: each link is to another
//! seat's read-only reference copy. Anything that can be written in place,
//! and every non-reference member, keeps a file of its own.
//!
//! Unix only: whether two names are one file is read from the inode, which a
//! stable `std` exposes nowhere else, and no Windows platform declares links.

#![cfg(unix)]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

mod common;

const REFERENCE: &str = "github/org/ref";
const OWNED: &str = "github/org/own";
const PROJECT: &str = "app";
const FILES: &[&str] = &["README", "src/a.txt", "src/b.txt"];

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path.join("src")).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    for file in FILES {
        std::fs::write(path.join(file), format!("{file}\n")).unwrap();
    }
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

/// A primary whose one project lists [`REFERENCE`] and [`OWNED`].
fn primary(parent: &Path) -> PathBuf {
    let ws = parent.join("primary");
    let mut manifest = String::new();
    let mut pins = Vec::new();
    for (repo, role) in [(REFERENCE, "reference"), (OWNED, "owned")] {
        let clone = ws.join(repo);
        let sha = init_repo(&clone);
        manifest.push_str(&format!(
            "[repositories.\"{repo}\"]\ntype = \"git\"\nurl = \"file://{}\"\n\
             version = \"main\"\nrole = \"{role}\"\n\n",
            common::url_path(&clone)
        ));
        pins.push((repo, common::file_url(&clone), sha));
    }
    let project_dir = ws.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project_dir).unwrap();
    common::git_in(&project_dir, &["init", "-b", "main"]);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    let pins: Vec<(&str, &str, &str)> = pins
        .iter()
        .map(|(repo, url, sha)| (*repo, url.as_str(), sha.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &pins);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    ws
}

/// Create seat `name` from the primary; returns its root and create's stderr.
fn seat(primary: &Path, name: &str) -> (PathBuf, String) {
    let output = common::rwv()
        .args(["workweave", PROJECT, "create", name])
        .current_dir(primary)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "create {name} failed:\n{stderr}");
    let root = primary
        .parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{PROJECT}--{name}"));
    (root, stderr)
}

fn run(args: &[&str], cwd: &Path) {
    let output = common::rwv().args(args).current_dir(cwd).output().unwrap();
    assert!(
        output.status.success(),
        "rwv {args:?} failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `(device, inode)`: two paths with the same one are one file.
fn file_of(path: &Path) -> (u64, u64) {
    let meta = std::fs::metadata(path).unwrap();
    (meta.dev(), meta.ino())
}

fn writable(path: &Path) -> bool {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o222 != 0
}

/// Whether seats here share reference files by link: the backend declares
/// links on Linux, and a filesystem that clones places a clone first.
fn links_expected(dir: &Path) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let out = std::process::Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(dir)
        .output()
        .unwrap();
    matches!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "ext2/ext3" | "tmpfs"
    )
}

/// Where links are expected, every reference file of the two seats is one
/// file; a test about what happens to a shared file tests nothing otherwise.
fn assert_shared_when_expected(one: &Path, two: &Path, dir: &Path) {
    if links_expected(dir) {
        for file in FILES {
            assert_eq!(
                file_of(&one.join(REFERENCE).join(file)),
                file_of(&two.join(REFERENCE).join(file)),
                "fixture check: {file} is not shared between the seats"
            );
        }
    }
}

#[test]
fn a_second_seat_shares_its_reference_files_with_the_first_and_never_its_owned_ones() {
    let tmp = common::tempdir().unwrap();
    let primary = primary(tmp.path());
    let (one, _) = seat(&primary, "one");
    let (two, stderr) = seat(&primary, "two");
    let links = links_expected(tmp.path());

    for file in FILES {
        let theirs = one.join(REFERENCE).join(file);
        let ours = two.join(REFERENCE).join(file);
        assert!(!writable(&ours), "{file} is writable in a reference member");
        assert_eq!(
            file_of(&ours) == file_of(&theirs),
            links,
            "{file}: sharing with the first seat, on {}",
            std::env::consts::OS
        );
        for other in [one.join(OWNED), primary.join(OWNED)] {
            assert_ne!(
                file_of(&two.join(OWNED).join(file)),
                file_of(&other.join(file)),
                "the owned member's {file} shares a file with {}",
                other.display()
            );
        }
        assert_eq!(
            std::fs::metadata(two.join(OWNED).join(file))
                .unwrap()
                .nlink(),
            1
        );
    }
    if links {
        let expected = format!(
            "rwv workweave create: {REFERENCE}: 0 reflinked, {} hardlinked, 0 written",
            FILES.len()
        );
        assert!(
            stderr.lines().any(|l| l == expected),
            "no `{expected}` in:\n{stderr}"
        );
    }
}

#[test]
fn deleting_a_seat_leaves_the_files_another_seat_shared_with_it() {
    let tmp = common::tempdir().unwrap();
    let primary = primary(tmp.path());
    let (one, _) = seat(&primary, "one");
    let (two, _) = seat(&primary, "two");
    assert_shared_when_expected(&one, &two, tmp.path());
    let before: Vec<_> = FILES
        .iter()
        .map(|f| file_of(&two.join(REFERENCE).join(f)))
        .collect();

    run(&["workweave", PROJECT, "delete", "one"], &primary);

    assert!(!one.exists());
    for (file, file_before) in FILES.iter().zip(before) {
        let ours = two.join(REFERENCE).join(file);
        assert_eq!(common::read_normalized(&ours), format!("{file}\n"));
        assert_eq!(file_of(&ours), file_before);
        assert!(!writable(&ours));
        assert_eq!(std::fs::metadata(&ours).unwrap().nlink(), 1);
    }
    assert_eq!(
        common::git_in(two.join(REFERENCE), &["status", "--porcelain"]),
        ""
    );
}

#[test]
fn a_sync_that_moves_one_seat_leaves_the_file_another_seat_shared_with_it() {
    let tmp = common::tempdir().unwrap();
    let primary = primary(tmp.path());
    let (one, _) = seat(&primary, "one");
    let (two, _) = seat(&primary, "two");
    assert_shared_when_expected(&one, &two, tmp.path());
    let shared = one.join(REFERENCE).join("src/a.txt");
    let before = (
        file_of(&shared),
        std::fs::metadata(&shared).unwrap().permissions().mode(),
    );
    let clone = primary.join(REFERENCE);
    std::fs::write(clone.join("src/a.txt"), "moved on\n").unwrap();
    common::git_in(&clone, &["commit", "-qam", "advance"]);
    run(&["lock", "--commit"], &primary);

    run(&["sync", "primary"], &two);

    assert_eq!(
        common::read_normalized(two.join(REFERENCE).join("src/a.txt")),
        "moved on\n"
    );
    assert_eq!(common::read_normalized(&shared), "src/a.txt\n");
    assert_eq!(
        (
            file_of(&shared),
            std::fs::metadata(&shared).unwrap().permissions().mode()
        ),
        before,
        "the other seat's file changed under the sync"
    );
}

#[test]
fn leaving_reference_gives_a_seat_private_writable_copies_and_leaves_the_other_read_only() {
    let tmp = common::tempdir().unwrap();
    let primary = primary(tmp.path());
    let (one, _) = seat(&primary, "one");
    let (two, _) = seat(&primary, "two");
    assert_shared_when_expected(&one, &two, tmp.path());
    let theirs_before: Vec<_> = FILES
        .iter()
        .map(|f| file_of(&one.join(REFERENCE).join(f)))
        .collect();

    run(&["role", REFERENCE, "owned"], &two);

    for (file, theirs_before) in FILES.iter().zip(theirs_before) {
        let ours = two.join(REFERENCE).join(file);
        let theirs = one.join(REFERENCE).join(file);
        assert!(
            writable(&ours),
            "{file} is still read-only after leaving reference"
        );
        assert_ne!(file_of(&ours), file_of(&theirs), "{file} is still shared");
        assert_eq!(common::read_normalized(&ours), format!("{file}\n"));
        assert!(
            !writable(&theirs),
            "the other seat's {file} became writable"
        );
        assert_eq!(file_of(&theirs), theirs_before);
    }
}
