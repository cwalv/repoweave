//! `rwv member set-recurse-submodules <repo> <true|false>` and
//! `rwv member add --no-recurse-submodules` record whether new checkouts of a
//! member populate its submodules.
//!
//!   - `false` writes `recurse-submodules = false` on the member's entry and
//!     `true` takes the key out again, `true` being the default;
//!   - a value `rwv.toml` already holds leaves it alone;
//!   - a path the manifest does not list is refused, and a value that is not
//!     a boolean is rejected before anything is read;
//!   - `member add --no-recurse-submodules` records `false` on the entry it
//!     adds.

use std::path::{Path, PathBuf};

use repoweave::manifest::{Manifest, SubmoduleRecursion};

mod common;

fn run(args: &[&str], dir: &Path) -> std::process::Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn describe(out: &std::process::Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn succeed(args: &[&str], dir: &Path) -> std::process::Output {
    let out = run(args, dir);
    assert!(
        out.status.success(),
        "`rwv {}` failed; {}",
        args.join(" "),
        describe(&out)
    );
    out
}

/// A bare repo with one commit, as a `file://` URL.
fn origin(tmp: &Path, name: &str) -> String {
    let bare = tmp.join("origin").join(format!("{name}.git"));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::git_in(
        tmp,
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    let seed = tmp.join(format!("seed-{name}"));
    common::git_in(
        tmp,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            seed.to_str().unwrap(),
        ],
    );
    common::git_in(&seed, &["config", "user.email", "test@test.com"]);
    common::git_in(&seed, &["config", "user.name", "Test"]);
    std::fs::write(seed.join("README"), "hello\n").unwrap();
    common::git_in(&seed, &["add", "-A"]);
    common::git_in(&seed, &["commit", "-qm", "initial"]);
    common::git_in(&seed, &["push", "-q", "origin", "main"]);
    std::fs::remove_dir_all(&seed).unwrap();
    common::file_url(&bare)
}

struct Weave {
    ws: PathBuf,
}

impl Weave {
    fn manifest_path(&self) -> PathBuf {
        self.ws.join("projects/demo/rwv.toml")
    }

    fn recursion_of(&self, repo: &str) -> SubmoduleRecursion {
        Manifest::from_path(&self.manifest_path())
            .unwrap()
            .iter_entries()
            .find(|(path, _)| path.as_str() == repo)
            .map(|(_, entry)| entry.recurse_submodules)
            .unwrap_or_else(|| panic!("no entry for {repo}"))
    }

    fn manifest_text(&self) -> String {
        std::fs::read_to_string(self.manifest_path()).unwrap()
    }
}

const ALPHA: &str = "local/origin/alpha";

/// A weave made by `rwv init demo` holding one member, alpha.
fn make_weave(tmp: &Path) -> Weave {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    succeed(&["init", "demo"], &ws);
    succeed(&["member", "add", &origin(tmp, "alpha")], &ws);
    Weave { ws }
}

#[test]
fn false_is_written_and_true_takes_the_key_out_again() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    assert_eq!(weave.recursion_of(ALPHA), SubmoduleRecursion::Recurse);
    assert!(!weave.manifest_text().contains("recurse-submodules"));

    succeed(
        &["member", "set-recurse-submodules", ALPHA, "false"],
        &weave.ws,
    );
    assert_eq!(weave.recursion_of(ALPHA), SubmoduleRecursion::Skip);
    assert!(
        weave.manifest_text().contains("recurse-submodules = false"),
        "{}",
        weave.manifest_text()
    );

    succeed(
        &["member", "set-recurse-submodules", ALPHA, "true"],
        &weave.ws,
    );
    assert_eq!(weave.recursion_of(ALPHA), SubmoduleRecursion::Recurse);
    assert!(
        !weave.manifest_text().contains("recurse-submodules"),
        "the default is not written out:\n{}",
        weave.manifest_text()
    );
}

#[test]
fn a_value_the_manifest_already_holds_changes_nothing() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let before = weave.manifest_text();

    let out = succeed(
        &["member", "set-recurse-submodules", ALPHA, "true"],
        &weave.ws,
    );

    assert_eq!(weave.manifest_text(), before);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already has recurse-submodules = true"),
        "{}",
        describe(&out)
    );
}

#[test]
fn a_path_the_manifest_does_not_list_is_refused() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let before = weave.manifest_text();

    let out = run(
        &[
            "member",
            "set-recurse-submodules",
            "github/nobody/nothing",
            "false",
        ],
        &weave.ws,
    );

    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found in manifest")
            && stderr.contains("rwv explain repo-not-in-manifest"),
        "{stderr}"
    );
    assert_eq!(weave.manifest_text(), before);
}

#[test]
fn a_value_that_is_not_a_boolean_is_rejected_before_anything_is_read() {
    let tmp = common::tempdir().unwrap();

    let out = run(
        &["member", "set-recurse-submodules", ALPHA, "maybe"],
        tmp.path(),
    );

    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid value 'maybe'")
            && stderr.contains("true")
            && stderr.contains("false"),
        "{stderr}"
    );
}

#[test]
fn add_with_no_recurse_submodules_records_false_on_the_new_entry() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());

    succeed(
        &[
            "member",
            "add",
            &origin(tmp.path(), "beta"),
            "--no-recurse-submodules",
        ],
        &weave.ws,
    );

    assert_eq!(
        weave.recursion_of("local/origin/beta"),
        SubmoduleRecursion::Skip
    );
    assert_eq!(weave.recursion_of(ALPHA), SubmoduleRecursion::Recurse);
}
