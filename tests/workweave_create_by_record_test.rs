//! `rwv workweave create`, driven through the shipped binary against weaves
//! with bare remotes.
//!
//! What must hold, each pinned below by the test that names it:
//!
//! - a bare create is a feature under the minted name onto the parent's
//!   checked-out branch, with members at their pins; the parent does not
//!   switch and what it holds uncommitted, `rwv.toml` and `rwv.lock`
//!   included, stays there, reported;
//! - an identical re-run on a clean workweave returns it, reported; any
//!   difference is refused (`name` taken) and changes nothing;
//! - `--branch` opens by the branch's record and never by guess; a branch
//!   checked out in another workspace is not taken, and `--detach` is the way
//!   to look at it, writing no record;
//! - `--dir` and `--basedir` together refuse; the flags that went are
//!   ordinary unknown arguments;
//! - `--set-branch` and `--set-new-branch` judge every flag before anything
//!   is written, on both create verbs.

use std::path::{Path, PathBuf};

mod common;

const PROJECT: &str = "p";
const LIB: &str = "github/o/lib";
const APP: &str = "github/o/app";

fn weave(members: &[&str]) -> common::PushWorkspace {
    let roles: Vec<(&str, &str)> = members.iter().map(|m| (*m, "owned")).collect();
    common::build_workspace(PROJECT, &roles)
}

fn project_dir(ws: &Path) -> PathBuf {
    ws.join("projects").join(PROJECT)
}

fn run(args: &[&str], dir: &Path) -> std::process::Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn describe(out: &std::process::Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
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
    describe(&out)
}

fn refuse(args: &[&str], dir: &Path) -> String {
    let out = run(args, dir);
    assert!(
        !out.status.success(),
        "`rwv {}` should refuse; {}",
        args.join(" "),
        describe(&out)
    );
    describe(&out)
}

fn workweave_of(ws: &Path, name: &str) -> PathBuf {
    ws.parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{PROJECT}--{name}"))
}

fn head_branch(repo: &Path) -> Option<String> {
    let out = common::git()
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap().trim().to_owned())
}

fn has_branch(repo: &Path, name: &str) -> bool {
    common::git()
        .args(["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")])
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success()
}

fn tip(repo: &Path, rev: &str) -> String {
    common::git_in(repo, &["rev-parse", rev])
}

fn record_of(ws: &Path, branch: &str) -> Option<String> {
    let out = common::git()
        .args(["show", &format!("rwv-records:{branch}")])
        .current_dir(project_dir(ws))
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap())
}

fn state_of(path: &Path) -> String {
    format!(
        "{}|{}|{}",
        common::git_in(path, &["status", "--porcelain=v1"]),
        common::git_in(path, &["rev-parse", "HEAD"]),
        head_branch(path).unwrap_or_default()
    )
}

#[test]
fn a_bare_create_is_a_feature_under_the_minted_name_with_members_at_their_pins() {
    let w = weave(&[LIB, APP]);
    let ws = &w.workspace;
    let pins = (tip(&ws.join(LIB), "HEAD"), tip(&ws.join(APP), "HEAD"));

    let out = succeed(&["workweave", "create", "--project", PROJECT, "ww"], ws);

    let seat = workweave_of(ws, "ww");
    assert_eq!(
        head_branch(&project_dir(&seat)).as_deref(),
        Some("p--ww"),
        "{out}"
    );
    let record = record_of(ws, "p--ww").expect("a record is written");
    assert!(record.contains("mode = \"feature\"") && record.contains("target = \"main\""));
    assert!(out.contains("target-recorded: p--ww lands"), "{out}");
    for (member, pin) in [(LIB, &pins.0), (APP, &pins.1)] {
        assert_eq!(head_branch(&seat.join(member)).as_deref(), Some("p--ww"));
        assert_eq!(&tip(&seat.join(member), "HEAD"), pin);
    }
}

