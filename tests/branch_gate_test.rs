//! The gate on an unrecorded branch, and the verbs that write a record as part
//! of what they mean: `init`, `clone`, `init --adopt` and `workweave create`.
//! Each is driven through the shipped binary against a weave built by `rwv
//! init`, with a bare repo as its origin where a remote is involved.
//!
//! What must hold, each pinned below by the test that names it:
//!
//! - the verbs that align, lock, land or publish refuse a checked-out branch
//!   with no record, naming `rwv fetch` and `rwv branch set-mode`, and change
//!   nothing; the verbs that only look, or that record, run;
//! - a branch whose record is conflicted, unreadable or on a cycle is refused
//!   the same way, by its own state;
//! - `init`, `clone` and `init --adopt` record the branch they leave checked
//!   out as `declared`, after reading the records the remote holds;
//! - a bare `workweave create` records `{target: the parent's branch, as: the
//!   parent's mode}` and says where it lands, and refuses before it makes
//!   anything when the parent's branch has no record;
//! - `status` shows the checked-out branch's record without writing one.

use std::path::PathBuf;

mod common;

use common::cli::{describe, refused, run, succeed};
use repoweave::diagnosis::State;

const RECORDS: &str = common::records::BRANCH;
const DECLARED: &str = "mode = \"declared\"\n";

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

struct Weave {
    tmp: tempfile::TempDir,
    ws: PathBuf,
}

impl Weave {
    /// A weave made by `rwv init demo`, whose project repo has a bare origin
    /// holding `main` and one commit.
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
        let bare = weave.origin();
        common::git_in(
            weave.tmp.path(),
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        common::git_in(
            &project,
            &["remote", "add", "origin", &common::file_url(&bare)],
        );
        common::git_in(&project, &["config", "rwv.url-remote", "origin"]);
        common::git_in(&project, &["push", "-q", "origin", "main"]);
        weave
    }

    fn origin(&self) -> PathBuf {
        self.tmp.path().join("origin.git")
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
        common::git_in(
            self.project(),
            &[
                "show",
                &format!("{RECORDS}:{}", common::records::path(branch)),
            ],
        )
    }

    fn state(&self) -> String {
        let project = self.project();
        format!(
            "{}\n{}\n{}",
            common::git_in(&project, &["rev-parse", "HEAD"]),
            common::git_in(
                &project,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads"
                ]
            ),
            common::git_in(&project, &["status", "--porcelain"]),
        )
    }

    fn raw_branch(&self, name: &str) {
        common::git_in(self.project(), &["switch", "-qc", name]);
    }
}

const GATED: &[&[&str]] = &[
    &["lock"],
    &["update"],
    &["update", "--remote"],
    &["pull"],
    &["sync", "primary"],
    &["sync-to", "primary"],
    &["push"],
];

#[test]
fn an_unrecorded_branch_refuses_the_verbs_that_align_lock_land_or_publish() {
    let weave = Weave::new();
    weave.raw_branch("raw");
    let before = weave.state();
    for args in GATED {
        let stderr = refused(&run(args, &weave.ws), State::TargetUnrecorded);
        assert!(
            stderr.contains("target unrecorded (raw)"),
            "`rwv {}`: {stderr}",
            args.join(" ")
        );
        assert!(stderr.contains("rwv fetch"), "{stderr}");
        assert!(
            stderr.contains("rwv branch set-mode raw feature --target <T>"),
            "{stderr}"
        );
        assert!(
            stderr.contains("rwv branch set-mode raw declared"),
            "{stderr}"
        );
        assert!(
            stderr.contains("--target candidates, nearest first: main"),
            "{stderr}"
        );
    }
    assert_eq!(weave.state(), before, "a refusal changed nothing");
}

