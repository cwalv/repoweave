//! Branch records: `rwv branch set-mode` and `rwv branch list`, and the records
//! branch they write, each driven through the shipped binary against a weave
//! made by `rwv init`.
//!
//! What must hold, each pinned below by the test that names it:
//!
//! - one entry per branch, written without a working tree, each commit naming
//!   the release that wrote it;
//! - a write is a compare-and-swap: one that loses re-reads and applies its
//!   change to what the winner wrote;
//! - a writer commits the merge of the records remotes before its own write,
//!   and a verb that only reads writes nothing;
//! - a clash is stored as a conflict, an entry that does not read is unreadable
//!   for its branch alone, and a remote added only to fetch contributes
//!   nothing;
//! - a target cycle is refused at write, and a name at or below the records
//!   branch is reserved;
//! - the records branch is never listed as a weave branch.

use std::path::{Path, PathBuf};

mod common;

const RECORDS: &str = "rwv-records";

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

fn succeed(args: &[&str], dir: &Path) -> String {
    let out = run(args, dir);
    assert!(
        out.status.success(),
        "`rwv {}` failed; {}",
        args.join(" "),
        describe(&out)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
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

fn assert_routes_name(stderr: &str) {
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, vec!["name"], "stderr was:\n{stderr}");
}

struct Weave {
    tmp: tempfile::TempDir,
    ws: PathBuf,
}

impl Weave {
    /// A weave made by `rwv init demo` whose project repo has one commit on
    /// `main`, recorded as declared.
    fn new() -> Self {
        let tmp = common::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join("projects")).unwrap();
        succeed(&["init", "demo"], &ws);
        let weave = Weave { tmp, ws };
        let project = weave.project();
        common::git_in(&project, &["config", "user.email", "test@test.com"]);
        common::git_in(&project, &["config", "user.name", "Test"]);
        common::git_in(&project, &["add", "-A"]);
        common::git_in(&project, &["commit", "-qm", "init"]);
        succeed(&["branch", "set-mode", "main", "declared"], &weave.ws);
        weave
    }

    fn project(&self) -> PathBuf {
        self.ws.join("projects/demo")
    }

    fn records_tip(&self) -> Option<String> {
        let out = common::git()
            .args([
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{RECORDS}"),
            ])
            .current_dir(self.project())
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8(out.stdout).unwrap().trim().to_owned())
    }

    fn entry(&self, branch: &str) -> String {
        common::git_in(self.project(), &["show", &format!("{RECORDS}:{branch}")])
    }

    /// A branch of the project repo with the same files as `main`.
    fn branch(&self, name: &str) {
        common::git_in(self.project(), &["branch", name, "main"]);
    }

    /// A bare repo the project repo takes `origin` from.
    fn with_origin(&self) -> PathBuf {
        let bare = self.tmp.path().join("origin.git");
        common::git_in(
            self.tmp.path(),
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        let project = self.project();
        common::git_in(
            &project,
            &["remote", "add", "origin", &common::file_url(&bare)],
        );
        common::git_in(&project, &["config", "rwv.url-remote", "origin"]);
        common::git_in(&project, &["push", "-q", "origin", "main"]);
        bare
    }

    /// The remote-tracking records branch of `remote`, moved to a commit
    /// holding `files` over `parent`.
    fn remote_records(&self, remote: &str, parent: Option<&str>, files: &[(&str, &str)]) -> String {
        let commit = common::records::commit(&self.project(), parent, files, &[], "remote records");
        common::git_in(
            self.project(),
            &[
                "update-ref",
                &format!("refs/remotes/{remote}/{RECORDS}"),
                &commit,
            ],
        );
        commit
    }
}

const DECLARED: &str = "mode = \"declared\"\n";

fn feature(target: &str, as_mode: &str) -> String {
    format!("mode = \"feature\"\ntarget = \"{target}\"\nas = \"{as_mode}\"\n")
}

