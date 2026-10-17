//! `rwv doctor --fix` for a workweave member that is a full clone of its own
//! instead of a worktree of the canonical clone (`standalone-in-workweave`).
//!
//!   - the clone is replaced by a worktree on the workweave's ephemeral
//!     branch, at the commit the clone had checked out, with nothing left
//!     behind and the canonical clone's checkout untouched;
//!   - while the clone holds anything the canonical clone would not give
//!     back, or there is nothing to link a worktree into, the repair refuses
//!     for that member, names why, and leaves the clone exactly as it was;
//!   - a birth that refuses after the clone was moved aside moves it back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod common;

const MEMBER: &str = "github/org/widget";
const PROJECT: &str = "app";
const SEAT: &str = "seat";

struct Weave {
    primary: PathBuf,
    seat: PathBuf,
    canonical: PathBuf,
    /// The member slot in the workweave, holding a full clone.
    clone: PathBuf,
    pin: String,
}

/// A workweave created by `rwv workweave create` whose member was then
/// deleted and cloned afresh from the canonical clone, so the canonical
/// clone still holds the workweave's branch, its receipt and a worktree
/// registration for the slot.
fn weave_with_standalone_member(tmp: &Path) -> Weave {
    let primary = tmp.join("primary");
    let canonical = primary.join(MEMBER);
    std::fs::create_dir_all(&canonical).unwrap();
    common::git_in(&canonical, &["init", "-b", "main"]);
    std::fs::write(canonical.join("README.md"), "widget\n").unwrap();
    common::git_in(&canonical, &["add", "."]);
    common::git_in(&canonical, &["commit", "-m", "initial"]);
    let pin = common::git_in(&canonical, &["rev-parse", "HEAD"]);

    let project_dir = primary.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project_dir).unwrap();
    common::git_in(&project_dir, &["init", "-b", "main"]);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"{MEMBER}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::file_url(&canonical)
        ),
    )
    .unwrap();
    let url = common::file_url(&canonical);
    common::fixture_lock(&project_dir, &[(MEMBER, url.as_str(), pin.as_str())]);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "manifest and lock"]);
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    common::rwv()
        .args(["workweave", PROJECT, "create", SEAT])
        .current_dir(&primary)
        .assert()
        .success();
    let seat = tmp.join(".workweaves").join(format!("{PROJECT}--{SEAT}"));
    let clone = seat.join(MEMBER);
    assert!(
        clone.join(".git").is_file(),
        "fixture: created as a worktree"
    );

    std::fs::remove_dir_all(&clone).unwrap();
    common::git_in(
        seat.join("github/org"),
        &["clone", "--quiet", canonical.to_str().unwrap(), "widget"],
    );
    assert!(clone.join(".git").is_dir(), "fixture: now a full clone");

    Weave {
        primary,
        seat,
        canonical,
        clone,
        pin,
    }
}

/// `(exit code, stdout)` of `rwv doctor --fix` in the workweave.
fn doctor_fix(weave: &Weave) -> (Option<i32>, String) {
    let out = common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(&weave.seat)
        .output()
        .unwrap();
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn standalone_findings(dir: &Path) -> usize {
    let out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(dir)
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json must print JSON ({e}); stdout:\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| {
            v["kind"] == "clone-topology"
                && v["sub_kind"]
                    .as_object()
                    .is_some_and(|o| o.contains_key("standalone-in-workweave"))
        })
        .count()
}

/// Every ref of a repository, and its HEAD and status.
fn repo_state(repo: &Path) -> (String, String, String) {
    (
        common::git_in(repo, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        common::git_in(repo, &["rev-parse", "HEAD"]),
        common::git_in(repo, &["status", "--porcelain", "--ignored"]),
    )
}

/// Every entry under `root`, `.git` included, keyed by its path relative to
/// `root`: its kind, its permissions, and its bytes (a link's target).
fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, (&'static str, u32, Vec<u8>)> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            #[cfg(unix)]
            let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions());
            #[cfg(not(unix))]
            let mode = u32::from(meta.permissions().readonly());
            let (kind, bytes) = if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                ("link", target.to_string_lossy().into_owned().into_bytes())
            } else if meta.is_dir() {
                pending.push(path.clone());
                ("dir", Vec::new())
            } else {
                ("file", std::fs::read(&path).unwrap())
            };
            out.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (kind, mode, bytes),
            );
        }
    }
    out
}

