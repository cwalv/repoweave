//! A refusal or finding that names a remedy names one that works: each test
//! drives the condition, asserts the remedy the operator reads, follows it,
//! and shows the verb that stopped now gets past that point.

use std::path::{Path, PathBuf};

use crate::common;

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
                 branch = \"main\"\nrole = \"owned\"\n\n"
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
        common::pin_every_member(&weave.ws);
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

    /// Clone a fresh member's own origin into the weave and append it to the
    /// manifest, committing only `rwv.toml` — `rwv.lock` is left exactly as
    /// `new` wrote it, so the member has no entry there. This is the state
    /// push's "missing from {lock}" arm reads.
    fn add_unlocked_member(&self, name: &str) {
        let src = self.tmp.path().join("src").join(name);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("README.md"), format!("{name}\n")).unwrap();
        git_init_with_commit(&src);
        let bare = self.tmp.path().join(format!("{name}.git"));
        common::git_in(
            self.tmp.path(),
            &[
                "clone",
                "--bare",
                &src.to_string_lossy(),
                &bare.to_string_lossy(),
            ],
        );
        let url = common::file_url(&bare);
        let clone = self.ws.join("github/acme").join(name);
        common::git_in(self.tmp.path(), &["clone", &url, &clone.to_string_lossy()]);
        common::git_in(&clone, &["config", "user.email", "test@test.com"]);
        common::git_in(&clone, &["config", "user.name", "Test"]);

        let manifest_path = self.project().join("rwv.toml");
        let manifest = std::fs::read_to_string(&manifest_path).unwrap();
        std::fs::write(
            &manifest_path,
            format!(
                "{manifest}\n[repositories.\"github/acme/{name}\"]\ntype = \"git\"\n\
                 url = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
            ),
        )
        .unwrap();
        common::git_in(self.project(), &["add", "rwv.toml"]);
        common::git_in(self.project(), &["commit", "-q", "-m", "add member"]);
    }
}

/// A member holding a commit its lock does not pin: push judges it `pin
/// unpinned` and names `rwv lock --commit`, which records the commit. Moving
/// the branch back without detaching it also gets push through, and a
/// detached HEAD at the pin is what push refuses next (`alignment`).
#[test]
fn an_unpinned_commit_names_the_remedy_push_accepts() {
    let weave = Weave::new(&["web"]);
    weave.set_project_head();
    let member = weave.ws.join("github/acme/web");
    let locked = common::git_in(&member, &["rev-parse", "HEAD"]);
    std::fs::write(member.join("ahead.txt"), "ahead\n").unwrap();
    common::git_in(&member, &["add", "-A"]);
    common::git_in(&member, &["commit", "-q", "-m", "ahead of the lock"]);

    let report = weave.refused(&["push"]);
    assert!(
        report.contains("github/acme/web: pin unpinned")
            && report.contains("ahead of the lock")
            && report.contains("rwv lock --commit")
            && report.contains("rwv explain pin"),
        "the push stops on the unpinned commit and names the lock that records it:\n{report}"
    );

    common::git_in(&member, &["checkout", "-q", &locked]);
    let report = weave.refused(&["push"]);
    assert!(
        report.contains("github/acme/web: alignment detached")
            && report.contains("rwv explain alignment"),
        "and push refuses the detached HEAD it leaves, as an alignment state:\n{report}"
    );
    common::git_in(&member, &["checkout", "-q", "main"]);

    common::git_in(&member, &["branch", "kept"]);
    common::git_in(&member, &["reset", "-q", "--keep", &locked]);
    let report = weave.ok(&["push"]);
    assert!(
        !report.contains("pin unpinned") && !report.contains("alignment"),
        "the branch moved back without detaching gets push through:\n{report}"
    );
    assert!(
        common::git_in(&member, &["log", "--oneline", "kept"]).contains("ahead of the lock"),
        "and the commits it moved off are kept"
    );

    std::fs::write(member.join("ahead-again.txt"), "ahead again\n").unwrap();
    common::git_in(&member, &["add", "-A"]);
    common::git_in(&member, &["commit", "-q", "-m", "ahead of the lock again"]);

    let report = weave.refused(&["push"]);
    assert!(
        report.contains("github/acme/web: pin unpinned"),
        "fixture: the push stops on the unpinned commit again:\n{report}"
    );

    weave.ok(&["lock", "--commit"]);
    let new_head = common::git_in(&member, &["rev-parse", "HEAD"]);
    let report = weave.ok(&["push"]);
    assert!(
        !report.contains("pin unpinned"),
        "`rwv lock --commit` recorded the member's new HEAD and push got past the \
         gate:\n{report}"
    );
    let lock = std::fs::read_to_string(weave.project().join("rwv.lock")).unwrap();
    assert!(
        lock.contains(&new_head),
        "the lock now pins the member's new HEAD:\n{lock}"
    );
}