#[test]
fn a_record_is_one_entry_per_branch_written_without_a_working_tree() {
    let weave = Weave::new();
    weave.branch("feat-x");
    succeed(
        &[
            "branch", "set-mode", "feat-x", "feature", "--target", "main",
        ],
        &weave.ws,
    );
    assert_eq!(weave.entry("main"), DECLARED.trim_end());
    assert_eq!(
        weave.entry("feat-x"),
        feature("main", "declared").trim_end()
    );

    let project = weave.project();
    let files = common::git_in(&project, &["ls-tree", "-r", "--name-only", RECORDS]);
    assert_eq!(files, "feat-x\nmain");
    assert_eq!(
        common::git_in(&project, &["status", "--porcelain"]),
        "",
        "no file of the working tree changed"
    );
    assert_eq!(
        common::git_in(&project, &["symbolic-ref", "--short", "HEAD"]),
        "main",
        "the checkout stayed where it was"
    );
    let out = common::git()
        .args(["merge-base", RECORDS, "main"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "the records branch shares no history with a weave branch"
    );
}

#[test]
fn every_records_commit_names_the_release_that_wrote_it() {
    let weave = Weave::new();
    weave.branch("feat-x");
    succeed(
        &[
            "branch", "set-mode", "feat-x", "feature", "--target", "main",
        ],
        &weave.ws,
    );
    let project = weave.project();
    let commits = common::git_in(&project, &["rev-list", RECORDS]);
    assert!(commits.lines().count() >= 2, "{commits}");
    let trailer = format!("Rwv-Writer: {}", repoweave::rwv_version());
    for commit in commits.lines() {
        let message = common::git_in(&project, &["log", "-1", "--format=%B", commit]);
        assert!(
            message.lines().any(|line| line == trailer),
            "{commit} names no release: {message:?}"
        );
    }
}

#[test]
fn a_target_that_would_close_a_cycle_is_refused_and_nothing_is_written() {
    let weave = Weave::new();
    weave.branch("a");
    weave.branch("b");
    succeed(
        &["branch", "set-mode", "a", "feature", "--target", "main"],
        &weave.ws,
    );
    succeed(
        &["branch", "set-mode", "b", "feature", "--target", "a"],
        &weave.ws,
    );
    let before = weave.records_tip();

    let stderr = refused(
        &["branch", "set-mode", "a", "feature", "--target", "b"],
        &weave.ws,
    );
    assert!(stderr.contains("target cycle (a)"), "{stderr}");
    assert!(stderr.contains("a -> b -> a"), "{stderr}");
    assert_routes_to(&stderr, "target");

    let stderr = refused(
        &["branch", "set-mode", "a", "feature", "--target", "a"],
        &weave.ws,
    );
    assert!(
        stderr.contains("a -> a"),
        "a branch is not its own target: {stderr}"
    );
    assert_eq!(
        weave.records_tip(),
        before,
        "a refused write changes nothing"
    );
}

#[test]
fn a_name_at_or_below_the_records_branch_is_reserved() {
    let weave = Weave::new();
    let before = weave.records_tip();
    for name in [RECORDS, "rwv-records/x"] {
        let stderr = refused(&["branch", "set-mode", name, "declared"], &weave.ws);
        assert!(
            stderr.contains(&format!("name reserved ({name})")),
            "{stderr}"
        );
        assert_routes_name(&stderr);
    }
    let stderr = refused(
        &["branch", "set-mode", "main", "feature", "--target", RECORDS],
        &weave.ws,
    );
    assert!(stderr.contains("name reserved"), "{stderr}");
    assert_eq!(weave.records_tip(), before);

    let sibling = succeed(&["branch", "list"], &weave.ws);
    assert!(
        !sibling.contains(RECORDS),
        "the records branch is no weave branch: {sibling}"
    );
}

#[test]
fn a_target_with_no_record_is_refused_naming_the_verb_that_records_it() {
    let weave = Weave::new();
    weave.branch("feat-x");
    weave.branch("loose");
    let stderr = refused(
        &[
            "branch", "set-mode", "feat-x", "feature", "--target", "loose",
        ],
        &weave.ws,
    );
    assert!(stderr.contains("target unrecorded (loose)"), "{stderr}");
    assert!(
        stderr.contains("rwv branch set-mode loose declared"),
        "{stderr}"
    );
    assert!(stderr.contains("rwv fetch"), "{stderr}");
    assert_routes_to(&stderr, "target");
}

#[test]
fn an_entry_that_does_not_read_is_unreadable_for_its_branch_alone() {
    let weave = Weave::new();
    weave.branch("odd");
    let tip = weave.records_tip().unwrap();
    let hand = common::records::commit(
        &weave.project(),
        Some(&tip),
        &[("odd", "mode = \"declared\"\nfrom-a-later-release = 1\n")],
        &[],
        "a later release\n\nRwv-Writer: 9.9.9\n",
    );
    common::git_in(
        weave.project(),
        &["update-ref", &format!("refs/heads/{RECORDS}"), &hand],
    );

    let listing = succeed(&["branch", "list"], &weave.ws);
    assert!(listing.contains("odd"), "{listing}");
    let odd = listing.lines().find(|l| l.contains("odd")).unwrap();
    assert!(odd.contains("unreadable"), "{odd}");
    assert!(
        !odd.contains("unrecorded"),
        "an entry that does not read is not an absent one: {odd}"
    );
    let main = listing.lines().find(|l| l.contains("main")).unwrap();
    assert!(
        main.contains("declared"),
        "other entries read as usual: {main}"
    );

    weave.branch("feat-x");
    let stderr = refused(
        &["branch", "set-mode", "feat-x", "feature", "--target", "odd"],
        &weave.ws,
    );
    assert!(stderr.contains("target unreadable (odd)"), "{stderr}");
    assert!(stderr.contains("written by rwv 9.9.9"), "{stderr}");
    assert!(
        stderr.contains("rwv branch set-mode odd declared"),
        "{stderr}"
    );
    assert_routes_to(&stderr, "target");

    succeed(&["branch", "set-mode", "odd", "declared"], &weave.ws);
    assert_eq!(weave.entry("odd"), DECLARED.trim_end());
}

#[test]
fn an_unreadable_entry_whose_commit_names_no_release_says_so() {
    let weave = Weave::new();
    weave.branch("odd");
    weave.branch("feat-x");
    let tip = weave.records_tip().unwrap();
    let hand = common::records::commit(
        &weave.project(),
        Some(&tip),
        &[("odd", "not toml {")],
        &[],
        "by hand",
    );
    common::git_in(
        weave.project(),
        &["update-ref", &format!("refs/heads/{RECORDS}"), &hand],
    );
    let stderr = refused(
        &["branch", "set-mode", "feat-x", "feature", "--target", "odd"],
        &weave.ws,
    );
    assert!(
        stderr.contains("its commit names no rwv release"),
        "{stderr}"
    );
}

#[test]
fn branch_list_and_log_never_show_the_records_branch() {
    let weave = Weave::new();
    weave.branch("feat-x");
    succeed(
        &[
            "branch", "set-mode", "feat-x", "feature", "--target", "main",
        ],
        &weave.ws,
    );
    let listing = succeed(&["branch", "list"], &weave.ws);
    assert!(listing.contains("feat-x"), "{listing}");
    assert!(!listing.contains(RECORDS), "{listing}");
    let json = succeed(&["branch", "list", "--json"], &weave.ws);
    assert!(!json.contains(RECORDS), "{json}");

    let log = succeed(&["log"], &weave.ws);
    assert!(
        !log.contains("records:"),
        "the records commits are no part of a log: {log}"
    );
    let stderr = refused(&["log", "--against", RECORDS], &weave.ws);
    assert!(stderr.contains("no-such-branch"), "{stderr}");
}

#[test]
fn a_verb_that_only_reads_writes_nothing_to_the_records_branch() {
    let weave = Weave::new();
    weave.with_origin();
    let local = weave.records_tip().unwrap();
    weave.remote_records("origin", Some(&local), &[("elsewhere", DECLARED)]);
    let refs_before = common::git_in(
        weave.project(),
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
        ],
    );

    let listing = succeed(&["branch", "list"], &weave.ws);
    assert!(listing.contains("main"), "{listing}");
    succeed(&["status"], &weave.ws);
    succeed(&["log"], &weave.ws);
    succeed(&["fetch"], &weave.ws);

    let refs_after = common::git_in(
        weave.project(),
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
        ],
    );
    assert_eq!(refs_before, refs_after, "no branch moved");
    assert_eq!(weave.records_tip().unwrap(), local);
}

