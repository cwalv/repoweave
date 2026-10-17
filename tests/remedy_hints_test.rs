//! A refusal or finding that names a remedy names one that works: each test
//! drives the condition, asserts the remedy the operator reads, follows it,
//! and shows the verb that stopped now gets past that point.

use std::path::{Path, PathBuf};

mod common;

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
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

/// A weave whose project `app` lists members cloned from bare remotes, with
/// the project repo itself published to an empty bare remote by `git push -u
/// origin main` — which leaves the clone's `origin/HEAD` unset.
struct Weave {
    tmp: tempfile::TempDir,
    ws: PathBuf,
}

impl Weave {
    fn new(members: &[&str]) -> Self {
        let tmp = common::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join("projects")).unwrap();
        let mut manifest = String::new();
        for name in members {
            let src = tmp.path().join("src").join(name);
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(src.join("README.md"), format!("{name}\n")).unwrap();
            git_init_with_commit(&src);
            let bare = tmp.path().join(format!("{name}.git"));
            common::git_in(
                tmp.path(),
                &[
                    "clone",
                    "--bare",
                    &src.to_string_lossy(),
                    &bare.to_string_lossy(),
                ],
            );
            let url = common::file_url(&bare);
            let clone = ws.join("github/acme").join(name);
            common::git_in(tmp.path(), &["clone", &url, &clone.to_string_lossy()]);
            common::git_in(&clone, &["config", "user.email", "test@test.com"]);
            common::git_in(&clone, &["config", "user.name", "Test"]);
            manifest.push_str(&format!(
                "[repositories.\"github/acme/{name}\"]\ntype = \"git\"\nurl = \"{url}\"\n\
                 version = \"main\"\nrole = \"owned\"\n\n"
            ));
        }
        let (ok, report) = rwv(&ws, &["init", "app"]);
        assert!(ok, "fixture: init failed:\n{report}");
        let project = ws.join("projects/app");
        common::git_in(&project, &["config", "user.email", "test@test.com"]);
        common::git_in(&project, &["config", "user.name", "Test"]);
        std::fs::write(project.join("rwv.toml"), manifest).unwrap();
        let weave = Weave { tmp, ws };
        weave.ok(&["doctor", "--fix"]);
        weave.ok(&["lock"]);
        common::git_in(&project, &["add", "-A"]);
        common::git_in(&project, &["commit", "-q", "-m", "members"]);
        let bare = weave.tmp.path().join("app.git");
        common::git_in(
            weave.tmp.path(),
            &["init", "-q", "--bare", &bare.to_string_lossy()],
        );
        common::git_in(
            &project,
            &["remote", "add", "origin", &common::file_url(&bare)],
        );
        common::git_in(&project, &["push", "-q", "-u", "origin", "main"]);
        weave
    }

    fn project(&self) -> PathBuf {
        self.ws.join("projects/app")
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, report) = rwv(&self.ws, args);
        assert!(ok, "`rwv {}` failed:\n{report}", args.join(" "));
        report
    }

    fn refused(&self, args: &[&str]) -> String {
        let (ok, report) = rwv(&self.ws, args);
        assert!(
            !ok,
            "`rwv {}` should have refused:\n{report}",
            args.join(" ")
        );
        report
    }

    fn set_project_head(&self) {
        common::git_in(self.project(), &["remote", "set-head", "origin", "-a"]);
    }
}

/// `git push -u origin main` publishes the branch and leaves `origin/HEAD`
/// unset, so the hint names only `git remote set-head`, which is what works.
#[test]
fn no_remote_default_branch_names_only_the_remedy_that_works() {
    let weave = Weave::new(&["web"]);
    let report = weave.refused(&["push"]);
    assert!(
        report.contains("rwv explain no-remote-default-branch"),
        "fixture: the push stops on the unset origin/HEAD:\n{report}"
    );
    assert!(
        report.contains("git fetch origin && git remote set-head origin -a"),
        "the hint names set-head:\n{report}"
    );
    assert!(
        !report.contains("push once"),
        "a push with an explicit branch is what left origin/HEAD unset; the hint \
         must not offer it:\n{report}"
    );

    common::git_in(weave.project(), &["fetch", "-q", "origin"]);
    weave.set_project_head();
    let (_, report) = rwv(&weave.ws, &["push"]);
    assert!(
        !report.contains("no-remote-default-branch"),
        "following the hint gets push past the gate:\n{report}"
    );
}