/// Let git rewrite the clone's index once its entries can no longer be
/// racily clean, so the reads doctor makes have no reason to rewrite it and
/// a byte comparison sees only what the repair did.
fn settle_index(clone: &Path) {
    std::thread::sleep(std::time::Duration::from_millis(1100));
    common::git_in(clone, &["status", "--porcelain"]);
}

/// The entries listed under the reason line of `report` containing `heading`.
fn listed_under<'a>(report: &'a str, heading: &str) -> Vec<&'a str> {
    report
        .lines()
        .skip_while(|l| !l.contains(heading))
        .skip(1)
        .take_while(|l| l.starts_with("      "))
        .map(str::trim)
        .collect()
}

fn store_of(checkout: &Path) -> PathBuf {
    let common_dir = common::git_in(checkout, &["rev-parse", "--git-common-dir"]);
    std::fs::canonicalize(checkout.join(common_dir)).unwrap()
}

fn aside(weave: &Weave) -> PathBuf {
    weave.seat.join("github/org/.widget.rwv-replaced")
}

#[test]
fn doctor_fix_replaces_the_clone_with_a_worktree_at_the_commit_it_had_checked_out() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());

    common::git_in(&weave.clone, &["checkout", "--quiet", "-b", "feature"]);
    std::fs::write(weave.clone.join("README.md"), "widget, featured\n").unwrap();
    common::git_in(&weave.clone, &["commit", "--quiet", "-am", "feature"]);
    let checked_out = common::git_in(&weave.clone, &["rev-parse", "HEAD"]);
    common::git_in(
        &weave.canonical,
        &[
            "fetch",
            "--quiet",
            weave.clone.to_str().unwrap(),
            "feature:feature",
        ],
    );
    assert_ne!(checked_out, weave.pin, "fixture: the clone is past the pin");
    assert_eq!(standalone_findings(&weave.seat), 1);
    let canonical_before = repo_state(&weave.canonical);

    // The exit status is not asserted: the clone stood past the pin before
    // the repair and the worktree stands where it stood, so `stale-lock` is
    // reported either way.
    let (_, out) = doctor_fix(&weave);
    assert!(
        !out.contains("clone-topology"),
        "doctor --fix must repair the member:\n{out}"
    );
    assert!(
        out.contains(&format!(
            "[fixed] core: replaced the full clone of {MEMBER} in workweave {SEAT} with a \
             worktree of the canonical clone at {}",
            &checked_out[..12]
        )),
        "{out}"
    );

    assert!(
        weave.clone.join(".git").is_file(),
        "the member must now be a linked worktree"
    );
    assert_eq!(
        store_of(&weave.clone),
        std::fs::canonicalize(weave.canonical.join(".git")).unwrap(),
        "the worktree must link into the canonical clone's store"
    );
    assert_eq!(
        common::git_in(&weave.clone, &["symbolic-ref", "--short", "HEAD"]),
        format!("{PROJECT}--{SEAT}"),
    );
    assert_eq!(
        common::git_in(&weave.clone, &["rev-parse", "HEAD"]),
        checked_out,
        "born at the commit the clone had checked out, not at the pin"
    );
    assert_eq!(
        std::fs::read_to_string(weave.clone.join("README.md")).unwrap(),
        "widget, featured\n"
    );
    assert!(
        !aside(&weave).exists(),
        "the replaced clone must not be left behind"
    );

    let canonical_after = repo_state(&weave.canonical);
    assert_eq!(
        (&canonical_after.1, &canonical_after.2),
        (&canonical_before.1, &canonical_before.2),
        "the canonical clone's checkout is not touched"
    );
    assert!(
        canonical_after
            .0
            .lines()
            .any(|l| l == format!("refs/heads/{PROJECT}--{SEAT} {checked_out}")),
        "the workweave's branch stands at the commit the clone had checked out:\n{}",
        canonical_after.0
    );

    assert_eq!(standalone_findings(&weave.seat), 0);
    assert_eq!(standalone_findings(&weave.primary), 0);
    common::rwv()
        .args(["status"])
        .current_dir(&weave.seat)
        .assert()
        .success();
}