#[test]
fn a_writer_commits_the_merge_of_the_remotes_records_before_its_own_write() {
    let weave = Weave::new();
    weave.with_origin();
    weave.branch("local-only");
    weave.branch("feat-x");
    let base = weave.records_tip().unwrap();
    succeed(&["branch", "set-mode", "local-only", "declared"], &weave.ws);
    let local = weave.records_tip().unwrap();
    let remote = weave.remote_records("origin", Some(&base), &[("colleague", DECLARED)]);

    succeed(
        &[
            "branch", "set-mode", "feat-x", "feature", "--target", "main",
        ],
        &weave.ws,
    );

    let project = weave.project();
    let tip = weave.records_tip().unwrap();
    let write = common::git_in(&project, &["rev-list", "--parents", "-n1", &tip]);
    let write: Vec<&str> = write.split(' ').collect();
    assert_eq!(
        write.len(),
        2,
        "the write is one commit on the merge: {write:?}"
    );
    let subject = common::git_in(&project, &["log", "-1", "--format=%s", &tip]);
    assert_eq!(subject, "records: set feat-x feature");
    let merge = common::git_in(&project, &["rev-list", "--parents", "-n1", write[1]]);
    let merge: Vec<&str> = merge.split(' ').collect();
    assert_eq!(
        &merge[1..],
        [local.as_str(), remote.as_str()],
        "the merge joins this machine's records and the remote's"
    );
    assert_eq!(
        common::git_in(&project, &["log", "-1", "--format=%s", write[1]]),
        "records: merge records"
    );
    assert_eq!(weave.entry("colleague"), DECLARED.trim_end());
    assert_eq!(weave.entry("local-only"), DECLARED.trim_end());
    assert_eq!(
        weave.entry("feat-x"),
        feature("main", "declared").trim_end()
    );
}