#[test]
fn pull_refuses_an_unrecorded_branch_before_it_takes_the_upstream_in() {
    let weave = Weave::new();
    weave.raw_branch("raw");
    let project = weave.project();
    common::git_in(&project, &["push", "-q", "-u", "origin", "raw"]);
    let other = weave.tmp.path().join("other");
    common::git_in(
        weave.tmp.path(),
        &[
            "clone",
            "-q",
            &common::file_url(weave.origin()),
            other.to_str().unwrap(),
        ],
    );
    common::git_in(&other, &["switch", "-q", "raw"]);
    std::fs::write(other.join("ahead.txt"), "ahead\n").unwrap();
    common::git_in(&other, &["add", "ahead.txt"]);
    common::git_in(&other, &["commit", "-qm", "ahead"]);
    common::git_in(&other, &["push", "-q", "origin", "raw"]);

    let before = weave.state();
    let stderr = refused(&run(&["pull"], &weave.ws), State::TargetUnrecorded);
    assert!(stderr.contains("target unrecorded (raw)"), "{stderr}");
    assert_eq!(
        weave.state(),
        before,
        "the project branch was not fast-forwarded before the refusal"
    );
}

#[test]
fn the_verbs_that_only_look_or_record_run_on_an_unrecorded_branch() {
    let weave = Weave::new();
    weave.raw_branch("raw");
    for args in [
        &["status"][..],
        &["log"],
        &["fetch"],
        &["backup", "list"],
        &["branch", "list"],
    ] {
        succeed(args, &weave.ws);
    }
    succeed(&["branch", "set-mode", "raw", "declared"], &weave.ws);
    succeed(&["lock"], &weave.ws);
}

#[test]
fn recording_the_branch_lets_the_gated_verbs_run() {
    let weave = Weave::new();
    weave.raw_branch("raw");
    refused(&run(&["lock"], &weave.ws), State::TargetUnrecorded);
    succeed(
        &["branch", "set-mode", "raw", "feature", "--target", "main"],
        &weave.ws,
    );
    succeed(&["lock"], &weave.ws);
}

#[test]
fn a_branch_whose_record_is_unreadable_conflicted_or_on_a_cycle_is_refused_by_its_state() {
    let weave = Weave::new();
    let project = weave.project();
    common::git_in(&project, &["branch", "a"]);
    common::git_in(&project, &["branch", "b"]);
    succeed(&["branch", "set-mode", "a", "declared"], &weave.ws);

    common::git_in(&project, &["switch", "-q", "a"]);
    common::records::edit_by_hand(&project, &[("a", "later = 1\n")], &[], "by hand");
    let stderr = refused(&run(&["lock"], &weave.ws), State::TargetUnreadable);
    assert!(stderr.contains("target unreadable (a)"), "{stderr}");
    assert!(
        stderr.contains("rwv branch set-mode a declared"),
        "{stderr}"
    );

    common::records::edit_by_hand(
        &project,
        &[
            (
                "a",
                "conflict = [\n  { mode = \"declared\" },\n  { mode = \"feature\", target = \"b\", as = \"declared\" },\n]\n",
            ),
            ("b", DECLARED),
        ],
        &[],
        "by hand",
    );
    let stderr = refused(&run(&["lock"], &weave.ws), State::TargetConflict);
    assert!(stderr.contains("target conflict (a)"), "{stderr}");
    assert!(
        stderr.contains("declared"),
        "both values are listed: {stderr}"
    );
    assert!(stderr.contains("feature onto b (as declared)"), "{stderr}");

    common::records::edit_by_hand(
        &project,
        &[
            (
                "a",
                "mode = \"feature\"\ntarget = \"b\"\nas = \"feature\"\n",
            ),
            (
                "b",
                "mode = \"feature\"\ntarget = \"a\"\nas = \"feature\"\n",
            ),
        ],
        &[],
        "by hand",
    );
    let stderr = refused(&run(&["lock"], &weave.ws), State::TargetCycle);
    assert!(stderr.contains("target cycle (a)"), "{stderr}");
    assert!(stderr.contains("a -> b -> a"), "{stderr}");

    succeed(&["branch", "set-mode", "a", "declared"], &weave.ws);
    succeed(&["lock"], &weave.ws);
}

#[test]
fn the_gate_commits_the_merge_of_the_remotes_records_before_it_reads() {
    let weave = Weave::new();
    weave.raw_branch("raw");
    refused(&run(&["lock"], &weave.ws), State::TargetUnrecorded);
    let project = weave.project();
    let tip = weave.records_tip().unwrap();
    let theirs = common::records::commit(
        &project,
        Some(&tip),
        &[("raw", DECLARED)],
        &[],
        "a colleague recorded it",
    );
    common::git_in(
        &project,
        &[
            "update-ref",
            &format!("refs/remotes/origin/{RECORDS}"),
            &theirs,
        ],
    );
    succeed(&["lock"], &weave.ws);
    assert_eq!(
        weave.records_tip().unwrap(),
        theirs,
        "the local records branch took the remote's tip, which holds the entry"
    );
}

