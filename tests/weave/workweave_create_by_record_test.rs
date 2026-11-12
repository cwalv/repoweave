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

use repoweave::diagnosis::State;
use repoweave::refusal::RefusalKind;
use std::path::{Path, PathBuf};

use crate::common;

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

fn refuse(args: &[&str], dir: &Path, state: State) -> String {
    let out = run(args, dir);
    common::cli::refused(&out, state)
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
    // raw lock bytes: the committed lock plus a blank line, so the file is
    // uncommitted without its pins changing; the builder writes only pins.
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
        std::fs::read_to_string(seat.join("rwv.toml"))
            .unwrap()
            .replace("\r\n", "\n"),
        committed_manifest.replace("\r\n", "\n"),
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
        let refused = refuse(&spelled, ws, State::NameTaken);
        assert!(refused.contains("name taken"), "{refused}");
    }
    std::fs::write(project_dir(&seat).join("scratch.txt"), "x").unwrap();
    common::git_in(project_dir(&seat), &["add", "scratch.txt"]);
    let dirty = refuse(&args, ws, State::NameTaken);
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

    for flag in ["--declared", "--target"] {
        let mut args = vec![
            "workweave",
            "create",
            "--project",
            PROJECT,
            "w2",
            "--branch",
            "feat-x",
            flag,
        ];
        if flag == "--target" {
            args.push("main");
        }
        let out = run(&args, ws);
        assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    }
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
        State::TargetUnrecorded,
    );
    assert!(unrecorded.contains("unrecorded"), "{unrecorded}");
    assert!(record_of(ws, "raw-branch").is_none());
    assert!(!workweave_of(ws, "w3").exists());
    assert!(unrecorded.contains("rwv fetch"), "{unrecorded}");
    assert!(
        unrecorded.contains("rwv branch set-mode raw-branch declared"),
        "{unrecorded}"
    );
    succeed(&["fetch"], ws);
    succeed(&["branch", "set-mode", "raw-branch", "declared"], ws);
    succeed(
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
    assert_eq!(
        head_branch(&project_dir(&workweave_of(ws, "w3"))).as_deref(),
        Some("raw-branch")
    );

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
        State::NameNoSuchBranch,
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
        State::CheckoutInRawWorktree,
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
        State::CheckoutInAnotherWorkspace,
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
    common::git_in(ws.join(LIB), &["branch", "lib-existing"]);
    common::git_in(ws.join(APP), &["branch", "app-existing"]);
    let lib_branches = common::git_in(ws.join(LIB), &["branch", "--list"]);
    let app_branches = common::git_in(ws.join(APP), &["branch", "--list"]);
    let ok = format!("{LIB}=lib-existing");
    let taken = format!("{APP}=app-existing");

    let out = refuse(
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "ww",
            "--declared",
            "--set-branch",
            &ok,
            "--set-new-branch",
            &taken,
        ],
        ws,
        State::NameExists,
    );

    assert!(out.contains("name exists"), "{out}");
    assert!(!workweave_of(ws, "ww").exists());
    assert_eq!(
        common::git_in(&project, &["rev-parse", "rwv-records"]),
        records_before
    );
    assert_eq!(
        common::git_in(ws.join(LIB), &["branch", "--list"]),
        lib_branches
    );
    assert_eq!(
        common::git_in(ws.join(APP), &["branch", "--list"]),
        app_branches
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
    common::git_in(ws.join(LIB), &["branch", "lib-existing"]);
    common::git_in(ws.join(APP), &["branch", "app-existing"]);
    let lib_branches = common::git_in(ws.join(LIB), &["branch", "--list"]);
    let app_branches = common::git_in(ws.join(APP), &["branch", "--list"]);
    let ok = format!("{LIB}=lib-existing");
    let taken = format!("{APP}=app-existing");

    let out = refuse(
        &[
            "branch",
            "create",
            "dec",
            "--declared",
            "--set-branch",
            &ok,
            "--set-new-branch",
            &taken,
        ],
        ws,
        State::NameExists,
    );

    assert!(out.contains("name exists"), "{out}");
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
    assert_eq!(
        common::git_in(ws.join(APP), &["branch", "--list"]),
        app_branches
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

fn lib_with_a_second_commit(ws: &Path) -> (String, String) {
    let lib = ws.join(LIB);
    let first = tip(&lib, "HEAD");
    std::fs::write(lib.join("more.txt"), "more").unwrap();
    common::git_in(&lib, &["add", "."]);
    common::git_in(&lib, &["commit", "-qm", "more"]);
    (first, tip(&lib, "HEAD"))
}

#[test]
fn a_branch_name_holding_at_is_created_under_that_name_and_pin_starts_it_at_the_revision() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let (first, _) = lib_with_a_second_commit(ws);
    let branch = format!("{LIB}=release@2024");
    let pin = format!("{LIB}={first}");

    succeed(
        &[
            "branch",
            "create",
            "dec",
            "--declared",
            "--set-new-branch",
            &branch,
            "--pin",
            &pin,
        ],
        ws,
    );

    assert!(has_branch(&ws.join(LIB), "release@2024"));
    assert_eq!(tip(&ws.join(LIB), "release@2024"), first);
    let manifest = std::fs::read_to_string(project_dir(ws).join("rwv.toml")).unwrap();
    assert!(manifest.contains("release@2024"), "{manifest}");
}

#[test]
fn a_pin_with_no_flag_naming_its_member_or_two_for_one_member_refuse_before_anything_is_written() {
    let w = weave(&[LIB, APP]);
    let ws = &w.workspace;
    let (first, _) = lib_with_a_second_commit(ws);
    let records_before = common::git_in(project_dir(ws), &["rev-parse", "rwv-records"]);
    let lib_branches = common::git_in(ws.join(LIB), &["branch", "--list"]);
    let branch = format!("{LIB}=lib-new");
    let orphan = format!("{APP}={first}");
    let pin = format!("{LIB}={first}");

    for verb in [
        vec!["branch", "create", "dec", "--declared"],
        vec![
            "workweave",
            "create",
            "--project",
            PROJECT,
            "ww",
            "--declared",
        ],
    ] {
        for pins in [vec![&orphan], vec![&pin, &pin]] {
            let mut args = verb.clone();
            args.extend(["--set-new-branch", &branch]);
            for pin in pins {
                args.extend(["--pin", pin]);
            }
            let out = run(&args, ws);
            common::cli::refused(&out, RefusalKind::InapplicableFlag);
            assert_eq!(head_branch(&project_dir(ws)).as_deref(), Some("main"));
            assert!(!workweave_of(ws, "ww").exists());
            assert_eq!(
                common::git_in(project_dir(ws), &["rev-parse", "rwv-records"]),
                records_before
            );
            assert_eq!(
                common::git_in(ws.join(LIB), &["branch", "--list"]),
                lib_branches
            );
        }
    }
}