#[test]
fn a_remote_added_only_to_fetch_contributes_nothing_until_it_is_a_push_target() {
    let weave = Weave::new();
    weave.with_origin();
    let project = weave.project();
    common::git_in(
        &project,
        &[
            "remote",
            "add",
            "colleague",
            "file:///nowhere/colleague.git",
        ],
    );
    let base = weave.records_tip().unwrap();
    weave.remote_records(
        "colleague",
        Some(&base),
        &[("main", &feature("elsewhere", "declared"))],
    );

    let listing = succeed(&["branch", "list"], &weave.ws);
    let main = listing.lines().find(|l| l.contains("main")).unwrap();
    assert!(
        main.contains("declared") && !main.contains("conflict"),
        "{main}"
    );
    weave.branch("feat-x");
    succeed(&["branch", "set-mode", "feat-x", "declared"], &weave.ws);
    assert_eq!(
        weave.entry("main"),
        DECLARED.trim_end(),
        "a write merged nothing from a remote that only fetches"
    );

    common::git_in(&project, &["config", "branch.main.pushRemote", "colleague"]);
    let listing = succeed(&["branch", "list"], &weave.ws);
    let main = listing.lines().find(|l| l.contains("main")).unwrap();
    assert!(
        main.contains("declared | feature") || main.contains("feature onto elsewhere"),
        "{main}"
    );
}