/// Run `doctor --fix` over a clone that holds something, and assert it
/// refused for that member naming `reason` and `item`, and changed nothing.
fn assert_refused_and_untouched(weave: &Weave, reason: &str, item: &str) {
    settle_index(&weave.clone);
    let clone_before = repo_state(&weave.clone);
    let bytes_before = tree_bytes(&weave.clone);
    let canonical_before = repo_state(&weave.canonical);

    let (code, out) = doctor_fix(weave);
    assert_eq!(code, Some(1), "a refused repair is an error:\n{out}");
    let refusal = format!("clone-topology fix left {SEAT}/{MEMBER} standing");
    let line = out
        .lines()
        .position(|l| l.contains(&refusal))
        .unwrap_or_else(|| panic!("no refusal for the member:\n{out}"));
    let report = out.lines().skip(line).collect::<Vec<_>>().join("\n");
    assert!(report.contains(reason), "must say `{reason}`:\n{out}");
    assert!(report.contains(item), "must name `{item}`:\n{out}");
    assert!(!out.contains("[fixed] core: replaced"), "{out}");

    assert!(weave.clone.join(".git").is_dir(), "still a full clone");
    assert!(
        tree_bytes(&weave.clone) == bytes_before,
        "the clone must be byte-for-byte as it was"
    );
    assert_eq!(repo_state(&weave.clone), clone_before, "clone untouched");
    assert_eq!(
        repo_state(&weave.canonical),
        canonical_before,
        "canonical clone untouched"
    );
    assert!(!aside(weave).exists(), "nothing moved aside");
    assert_eq!(standalone_findings(&weave.seat), 1, "still reported");
}

#[test]
fn refuses_while_the_clone_holds_commits_the_canonical_clone_lacks() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    std::fs::write(weave.clone.join("README.md"), "only here\n").unwrap();
    common::git_in(&weave.clone, &["commit", "--quiet", "-am", "unique"]);

    assert_refused_and_untouched(
        &weave,
        "hold commits no ref of the canonical clone",
        "refs/heads/main",
    );
}

#[test]
fn refuses_while_the_canonical_clone_holds_the_commits_but_no_ref_reaches_them() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    std::fs::write(weave.clone.join("README.md"), "unreferenced\n").unwrap();
    common::git_in(&weave.clone, &["commit", "--quiet", "-am", "unique"]);
    let unique = common::git_in(&weave.clone, &["rev-parse", "HEAD"]);
    common::git_in(
        &weave.canonical,
        &[
            "fetch",
            "--quiet",
            weave.clone.to_str().unwrap(),
            "main:held",
        ],
    );
    common::git_in(&weave.canonical, &["branch", "--quiet", "-D", "held"]);
    common::git_in(
        &weave.canonical,
        &["cat-file", "-e", &format!("{unique}^{{commit}}")],
    );

    assert_refused_and_untouched(
        &weave,
        "hold commits no ref of the canonical clone",
        "refs/heads/main",
    );
}

#[test]
fn refuses_while_the_clone_holds_uncommitted_changes() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    std::fs::write(weave.clone.join("README.md"), "edited\n").unwrap();

    assert_refused_and_untouched(
        &weave,
        "it holds uncommitted or untracked files",
        "README.md",
    );
}

#[test]
fn refuses_while_the_clone_holds_untracked_files() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    std::fs::write(weave.clone.join("notes.txt"), "scratch\n").unwrap();

    assert_refused_and_untouched(
        &weave,
        "it holds uncommitted or untracked files",
        "notes.txt",
    );
}

#[test]
fn refuses_while_the_clone_holds_ignored_files() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    std::fs::write(weave.clone.join(".git/info/exclude"), "*.env\n").unwrap();
    std::fs::write(weave.clone.join("local.env"), "TOKEN=only-here\n").unwrap();
    assert_eq!(
        common::git_in(&weave.clone, &["status", "--porcelain"]),
        "",
        "fixture: plain status reads clean"
    );

    assert_refused_and_untouched(&weave, "it holds ignored files", "local.env");
}

#[test]
fn lists_ignored_files_apart_from_untracked_ones_and_names_their_removal() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    std::fs::write(weave.clone.join(".git/info/exclude"), "*.env\n").unwrap();
    std::fs::write(weave.clone.join("local.env"), "TOKEN=only-here\n").unwrap();
    std::fs::write(weave.clone.join("notes.txt"), "scratch\n").unwrap();

    let (code, out) = doctor_fix(&weave);
    assert_eq!(code, Some(1), "{out}");
    assert_eq!(
        listed_under(&out, "it holds uncommitted or untracked files"),
        vec!["notes.txt"],
        "{out}"
    );
    assert_eq!(
        listed_under(&out, "it holds ignored files"),
        vec!["local.env"],
        "{out}"
    );
    let heading = out
        .lines()
        .find(|l| l.contains("it holds ignored files"))
        .unwrap();
    assert!(
        heading.contains("widget clean -X -n`") && heading.contains("widget clean -X -f`"),
        "the ignored files' heading must name the preview and the removal: {heading}"
    );
    assert_eq!(
        std::fs::read_to_string(weave.clone.join("local.env")).unwrap(),
        "TOKEN=only-here\n",
        "rwv names the removal and never runs it"
    );
}

