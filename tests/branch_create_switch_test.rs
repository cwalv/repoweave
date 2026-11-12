//! `rwv branch create` and `rwv switch`, driven through the shipped binary
//! against weaves with bare remotes.
//!
//! What must hold, each pinned below by the test that names it:
//!
//! - a bare create is a feature onto the checked-out branch, `--target` a
//!   feature onto that branch, `--declared` a declared branch; each writes its
//!   record, puts the project repo on the new branch and gives a feature's
//!   owned members branches of the same name at their pins;
//! - a create from a detached project repo ends on the new branch, starting at
//!   the HEAD it left and carrying the uncommitted `rwv.toml`;
//! - a reserved, existing, declared or other-feature name refuses and nothing
//!   is created;
//! - `switch` reopens a recorded branch bare, opens an unrecorded one only
//!   with a spelling, refuses a spelling that differs from the record, and
//!   refuses a branch checked out in another workspace;
//! - `switch` judges every checkout it must move before anything moves: an
//!   operation in progress, edits it would overwrite and an uncommitted lock
//!   file refuse and change nothing, and edits it can carry are carried;
//! - `switch` onto a pre-schema tip moves the project repo and stops before
//!   the members;
//! - `switch --detach <rev>` detaches the workspace and writes no record.

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

fn routes(refusal: &str) -> Vec<&str> {
    refusal
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect()
}

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "."]);
    common::git_in(dir, &["commit", "-qm", file]);
    common::git_in(dir, &["rev-parse", "HEAD"])
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

fn workspace_state(project: &Path) -> String {
    let dir = common::git_in(project, &["rev-parse", "--absolute-git-dir"]);
    std::fs::read_to_string(Path::new(&dir).join("rwv-workspace-state")).unwrap_or_default()
}

fn git_ok(dir: &Path, args: &[&str]) {
    common::git_in(dir, args);
}

#[test]
fn a_bare_create_is_a_feature_onto_the_checked_out_branch_with_member_branches_at_their_pins() {
    let w = weave(&[LIB, APP]);
    let ws = &w.workspace;
    let pins = (tip(&ws.join(LIB), "HEAD"), tip(&ws.join(APP), "HEAD"));

    let out = succeed(&["branch", "create", "feat-x"], ws);

    assert!(out.contains("target-recorded: feat-x lands"), "{out}");
    assert_eq!(head_branch(&project_dir(ws)).as_deref(), Some("feat-x"));
    let record = record_of(ws, "feat-x").expect("a record is written");
    assert!(record.contains("mode = \"feature\"") && record.contains("target = \"main\""));
    for (member, pin) in [(LIB, &pins.0), (APP, &pins.1)] {
        assert_eq!(head_branch(&ws.join(member)).as_deref(), Some("feat-x"));
        assert_eq!(&tip(&ws.join(member), "HEAD"), pin);
    }
    assert!(workspace_state(&project_dir(ws)).contains("feat-x"));
}

#[test]
fn a_target_create_starts_at_the_targets_tip_and_records_a_feature_onto_it() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    succeed(&["branch", "create", "dev", "--declared"], ws);
    let dev_tip = commit_in(&project, "dev.txt");
    succeed(&["switch", "main"], ws);

    succeed(&["branch", "create", "feat-y", "--target", "dev"], ws);

    assert_eq!(tip(&project, "HEAD"), dev_tip);
    assert_eq!(head_branch(&project).as_deref(), Some("feat-y"));
    let record = record_of(ws, "feat-y").unwrap();
    assert!(record.contains("target = \"dev\"") && record.contains("as = \"declared\""));
    assert_eq!(head_branch(&ws.join(LIB)).as_deref(), Some("feat-y"));
}

#[test]
fn a_declared_create_starts_at_the_checked_out_commit_and_leaves_members_on_their_declared_branches(
) {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let main_tip = tip(&project, "HEAD");

    succeed(&["branch", "create", "rel", "--declared"], ws);

    assert_eq!(tip(&project, "HEAD"), main_tip);
    assert_eq!(head_branch(&project).as_deref(), Some("rel"));
    assert_eq!(record_of(ws, "rel").unwrap().trim(), "mode = \"declared\"");
    assert_eq!(head_branch(&ws.join(LIB)).as_deref(), Some("main"));
    assert!(!has_branch(&ws.join(LIB), "rel"));
}

#[test]
fn at_starts_the_branch_at_the_revision_it_names() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let earlier = commit_in(&project, "a.txt");
    commit_in(&project, "b.txt");

    succeed(&["branch", "create", "old", "--declared", "--at", &earlier], ws);

    assert_eq!(tip(&project, "HEAD"), earlier);
    assert_eq!(head_branch(&project).as_deref(), Some("old"));
}