#[test]
fn a_remote_becomes_a_records_remote_by_each_of_gits_push_settings() {
    let weave = Weave::new();
    weave.with_origin();
    let project = weave.project();
    common::git_in(
        &project,
        &[
            "remote",
            "add",
            "colleague",
            "file:///nowhere/colleague.git",
        ],
    );
    weave.branch("feat.x");
    let base = weave.records_tip().unwrap();
    weave.remote_records(
        "colleague",
        Some(&base),
        &[("main", &feature("elsewhere", "declared"))],
    );
    let reads_the_remote = || {
        let listing = succeed(&["branch", "list"], &weave.ws);
        let main = listing.lines().find(|l| l.contains("main")).unwrap();
        main.contains("feature onto elsewhere")
    };
    assert!(!reads_the_remote());

    for (key, value) in [
        ("remote.pushDefault", "colleague"),
        ("branch.feat.x.remote", "colleague"),
        ("branch.feat.x.pushRemote", "colleague"),
    ] {
        common::git_in(&project, &["config", key, value]);
        assert!(reads_the_remote(), "{key} makes colleague a records remote");
        common::git_in(&project, &["config", "--unset", key]);
        assert!(!reads_the_remote(), "unsetting {key} ends it");
    }
}

#[test]
fn a_lost_compare_and_swap_rereads_and_applies_the_write_to_what_the_winner_wrote() {
    if !cfg!(feature = "test-pause") {
        common::report_skip(
            "this test holds rwv at a pause point, which needs --features test-pause",
        );
        return;
    }
    let weave = Weave::new();
    weave.branch("first");
    weave.branch("second");
    let mut command = common::rwv_std();
    command
        .args(["branch", "set-mode", "first", "declared"])
        .current_dir(&weave.ws);
    let held =
        common::pause::Paused::spawn(command, "records-write", &weave.tmp.path().join("pause"))
            .unwrap_or_else(|e| panic!("{e}"));

    succeed(&["branch", "set-mode", "second", "declared"], &weave.ws);
    let winner = weave.records_tip().unwrap();

    let out = held.release();
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(weave.entry("first"), DECLARED.trim_end());
    assert_eq!(
        weave.entry("second"),
        DECLARED.trim_end(),
        "the loser applied its change to what the winner wrote"
    );
    let loser = weave.records_tip().unwrap();
    assert_eq!(
        common::git_in(weave.project(), &["rev-parse", &format!("{loser}^")]),
        winner,
        "the loser's write sits on the winner's"
    );
}

#[test]
fn recording_a_feature_reports_where_it_lands() {
    let weave = Weave::new();
    weave.branch("feat-x");
    weave.branch("feat-y");
    let out = succeed(
        &[
            "branch", "set-mode", "feat-x", "feature", "--target", "main",
        ],
        &weave.ws,
    );
    assert!(
        out.contains("target-recorded: feat-x lands into each branch main declares"),
        "{out}"
    );
    let out = succeed(
        &[
            "branch", "set-mode", "feat-y", "feature", "--target", "feat-x",
        ],
        &weave.ws,
    );
    assert!(
        out.contains("target-recorded: feat-y lands into feat-x's member branches"),
        "{out}"
    );
}

#[test]
fn recording_a_feature_lists_the_members_whose_entry_differs_from_the_targets() {
    let weave = Weave::new();
    let project = weave.project();
    common::git_in(&project, &["switch", "-qc", "feat-x"]);
    let manifest = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "{manifest}\n[repositories.\"github/o/lib\"]\ntype = \"git\"\nurl = \"file:///x/lib.git\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    common::git_in(&project, &["commit", "-qam", "a member"]);
    common::git_in(&project, &["switch", "-q", "main"]);
    let out = succeed(
        &[
            "branch", "set-mode", "feat-x", "feature", "--target", "main",
        ],
        &weave.ws,
    );
    assert!(
        out.contains("members whose entry in feat-x differs from main's: github/o/lib"),
        "{out}"
    );
}

