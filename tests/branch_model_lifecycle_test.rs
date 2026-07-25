//! The branch model's create / delete / remove lifecycle
//! (`docs/repoweave/branch-model.md` §3.5, §4.6(3)(4), §5, §5.1, §6.1, R4).
//!
//! What these tests are for, stated once:
//!
//!   1. **Ownership is by record, never by name shape (R2).** The shipped
//!      code decided "is this branch rwv's?" by parsing the name, so a
//!      hand-made `my--feature/wip` was rwv's property. The tests below fix
//!      the inverse: a branch rwv holds no receipt for survives every verb
//!      that used to glob for it, and the ones rwv *did* record are gone.
//!   2. **Break-the-guard, not happy-path.** Each test's fixture contains
//!      the thing that used to be destroyed, at a name and tip that make the
//!      destruction observable. Restore the prefix glob, or the create-retry
//!      force-delete, and these fail.
//!   3. **Refuse rather than destroy.** Where no warrant is constructible the
//!      verb must stop and say so; "left it alone and reported" is the
//!      assertion, not "succeeded".

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process;

mod common;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn rwv() -> Command {
    common::rwv()
}

/// Run git in `dir`, panicking on failure.
fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

/// Run git in `dir`, returning trimmed stdout.
fn git_out(args: &[&str], dir: &Path) -> String {
    let output = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should be available");
    assert!(
        output.status.success(),
        "git {:?} in {} failed: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Every local branch name in `repo`.
fn branch_names(repo: &Path) -> Vec<String> {
    git_out(
        &[
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/heads/",
        ],
        repo,
    )
    .lines()
    .map(|l| l.trim().to_string())
    .filter(|s| !s.is_empty())
    .collect()
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// A workspace with one project (`web-app`) and one manifest repo.
fn make_workspace(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.yaml"),
        format!(
            "repositories:\n  \
             github/org/repo:\n    \
             type: git\n    \
             url: file://{repo}\n    \
             version: main\n    \
             role: owned\n",
            repo = repo_path.display()
        ),
    )
    .unwrap();
    ws
}

/// Create a branch at HEAD carrying a commit reachable from nowhere else,
/// and return that commit's SHA. This is what makes a destroy *observable*:
/// deleting the branch strands the commit.
fn hand_made_branch_with_unique_commit(repo: &Path, name: &str, file: &str) -> String {
    let on_entry = git_out(&["symbolic-ref", "--short", "HEAD"], repo);
    git(&["checkout", "-b", name], repo);
    std::fs::write(repo.join(file), "operator work").unwrap();
    git(&["add", "-A"], repo);
    git(&["commit", "-m", "work only this branch can reach"], repo);
    let sha = git_out(&["rev-parse", name], repo);
    git(&["checkout", &on_entry], repo);
    sha
}

// ---------------------------------------------------------------------------
// R2 — a ref that merely LOOKS like rwv's is not rwv's
// ---------------------------------------------------------------------------

/// The §2.1 `[S]` scenario, inverted: `workweave delete` destroys exactly the
/// ref it recorded creating, and nothing that merely resembles it.
///
/// The shipped delete globbed the prefix `{project}--{workweave}` and
/// force-deleted everything it returned. A prefix is not a namespace: for
/// workweave `real`, the glob also claims `web-app--really-mine`, an
/// unrelated branch of the operator's. That is the case with teeth here —
/// restore the glob-and-destroy and this test fails on it, with the commit
/// only that branch reached left dangling.
///
/// `my--feature/wip` and `dependabot--npm/lodash` are the two names §2.1
/// records by hand. They sit outside this workweave's prefix, so delete never
/// had a route to them; they are here because the `<a>--<b>/<c>` shape is what
/// the *doctor* side reads as ownership, and this is the fixture that pass
/// will be extended over.
///
/// **What this does NOT yet pin.** `rwv doctor --fix` runs in the middle of
/// this flow and leaves all three standing — but for its own reason, not this
/// bead's: `check.rs` still decides ownership by parsing the name, and spares
/// these only because each carries a commit that is not an ancestor of
/// primary, which makes them live-class. §4.6(4) says that parser can be
/// deleted outright once deletion goes through the registry; doing so is the
/// check.rs cutover (fo-opmmoz.9). Until then a *safe-class* lookalike — same
/// shape, no unique commit — is still deleted by `doctor --fix`, and adding
/// that case here belongs with the change that fixes it.
#[test]
fn delete_destroys_only_the_ref_it_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    let repo = ws.join("github/org/repo");

    // Inside the shipped prefix glob for workweave `real`, and not rwv's.
    let sibling_sha =
        hand_made_branch_with_unique_commit(&repo, "web-app--really-mine", "sibling.txt");
    // The two names §2.1 records.
    let feature_sha = hand_made_branch_with_unique_commit(&repo, "my--feature/wip", "wip.txt");
    let bot_sha = hand_made_branch_with_unique_commit(&repo, "dependabot--npm/lodash", "bump.txt");

    // A real workweave, so there IS a recorded ref for the verbs to find and
    // destroy — otherwise "nothing was deleted" would prove nothing.
    rwv()
        .args(["workweave", "web-app", "create", "real"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    assert!(
        branch_names(&repo).iter().any(|b| b == "web-app--real"),
        "precondition: the create must have recorded and written its own ref"
    );

    let _ = rwv()
        .args(["doctor", "--fix"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert();

    rwv()
        .args(["workweave", "web-app", "delete", "real"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let after = branch_names(&repo);

    // The recorded ref is gone — the verb did run and did destroy.
    assert!(
        !after.iter().any(|b| b == "web-app--real"),
        "the RECORDED ref should have been destroyed; branches: {after:?}"
    );

    // Everything else is untouched, tips included.
    for (name, sha) in [
        ("web-app--really-mine", &sibling_sha),
        ("my--feature/wip", &feature_sha),
        ("dependabot--npm/lodash", &bot_sha),
    ] {
        assert!(
            after.iter().any(|b| b == name),
            "branch {name} is not rwv's and must survive; branches: {after:?}"
        );
        assert_eq!(
            &git_out(&["rev-parse", name], &repo),
            sha,
            "{name} must still point at the operator's commit"
        );
    }
}

/// The report side of the same rule: delete says what it left behind, so
/// "not rwv's" is visible rather than merely silent.
///
/// The leftover lives in a repo the workweave never materialized — added to
/// the manifest after the create, which is how a store ends up inside a
/// workweave's scope with no ref of rwv's in it. That is not fixture
/// convenience: git cannot hold `refs/heads/p--ww` and `refs/heads/p--ww/x`
/// at the same time, so in a store where rwv's flat ref stands, a lookalike
/// under it cannot exist to be reported.
#[test]
fn delete_reports_the_branches_it_will_not_touch() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "reported"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // A second repo, declared after the create, carrying a branch inside the
    // workweave's namespace that rwv did not write. This is the one the
    // prefix glob would have taken.
    let repo2 = ws.join("github/org/other");
    init_repo_with_commit(&repo2);
    let manifest = ws.join("projects/web-app/rwv.yaml");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text.push_str(&format!(
        "  github/org/other:\n    \
         type: git\n    \
         url: file://{repo}\n    \
         version: main\n    \
         role: owned\n",
        repo = repo2.display()
    ));
    std::fs::write(&manifest, text).unwrap();
    let leftover_sha =
        hand_made_branch_with_unique_commit(&repo2, "web-app--reported/mine", "mine.txt");

    let assert = rwv()
        .args(["workweave", "web-app", "delete", "reported"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();

    assert!(
        stderr.contains("web-app--reported/mine") && stderr.contains("not recorded as rwv's"),
        "delete must report the leftover it is not allowed to touch; got:\n{stderr}"
    );
    assert!(
        branch_names(&repo2)
            .iter()
            .any(|b| b == "web-app--reported/mine"),
        "the reported leftover must still exist"
    );
    assert_eq!(
        git_out(&["rev-parse", "web-app--reported/mine"], &repo2),
        leftover_sha,
        "the reported leftover must still point at the operator's commit"
    );
}

// ---------------------------------------------------------------------------
// §3.5 — name uniqueness is checked against the index, not the directory
// ---------------------------------------------------------------------------

/// `--dir` walks past the directory-existence check, and the index insert is
/// last-writer-wins, so two workweaves of one project could take the same
/// name — and under flat ephemeral names they would then mint the *same*
/// branch in the same store.
/// The **flat** leftover: the workweave's own ephemeral name standing in a
/// store with no receipt behind it — the §7.1 arm 2 population, before the
/// migration adopts it.
///
/// `is_this_workweaves_namespace` has to claim the flat name itself, not just
/// what sits under it. Drop the `==` half and this branch stops being
/// reported at all: delete says nothing, and the operator is left with a ref
/// no rwv verb will ever mention again. Nothing else in the suite reaches
/// that half — every other leftover fixture is spelled `{flat}/<segment>`,
/// which the prefix half claims on its own.
#[test]
fn delete_reports_a_flat_leftover_it_holds_no_receipt_for() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "unowned"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Same shape as `delete_reports_the_branches_it_will_not_touch`: a second
    // repo declared after the create, so the store is in the workweave's
    // scope with no ref of rwv's in it. Here the leftover carries the flat
    // name the workweave itself mints.
    let repo2 = ws.join("github/org/other");
    init_repo_with_commit(&repo2);
    let manifest = ws.join("projects/web-app/rwv.yaml");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text.push_str(&format!(
        "  github/org/other:\n    \
         type: git\n    \
         url: file://{repo}\n    \
         version: main\n    \
         role: owned\n",
        repo = repo2.display()
    ));
    std::fs::write(&manifest, text).unwrap();
    let leftover_sha = hand_made_branch_with_unique_commit(&repo2, "web-app--unowned", "mine.txt");

    let assert = rwv()
        .args(["workweave", "web-app", "delete", "unowned"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();

    assert!(
        stderr.contains("web-app--unowned") && stderr.contains("not recorded as rwv's"),
        "delete must report the flat leftover it is not allowed to touch; got:\n{stderr}"
    );
    assert!(
        branch_names(&repo2).iter().any(|b| b == "web-app--unowned"),
        "the flat leftover must still exist"
    );
    assert_eq!(
        git_out(&["rev-parse", "web-app--unowned"], &repo2),
        leftover_sha,
        "and it must still point at the operator's commit"
    );
}

#[test]
fn create_refuses_a_name_the_index_already_records() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "dup"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let elsewhere = tmp.path().join("elsewhere");
    let assert = rwv()
        .args([
            "workweave",
            "web-app",
            "create",
            "dup",
            "--dir",
            elsewhere.to_str().unwrap(),
        ])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("already records a workweave named `dup`"),
        "the refusal must name the recorded duplicate; got:\n{stderr}"
    );
    assert!(
        stderr.contains("web-app--dup"),
        "the refusal must name the branch both would mint; got:\n{stderr}"
    );
    assert!(
        !elsewhere.exists(),
        "the duplicate must not be materialized at {}",
        elsewhere.display()
    );

    // The first workweave is untouched.
    assert!(weaveroot.join("web-app--dup").exists());
}

// ---------------------------------------------------------------------------
// R4 — a DESTROY-STORE requires the store to be unclaimed
// ---------------------------------------------------------------------------

/// `rwv remove --delete` used to `remove_dir_all` the canonical store while a
/// workweave's live worktree was still linked into it: the workweave's `.git`
/// files point *inside* the directory being removed, so the whole workweave
/// is gutted. R4 refuses while any worktree is registered or any receipt
/// stands, and the same command succeeds once the workweave is deleted (which
/// removes the worktree and retracts the receipt) — the delete-then-prune
/// ordering §5's `prune_dropped_repo` row describes.
#[test]
fn remove_delete_refuses_a_claimed_store_and_succeeds_after_the_claim_is_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    let repo = ws.join("github/org/repo");

    rwv()
        .args(["workweave", "web-app", "create", "live"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    let ww_checkout = weaveroot.join("web-app--live/github/org/repo");
    assert!(ww_checkout.exists(), "precondition: workweave checkout");

    let assert = rwv()
        .args([
            "remove",
            "github/org/repo",
            "--delete",
            "--project",
            "web-app",
        ])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("the store is still claimed"),
        "R4 refusal expected; got:\n{stderr}"
    );
    assert!(
        stderr.contains("live worktree registered"),
        "the refusal must name the live worktree claim; got:\n{stderr}"
    );
    assert!(
        stderr.contains("ownership receipt for branch web-app--live"),
        "the refusal must name the standing receipt; got:\n{stderr}"
    );
    assert!(
        repo.exists() && ww_checkout.exists(),
        "nothing may be destroyed by a refused DESTROY-STORE"
    );

    // Retire the claims in the order R4 requires, then the store may go.
    rwv()
        .args(["workweave", "web-app", "delete", "live"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args([
            "remove",
            "github/org/repo",
            "--delete",
            "--project",
            "web-app",
        ])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    assert!(
        !repo.exists(),
        "an unclaimed store is removable: {} should be gone",
        repo.display()
    );
}

// ---------------------------------------------------------------------------
// §6.1 — the Claude WorktreeRemove hook can construct no warrant
// ---------------------------------------------------------------------------

/// The hook used to pass both waivers unconditionally, which made it the one
/// path where a dirty *and* diverged workweave was destroyed with no operator
/// confirmation. It is fire-and-forget (always exits 0), so the assertion is
/// that the workweave and its work SURVIVE.
#[test]
fn claude_worktree_remove_hook_does_not_destroy_uncommitted_work() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "hooked"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--hooked");
    let ww_checkout = ww_dir.join("github/org/repo");
    std::fs::write(ww_checkout.join("README"), "edited, never committed").unwrap();

    let payload = serde_json::json!({
        "hook_event_name": "WorktreeRemove",
        "worktree_path": ww_dir.to_str().unwrap(),
    })
    .to_string();

    rwv()
        .args(["workweave", "--claude-hook"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .write_stdin(payload)
        .assert()
        .success(); // fire-and-forget: always exits 0

    assert!(
        ww_dir.exists(),
        "the hook must not destroy a workweave holding uncommitted work"
    );
    assert_eq!(
        std::fs::read_to_string(ww_checkout.join("README")).unwrap(),
        "edited, never committed",
        "the operator's edit must survive"
    );
}