#[test]
fn a_create_from_a_detached_project_repo_starts_at_its_head_carries_the_file_and_ends_on_the_branch(
) {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    git_ok(&project, &["checkout", "-q", "--detach"]);
    let detached_at = tip(&project, "HEAD");
    let mut toml = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    toml.push_str("# an edit made while detached\n");
    std::fs::write(project.join("rwv.toml"), &toml).unwrap();

    let bare = refuse(&["branch", "create", "w"], ws);
    assert!(bare.contains("--declared") && bare.contains("--target"), "{bare}");
    assert!(!has_branch(&project, "w"));
    let switched = refuse(&["switch", "main"], ws);
    assert!(
        switched.contains("rwv.toml or rwv.lock uncommitted")
            && switched.contains("rwv branch create <W> --declared")
            && !switched.contains("rwv lock --commit"),
        "{switched}"
    );
    assert_eq!(head_branch(&project), None);

    succeed(&["branch", "create", "w", "--declared"], ws);

    assert_eq!(head_branch(&project).as_deref(), Some("w"));
    assert_eq!(tip(&project, "w"), detached_at);
    assert_eq!(
        std::fs::read_to_string(project.join("rwv.toml")).unwrap(),
        toml,
        "the uncommitted rwv.toml is carried"
    );
}

#[test]
fn a_reserved_or_taken_name_refuses_and_creates_nothing() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let records_before = tip(&project, "rwv-records");

    for reserved in ["rwv-records", "rwv-records/x"] {
        let out = refuse(&["branch", "create", reserved, "--declared"], ws);
        assert!(out.contains("name reserved"), "{out}");
        assert_eq!(routes(&out), ["name"]);
        assert!(
            !out.contains("rwv branch rename"),
            "a create offers another name, not a rename: {out}"
        );
    }
    let exists = refuse(&["branch", "create", "main"], ws);
    assert!(exists.contains("name exists"), "{exists}");
    assert!(exists.contains("rwv switch main"), "{exists}");

    git_ok(&ws.join(LIB), &["branch", "member-has-it"]);
    let member = refuse(&["branch", "create", "member-has-it"], ws);
    assert!(member.contains("name exists"), "{member}");

    assert!(!has_branch(&project, "member-has-it"));
    assert!(!has_branch(&project, "rwv-records/x"));
    assert_eq!(head_branch(&project).as_deref(), Some("main"));
    assert_eq!(tip(&project, "rwv-records"), records_before);
}

#[test]
fn a_feature_cannot_take_a_name_a_live_branch_declares_for_a_member() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    succeed(&["branch", "create", "rel", "--declared"], ws);
    let toml = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        toml.replace("branch = \"main\"", "branch = \"feat-d\""),
    )
    .unwrap();
    git_ok(&project, &["commit", "-qam", "declare feat-d"]);
    succeed(&["switch", "main"], ws);

    let out = refuse(&["branch", "create", "feat-d"], ws);

    assert!(out.contains("name declared"), "{out}");
    assert!(!has_branch(&project, "feat-d"));
    succeed(&["branch", "create", "feat-d", "--declared"], ws);
}

#[test]
fn an_unrecorded_branch_is_not_opened_by_guess() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    git_ok(&project, &["switch", "-q", "-c", "raw"]);

    let out = refuse(&["branch", "create", "x"], ws);

    assert!(out.contains("target unrecorded"), "{out}");
    assert!(!has_branch(&project, "x") && record_of(ws, "x").is_none());
    assert_eq!(head_branch(&project).as_deref(), Some("raw"));
    let targeted = refuse(&["branch", "create", "x", "--target", "raw"], ws);
    assert!(targeted.contains("target unrecorded (raw)"), "{targeted}");
    assert!(!has_branch(&project, "x"));
}

#[test]
fn a_diverged_target_refuses_a_create_that_starts_at_its_tip_and_at_names_this_clones() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let other = ws.parent().unwrap().join("colleague");
    common::git_in(
        ws.parent().unwrap(),
        &[
            "clone",
            "-q",
            w.project_bare.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    commit_in(&other, "theirs.txt");
    git_ok(&other, &["push", "-q", "origin", "main"]);
    let mine = commit_in(&project, "mine.txt");
    git_ok(&project, &["fetch", "-q", "origin"]);

    let out = refuse(&["branch", "create", "f"], ws);

    assert!(out.contains("push-branch diverged"), "{out}");
    assert!(out.contains("rerun with --at main"), "{out}");
    assert!(!out.contains("--merge-upstream") && !out.contains("--reset-to-upstream"));
    assert!(!has_branch(&project, "f"));
    succeed(&["branch", "create", "f", "--at", "main"], ws);
    assert_eq!(tip(&project, "f"), mine);
}

#[test]
fn switch_reopens_a_recorded_branch_bare_and_aligns_the_members_both_ways() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    succeed(&["branch", "create", "feat-x"], ws);
    let on_feature = commit_in(&ws.join(LIB), "lib-work.txt");

    succeed(&["switch", "main"], ws);
    assert_eq!(head_branch(&project).as_deref(), Some("main"));
    assert_eq!(head_branch(&ws.join(LIB)).as_deref(), Some("main"));

    succeed(&["switch", "feat-x"], ws);
    assert_eq!(head_branch(&project).as_deref(), Some("feat-x"));
    assert_eq!(head_branch(&ws.join(LIB)).as_deref(), Some("feat-x"));
    assert_eq!(tip(&ws.join(LIB), "HEAD"), on_feature);
    assert!(workspace_state(&project).contains("feat-x"));
}