#[test]
fn a_change_of_mode_lists_the_features_that_recorded_the_old_one() {
    let weave = Weave::new();
    weave.branch("feat-x");
    weave.branch("base");
    succeed(&["branch", "set-mode", "base", "declared"], &weave.ws);
    succeed(
        &[
            "branch", "set-mode", "feat-x", "feature", "--target", "main",
        ],
        &weave.ws,
    );
    let out = succeed(
        &["branch", "set-mode", "main", "feature", "--target", "base"],
        &weave.ws,
    );
    assert!(
        out.contains("dependent: feat-x records main as declared, and main is now feature"),
        "{out}"
    );
    assert!(
        out.contains("rwv branch set-mode feat-x feature --target main"),
        "{out}"
    );
}

#[test]
fn the_checked_out_branch_is_recorded_when_no_branch_is_named() {
    let weave = Weave::new();
    let project = weave.project();
    weave.branch("declared");
    common::git_in(&project, &["switch", "-q", "-c", "feat-x"]);
    succeed(
        &["branch", "set-mode", "feature", "--target", "main"],
        &weave.ws,
    );
    assert_eq!(
        weave.entry("feat-x"),
        feature("main", "declared").trim_end()
    );

    succeed(&["branch", "set-mode", "declared", "declared"], &weave.ws);
    assert_eq!(
        weave.entry("declared"),
        DECLARED.trim_end(),
        "with two words the first is the branch, even when it is named like a mode"
    );
}

#[test]
fn a_branch_that_exists_nowhere_cannot_be_recorded() {
    let weave = Weave::new();
    let stderr = refused(&["branch", "set-mode", "nope", "declared"], &weave.ws);
    assert!(stderr.contains("name no-such-branch (nope)"), "{stderr}");
    assert_routes_name(&stderr);
}

#[test]
fn a_mode_flag_misuse_is_a_usage_error() {
    let weave = Weave::new();
    let stderr = refused(&["branch", "set-mode", "main", "feature"], &weave.ws);
    assert!(stderr.contains("`feature` needs `--target"), "{stderr}");
    let stderr = refused(
        &["branch", "set-mode", "main", "declared", "--target", "main"],
        &weave.ws,
    );
    assert!(
        stderr.contains("`declared` takes no `--target`"),
        "{stderr}"
    );
    let stderr = refused(&["branch", "set-mode", "main", "sideways"], &weave.ws);
    assert!(stderr.contains("`sideways` is not a mode"), "{stderr}");
}

#[test]
fn branch_list_marks_what_no_remote_holds_and_what_is_only_on_a_remote() {
    let weave = Weave::new();
    weave.with_origin();
    let tip = weave.records_tip().unwrap();
    weave.remote_records("origin", None, &[("main", DECLARED)]);
    weave.branch("feat-x");
    succeed(
        &[
            "branch", "set-mode", "feat-x", "feature", "--target", "main",
        ],
        &weave.ws,
    );
    let _ = tip;
    let json = succeed(&["branch", "list", "--json"], &weave.ws);
    let envelope: serde_json::Value = serde_json::from_str(&json).unwrap();
    let branches = envelope["branches"].as_array().unwrap();
    let by_name = |name: &str| {
        branches
            .iter()
            .find(|b| b["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not listed: {json}"))
    };
    assert_eq!(by_name("main")["record"]["state"], "declared");
    assert_eq!(by_name("main")["published"], true);
    assert_eq!(by_name("main")["checked_out"], true);
    assert_eq!(by_name("feat-x")["record"]["state"], "feature");
    assert_eq!(by_name("feat-x")["record"]["target_branch"], "main");
    assert_eq!(by_name("feat-x")["record"]["as"], "declared");
    assert_eq!(by_name("feat-x")["published"], false);
    let text = succeed(&["branch", "list"], &weave.ws);
    let line = text.lines().find(|l| l.contains("feat-x")).unwrap();
    assert!(line.contains("[unpublished]"), "{line}");
}

