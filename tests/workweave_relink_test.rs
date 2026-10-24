//! A workweave member checkout that git can no longer open, or that is gone,
//! is given back through what the canonical clone still holds for it: its
//! registration of the checkout, else the workweave's branch at its own tip,
//! else that branch created at the revision `rwv.lock` pins. Each test
//! follows the advice the tool prints and asserts the state it leaves: the
//! checkout opens, the branch tip did not move, the commits are reachable,
//! and doctor reports what it reported before the damage.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const MEMBER: &str = "local/acme/alpha";
const BRANCH: &str = "demo--feat";

struct Weave {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    workweave: PathBuf,
}

impl Weave {
    /// A primary weave whose project `demo` holds one member, locked and
    /// committed, and a workweave `feat` of it.
    fn new() -> Self {
        let tmp = common::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        std::fs::create_dir_all(primary.join("projects")).unwrap();
        let origin = common::under(tmp.path(), "origins/acme/alpha.git");
        std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
        common::init_bare_repo_with_commit(&origin);
        rwv(&primary, &["init", "demo"]);
        rwv(&primary, &["add", &common::file_url(&origin)]);
        rwv(&primary, &["lock"]);
        let project = primary.join("projects").join("demo");
        common::git_in(&project, &["add", "-A"]);
        common::git_in(&project, &["commit", "--quiet", "-m", "init"]);
        rwv(&primary, &["workweave", "demo", "create", "feat"]);
        let workweave = tmp.path().join(".workweaves").join(BRANCH);
        assert!(common::under(&workweave, MEMBER).join(".git").is_file());
        Self {
            _tmp: tmp,
            primary,
            workweave,
        }
    }

    fn checkout(&self) -> PathBuf {
        common::under(&self.workweave, MEMBER)
    }

    fn canonical(&self) -> PathBuf {
        common::under(&self.primary, MEMBER)
    }

    /// Commit on the workweave's branch, past the revision the lock pins.
    fn commit_unlanded(&self) -> String {
        let checkout = self.checkout();
        std::fs::write(checkout.join("unlanded"), "work\n").unwrap();
        common::git_in(&checkout, &["add", "unlanded"]);
        common::git_in(&checkout, &["commit", "--quiet", "-m", "unlanded"]);
        head(&checkout)
    }

    fn branch_tip(&self) -> String {
        common::git_in(
            self.canonical(),
            &["rev-parse", &format!("refs/heads/{BRANCH}")],
        )
        .trim()
        .to_owned()
    }
}