#[test]
fn switch_opens_an_unrecorded_branch_only_with_a_spelling_and_refuses_one_that_differs() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    git_ok(&project, &["branch", "raw"]);
    git_ok(&project, &["branch", "raw2"]);

    let bare = refuse(&["switch", "raw"], ws);
    assert!(bare.contains("target unrecorded (raw)"), "{bare}");
    assert_eq!(head_branch(&project).as_deref(), Some("main"));
    assert!(record_of(ws, "raw").is_none());

    succeed(&["switch", "raw", "--target", "main"], ws);
    assert_eq!(head_branch(&project).as_deref(), Some("raw"));
    assert!(record_of(ws, "raw").unwrap().contains("target = \"main\""));
    assert_eq!(head_branch(&ws.join(LIB)).as_deref(), Some("raw"));

    git_ok(&project, &["branch", "raw3"]);
    succeed(&["branch", "set-mode", "raw3", "declared"], ws);
    let differing = refuse(&["switch", "raw", "--target", "raw3"], ws);
    assert!(differing.contains("target conflict"), "{differing}");
    assert!(differing.contains("rwv branch set-mode"), "{differing}");
    let also = refuse(&["switch", "raw", "--declared"], ws);
    assert!(also.contains("target conflict"), "{also}");
    assert_eq!(head_branch(&project).as_deref(), Some("raw"));

    succeed(&["switch", "raw", "--target", "main"], ws);
    succeed(&["switch", "raw2", "--declared"], ws);
    assert_eq!(record_of(ws, "raw2").unwrap().trim(), "mode = \"declared\"");
    assert_eq!(head_branch(&ws.join(LIB)).as_deref(), Some("main"));
}

#[test]
fn switch_refuses_a_project_branch_checked_out_in_another_workspace() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    succeed(&["workweave", "create", "--project", PROJECT, "side"], ws);
    let minted = format!("{PROJECT}--side");

    let out = refuse(&["switch", &minted], ws);

    assert_eq!(routes(&out), ["checkout"], "{out}");
    assert!(out.contains("rwv switch <other>") || out.contains("rwv switch"), "{out}");
    assert_eq!(head_branch(&project).as_deref(), Some("main"));
    assert_eq!(head_branch(&ws.join(LIB)).as_deref(), Some("main"));
}

#[test]
fn switch_with_no_branch_refuses_naming_update_and_no_such_branch_names_the_fetch() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;

    let none = refuse(&["switch"], ws);
    assert!(none.contains("rwv update") || none.contains("--detach"), "{none}");
    let missing = refuse(&["switch", "nowhere"], ws);
    assert!(missing.contains("name no-such-branch (nowhere)"), "{missing}");
    assert!(missing.contains("rwv fetch"), "{missing}");
    assert!(
        !missing.contains("switch --detach"),
        "no member has it, so no member choice is printed: {missing}"
    );
}

#[test]
fn a_branch_only_a_member_has_is_refused_naming_the_member_and_a_way_to_look_at_it() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    git_ok(&ws.join(LIB), &["branch", "member-only"]);

    let out = refuse(&["switch", "member-only"], ws);

    assert!(out.contains(LIB) && out.contains("name no-such-branch"), "{out}");
    assert!(out.contains("switch --detach"), "{out}");
    assert_eq!(head_branch(&project).as_deref(), Some("main"));
}

#[test]
fn an_operation_in_progress_in_a_member_it_must_move_refuses_before_anything_moves() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let lib = ws.join(LIB);
    succeed(&["branch", "create", "feat-x"], ws);
    succeed(&["switch", "main"], ws);
    git_ok(&lib, &["switch", "-q", "-c", "side"]);
    commit_in(&lib, "side.txt");
    git_ok(&lib, &["switch", "-q", "main"]);
    git_ok(&lib, &["merge", "-q", "--no-commit", "--no-ff", "side"]);
    let state_before = workspace_state(&project);

    let out = refuse(&["switch", "feat-x"], ws);

    assert!(out.contains("edits mid-operation"), "{out}");
    assert_eq!(head_branch(&project).as_deref(), Some("main"), "{out}");
    assert_eq!(head_branch(&lib).as_deref(), Some("main"));
    assert_eq!(workspace_state(&project), state_before);
}