#[test]
fn entries_changed_on_both_sides_are_stored_as_a_conflict_that_set_mode_settles() {
    let weave = Weave::new();
    weave.with_origin();
    weave.branch("dev");
    weave.branch("other");
    weave.branch("clash");
    succeed(&["branch", "set-mode", "dev", "declared"], &weave.ws);
    succeed(&["branch", "set-mode", "other", "declared"], &weave.ws);
    succeed(&["branch", "set-mode", "clash", "declared"], &weave.ws);
    let base = weave.records_tip().unwrap();
    succeed(
        &["branch", "set-mode", "clash", "feature", "--target", "dev"],
        &weave.ws,
    );
    weave.remote_records(
        "origin",
        Some(&base),
        &[("clash", &feature("other", "declared"))],
    );

    succeed(&["branch", "set-mode", "dev", "declared"], &weave.ws);
    let stored = weave.entry("clash");
    assert!(stored.starts_with("conflict = ["), "{stored}");
    assert!(stored.contains("target = \"dev\""), "{stored}");
    assert!(stored.contains("target = \"other\""), "{stored}");
    let listing = succeed(&["branch", "list"], &weave.ws);
    let clash = listing.lines().find(|l| l.contains("clash")).unwrap();
    assert!(clash.contains("conflict"), "{clash}");

    succeed(
        &[
            "branch", "set-mode", "clash", "feature", "--target", "other",
        ],
        &weave.ws,
    );
    assert_eq!(
        weave.entry("clash"),
        feature("other", "declared").trim_end()
    );
}

// ---------------------------------------------------------------------------
// The drop rule
// ---------------------------------------------------------------------------

impl Weave {
    fn has_entry(&self, branch: &str) -> bool {
        common::git()
            .args(["cat-file", "-e", &format!("{RECORDS}:{branch}")])
            .current_dir(self.project())
            .output()
            .unwrap()
            .status
            .success()
    }
}

#[test]
fn deleting_a_branch_drops_its_record_so_a_name_below_it_can_be_recorded() {
    let weave = Weave::new();
    weave.branch("a");
    succeed(&["branch", "set-mode", "a", "declared"], &weave.ws);
    assert!(weave.has_entry("a"));

    succeed(&["branch", "delete", "a", "--target", "main"], &weave.ws);
    assert!(
        !weave.has_entry("a"),
        "the deleted branch's entry is dropped"
    );

    // An entry is a file at the branch's name, so a stale `a` would make `a/b`
    // a file inside a file.
    weave.branch("a/b");
    succeed(&["branch", "set-mode", "a/b", "declared"], &weave.ws);
    assert_eq!(weave.entry("a/b"), DECLARED.trim_end());
}

#[test]
fn a_stale_entry_goes_with_the_next_deletion_whatever_branch_it_names() {
    let weave = Weave::new();
    weave.branch("stale");
    weave.branch("gone");
    succeed(&["branch", "set-mode", "stale", "declared"], &weave.ws);
    succeed(&["branch", "set-mode", "gone", "declared"], &weave.ws);
    common::git_in(weave.project(), &["branch", "-D", "stale"]);
    assert!(weave.has_entry("stale"), "a raw delete leaves the entry");

    succeed(&["branch", "delete", "gone", "--target", "main"], &weave.ws);

    assert!(!weave.has_entry("gone"));
    assert!(!weave.has_entry("stale"));
    assert!(
        weave.has_entry("main"),
        "an entry whose branch exists stays"
    );
}

