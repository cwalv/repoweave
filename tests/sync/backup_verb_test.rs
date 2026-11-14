//! `rwv backup list|drop|prune`, and the verbs that make a backup before they
//! discard: each is driven through the shipped binary against a weave built by
//! `rwv init` and `rwv member add` from `file://` origins.
//!
//! What must hold, each pinned below by the test that names it:
//!
//! - a backup is made before any discard, and holds what was discarded;
//! - `member remove --delete` refuses while the clone alone holds commits;
//! - the pin rule counts every project's live locks, remote-tracking branches
//!   included;
//! - `prune` drops only what a branch already reaches, and trusts no remote it
//!   could not fetch;
//! - `drop` and `prune` refuse mid-operation, `list` does not.

use std::path::{Path, PathBuf};

use crate::common;

const ALPHA: &str = "local/origin/alpha";

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

fn refused(args: &[&str], dir: &Path) -> String {
    let out = run(args, dir);
    assert!(
        !out.status.success(),
        "`rwv {}` should have been refused; {}",
        args.join(" "),
        describe(&out)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, vec![token], "stderr was:\n{stderr}");
}

/// A bare repo holding a README, no ecosystem files for an integration to act
/// on, and its `file://` URL.
fn plain_origin(tmp: &Path, name: &str) -> String {
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
    std::fs::write(seed.join("README.md"), format!("{name}\n")).unwrap();
    common::git_in(&seed, &["add", "-A"]);
    common::git_in(&seed, &["commit", "-qm", "initial"]);
    common::git_in(&seed, &["push", "-q", "origin", "main"]);
    std::fs::remove_dir_all(&seed).unwrap();
    common::file_url(&bare)
}

struct Weave {
    tmp: tempfile::TempDir,
    ws: PathBuf,
    alpha_url: String,
}