fn rwv(dir: &Path, args: &[&str]) -> Output {
    let out = common::rwv().args(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "rwv {args:?} in {} failed:\n{}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// `rwv doctor --fix` in `dir`, whose exit status reports what it left
/// standing; a branch ahead of the lock leaves a stale-lock error by design,
/// so callers judge the state by [`findings`].
fn doctor_fix(dir: &Path) -> String {
    let out = common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(dir)
        .output()
        .unwrap();
    text(&out)
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn head(checkout: &Path) -> String {
    common::git_in(checkout, &["rev-parse", "HEAD"])
        .trim()
        .to_owned()
}

/// Every finding `rwv doctor --json` reports in `dir`, as `kind/sub-kind
/// path`, `--all` from the primary weave.
fn findings(dir: &Path, all: bool) -> Vec<String> {
    let mut args = vec!["doctor", "--json"];
    if all {
        args.push("--all");
    }
    let out = common::rwv().args(&args).current_dir(dir).output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{}", text(&out)));
    let mut found: Vec<String> = json["violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            let kind = v["kind"].as_str().unwrap_or_default();
            let path = v["path"].as_str().unwrap_or_default();
            match v["sub_kind"].as_str() {
                Some(sub) => format!("{kind}/{sub} {path}"),
                None => format!("{kind} {path}"),
            }
        })
        .collect();
    found.sort();
    found
}

/// The line `rwv doctor` prints in `dir` that contains `marker`.
fn advice(dir: &Path, args: &[&str], marker: &str) -> String {
    let out = common::rwv().args(args).current_dir(dir).output().unwrap();
    let report = text(&out);
    report
        .lines()
        .find(|l| l.contains(marker))
        .unwrap_or_else(|| panic!("no line naming `{marker}` from rwv {args:?}:\n{report}"))
        .to_owned()
}

fn assert_names(advice: &str, steps: &[&str]) {
    for step in steps {
        assert!(
            advice.contains(step),
            "the advice must name {step}: {advice}"
        );
    }
}

fn status(checkout: &Path) -> String {
    let out = common::git()
        .args(["status", "--porcelain", "--branch"])
        .current_dir(checkout)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git status must open the checkout: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn is_live_edit(finding: &str) -> bool {
    finding.starts_with("index-drift/") || finding.starts_with("working-tree-drift/")
}

// ---------------------------------------------------------------------------
// The canonical clone is deleted and cloned again: no registration and no
// branch survive, so the branch is created at the lock's revision.
// ---------------------------------------------------------------------------

#[test]
fn a_deleted_canonical_clone_is_fetched_then_the_workweave_checkout_is_relinked() {
    let w = Weave::new();
    std::fs::remove_dir_all(w.canonical()).unwrap();

    for dir in [&w.primary, &w.workweave] {
        let found = findings(dir, dir == &w.primary);
        assert!(
            found.contains(&format!("missing-canonical-clone {MEMBER}")),
            "{found:?}"
        );
        assert!(
            !found.iter().any(|f| is_live_edit(f)),
            "a checkout git cannot open holds no edits doctor can see: {found:?}"
        );
    }
    let printed = advice(&w.primary, &["doctor", "--all"], "canonical clone for");
    assert_names(
        &printed,
        &[
            "`rwv fetch --project demo` at the primary weave",
            "`rwv doctor --fix` there",
        ],
    );

    rwv(&w.primary, &["fetch", "--project", "demo"]);
    let between = findings(&w.workweave, false);
    assert!(
        between.contains(&format!("workweave-drift/dangling-link {MEMBER}"))
            && !between
                .iter()
                .any(|f| is_live_edit(f) || f.starts_with("dangling-reference")),
        "between the steps the checkout is a dangling link, never live edits: {between:?}"
    );
    let fixed = doctor_fix(&w.primary);
    assert!(
        fixed.contains(&format!(
            "relinked the worktree for {MEMBER} in workweave {BRANCH}: on branch `{BRANCH}`, \
             created at"
        )),
        "--fix says the branch was created at the lock's revision: {fixed}"
    );

    assert!(status(&w.checkout()).starts_with(&format!("## {BRANCH}\n")));
    assert_eq!(head(&w.checkout()), w.branch_tip());
    assert_eq!(findings(&w.primary, true), Vec::<String>::new());
    assert_eq!(findings(&w.workweave, false), Vec::<String>::new());
}

#[test]
fn an_uncommitted_edit_survives_the_relink_onto_a_recreated_branch() {
    let w = Weave::new();
    let edited = w.checkout().join("README");
    let original = std::fs::read_to_string(&edited).unwrap_or_default();
    std::fs::write(&edited, format!("{original}edited\n")).unwrap();
    std::fs::remove_dir_all(w.canonical()).unwrap();

    rwv(&w.primary, &["fetch", "--project", "demo"]);
    doctor_fix(&w.primary);

    assert_eq!(
        std::fs::read_to_string(&edited).unwrap(),
        format!("{original}edited\n")
    );
    assert!(
        status(&w.checkout()).contains(" M README"),
        "the edit reads as an edit of the recreated branch"
    );
}

// ---------------------------------------------------------------------------
// The canonical clone still registers the checkout: only the link is
// rewritten, and HEAD, index, branch and files stand as they were.
// ---------------------------------------------------------------------------

#[test]
fn a_surviving_registration_is_relinked_keeping_head_index_and_branch() {
    let w = Weave::new();
    let unlanded = w.commit_unlanded();
    let checkout = w.checkout();
    std::fs::write(checkout.join("staged"), "staged\n").unwrap();
    common::git_in(&checkout, &["add", "staged"]);
    std::fs::write(checkout.join("unlanded"), "work\nedited\n").unwrap();
    let before = findings(&w.workweave, false);
    let link = checkout.join(".git");
    std::fs::write(&link, "gitdir: /nowhere/.git/worktrees/alpha\n").unwrap();

    let found = findings(&w.workweave, false);
    assert_eq!(
        found,
        vec![format!("workweave-drift/dangling-link {MEMBER}")]
    );
    let printed = advice(
        &w.workweave,
        &["doctor"],
        "worktree link to a missing git directory",
    );
    assert_names(&printed, &["`rwv doctor --fix`"]);

    let fixed = doctor_fix(&w.workweave);
    assert!(
        fixed.contains("relinked through the registration"),
        "{fixed}"
    );

    assert_eq!(head(&checkout), unlanded);
    assert_eq!(w.branch_tip(), unlanded);
    let after = status(&checkout);
    assert!(after.contains("A  staged"), "the index survives: {after}");
    assert!(after.contains(" M unlanded"), "the edit survives: {after}");
    assert_eq!(findings(&w.workweave, false), before);
}

// ---------------------------------------------------------------------------
// The registration is gone and the workweave's branch survives, ahead of the
// lock: the checkout is attached to the branch where it stands, never reset
// to the revision the lock pins.
// ---------------------------------------------------------------------------

#[test]
fn a_surviving_branch_ahead_of_the_lock_is_attached_at_its_own_tip() {
    let w = Weave::new();
    let unlanded = w.commit_unlanded();
    let before = findings(&w.workweave, false);
    let checkout = w.checkout();
    let registration = common::git_in(&checkout, &["rev-parse", "--absolute-git-dir"]);
    std::fs::remove_dir_all(registration.trim()).unwrap();

    assert_eq!(
        findings(&w.workweave, false),
        vec![format!("workweave-drift/dangling-link {MEMBER}")]
    );
    let fixed = doctor_fix(&w.workweave);
    assert!(
        fixed.contains(&format!("attached to its branch `{BRANCH}` at its own tip")),
        "{fixed}"
    );

    assert_eq!(w.branch_tip(), unlanded, "the branch did not move");
    assert_eq!(head(&checkout), unlanded);
    assert_eq!(status(&checkout), format!("## {BRANCH}\n"));
    assert_eq!(findings(&w.workweave, false), before);
}

// ---------------------------------------------------------------------------
// The checkout directory is removed by hand. Its registration is stale and
// is dropped, and the branch is attached at its own tip — whether it moved
// past the revision rwv created it at or not.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Branch {
    Moved,
    Unmoved,
}

/// The workweave with its member checkout removed, the branch tip, and what
/// doctor reported before the removal.
fn removed_checkout(branch: Branch) -> (Weave, String, Vec<String>, Option<String>) {
    let w = Weave::new();
    let unlanded = (branch == Branch::Moved).then(|| w.commit_unlanded());
    let before = findings(&w.workweave, false);
    let tip = w.branch_tip();
    std::fs::remove_dir_all(w.checkout()).unwrap();
    (w, tip, before, unlanded)
}

fn assert_restored(w: &Weave, tip: &str, before: &[String], unlanded: Option<&str>) {
    assert_eq!(w.branch_tip(), tip, "the branch did not move");
    assert_eq!(head(&w.checkout()), tip);
    assert_eq!(status(&w.checkout()), format!("## {BRANCH}\n"));
    if let Some(commit) = unlanded {
        common::git_in(
            w.checkout(),
            &["cat-file", "-e", &format!("{commit}^{{commit}}")],
        );
    }
    assert_eq!(findings(&w.workweave, false), before);
}

/// `rwv workweave log` names the missing checkout and still lists the
/// branch's unique commits; its advice, `rwv fetch` in the workweave,
/// restores the checkout.
fn follow_workweave_log(branch: Branch) {
    let (w, tip, before, unlanded) = removed_checkout(branch);
    let log = text(&rwv(&w.workweave, &["workweave", "demo", "log"]));
    assert_names(
        &log,
        &[
            "workweave checkout missing at",
            "run `rwv fetch` in that weave",
        ],
    );
    match &unlanded {
        Some(commit) => assert!(
            log.contains(&commit[..12]) && log.contains("unlanded"),
            "the branch's unique commit is listed: {log}"
        ),
        None => assert!(log.contains("(no unique commits vs parent)"), "{log}"),
    }

    let fetched = text(&rwv(&w.workweave, &["fetch"]));
    assert!(
        fetched.contains(&format!("attached to its branch `{BRANCH}` at its own tip")),
        "{fetched}"
    );
    doctor_fix(&w.workweave);
    assert_restored(&w, &tip, &before, unlanded.as_deref());
}

#[test]
fn workweave_log_advice_restores_a_removed_checkout_whose_branch_moved() {
    follow_workweave_log(Branch::Moved);
}

#[test]
fn workweave_log_advice_restores_a_removed_checkout_whose_branch_did_not_move() {
    follow_workweave_log(Branch::Unmoved);
}

fn follow_doctor_fix(branch: Branch) {
    let (w, tip, before, unlanded) = removed_checkout(branch);
    let printed = advice(&w.workweave, &["doctor"], "missing worktree");
    assert_names(&printed, &["`rwv doctor --fix`"]);
    doctor_fix(&w.workweave);
    assert_restored(&w, &tip, &before, unlanded.as_deref());
}

#[test]
fn doctor_fix_restores_a_removed_checkout_whose_branch_moved() {
    follow_doctor_fix(Branch::Moved);
}

#[test]
fn doctor_fix_restores_a_removed_checkout_whose_branch_did_not_move() {
    follow_doctor_fix(Branch::Unmoved);
}

// ---------------------------------------------------------------------------
// `rwv each` names the same repair wherever it runs.
// ---------------------------------------------------------------------------

fn each_advice(dir: &Path) -> String {
    let rwv_bin = PathBuf::from(env!("CARGO_BIN_EXE_rwv"));
    let bins = rwv_bin.parent().unwrap().to_path_buf();
    let rest = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bins).chain(std::env::split_paths(&rest))).unwrap();
    let out = common::rwv()
        .args(["each", "--", "git", "status"])
        .current_dir(dir)
        .env("PATH", path)
        .output()
        .unwrap();
    let report = text(&out);
    report
        .lines()
        .find(|l| l.contains(&format!("{MEMBER}: not present on disk")))
        .unwrap_or_else(|| panic!("no not-present line:\n{report}"))
        .to_owned()
}

const EACH_STEPS: &[&str] = &["run `rwv fetch`", "then `rwv doctor --fix`"];

#[test]
fn rwv_each_advice_in_the_workweave_restores_a_removed_checkout() {
    let (w, tip, before, unlanded) = removed_checkout(Branch::Moved);
    assert_names(&each_advice(&w.workweave), EACH_STEPS);
    rwv(&w.workweave, &["fetch"]);
    doctor_fix(&w.workweave);
    assert_restored(&w, &tip, &before, unlanded.as_deref());
}

#[test]
fn rwv_each_advice_in_the_primary_restores_a_deleted_canonical_clone_and_its_workweave() {
    let w = Weave::new();
    std::fs::remove_dir_all(w.canonical()).unwrap();
    assert_names(&each_advice(&w.primary), EACH_STEPS);
    rwv(&w.primary, &["fetch"]);
    doctor_fix(&w.primary);

    assert_eq!(status(&w.checkout()), format!("## {BRANCH}\n"));
    assert_eq!(findings(&w.primary, true), Vec::<String>::new());
    assert_eq!(findings(&w.workweave, false), Vec::<String>::new());
}