#[test]
fn edits_the_move_would_overwrite_refuse_before_anything_moves_and_other_edits_are_carried() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let lib = ws.join(LIB);
    std::fs::write(project.join("notes.txt"), "tracked note").unwrap();
    git_ok(&project, &["add", "notes.txt"]);
    git_ok(&project, &["commit", "-qm", "notes"]);
    succeed(&["branch", "create", "feat-x"], ws);
    std::fs::write(lib.join("README"), "changed on the feature").unwrap();
    git_ok(&lib, &["commit", "-qam", "feature readme"]);
    succeed(&["switch", "main"], ws);

    std::fs::write(lib.join("README"), "local edit").unwrap();
    let refused = refuse(&["switch", "feat-x"], ws);
    assert!(refused.contains("edits dirty"), "{refused}");
    assert_eq!(head_branch(&project).as_deref(), Some("main"));
    assert_eq!(head_branch(&lib).as_deref(), Some("main"));
    assert_eq!(std::fs::read_to_string(lib.join("README")).unwrap(), "local edit");

    git_ok(&lib, &["checkout", "-q", "--", "README"]);
    succeed(&["switch", "feat-x"], ws);
    std::fs::write(project.join("notes.txt"), "carried edit").unwrap();
    let carried = succeed(&["switch", "main"], ws);
    assert_eq!(head_branch(&project).as_deref(), Some("main"), "{carried}");
    assert_eq!(
        std::fs::read_to_string(project.join("notes.txt")).unwrap(),
        "carried edit"
    );
}

#[test]
fn an_uncommitted_rwv_toml_refuses_a_switch_naming_the_lock_commit() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    succeed(&["branch", "create", "feat-x"], ws);
    succeed(&["switch", "main"], ws);
    let mut toml = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    toml.push_str("# edit\n");
    std::fs::write(project.join("rwv.toml"), toml).unwrap();

    let out = refuse(&["switch", "feat-x"], ws);

    assert!(out.contains("rwv.toml or rwv.lock uncommitted"), "{out}");
    assert!(out.contains("rwv lock --commit"), "{out}");
    assert_eq!(head_branch(&project).as_deref(), Some("main"));
}

#[test]
fn a_switch_onto_a_pre_schema_tip_moves_the_project_repo_and_stops_before_the_members() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let lib = ws.join(LIB);
    succeed(&["branch", "create", "feat-x"], ws);
    succeed(&["switch", "main"], ws);
    git_ok(&project, &["switch", "-q", "-c", "old"]);
    let toml = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        toml.replace("branch = \"main\"", "version = \"main\""),
    )
    .unwrap();
    git_ok(&project, &["commit", "-qam", "an earlier release's keys"]);
    git_ok(&project, &["switch", "-q", "main"]);
    succeed(&["branch", "set-mode", "old", "declared"], ws);
    let state_before = workspace_state(&project);

    let out = refuse(&["switch", "old"], ws);

    assert!(out.contains("unparseable-project here"), "{out}");
    assert!(out.contains("rwv doctor --fix"), "{out}");
    assert_eq!(head_branch(&project).as_deref(), Some("old"));
    assert_eq!(head_branch(&lib).as_deref(), Some("main"));
    assert_eq!(workspace_state(&project), state_before);
}

#[test]
fn switch_detach_makes_a_detached_workspace_at_the_pins_and_writes_no_record() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    let lib = ws.join(LIB);
    let earlier = tip(&project, "HEAD");
    let lib_pin = tip(&lib, "HEAD");
    commit_in(&project, "later.txt");
    let records_before = tip(&project, "rwv-records");
    commit_in(&lib, "lib-later.txt");

    succeed(&["switch", "--detach", &earlier], ws);

    assert_eq!(head_branch(&project), None);
    assert_eq!(tip(&project, "HEAD"), earlier);
    assert_eq!(head_branch(&lib), None);
    assert_eq!(tip(&lib, "HEAD"), lib_pin);
    assert_eq!(tip(&project, "rwv-records"), records_before);
    assert!(workspace_state(&project).contains("detached"));
    succeed(&["update"], ws);
    assert_eq!(head_branch(&project), None);
}

#[test]
fn switching_from_a_detached_project_repo_keeps_what_it_held_and_ends_on_a_branch() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    succeed(&["switch", "--detach", "HEAD"], ws);
    let stranded = commit_in(&project, "stranded.txt");

    succeed(&["switch", "main"], ws);

    assert_eq!(head_branch(&project).as_deref(), Some("main"));
    let backups = succeed(&["backup", "list"], ws);
    assert!(backups.contains(&stranded[..7]), "{backups}");
}