#[test]
fn the_parent_does_not_switch_and_its_uncommitted_files_stay_where_they_are_reported() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let committed_manifest = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    let committed_lock = std::fs::read_to_string(project.join("rwv.lock")).unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!("{committed_manifest}# edited in the parent\n"),
    )
    .unwrap();
    std::fs::write(
        project.join("rwv.lock"),
        format!("{}\n\n", committed_lock.trim_end()),
    )
    .unwrap();
    std::fs::write(project.join("notes.txt"), "untracked").unwrap();
    let before = state_of(&project);
    let manifest_before = std::fs::read_to_string(project.join("rwv.toml")).unwrap();

    let out = succeed(&["workweave", "create", "--project", PROJECT, "ww"], ws);

    assert_eq!(state_of(&project), before, "the parent changed: {out}");
    assert_eq!(
        std::fs::read_to_string(project.join("rwv.toml")).unwrap(),
        manifest_before
    );
    assert_eq!(head_branch(&project).as_deref(), Some("main"));
    for name in ["rwv.toml", "rwv.lock", "notes.txt"] {
        assert!(out.contains(name), "the report names {name}: {out}");
    }
    assert!(out.contains("stay there"), "{out}");
    let seat = project_dir(&workweave_of(ws, "ww"));
    assert_eq!(
        std::fs::read_to_string(seat.join("rwv.toml")).unwrap(),
        committed_manifest,
        "the new workweave starts from the committed manifest"
    );
    assert!(!seat.join("notes.txt").exists());
}

#[test]
fn an_identical_rerun_on_a_clean_workweave_returns_it_and_any_difference_refuses_changing_nothing()
{
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let args = ["workweave", "create", "--project", PROJECT, "ww"];
    succeed(&args, ws);
    let seat = workweave_of(ws, "ww");
    std::fs::write(seat.join("agent-state.txt"), "kept").unwrap();
    let before = (
        state_of(&project_dir(&seat)),
        state_of(&seat.join(LIB)),
        record_of(ws, "p--ww"),
    );

    let again = succeed(&args, ws);
    assert!(again.contains("returned as it is"), "{again}");
    assert_eq!(
        std::fs::read_to_string(seat.join("agent-state.txt")).unwrap(),
        "kept"
    );

    for different in [
        vec!["--new-branch", "other"],
        vec!["--declared"],
        vec!["--detach", "main"],
    ] {
        let mut spelled = args.to_vec();
        spelled.extend(different.iter());
        let refused = refuse(&spelled, ws);
        assert!(refused.contains("name taken"), "{refused}");
    }
    std::fs::write(project_dir(&seat).join("scratch.txt"), "x").unwrap();
    common::git_in(project_dir(&seat), &["add", "scratch.txt"]);
    let dirty = refuse(&args, ws);
    assert!(dirty.contains("name taken"), "{dirty}");
    assert!(dirty.contains("uncommitted changes"), "{dirty}");

    common::git_in(project_dir(&seat), &["reset", "-q", "--", "scratch.txt"]);
    std::fs::remove_file(project_dir(&seat).join("scratch.txt")).unwrap();
    assert_eq!(
        (
            state_of(&project_dir(&seat)),
            state_of(&seat.join(LIB)),
            record_of(ws, "p--ww")
        ),
        before
    );
    assert!(!has_branch(&project_dir(ws), "other"));
}