#[test]
fn an_entry_a_merge_brought_in_is_kept_by_that_merge_and_dropped_by_the_next() {
    let weave = Weave::new();
    weave.with_origin();
    weave.branch("first");
    weave.branch("second");
    succeed(&["branch", "set-mode", "first", "declared"], &weave.ws);
    succeed(&["branch", "set-mode", "second", "declared"], &weave.ws);
    let base = weave.records_tip().unwrap();
    weave.remote_records("origin", Some(&base), &[("ghost", DECLARED)]);

    succeed(
        &["branch", "delete", "first", "--target", "main"],
        &weave.ws,
    );
    assert!(
        weave.has_entry("ghost"),
        "a record that reached a remote just before its branch is not lost in the gap"
    );

    succeed(
        &["branch", "delete", "second", "--target", "main"],
        &weave.ws,
    );
    assert!(
        !weave.has_entry("ghost"),
        "once this machine holds the entry and no branch has the name, it goes"
    );
}

#[test]
fn a_drop_merged_from_a_remote_is_undone_for_a_branch_that_exists_here() {
    let weave = Weave::new();
    weave.with_origin();
    weave.branch("here");
    weave.branch("nowhere");
    succeed(&["branch", "set-mode", "here", "declared"], &weave.ws);
    succeed(&["branch", "set-mode", "nowhere", "declared"], &weave.ws);
    common::git_in(weave.project(), &["branch", "-D", "nowhere"]);
    let base = weave.records_tip().unwrap();
    let dropped = common::records::commit(
        &weave.project(),
        Some(&base),
        &[],
        &["here", "nowhere"],
        "another machine's drop",
    );
    common::git_in(
        weave.project(),
        &[
            "update-ref",
            &format!("refs/remotes/origin/{RECORDS}"),
            &dropped,
        ],
    );

    succeed(&["branch", "set-mode", "main", "declared"], &weave.ws);

    assert_eq!(
        weave.entry("here"),
        DECLARED.trim_end(),
        "the entry is kept where its branch exists"
    );
    assert!(
        !weave.has_entry("nowhere"),
        "the drop stands where no branch of the name does"
    );
}

#[test]
fn nothing_drops_while_a_fetch_went_unconfirmed() {
    let weave = Weave::new();
    let bare = weave.with_origin();
    let project = weave.project();
    weave.branch("published");
    weave.branch("stale");
    succeed(&["branch", "set-mode", "published", "declared"], &weave.ws);
    succeed(&["branch", "set-mode", "stale", "declared"], &weave.ws);
    common::git_in(&project, &["push", "-q", "-u", "origin", "published"]);
    common::git_in(&project, &["branch", "-D", "stale"]);
    std::fs::remove_dir_all(&bare).unwrap();

    let out = run(
        &["branch", "delete", "published", "--target", "main"],
        &weave.ws,
    );

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unconfirmed"),
        "the branch is kept, the cause named; {}",
        describe(&out)
    );
    assert!(weave.has_entry("published"));
    assert!(
        weave.has_entry("stale"),
        "no entry drops while a remote could not be asked"
    );
}

#[test]
fn an_entry_for_a_branch_a_workspace_is_aligned_for_is_kept() {
    let weave = Weave::new();
    weave.branch("wip");
    weave.branch("other");
    succeed(&["branch", "set-mode", "wip", "declared"], &weave.ws);
    succeed(&["branch", "set-mode", "other", "declared"], &weave.ws);
    let project = weave.project();
    common::git_in(&project, &["switch", "-q", "wip"]);
    succeed(&["update"], &weave.ws);
    common::git_in(&project, &["switch", "-q", "main"]);
    common::git_in(&project, &["branch", "-D", "wip"]);

    succeed(
        &["branch", "delete", "other", "--target", "main"],
        &weave.ws,
    );

    assert!(
        weave.has_entry("wip"),
        "this workspace last aligned its members for wip, so its record stays"
    );
    assert!(!weave.has_entry("other"));
}