/// A member added to the manifest after `rwv.lock` was last written: push
/// refuses it as `pin none`, `rwv lock` leaves it without a pin, and `rwv
/// member pin` is what gives it one.
#[test]
fn push_names_the_first_pin_verbs_for_a_member_missing_from_the_lock() {
    let weave = Weave::new(&["web"]);
    weave.set_project_head();
    weave.add_unlocked_member("api");

    let report = weave.refused(&["push"]);
    assert!(
        report.contains("github/acme/api: pin none")
            && report.contains("rwv member pin github/acme/api <rev> --commit")
            && report.contains("rwv update --remote --repo github/acme/api --commit"),
        "the refusal names the verbs that give the member its first pin:\n{report}"
    );

    weave.ok(&["lock", "--commit"]);
    let lock = std::fs::read_to_string(weave.project().join("rwv.lock")).unwrap();
    assert!(
        !lock.contains("github/acme/api"),
        "`rwv lock` gives no first pin:\n{lock}"
    );

    weave.ok(&["member", "pin", "github/acme/api", "HEAD", "--commit"]);
    let report = weave.ok(&["push"]);
    assert!(
        !report.contains("pin none"),
        "`rwv member pin` gave the member its pin and push got past the gate:\n{report}"
    );
    let lock = std::fs::read_to_string(weave.project().join("rwv.lock")).unwrap();
    assert!(
        lock.contains("github/acme/api"),
        "the lock now carries the previously-missing member:\n{lock}"
    );
}

/// `rwv doctor --locked` over a missing checkout names `rwv update`, the verb
/// that re-materializes it.
#[test]
fn doctor_locked_names_update_for_a_missing_checkout() {
    let weave = Weave::new(&["web"]);
    let member = weave.ws.join("github/acme/web");
    std::fs::remove_dir_all(&member).unwrap();

    let report = weave.refused(&["doctor", "--locked"]);
    assert!(
        report.contains("github/acme/web: missing on disk (lock pins")
            && report.contains("); run `rwv update` to materialize it"),
        "the line names update:\n{report}"
    );
    assert!(
        !report.contains("rwv sync"),
        "sync brings in a branch's state; it is not the repair here:\n{report}"
    );

    weave.ok(&["update"]);
    assert!(
        member.join(".git").exists(),
        "the update re-materialized it"
    );
    weave.ok(&["doctor", "--locked"]);
}