#[test]
fn opening_reads_the_branchs_record_and_never_guesses_one() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    succeed(&["branch", "create", "feat-x"], ws);
    succeed(&["switch", "main"], ws);

    let out = succeed(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "ww",
            "--branch",
            "feat-x",
        ],
        ws,
    );
    let seat = workweave_of(ws, "ww");
    assert_eq!(head_branch(&project_dir(&seat)).as_deref(), Some("feat-x"));
    assert_eq!(
        head_branch(&seat.join(LIB)).as_deref(),
        Some("feat-x"),
        "{out}"
    );
    let recorded = record_of(ws, "feat-x");

    let differs = refuse(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "w2",
            "--branch",
            "feat-x",
            "--declared",
        ],
        ws,
    );
    assert!(differs.contains("rwv branch set-mode"), "{differs}");
    assert_eq!(record_of(ws, "feat-x"), recorded);

    common::git_in(&project, &["branch", "raw-branch"]);
    let unrecorded = refuse(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "w3",
            "--branch",
            "raw-branch",
        ],
        ws,
    );
    assert!(unrecorded.contains("unrecorded"), "{unrecorded}");
    assert!(record_of(ws, "raw-branch").is_none());
    assert!(!workweave_of(ws, "w3").exists());

    let missing = refuse(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "w4",
            "--branch",
            "no-such",
        ],
        ws,
    );
    assert!(
        missing.contains("no-such-branch") || missing.contains("no such branch"),
        "{missing}"
    );
    assert!(!workweave_of(ws, "w4").exists());
}

#[test]
fn a_branch_checked_out_in_another_workspace_is_not_taken_and_detach_looks_at_it_writing_no_record()
{
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let records_before = common::git_in(&project, &["rev-parse", "rwv-records"]);

    let primary_has_main = refuse(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "ww",
            "--branch",
            "main",
        ],
        ws,
    );
    assert!(primary_has_main.contains("checkout"), "{primary_has_main}");
    assert!(!workweave_of(ws, "ww").exists());

    succeed(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "ww1",
            "--new-branch",
            "feat-x",
        ],
        ws,
    );
    let ww1_has_it = refuse(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "ww2",
            "--branch",
            "feat-x",
        ],
        ws,
    );
    assert!(ww1_has_it.contains("checkout"), "{ww1_has_it}");
    assert!(ww1_has_it.contains("--detach feat-x"), "{ww1_has_it}");
    assert!(!workweave_of(ws, "ww2").exists());

    let records_mid = common::git_in(&project, &["rev-parse", "rwv-records"]);
    for (name, rev) in [("look1", "feat-x"), ("look2", "main")] {
        succeed(
            &[
                "workweave",
                "create",
                "--project",
                PROJECT,
                name,
                "--detach",
                rev,
            ],
            ws,
        );
        let seat = workweave_of(ws, name);
        assert_eq!(head_branch(&project_dir(&seat)), None, "{name} is detached");
        assert_eq!(head_branch(&seat.join(LIB)), None);
        assert_eq!(tip(&project_dir(&seat), "HEAD"), tip(&project, rev));
    }
    assert_ne!(
        records_before, records_mid,
        "the feature create wrote its record"
    );
    assert_eq!(
        common::git_in(&project, &["rev-parse", "rwv-records"]),
        records_mid,
        "a detach writes no record"
    );
    assert_eq!(head_branch(&project).as_deref(), Some("main"));
}

#[test]
fn dir_and_basedir_together_refuse_naming_both() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let out = run(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "ww",
            "--dir",
            "a",
            "--basedir",
            "b",
        ],
        ws,
    );
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    let text = describe(&out);
    assert!(
        text.contains("--dir") && text.contains("--basedir"),
        "{text}"
    );
    assert!(!workweave_of(ws, "ww").exists());
}

#[test]
fn the_flags_that_went_are_ordinary_unknown_arguments() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    for flag in ["--capture-dirty", "--replace-existing", "--from"] {
        let mut args = vec!["workweave", "create", "--project", PROJECT, "ww", flag];
        if flag == "--from" {
            args.push("primary");
        }
        let out = run(&args, ws);
        assert_eq!(out.status.code(), Some(2), "{flag}: {}", describe(&out));
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            stderr.contains(&format!("unexpected argument '{flag}'")),
            "{flag}: {stderr}"
        );
        assert!(!workweave_of(ws, "ww").exists());
    }
}