#[test]
fn refuses_while_worktrees_link_into_the_clone() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    let dependent = tmp.path().join("dependent");
    common::git_in(
        &weave.clone,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "other",
            dependent.to_str().unwrap(),
        ],
    );

    assert_refused_and_untouched(&weave, "other worktrees link into its store", "dependent");
}

#[test]
fn refuses_while_something_stands_where_the_clone_would_be_moved() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    let occupant = aside(&weave);
    std::fs::create_dir_all(&occupant).unwrap();
    std::fs::write(occupant.join("kept.txt"), "left by an earlier run\n").unwrap();
    let clone_before = repo_state(&weave.clone);

    let (code, out) = doctor_fix(&weave);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains(&format!("clone-topology fix left {SEAT}/{MEMBER} standing")),
        "{out}"
    );
    assert!(out.contains(".widget.rwv-replaced already exists"), "{out}");
    assert_eq!(
        std::fs::read_to_string(occupant.join("kept.txt")).unwrap(),
        "left by an earlier run\n",
        "the occupant is not touched"
    );
    assert!(weave.clone.join(".git").is_dir(), "still a full clone");
    assert_eq!(repo_state(&weave.clone), clone_before, "clone untouched");
}

#[test]
fn a_birth_that_refuses_moves_the_clone_back() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    let side = common::git_in(
        &weave.canonical,
        &[
            "commit-tree",
            "-p",
            &weave.pin,
            "-m",
            "side",
            &format!("{}^{{tree}}", weave.pin),
        ],
    );
    common::git_in(
        &weave.canonical,
        &[
            "update-ref",
            &format!("refs/heads/{PROJECT}--{SEAT}"),
            &side,
        ],
    );
    settle_index(&weave.clone);
    let clone_before = repo_state(&weave.clone);
    let bytes_before = tree_bytes(&weave.clone);

    let (code, out) = doctor_fix(&weave);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains(&format!("clone-topology fix left {SEAT}/{MEMBER} standing")),
        "{out}"
    );
    assert!(out.contains("has moved since rwv created it"), "{out}");
    assert!(
        weave.clone.join(".git").is_dir(),
        "the clone must be back at the slot, not stranded or gone:\n{out}"
    );
    assert!(
        tree_bytes(&weave.clone) == bytes_before,
        "the clone must be byte-for-byte as it was"
    );
    assert_eq!(repo_state(&weave.clone), clone_before, "clone untouched");
    assert!(!aside(&weave).exists(), "nothing left aside");
    assert_eq!(
        common::git_in(
            &weave.canonical,
            &["rev-parse", &format!("refs/heads/{PROJECT}--{SEAT}")]
        ),
        side,
        "the moved branch is not destroyed"
    );
}

#[test]
fn refuses_while_there_is_no_canonical_clone_to_link_into() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    std::fs::remove_dir_all(&weave.canonical).unwrap();
    let clone_before = repo_state(&weave.clone);

    let (code, out) = doctor_fix(&weave);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains(&format!("clone-topology fix left {SEAT}/{MEMBER} standing")),
        "{out}"
    );
    assert!(out.contains("there is no canonical clone at"), "{out}");
    assert!(weave.clone.join(".git").is_dir(), "still a full clone");
    assert_eq!(repo_state(&weave.clone), clone_before, "clone untouched");
    assert!(!aside(&weave).exists(), "nothing moved aside");
}

#[test]
fn refuses_while_the_clone_has_no_commit_checked_out() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_with_standalone_member(tmp.path());
    common::git_in(&weave.clone, &["symbolic-ref", "HEAD", "refs/heads/unborn"]);
    let refs_before = common::git_in(
        &weave.clone,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );

    let (code, out) = doctor_fix(&weave);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains(&format!("clone-topology fix left {SEAT}/{MEMBER} standing")),
        "{out}"
    );
    assert!(out.contains("its HEAD names no commit"), "{out}");
    assert!(weave.clone.join(".git").is_dir(), "still a full clone");
    assert_eq!(
        common::git_in(
            &weave.clone,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        refs_before
    );
}