#[test]
fn init_records_the_branch_it_leaves_checked_out_as_declared() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    succeed(&["init", "demo"], &ws);
    let project = ws.join("projects/demo");
    assert_eq!(
        common::git_in(&project, &["show", &format!("{RECORDS}:main")]),
        DECLARED.trim_end()
    );
    assert_eq!(
        common::git_in(&project, &["rev-list", "--count", RECORDS]),
        "1"
    );
    let rerun = run(&["init", "demo"], &ws);
    assert!(!rerun.status.success(), "{}", describe(&rerun));
    assert_eq!(
        common::git_in(&project, &["rev-list", "--count", RECORDS]),
        "1",
        "a refused init wrote no record"
    );
}

#[test]
fn clone_reads_the_remotes_records_first_and_records_only_what_they_lack() {
    let weave = Weave::new();
    let project = weave.project();
    let tip = weave.records_tip().unwrap();
    let published = common::records::commit(
        &project,
        Some(&tip),
        &[
            (
                "feat-x",
                "mode = \"feature\"\ntarget = \"main\"\nas = \"declared\"\n",
            ),
            (
                "main",
                "mode = \"feature\"\ntarget = \"release\"\nas = \"declared\"\n",
            ),
        ],
        &[],
        "published",
    );
    common::git_in(
        &project,
        &["update-ref", &format!("refs/heads/{RECORDS}"), &published],
    );
    common::git_in(&project, &["push", "-q", "origin", RECORDS]);

    let elsewhere = weave.tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    succeed(&["clone", &common::file_url(weave.origin())], &elsewhere);
    let cloned = elsewhere.join("projects/origin");
    assert_eq!(
        common::git_in(&cloned, &["rev-parse", RECORDS]),
        published,
        "the clone took the remote's records as they were: main is recorded there"
    );
    assert_eq!(
        common::git_in(&cloned, &["show", &format!("{RECORDS}:main")]),
        "mode = \"feature\"\ntarget = \"release\"\nas = \"declared\"",
        "a record the remote holds for the trunk is not overwritten with declared"
    );
}

#[test]
fn clone_writes_the_trunk_on_top_of_records_that_lack_it() {
    let weave = Weave::new();
    let project = weave.project();
    let orphan = common::records::commit(&project, None, &[("other", DECLARED)], &[], "published");
    common::git_in(
        &project,
        &["update-ref", &format!("refs/heads/{RECORDS}"), &orphan],
    );
    common::git_in(&project, &["push", "-q", "origin", RECORDS]);

    let elsewhere = weave.tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    succeed(&["clone", &common::file_url(weave.origin())], &elsewhere);
    let cloned = elsewhere.join("projects/origin");
    assert_eq!(
        common::git_in(&cloned, &["rev-parse", &format!("{RECORDS}^")]),
        orphan
    );
    assert_eq!(
        common::git_in(&cloned, &["show", &format!("{RECORDS}:main")]),
        DECLARED.trim_end()
    );
    assert_eq!(
        common::git_in(&cloned, &["show", &format!("{RECORDS}:other")]),
        DECLARED.trim_end()
    );
}

#[test]
fn init_adopt_records_the_adopted_trunk() {
    let weave = Weave::new();
    let elsewhere = weave.tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    succeed(
        &["init", "--adopt", &common::file_url(weave.origin())],
        &elsewhere,
    );
    let adopted = elsewhere.join("projects/origin");
    assert_eq!(
        common::git_in(&adopted, &["show", &format!("{RECORDS}:main")]),
        DECLARED.trim_end()
    );
}