/// A member whose HEAD is not its lock commit: the hint's two remedies are
/// recording the local state, and moving the branch back without detaching
/// it — a detached HEAD at the lock commit is what push refuses next.
#[test]
fn lock_state_mismatch_names_remedies_that_push_accepts() {
    let weave = Weave::new(&["web"]);
    weave.set_project_head();
    let member = weave.ws.join("github/acme/web");
    let locked = common::git_in(&member, &["rev-parse", "HEAD"]);
    std::fs::write(member.join("ahead.txt"), "ahead\n").unwrap();
    common::git_in(&member, &["add", "-A"]);
    common::git_in(&member, &["commit", "-q", "-m", "ahead of the lock"]);

    let report = weave.refused(&["push"]);
    assert!(
        report.contains("rwv explain lock-state-mismatch"),
        "fixture: the push stops on the lock mismatch:\n{report}"
    );
    assert!(
        report.contains(
            "Hint: run `rwv lock --commit` to record each listed repo's HEAD in the lock, or \
             move the repo's branch to its lock commit without detaching it: `git reset \
             --keep <lock commit>` in the repo (after `git branch <name>` to keep the commits \
             it moves off)."
        ),
        "the hint names remedies that work:\n{report}"
    );
    assert!(
        !report.contains("`git checkout` in each repo"),
        "a checkout of the lock commit detaches HEAD, which push refuses:\n{report}"
    );

    common::git_in(&member, &["checkout", "-q", &locked]);
    let report = weave.refused(&["push"]);
    assert!(
        !report.contains("lock-state-mismatch"),
        "fixture: the checkout agrees with the lock:\n{report}"
    );
    assert!(
        report.contains("github/acme/web: detached HEAD")
            && report.contains("rwv explain unpushable-repo-branch"),
        "and push refuses the detached HEAD it leaves:\n{report}"
    );
    common::git_in(&member, &["checkout", "-q", "main"]);

    common::git_in(&member, &["branch", "kept"]);
    common::git_in(&member, &["reset", "-q", "--keep", &locked]);
    let report = weave.ok(&["push"]);
    assert!(
        !report.contains("lock-state-mismatch") && !report.contains("unpushable-repo-branch"),
        "the branch moved back without detaching gets push through:\n{report}"
    );
    assert!(
        common::git_in(&member, &["log", "--oneline", "kept"]).contains("ahead of the lock"),
        "and the commits it moved off are kept"
    );
}

/// `rwv doctor --locked` over a missing checkout names in-place `rwv fetch`,
/// the verb that re-materializes it.
#[test]
fn doctor_locked_names_fetch_for_a_missing_checkout() {
    let weave = Weave::new(&["web"]);
    let member = weave.ws.join("github/acme/web");
    std::fs::remove_dir_all(&member).unwrap();

    let report = weave.refused(&["doctor", "--locked"]);
    assert!(
        report.contains("github/acme/web: missing on disk (lock pins")
            && report.contains("); run `rwv fetch` with no source to materialize it"),
        "the line names in-place fetch:\n{report}"
    );
    assert!(
        !report.contains("rwv sync"),
        "sync brings in another workspace's state; it is not the repair here:\n{report}"
    );

    weave.ok(&["fetch"]);
    assert!(member.join(".git").exists(), "the fetch re-materialized it");
    weave.ok(&["doctor", "--locked"]);
}

/// `rwv remove --delete` regenerates the workspace file as it will stand once
/// the checkout is gone, so no exclude names the deleted path and doctor has
/// no drift to report.
#[test]
fn remove_delete_leaves_no_stale_exclude_and_no_vscode_drift() {
    let weave = Weave::new(&["web", "api"]);
    let workspace_file = weave.project().join("app.code-workspace");
    assert!(
        workspace_file.is_file(),
        "fixture: the vscode integration wrote the workspace file"
    );

    let report = weave.ok(&["remove", "github/acme/api", "--delete"]);
    assert!(
        !weave.ws.join("github/acme/api").exists(),
        "fixture: the checkout is deleted:\n{report}"
    );
    let content = std::fs::read_to_string(&workspace_file).unwrap();
    assert!(
        !content.contains("github/acme/api"),
        "no entry names the deleted path:\n{content}"
    );

    let out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(&weave.ws)
        .output()
        .expect("rwv should run");
    let doctor: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("doctor --json prints a report");
    let issues = doctor["issues"]
        .as_array()
        .expect("the report carries an issues array");
    assert!(
        issues
            .iter()
            .all(|i| i["integration"] != "vscode-workspace"),
        "doctor reports no vscode-workspace finding after the delete:\n{doctor:#}"
    );
}

/// The same inside a workweave, whose workspace file writes through to the
/// project repo: the worktree goes and no exclude names it.
#[test]
fn remove_delete_in_a_workweave_leaves_no_stale_exclude() {
    let weave = Weave::new(&["web", "api"]);
    weave.ok(&["workweave", "app", "create", "w"]);
    let seat = weave.tmp.path().join(".workweaves/app--w");
    let workspace_file = seat.join("projects/app/app.code-workspace");
    assert!(
        workspace_file.is_file(),
        "fixture: the workweave carries the workspace file"
    );

    let (ok, report) = rwv(&seat, &["remove", "github/acme/api", "--delete"]);
    assert!(
        ok,
        "`rwv remove --delete` in the workweave failed:\n{report}"
    );
    assert!(
        !seat.join("github/acme/api").exists(),
        "fixture: the worktree is deleted:\n{report}"
    );
    let content = std::fs::read_to_string(&workspace_file).unwrap();
    assert!(
        !content.contains("github/acme/api"),
        "no entry names the deleted path:\n{content}"
    );
}