/// `rwv member remove --delete` regenerates the workspace file as it will stand once
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

    let report = weave.ok(&["member", "remove", "github/acme/api", "--delete"]);
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
    weave.ok(&["workweave", "create", "--project", "app", "w"]);
    let seat = weave.tmp.path().join(".workweaves/app--w");
    let workspace_file = seat.join("projects/app/app.code-workspace");
    assert!(
        workspace_file.is_file(),
        "fixture: the workweave carries the workspace file"
    );

    let (ok, report) = rwv(&seat, &["member", "remove", "github/acme/api", "--delete"]);
    assert!(
        ok,
        "`rwv member remove --delete` in the workweave failed:\n{report}"
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

/// A bare remote at `<tmp>/origins/<name>.git` holding one commit, and its
/// `file://` URL.
fn origin_with_commit(tmp: &Path, name: &str) -> String {
    let bare = common::under(tmp, &format!("origins/{name}.git"));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    common::file_url(&bare)
}

fn rwv_ok(cwd: &Path, args: &[&str]) -> String {
    let (ok, report) = rwv(cwd, args);
    assert!(ok, "`rwv {}` failed:\n{report}", args.join(" "));
    report
}

fn rwv_refused(cwd: &Path, args: &[&str]) -> String {
    let (ok, report) = rwv(cwd, args);
    assert!(
        !ok,
        "`rwv {}` should have refused:\n{report}",
        args.join(" ")
    );
    report
}

/// The `rwv …` command a report names in backticks starting with `prefix`,
/// split into the arguments after `rwv`.
fn named_rwv_command(report: &str, prefix: &str) -> Vec<String> {
    let span = report
        .split('`')
        .skip(1)
        .step_by(2)
        .find(|span| span.starts_with(prefix))
        .unwrap_or_else(|| panic!("the report must name `{prefix}…`:\n{report}"));
    span.split_whitespace().skip(1).map(str::to_owned).collect()
}

/// A primary weave holding project `pa`, which lists `local/origins/a`, and
/// project `pb`, which the primary presents until a test activates `pa`.
/// Returns the weave root.
fn weave_with_two_projects(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    let a = origin_with_commit(tmp, "a");
    rwv_ok(&ws, &["init", "pa"]);
    rwv_ok(&ws, &["member", "add", &a]);
    rwv_ok(&ws, &["lock", "--commit"]);
    rwv_ok(&ws, &["init", "pb"]);
    ws
}

fn commit_project(ws: &Path, project: &str) {
    let dir = ws.join("projects").join(project);
    common::git_in(&dir, &["add", "-A"]);
    common::commit_staged(&dir, "settled");
}

/// The full-clone repair names the same update when the canonical clone it
/// would link into is gone, and replaces the clone once that update has run.
#[test]
fn standalone_repair_names_the_update_of_the_workweaves_own_project() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_two_projects(tmp.path());
    let x = origin_with_commit(tmp.path(), "x");
    rwv_ok(&ws, &["member", "add", &x]);
    rwv_ok(&ws, &["lock"]);
    commit_project(&ws, "pb");
    rwv_ok(&ws, &["workweave", "create", "--project", "pb", "w"]);
    let seat = tmp.path().join(".workweaves").join("pb--w");
    let canonical = common::under(&ws, "local/origins/x");
    let slot = common::under(&seat, "local/origins/x");
    std::fs::remove_dir_all(&slot).unwrap();
    common::git_in(
        slot.parent().unwrap(),
        &["clone", "--quiet", &canonical.to_string_lossy(), "x"],
    );
    rwv_ok(&ws, &["activate", "pa"]);
    std::fs::remove_dir_all(&canonical).unwrap();

    let refused = rwv_refused(&seat, &["doctor", "--fix"]);
    assert!(
        refused.contains("there is no canonical clone at"),
        "{refused}"
    );
    let update = named_rwv_command(&refused, "rwv update");
    assert_eq!(update, ["update", "--project", "pb"], "{refused}");

    let update: Vec<&str> = update.iter().map(String::as_str).collect();
    rwv_ok(&ws, &update);
    let (_, fixed) = rwv(&seat, &["doctor", "--fix"]);
    assert!(
        fixed.contains(
            "[fixed] core: replaced the full clone of local/origins/x in workweave pb--w"
        ),
        "{fixed}"
    );
    assert!(slot.join(".git").is_file(), "now a linked worktree");
}