impl Weave {
    /// A weave made by `rwv init demo` holding the owned repo `alpha`, with
    /// the project committed and locked.
    fn new() -> Self {
        let tmp = common::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join("projects")).unwrap();
        succeed(&["init", "demo"], &ws);
        let alpha_url = plain_origin(tmp.path(), "alpha");
        succeed(&["member", "add", &alpha_url], &ws);
        let project = ws.join("projects/demo");
        common::git_in(&project, &["config", "user.email", "test@test.com"]);
        common::git_in(&project, &["config", "user.name", "Test"]);
        succeed(&["lock"], &ws);
        Weave { tmp, ws, alpha_url }
    }

    fn alpha(&self) -> PathBuf {
        common::under(&self.ws, ALPHA)
    }

    fn workweave_dir(&self, name: &str) -> PathBuf {
        self.tmp
            .path()
            .join(".workweaves")
            .join(format!("demo--{name}"))
    }

    /// A workweave `name` holding one commit in `alpha`, returned with the
    /// commit's id.
    fn workweave_with_a_commit(&self, name: &str) -> (PathBuf, String) {
        succeed(
            &[
                "workweave",
                "create",
                "--project",
                "demo",
                name,
                "--no-materialize",
            ],
            &self.ws,
        );
        let dir = self.workweave_dir(name);
        let alpha = common::under(&dir, ALPHA);
        common::git_in(&alpha, &["config", "user.email", "test@test.com"]);
        common::git_in(&alpha, &["config", "user.name", "Test"]);
        std::fs::write(alpha.join("work.txt"), "work\n").unwrap();
        common::git_in(&alpha, &["add", "work.txt"]);
        common::git_in(&alpha, &["commit", "-qm", "unmerged work"]);
        let commit = common::git_in(&alpha, &["rev-parse", "HEAD"]);
        (dir, commit)
    }

    /// Give up `name`, consenting to losing its unmerged commits.
    fn give_up(&self, name: &str) {
        succeed(
            &[
                "workweave",
                "delete",
                "--project",
                "demo",
                name,
                "--discard-unlanded",
            ],
            &self.ws,
        );
    }

    /// `rwv backup list --json`'s backups of `repo`, as `(name, tip)`.
    fn backups_of(&self, repo: &str) -> Vec<(String, String)> {
        let out = succeed(&["backup", "list", "--json", "--project", "demo"], &self.ws);
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        json["repos"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["path"] == repo)
            .flat_map(|r| r["backups"].as_array().unwrap().clone())
            .map(|b| {
                (
                    b["name"].as_str().unwrap().to_owned(),
                    b["tip"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

fn refs_under(repo: &Path, prefix: &str) -> Vec<(String, String)> {
    common::git_in(
        repo,
        &["for-each-ref", "--format=%(refname) %(objectname)", prefix],
    )
    .lines()
    .filter_map(|l| l.split_once(' '))
    .map(|(r, o)| (r.to_owned(), o.to_owned()))
    .collect()
}

/// A backup is made before any discard, and it holds what was discarded: the
/// workweave's branch tip, which nothing else reaches once the branch is gone.
/// `list` shows it with the commit only it holds, and `drop` removes it.
///
/// Reverting `Vcs::write_backup` for git to return the `Backup` without
/// writing its ref leaves the commit reached by nothing and reddens the first
/// assertion after the delete.
#[test]
fn a_discarded_workweave_branch_is_kept_as_a_backup_that_list_shows_and_drop_drops() {
    let weave = Weave::new();
    let (dir, commit) = weave.workweave_with_a_commit("anon");

    weave.give_up("anon");

    assert!(!dir.exists(), "the workweave is gone");
    let kept = weave.backups_of(ALPHA);
    assert_eq!(
        kept.iter().map(|(_, tip)| tip.as_str()).collect::<Vec<_>>(),
        vec![commit.as_str()],
        "the discarded tip is a backup: {kept:?}"
    );
    assert!(kept[0].0.starts_with("demo--anon/"), "{kept:?}");

    let listing =
        String::from_utf8_lossy(&succeed(&["backup", "list"], &weave.ws).stdout).into_owned();
    assert!(listing.contains(ALPHA), "{listing}");
    assert!(listing.contains(&kept[0].0), "{listing}");
    assert!(listing.contains("unmerged work"), "{listing}");

    let dropped =
        String::from_utf8_lossy(&succeed(&["backup", "drop", ALPHA, &kept[0].0], &weave.ws).stdout)
            .into_owned();
    assert!(dropped.contains(&kept[0].0), "{dropped}");
    assert_eq!(weave.backups_of(ALPHA), vec![]);
    assert_eq!(refs_under(&weave.alpha(), "refs/rwv/backup/"), vec![]);
}

/// `--discard-uncommitted` keeps the edits as a commit on the checkout's HEAD:
/// a tracked change and an untracked file, but not an ignored one.
///
/// Dropping the `backup_edits` call from `keep_work_before_discarding` leaves
/// the delete discarding the edits with nothing kept, and reddens this test.
#[test]
fn discard_uncommitted_keeps_the_edits_as_a_backup() {
    let weave = Weave::new();
    succeed(
        &[
            "workweave",
            "create",
            "--project",
            "demo",
            "wip",
            "--no-materialize",
        ],
        &weave.ws,
    );
    let alpha = common::under(weave.workweave_dir("wip"), ALPHA);
    std::fs::write(alpha.join("README.md"), "edited\n").unwrap();
    std::fs::write(alpha.join("scratch.txt"), "untracked\n").unwrap();
    std::fs::write(alpha.join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(alpha.join("ignored.txt"), "noise\n").unwrap();

    let refusal = refused(
        &["workweave", "delete", "--project", "demo", "wip"],
        &weave.ws,
    );
    assert!(refusal.contains("--discard-uncommitted"), "{refusal}");
    assert_eq!(weave.backups_of(ALPHA), vec![], "a refusal keeps nothing");

    succeed(
        &[
            "workweave",
            "delete",
            "--project",
            "demo",
            "wip",
            "--discard-uncommitted",
        ],
        &weave.ws,
    );

    let kept = weave.backups_of(ALPHA);
    assert_eq!(kept.len(), 1, "{kept:?}");
    let tip = &kept[0].1;
    let primary = weave.alpha();
    assert_eq!(
        common::git_in(&primary, &["show", &format!("{tip}:README.md")]),
        "edited"
    );
    assert_eq!(
        common::git_in(&primary, &["show", &format!("{tip}:scratch.txt")]),
        "untracked"
    );
    let files = common::git_in(&primary, &["ls-tree", "-r", "--name-only", tip]);
    assert!(
        !files.lines().any(|f| f == "ignored.txt"),
        "an ignored file is not an edit: {files}"
    );
}

/// Commits a detached HEAD holds that no branch reaches are lost with the
/// worktree, so `--discard-unlanded` keeps them too, under `HEAD/`.
///
/// Dropping the `backup_unpreserved_head` call from
/// `keep_work_before_discarding` leaves them reached by nothing and reddens
/// this test.
#[test]
fn a_detached_head_with_commits_no_branch_reaches_is_kept_as_a_backup() {
    let weave = Weave::new();
    succeed(
        &[
            "workweave",
            "create",
            "--project",
            "demo",
            "peek",
            "--no-materialize",
        ],
        &weave.ws,
    );
    let alpha = common::under(weave.workweave_dir("peek"), ALPHA);
    common::git_in(&alpha, &["config", "user.email", "test@test.com"]);
    common::git_in(&alpha, &["config", "user.name", "Test"]);
    common::git_in(&alpha, &["checkout", "-q", "--detach"]);
    std::fs::write(alpha.join("loose.txt"), "loose\n").unwrap();
    common::git_in(&alpha, &["add", "loose.txt"]);
    common::git_in(&alpha, &["commit", "-qm", "loose work"]);
    let loose = common::git_in(&alpha, &["rev-parse", "HEAD"]);

    let refusal = refused(
        &["workweave", "delete", "--project", "demo", "peek"],
        &weave.ws,
    );
    assert!(refusal.contains("--discard-unlanded"), "{refusal}");
    assert_eq!(weave.backups_of(ALPHA), vec![], "a refusal keeps nothing");

    weave.give_up("peek");

    let kept = weave.backups_of(ALPHA);
    assert_eq!(
        kept.iter().map(|(_, tip)| tip.as_str()).collect::<Vec<_>>(),
        vec![loose.as_str()],
        "{kept:?}"
    );
    assert!(kept[0].0.starts_with("HEAD/"), "{kept:?}");
}

/// dc10-store-destroy: a clone whose only unreached commits are a backup's is
/// not destroyed by `member remove --delete`. The refusal names `rwv backup
/// list`, the backup survives it, and dropping the backup is what lets the
/// delete through.
///
/// Removing the backup scan from `plan_checkout_removal` lets the delete
/// destroy the store and the backup with it, and reddens the refusal.
#[test]
fn member_remove_delete_refuses_while_a_backup_holds_commits_only_the_clone_has() {
    let weave = Weave::new();
    let (_, commit) = weave.workweave_with_a_commit("anon");
    weave.give_up("anon");
    assert_eq!(weave.backups_of(ALPHA).len(), 1);

    let stderr = refused(&["member", "remove", ALPHA, "--delete"], &weave.ws);
    assert!(stderr.contains("rwv backup list"), "{stderr}");
    assert!(stderr.contains(&weave.backups_of(ALPHA)[0].0), "{stderr}");
    assert_routes_to(&stderr, "dropped-repo-has-unique-commits");
    assert!(weave.alpha().exists(), "the clone is still there");
    assert_eq!(
        weave
            .backups_of(ALPHA)
            .into_iter()
            .map(|(_, tip)| tip)
            .collect::<Vec<_>>(),
        vec![commit],
        "the backup survived the refusal"
    );

    succeed(&["backup", "drop", ALPHA, "--all"], &weave.ws);
    succeed(&["member", "remove", ALPHA, "--delete"], &weave.ws);
    assert!(!weave.alpha().exists(), "with the backup dropped, it goes");
}

/// A second project sharing `alpha` whose branch `main` locks `alpha` at
/// `commit`, which only a backup reaches.
fn pin_in_another_project(weave: &Weave, commit: &str) {
    let ws = &weave.ws;
    succeed(&["init", "other"], ws);
    succeed(
        &["member", "add", &weave.alpha_url, "--project", "other"],
        ws,
    );
    let other = ws.join("projects/other");
    common::git_in(&other, &["config", "user.email", "test@test.com"]);
    common::git_in(&other, &["config", "user.name", "Test"]);
    common::fixture_lock(&other, &[(ALPHA, &weave.alpha_url, commit)]);
    common::git_in(&other, &["add", "-A"]);
    common::git_in(&other, &["commit", "-qm", "pin alpha at the backup"]);
}

/// The pin rule counts every project's live locks. The backup is the only
/// ref reaching a commit that project `other`'s `main` locks `alpha` at, so
/// dropping it from project `demo` is refused, naming the pin and the lock;
/// the same holds when only a remote-tracking branch of `other` carries the
/// lock; and once no live lock pins the commit the drop goes through.
///
/// Restricting `live_pins` to the verb's own project lets the first drop
/// through and reddens this test.
#[test]
fn the_pin_rule_counts_every_projects_live_locks() {
    let weave = Weave::new();
    let (_, commit) = weave.workweave_with_a_commit("anon");
    weave.give_up("anon");
    let name = weave.backups_of(ALPHA).remove(0).0;
    pin_in_another_project(&weave, &commit);
    let other = weave.ws.join("projects/other");

    let stderr = refused(
        &["backup", "drop", ALPHA, &name, "--project", "demo"],
        &weave.ws,
    );
    assert!(stderr.contains("pin would be unreachable"), "{stderr}");
    assert!(stderr.contains(&commit[..12]), "{stderr}");
    assert!(stderr.contains("other main"), "the lock is named: {stderr}");
    assert_routes_to(&stderr, "pin");
    assert_eq!(weave.backups_of(ALPHA).len(), 1, "nothing was dropped");

    let lock_commit = common::git_in(&other, &["rev-parse", "HEAD"]);
    common::git_in(
        &other,
        &["update-ref", "refs/remotes/origin/pinned", &lock_commit],
    );
    let main_tip = common::git_in(weave.alpha(), &["rev-parse", "main"]);
    common::fixture_lock(&other, &[(ALPHA, &weave.alpha_url, &main_tip)]);
    common::git_in(&other, &["add", "-A"]);
    common::git_in(&other, &["commit", "-qm", "unpin alpha"]);
    let stderr = refused(
        &["backup", "drop", ALPHA, &name, "--project", "demo"],
        &weave.ws,
    );
    assert!(
        stderr.contains("other origin/pinned") && !stderr.contains("other main"),
        "only a remote-tracking branch's lock is live now: {stderr}"
    );

    common::git_in(&other, &["update-ref", "-d", "refs/remotes/origin/pinned"]);
    succeed(
        &["backup", "drop", ALPHA, &name, "--project", "demo"],
        &weave.ws,
    );
    assert_eq!(weave.backups_of(ALPHA), vec![]);
}

/// A drop of several backups judges the pin rule over the whole set: a commit
/// two backups both reach is not safe to lose with the pair, though either one
/// alone leaves the other reaching it.
#[test]
fn the_pin_rule_judges_the_set_of_backups_a_drop_removes() {
    let weave = Weave::new();
    let (_, commit) = weave.workweave_with_a_commit("anon");
    weave.give_up("anon");
    let alpha = weave.alpha();
    let twin = format!("refs/rwv/backup/twin/{}", &commit[..12]);
    common::git_in(&alpha, &["update-ref", &twin, &commit]);
    let names: Vec<String> = weave
        .backups_of(ALPHA)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    pin_in_another_project(&weave, &commit);

    let stderr = refused(
        &["backup", "drop", ALPHA, "--all", "--project", "demo"],
        &weave.ws,
    );
    assert_routes_to(&stderr, "pin");
    assert_eq!(weave.backups_of(ALPHA).len(), 2, "nothing was dropped");

    succeed(
        &["backup", "drop", ALPHA, &names[0], "--project", "demo"],
        &weave.ws,
    );
    assert_eq!(
        weave.backups_of(ALPHA).len(),
        1,
        "the other still reaches it"
    );
    let stderr = refused(
        &["backup", "drop", ALPHA, &names[1], "--project", "demo"],
        &weave.ws,
    );
    assert_routes_to(&stderr, "pin");
}

/// `prune` drops a backup only when a branch reaches everything it holds, and
/// counts remote-tracking branches only when every remote of the repo was
/// confirmed by the pruning fetch: with one remote unreachable, a backup that
/// only a remote-tracking branch vouches for stays, and goes once the remote
/// is gone.
///
/// Counting remote-tracking branches whatever the fetch confirmed drops the
/// lone backup while `dead` is unreachable, and reddens this test.
#[test]
fn prune_drops_what_a_branch_reaches_and_trusts_no_unconfirmed_remote() {
    let weave = Weave::new();
    let alpha = weave.alpha();
    let main_tip = common::git_in(&alpha, &["rev-parse", "HEAD"]);
    common::git_in(&alpha, &["config", "user.email", "test@test.com"]);
    common::git_in(&alpha, &["config", "user.name", "Test"]);

    let reached = format!("refs/rwv/backup/old/{}", &main_tip[..12]);
    common::git_in(&alpha, &["update-ref", &reached, &main_tip]);

    std::fs::write(alpha.join("lone.txt"), "lone\n").unwrap();
    common::git_in(&alpha, &["add", "lone.txt"]);
    common::git_in(&alpha, &["commit", "-qm", "alone"]);
    let alone = common::git_in(&alpha, &["rev-parse", "HEAD"]);
    common::git_in(&alpha, &["reset", "-q", "--hard", &main_tip]);
    let held = format!("refs/rwv/backup/lone/{}", &alone[..12]);
    common::git_in(&alpha, &["update-ref", &held, &alone]);

    let out = succeed(&["backup", "prune"], &weave.ws);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("old/{}", &main_tip[..12])),
        "{stdout}"
    );
    assert_eq!(
        refs_under(&alpha, "refs/rwv/backup/"),
        vec![(held.clone(), alone.clone())],
        "only the backup a branch reaches goes: {stdout}"
    );

    let vouching = weave.tmp.path().join("vouching.git");
    common::git_in(
        weave.tmp.path(),
        &["init", "-q", "--bare", vouching.to_str().unwrap()],
    );
    common::git_in(
        &alpha,
        &[
            "push",
            "-q",
            vouching.to_str().unwrap(),
            &format!("{alone}:refs/heads/vouch"),
        ],
    );
    common::git_in(
        &alpha,
        &["remote", "add", "vouching", vouching.to_str().unwrap()],
    );
    common::git_in(&alpha, &["fetch", "-q", "vouching"]);
    common::git_in(
        &alpha,
        &["remote", "add", "dead", "/nonexistent/remote.git"],
    );

    let out = succeed(&["backup", "prune"], &weave.ws);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("remote dead unconfirmed"), "{err}");
    assert_eq!(
        refs_under(&alpha, "refs/rwv/backup/"),
        vec![(held.clone(), alone.clone())],
        "an unconfirmed remote means no remote-tracking branch is counted: {err}"
    );

    common::git_in(&alpha, &["remote", "remove", "dead"]);
    succeed(&["backup", "prune"], &weave.ws);
    assert_eq!(
        refs_under(&alpha, "refs/rwv/backup/"),
        vec![],
        "with every remote confirmed, vouching/vouch reaches it"
    );
}

/// `list` runs while an operation is in progress; `drop` and `prune` decline.
#[test]
fn list_runs_mid_operation_and_drop_and_prune_do_not() {
    let weave = Weave::new();
    let (_, _) = weave.workweave_with_a_commit("anon");
    weave.give_up("anon");
    let name = weave.backups_of(ALPHA).remove(0).0;

    std::fs::write(
        weave.ws.join(".rwv-op"),
        format!(
            "{{\"id\": \"20991231T000001Z\", \"verb\": \"sync\", \"history\": null, \
             \"project\": \"demo\", \"source\": \"{root}\", \"target\": \"{root}\", \
             \"retire\": false, \"phase\": \"replay\", \"advanced_tips\": {{}}, \
             \"converged_tips\": {{}}, \
             \"started_at\": \"2026-05-27T10:00:00Z\", \"schema\": 7, \"release\": \"0.0.0\", \"take_in\": null}}",
            root = common::json_escaped(&weave.ws),
        ),
    )
    .unwrap();

    succeed(&["backup", "list"], &weave.ws);
    for args in [
        &["backup", "drop", ALPHA, name.as_str()][..],
        &["backup", "prune"],
    ] {
        let refusal = refused(args, &weave.ws);
        assert_routes_to(&refusal, "op-in-progress");
    }
    assert_eq!(weave.backups_of(ALPHA).len(), 1, "nothing was dropped");
}

/// The project repo is named `projects/<name>`, a name `list` does not show is
/// reported and skipped, and a repo spelled the way the record spells it is
/// no repo.
#[test]
fn drop_takes_a_repo_as_list_names_it() {
    let weave = Weave::new();

    succeed(&["backup", "drop", "projects/demo", "--all"], &weave.ws);

    let out = succeed(&["backup", "drop", ALPHA, "no/such-backup"], &weave.ws);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no backup named no/such-backup"), "{err}");

    let stderr = refused(&["backup", "drop", "(project)", "--all"], &weave.ws);
    assert!(stderr.contains("projects/<name>"), "{stderr}");
    let stderr = refused(
        &["backup", "drop", "projects/elsewhere", "--all"],
        &weave.ws,
    );
    assert!(stderr.contains("project `elsewhere`"), "{stderr}");
    assert!(stderr.contains("projects/demo"), "{stderr}");
    let stderr = refused(&["backup", "drop", "github/not/here", "--all"], &weave.ws);
    assert!(stderr.contains("no repo in this weave"), "{stderr}");
}

/// A pin no branch, remote-tracking branch or backup reaches (a tag holds
/// it here) is not one a drop can leave unreached, so it refuses neither
/// `drop` nor `prune`.
#[test]
fn a_pin_nothing_the_drop_removes_reaches_does_not_refuse_it() {
    let weave = Weave::new();
    let (_, _) = weave.workweave_with_a_commit("anon");
    weave.give_up("anon");
    let name = weave.backups_of(ALPHA).remove(0).0;
    let alpha = weave.alpha();
    let tagged = common::git_in(
        &alpha,
        &["commit-tree", "main^{tree}", "-p", "main", "-m", "tagged"],
    );
    common::git_in(&alpha, &["tag", "only-a-tag", &tagged]);
    pin_in_another_project(&weave, &tagged);

    succeed(&["backup", "prune", "--project", "demo"], &weave.ws);
    succeed(
        &["backup", "drop", ALPHA, &name, "--project", "demo"],
        &weave.ws,
    );
    assert_eq!(weave.backups_of(ALPHA), vec![]);
}

/// `drop` takes names or `--all`: the bare form names neither and drops
/// nothing, `--all` beside a name is refused, a named drop drops only what it
/// names, and `--all` drops the rest.
#[test]
fn drop_needs_names_or_all_and_drops_only_what_it_was_told() {
    let weave = Weave::new();
    let (_, commit) = weave.workweave_with_a_commit("anon");
    weave.give_up("anon");
    let alpha = weave.alpha();
    for twin in ["one", "two"] {
        let name = format!("refs/rwv/backup/{twin}/{}", &commit[..12]);
        common::git_in(&alpha, &["update-ref", &name, &commit]);
    }
    let names: Vec<String> = weave
        .backups_of(ALPHA)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names.len(), 3, "{names:?}");

    let bare = run(&["backup", "drop", ALPHA], &weave.ws);
    assert_eq!(bare.status.code(), Some(2), "{}", describe(&bare));
    let usage = String::from_utf8_lossy(&bare.stderr);
    assert!(usage.contains("--all"), "{usage}");
    assert_eq!(
        weave.backups_of(ALPHA).len(),
        3,
        "the bare form dropped nothing"
    );

    let both = run(&["backup", "drop", ALPHA, &names[0], "--all"], &weave.ws);
    assert_eq!(both.status.code(), Some(2), "{}", describe(&both));
    assert_eq!(weave.backups_of(ALPHA).len(), 3);

    succeed(&["backup", "drop", ALPHA, &names[0]], &weave.ws);
    let left: Vec<String> = weave
        .backups_of(ALPHA)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(left, names[1..].to_vec(), "only the named one went");

    succeed(&["backup", "drop", ALPHA, "--all"], &weave.ws);
    assert_eq!(weave.backups_of(ALPHA), vec![]);
}