#[test]
fn set_flags_on_a_workweave_create_are_judged_before_anything_is_written() {
    let w = weave(&[LIB, APP]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let records_before = common::git_in(&project, &["rev-parse", "rwv-records"]);
    let lib_branches = common::git_in(ws.join(LIB), &["branch", "--list"]);
    let ok = format!("{LIB}=lib-b");
    let missing = format!("{APP}=no-such-branch");

    let out = refuse(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "ww",
            "--declared",
            "--set-new-branch",
            &ok,
            "--set-branch",
            &missing,
        ],
        ws,
    );

    assert!(
        out.contains("no-such-branch") || out.contains("no such branch"),
        "{out}"
    );
    assert!(!workweave_of(ws, "ww").exists());
    assert_eq!(
        common::git_in(&project, &["rev-parse", "rwv-records"]),
        records_before
    );
    assert_eq!(
        common::git_in(ws.join(LIB), &["branch", "--list"]),
        lib_branches
    );
    assert!(!has_branch(&project, "p--ww"));
}

#[test]
fn set_flags_on_a_workweave_create_set_the_entries_in_the_branchs_first_commit() {
    let w = weave(&[LIB, APP]);
    let ws = &w.workspace;
    let before = tip(&project_dir(ws), "HEAD");
    let spec = format!("{LIB}=lib-b");

    succeed(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "ww",
            "--declared",
            "--new-branch",
            "dec",
            "--set-new-branch",
            &spec,
        ],
        ws,
    );

    let seat = workweave_of(ws, "ww");
    let project = project_dir(&seat);
    assert_eq!(head_branch(&project).as_deref(), Some("dec"));
    assert!(has_branch(&ws.join(LIB), "lib-b"));
    let manifest = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    assert!(manifest.contains("lib-b"), "{manifest}");
    assert_eq!(
        common::git_in(
            &project,
            &["rev-list", "--count", &format!("{before}..HEAD")]
        ),
        "1"
    );
    assert_eq!(head_branch(&seat.join(LIB)).as_deref(), Some("lib-b"));
    assert_eq!(common::git_in(&project, &["status", "--porcelain"]), "");
}

#[test]
fn set_flags_on_a_branch_create_are_judged_before_anything_is_written() {
    let w = weave(&[LIB, APP]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let records_before = common::git_in(&project, &["rev-parse", "rwv-records"]);
    let lib_branches = common::git_in(ws.join(LIB), &["branch", "--list"]);
    let ok = format!("{LIB}=lib-b");
    let missing = format!("{APP}=no-such-branch");

    let out = refuse(
        &[
            "branch",
            "create",
            "dec",
            "--declared",
            "--set-new-branch",
            &ok,
            "--set-branch",
            &missing,
        ],
        ws,
    );

    assert!(
        out.contains("no-such-branch") || out.contains("no such branch"),
        "{out}"
    );
    assert_eq!(head_branch(&project).as_deref(), Some("main"));
    assert!(!has_branch(&project, "dec"));
    assert_eq!(
        common::git_in(&project, &["rev-parse", "rwv-records"]),
        records_before
    );
    assert_eq!(
        common::git_in(ws.join(LIB), &["branch", "--list"]),
        lib_branches
    );
}

#[test]
fn set_flags_on_a_branch_create_set_the_entries_in_the_branchs_first_commit() {
    let w = weave(&[LIB, APP]);
    let ws = &w.workspace;
    let main_tip = tip(&project_dir(ws), "HEAD");
    let spec = format!("{LIB}=lib-b");

    succeed(
        &[
            "branch",
            "create",
            "dec",
            "--declared",
            "--set-new-branch",
            &spec,
        ],
        ws,
    );

    let project = project_dir(ws);
    assert_eq!(head_branch(&project).as_deref(), Some("dec"));
    let manifest = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    assert!(manifest.contains("lib-b"), "{manifest}");
    assert_eq!(
        common::git_in(
            &project,
            &["rev-list", "--count", &format!("{main_tip}..HEAD")]
        ),
        "1"
    );
    assert_eq!(head_branch(&ws.join(LIB)).as_deref(), Some("lib-b"));
}