#[test]
fn a_bare_workweave_create_records_a_feature_onto_the_parents_branch_and_says_where_it_lands() {
    let weave = Weave::new();
    let out = succeed(
        &["workweave", "create", "--project", "demo", "ww"],
        &weave.ws,
    );
    assert_eq!(
        weave.entry("demo--ww"),
        "mode = \"feature\"\ntarget = \"main\"\nas = \"declared\""
    );
    assert!(
        text(&out).contains("target-recorded: demo--ww lands into each branch main declares"),
        "{}",
        describe(&out)
    );

    let seat = weave.tmp.path().join(".workweaves/demo--ww");
    let nested = succeed(
        &["workweave", "create", "--project", "demo", "inner"],
        &seat,
    );
    assert_eq!(
        weave.entry("demo--inner"),
        "mode = \"feature\"\ntarget = \"demo--ww\"\nas = \"feature\"",
        "a workweave made from a workweave targets its branch, as the feature it is"
    );
    assert!(
        text(&nested)
            .contains("target-recorded: demo--inner lands into demo--ww's member branches"),
        "{}",
        describe(&nested)
    );
}

#[test]
fn a_workweave_create_from_an_unrecorded_branch_refuses_before_it_makes_anything() {
    let weave = Weave::new();
    weave.raw_branch("raw");
    let before = weave.state();
    refused(
        &run(
            &["workweave", "create", "--project", "demo", "ww"],
            &weave.ws,
        ),
        State::TargetUnrecorded,
    );
    assert!(stderr.contains("target unrecorded (raw)"), "{stderr}");
    assert!(
        stderr.contains("rwv branch set-mode raw declared"),
        "{stderr}"
    );
    assert!(!weave.tmp.path().join(".workweaves/demo--ww").exists());
    assert_eq!(weave.state(), before);
}

#[test]
fn a_workweave_create_from_a_detached_project_repo_refuses() {
    let weave = Weave::new();
    common::git_in(weave.project(), &["switch", "-q", "--detach"]);
    refused(
        &run(
            &["workweave", "create", "--project", "demo", "ww"],
            &weave.ws,
        ),
        State::AlignmentProjectDetached,
    );
    assert!(!weave.tmp.path().join(".workweaves/demo--ww").exists());
}

#[test]
fn creating_a_workweave_again_records_a_branch_an_earlier_create_left_without_one() {
    let weave = Weave::new();
    succeed(
        &["workweave", "create", "--project", "demo", "ww"],
        &weave.ws,
    );
    common::records::edit_by_hand(
        &weave.project(),
        &[],
        &["demo--ww"],
        "an earlier create stopped before recording",
    );
    assert!(common::git()
        .args(["show", &format!("{RECORDS}:demo--ww")])
        .current_dir(weave.project())
        .output()
        .unwrap()
        .stdout
        .is_empty());

    succeed(
        &["workweave", "create", "--project", "demo", "ww"],
        &weave.ws,
    );
    assert_eq!(
        weave.entry("demo--ww"),
        "mode = \"feature\"\ntarget = \"main\"\nas = \"declared\""
    );
}

#[test]
fn status_shows_the_checked_out_branchs_record_and_writes_nothing() {
    let weave = Weave::new();
    succeed(
        &["workweave", "create", "--project", "demo", "ww"],
        &weave.ws,
    );
    let tip = weave.records_tip();
    let ahead = common::records::commit(
        &weave.project(),
        tip.as_deref(),
        &[("colleague", DECLARED)],
        &[],
        "a colleague recorded a branch",
    );
    common::git_in(
        weave.project(),
        &[
            "update-ref",
            &format!("refs/remotes/origin/{RECORDS}"),
            &ahead,
        ],
    );
    let out = succeed(&["status"], &weave.ws);
    assert!(
        text(&out).contains("record: demo main: declared; landed into by demo--ww"),
        "{}",
        describe(&out)
    );
    let json = succeed(&["status", "--json"], &weave.ws);
    let envelope: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let record = &envelope["records"][0];
    assert_eq!(record["project"], "demo");
    assert_eq!(record["branch"], "main");
    assert_eq!(record["record"]["state"], "declared");
    assert_eq!(record["dependents"][0], "demo--ww");

    weave.raw_branch("raw");
    let out = succeed(&["status"], &weave.ws);
    assert!(
        text(&out).contains("record: demo raw: unrecorded"),
        "{}",
        describe(&out)
    );
    assert_eq!(weave.records_tip(), tip, "status wrote no record");
}
