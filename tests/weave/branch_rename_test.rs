//! `rwv branch rename`, driven through the shipped binary against weaves with
//! bare remotes.
//!
//! What must hold, each pinned below by the test that names it:
//!
//! - a feature's rename moves its project branch, its owned members' branches,
//!   its record, its stored entries, every record that lands into it and the
//!   workspace's aligned branch, and leaves nothing under the old name;
//! - a member branch some live weave branch declares is protected: the rename
//!   refuses and changes nothing;
//! - a declared-mode project branch is renamed: it is protected only against
//!   rewrites;
//! - the new name is refused where it exists, locally or on a remote, unless
//!   `--adopt` takes up the remote's, which refuses a diverged one;
//! - a minted name and a gone project push branch print the rename.

use std::path::{Path, PathBuf};

use crate::common;

use repoweave::diagnosis::State;

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

fn assert_routes_to(refusal: &str, token: &str) {
    let routes: Vec<&str> = refusal
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, [token], "the refusal was:\n{refusal}");
}

fn minted(name: &str) -> String {
    format!("{PROJECT}--{name}")
}

fn create_workweave(ws: &Path, name: &str) -> PathBuf {
    succeed(&["workweave", "create", "--project", PROJECT, name], ws);
    ws.parent().unwrap().join(".workweaves").join(minted(name))
}

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "."]);
    common::git_in(dir, &["commit", "-qm", file]);
    common::git_in(dir, &["rev-parse", "HEAD"])
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

fn record_of(ws: &Path, branch: &str) -> Option<String> {
    let out = common::git()
        .args([
            "show",
            &format!("rwv-records:{}", common::records::path(branch)),
        ])
        .current_dir(project_dir(ws))
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap())
}

fn upstream_of(repo: &Path, branch: &str) -> Option<String> {
    let out = common::git()
        .args(["config", "--get", &format!("branch.{branch}.merge")])
        .current_dir(repo)
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap().trim().to_owned())
}

fn workspace_state(project: &Path) -> String {
    let dir = common::git_in(project, &["rev-parse", "--absolute-git-dir"]);
    std::fs::read_to_string(Path::new(&dir).join("rwv-workspace-state")).unwrap_or_default()
}

#[test]
fn a_feature_rename_moves_the_branches_the_record_the_dependents_and_the_aligned_branch() {
    let w = weave(&[LIB, APP]);
    let ww = create_workweave(&w.workspace, "anon");
    let old = minted("anon");
    let work = commit_in(&ww.join(LIB), "lib.txt");
    common::git_in(project_dir(&w.workspace), &["branch", "stacked", &old]);
    succeed(
        &["branch", "set-mode", "stacked", "feature", "--target", &old],
        &ww,
    );
    succeed(&["update"], &ww);
    assert!(workspace_state(&project_dir(&ww)).contains(&old));

    let out = succeed(&["branch", "rename", "feat-x"], &ww);

    for repo in [project_dir(&ww), ww.join(LIB), ww.join(APP)] {
        assert!(!has_branch(&repo, &old), "{} keeps {old}", repo.display());
        assert!(has_branch(&repo, "feat-x"), "{}", repo.display());
    }
    assert_eq!(common::git_in(ww.join(LIB), &["rev-parse", "feat-x"]), work);
    assert_eq!(record_of(&w.workspace, &old), None, "{}", describe(&out));
    assert!(record_of(&w.workspace, "feat-x")
        .unwrap()
        .contains("target = \"main\""));
    assert!(record_of(&w.workspace, "stacked")
        .unwrap()
        .contains("target = \"feat-x\""));
    let state = workspace_state(&project_dir(&ww));
    assert!(state.contains("feat-x") && !state.contains(&old), "{state}");
    assert_eq!(
        common::git_in(project_dir(&ww), &["branch", "--show-current"]),
        "feat-x"
    );
}

fn declare_lib_on(w: &common::PushWorkspace, line: &str, branch: &str) {
    let project = project_dir(&w.workspace);
    common::git_in(&project, &["switch", "-qc", line]);
    let manifest = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    let lib_entry = manifest.find(LIB).unwrap();
    let (head, tail) = manifest.split_at(lib_entry);
    let tail = tail.replacen("branch = \"main\"", &format!("branch = \"{branch}\""), 1);
    std::fs::write(project.join("rwv.toml"), format!("{head}{tail}")).unwrap();
    common::git_in(&project, &["commit", "-qam", "declare"]);
    common::git_in(&project, &["switch", "-q", "main"]);
    succeed(&["branch", "set-mode", line, "declared"], &w.workspace);
}

#[test]
fn a_member_branch_a_live_weave_branch_declares_protects_the_rename_and_nothing_changes() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "anon");
    let old = minted("anon");
    declare_lib_on(&w, "line", &old);
    let record_before = record_of(&w.workspace, &old);

    let stderr = refuse(&["branch", "rename", "feat-x"], &ww);

    assert!(
        stderr.contains("protected declared") && stderr.contains("line (projects/p)"),
        "{stderr}"
    );
    assert_routes_to(&stderr, "protected");
    assert!(has_branch(&ww.join(LIB), &old));
    assert!(!has_branch(&ww.join(LIB), "feat-x"));
    assert!(has_branch(&project_dir(&ww), &old));
    assert!(!has_branch(&project_dir(&ww), "feat-x"));
    assert_eq!(record_of(&w.workspace, &old), record_before);
    assert_eq!(record_of(&w.workspace, "feat-x"), None);
}

#[test]
fn a_declared_mode_project_branch_is_renamed_leaving_its_members_alone() {
    let w = weave(&[LIB]);
    common::git_in(project_dir(&w.workspace), &["branch", "stacked", "main"]);
    succeed(
        &[
            "branch", "set-mode", "stacked", "feature", "--target", "main",
        ],
        &w.workspace,
    );

    let out = succeed(&["branch", "rename", "trunk"], &w.workspace);

    assert!(has_branch(&project_dir(&w.workspace), "trunk"));
    assert!(!has_branch(&project_dir(&w.workspace), "main"));
    assert!(has_branch(&w.workspace.join(LIB), "main"), "members stay");
    assert!(!has_branch(&w.workspace.join(LIB), "trunk"));
    assert_eq!(record_of(&w.workspace, "main"), None);
    assert!(record_of(&w.workspace, "trunk")
        .unwrap()
        .contains("declared"));
    assert!(record_of(&w.workspace, "stacked")
        .unwrap()
        .contains("target = \"trunk\""));
    let text = describe(&out);
    assert!(
        text.contains("left: projects/p main: origin still has it")
            && text.contains("rwv branch delete main --target trunk --delete-remote"),
        "{text}"
    );
}

#[test]
fn a_new_name_that_exists_locally_is_refused_and_nothing_changes() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "anon");
    let old = minted("anon");
    common::git_in(project_dir(&ww), &["branch", "taken"]);

    let refusal = refuse(&["branch", "rename", "taken"], &ww);

    assert!(refusal.contains("name exists (taken)"), "{refusal}");
    assert!(has_branch(&project_dir(&ww), &old));
    assert!(has_branch(&ww.join(LIB), &old));
    assert!(record_of(&w.workspace, &old).is_some());
}

fn publish_project_branch(w: &common::PushWorkspace, name: &str, with_commit: bool) {
    let project = project_dir(&w.workspace);
    common::git_in(&project, &["switch", "-qc", &format!("tmp-{name}")]);
    if with_commit {
        commit_in(&project, &format!("{name}.txt"));
    }
    common::git_in(
        &project,
        &["push", "-q", "origin", &format!("tmp-{name}:{name}")],
    );
    common::git_in(&project, &["switch", "-q", "main"]);
    common::git_in(&project, &["branch", "-qD", &format!("tmp-{name}")]);
    common::git_in(&project, &["fetch", "-q", "origin"]);
}

#[test]
fn a_new_name_a_remote_has_is_refused_without_adopt_and_taken_up_with_it() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "anon");
    let old = minted("anon");
    publish_project_branch(&w, "forge-name", false);

    let refusal = refuse(&["branch", "rename", "forge-name"], &ww);
    assert!(
        refusal.contains("name exists (forge-name)")
            && refusal.contains("rwv branch rename <new> --adopt"),
        "{refusal}"
    );
    assert!(has_branch(&project_dir(&ww), &old));

    let out = succeed(&["branch", "rename", "forge-name", "--adopt"], &ww);

    assert!(
        has_branch(&project_dir(&ww), "forge-name"),
        "{}",
        describe(&out)
    );
    assert_eq!(
        upstream_of(&project_dir(&ww), "forge-name").as_deref(),
        Some("refs/heads/forge-name")
    );
    assert_eq!(upstream_of(&ww.join(LIB), "forge-name"), None);
}

#[test]
fn adopting_a_remote_branch_that_has_diverged_is_refused_and_nothing_changes() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "anon");
    let old = minted("anon");
    commit_in(&project_dir(&ww), "mine.txt");
    publish_project_branch(&w, "forge-name", true);

    let stderr = refuse(&["branch", "rename", "forge-name", "--adopt"], &ww);

    assert!(stderr.contains("push-branch diverged"), "{stderr}");
    assert!(stderr.contains("mine.txt"), "{stderr}");
    assert!(has_branch(&project_dir(&ww), &old));
    assert!(has_branch(&ww.join(LIB), &old));
    assert!(record_of(&w.workspace, &old).is_some());
}

#[test]
fn adopting_a_name_no_remote_has_is_refused() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "anon");

    let refusal = refuse(&["branch", "rename", "nowhere", "--adopt"], &ww);

    assert!(refusal.contains("name no-such-branch"), "{refusal}");
    assert!(has_branch(&project_dir(&ww), &minted("anon")));
}

#[test]
fn the_records_branch_name_is_reserved_and_an_unrecorded_branch_has_no_record_to_move() {
    let w = weave(&[LIB]);
    let reserved = refuse(&["branch", "rename", "rwv-records"], &w.workspace);
    assert!(reserved.contains("name reserved"), "{reserved}");

    common::git_in(project_dir(&w.workspace), &["switch", "-qc", "raw"]);
    let unrecorded = refuse(&["branch", "rename", "other"], &w.workspace);
    assert!(unrecorded.contains("target unrecorded"), "{unrecorded}");
    assert!(has_branch(&project_dir(&w.workspace), "raw"));
}

#[test]
fn a_rename_that_stopped_after_a_member_is_completed_by_running_it_again() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "anon");
    let old = minted("anon");
    common::git_in(ww.join(LIB), &["branch", "-m", &old, "feat-x"]);

    succeed(&["branch", "rename", "feat-x"], &ww);

    assert!(has_branch(&project_dir(&ww), "feat-x"));
    assert!(has_branch(&ww.join(LIB), "feat-x"));
    assert!(record_of(&w.workspace, "feat-x").is_some());
    assert_eq!(record_of(&w.workspace, &old), None);
}

#[test]
fn push_of_a_minted_name_offers_the_rename() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "anon");

    let refusal = refuse(&["push"], &ww);

    assert!(refusal.contains("name minted"), "{refusal}");
    assert!(refusal.contains("rwv branch rename <new>"), "{refusal}");
}

#[test]
fn a_gone_project_push_branch_offers_the_rename_that_adopts() {
    let w = weave(&[LIB]);
    let project = project_dir(&w.workspace);
    common::git_in(&project, &["switch", "-qc", "feat"]);
    succeed(
        &["branch", "set-mode", "feat", "feature", "--target", "main"],
        &w.workspace,
    );
    commit_in(&project, "work.txt");
    common::git_in(&project, &["push", "-q", "-u", "origin", "feat"]);
    common::git_in(&project, &["push", "-q", "origin", "--delete", "feat"]);
    common::git_in(&project, &["fetch", "-q", "--prune", "origin"]);
    succeed(&["update"], &w.workspace);
    succeed(&["lock", "--commit"], &w.workspace);

    let out = run(&["push"], &w.workspace);

    let text = describe(&out);
    assert!(text.contains("push-branch gone"), "{text}");
    assert!(text.contains("rwv branch rename <new> --adopt"), "{text}");
}

#[test]
fn a_declared_mode_project_branch_is_protected_against_an_overwrite_and_an_unlanded_remote_delete()
{
    let w = weave(&[LIB]);
    common::git_in(project_dir(&w.workspace), &["push", "-q", "origin", "main"]);

    let stderr = refuse(&["push", "--overwrite", "projects/p"], &w.workspace);
    assert!(
        stderr.contains("protected declared mode (main)"),
        "{stderr}"
    );
    assert_routes_to(&stderr, "protected");

    common::git_in(project_dir(&w.workspace), &["branch", "side"]);
    succeed(&["branch", "set-mode", "side", "declared"], &w.workspace);
    let stderr = refuse(
        &[
            "branch",
            "delete",
            "main",
            "--target",
            "side",
            "--discard-unlanded",
            "--delete-remote-unlanded",
            "projects/p",
        ],
        &w.workspace,
    );
    assert!(
        stderr.contains("protected declared mode (main)"),
        "{stderr}"
    );
    assert_routes_to(&stderr, "protected");
}

fn record_without_branch(w: &common::PushWorkspace, name: &str, mode: &[&str]) {
    let project = project_dir(&w.workspace);
    common::git_in(&project, &["branch", name]);
    let mut args = vec!["branch", "set-mode", name];
    args.extend_from_slice(mode);
    succeed(&args, &w.workspace);
    common::git_in(&project, &["branch", "-D", name]);
}

#[test]
fn a_new_name_holding_a_record_of_another_value_is_refused_and_nothing_changes() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "anon");
    let old = minted("anon");
    record_without_branch(&w, "held", &["declared"]);
    let before = record_of(&w.workspace, "held");

    let stderr = refuse(&["branch", "rename", "held"], &ww);

    assert!(stderr.contains("name exists (held)"), "{stderr}");
    assert!(has_branch(&project_dir(&ww), &old));
    assert!(has_branch(&ww.join(LIB), &old));
    assert!(record_of(&w.workspace, &old).is_some());
    assert_eq!(record_of(&w.workspace, "held"), before);

    publish_project_branch(&w, "held", false);
    let stderr = refuse(&["branch", "rename", "held", "--adopt"], &ww);
    assert!(stderr.contains("target differs (held)"), "{stderr}");
    common::cli::refused(
        &run(&["branch", "rename", "held", "--adopt"], &ww),
        State::TargetDiffers,
    );
    assert!(
        stderr.contains("declared") && stderr.contains("feature onto main"),
        "{stderr}"
    );
    assert!(has_branch(&project_dir(&ww), &old));
    assert_eq!(record_of(&w.workspace, "held"), before);
}

#[test]
fn a_new_name_holding_the_same_record_is_taken_up_with_adopt() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "anon");
    record_without_branch(&w, "held", &["feature", "--target", "main"]);
    publish_project_branch(&w, "held", false);

    succeed(&["branch", "rename", "held", "--adopt"], &ww);

    assert!(has_branch(&project_dir(&ww), "held"));
    assert_eq!(record_of(&w.workspace, &minted("anon")), None);
}

#[test]
fn a_rename_reports_a_workspace_state_that_reads_as_absent() {
    let w = weave(&[LIB]);
    let project = project_dir(&w.workspace);
    let dir = common::git_in(&project, &["rev-parse", "--absolute-git-dir"]);
    let state = Path::new(&dir).join("rwv-workspace-state");
    std::fs::write(&state, "{ not json").unwrap();

    let out = succeed(&["branch", "rename", "trunk"], &w.workspace);

    let text = describe(&out);
    assert!(
        text.contains("workspace state of projects/p reads as absent"),
        "{text}"
    );
}

#[test]
fn a_rename_that_would_close_a_target_cycle_is_refused_before_any_branch_moves() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    succeed(&["branch", "create", "old"], ws);
    common::git_in(&project, &["branch", "x", "main"]);
    common::records::edit_by_hand(
        &project,
        &[
            (
                "old",
                "mode = \"feature\"\ntarget = \"x\"\nas = \"feature\"\n",
            ),
            (
                "x",
                "mode = \"feature\"\ntarget = \"new\"\nas = \"declared\"\n",
            ),
        ],
        &[],
        "a target that is gone",
    );

    common::cli::refused(&run(&["branch", "rename", "new"], ws), State::TargetCycle);

    assert!(has_branch(&ws.join(LIB), "old") && !has_branch(&ws.join(LIB), "new"));
    assert!(has_branch(&project, "old") && !has_branch(&project, "new"));
}

#[test]
fn a_never_recorded_branch_does_not_take_up_a_stale_record_of_the_new_name() {
    let w = weave(&[LIB]);
    let project = project_dir(&w.workspace);
    common::git_in(&project, &["switch", "-qc", "raw"]);
    common::records::edit_by_hand(&project, &[("new", "mode = \"declared\"\n")], &[], "stale");

    common::cli::refused(
        &run(&["branch", "rename", "new"], &w.workspace),
        State::TargetUnrecorded,
    );
    assert!(has_branch(&project, "raw") && !has_branch(&project, "new"));
}

#[test]
fn a_rename_that_stopped_after_its_record_moved_is_completed_by_running_it_again() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    succeed(&["branch", "create", "old"], ws);
    let git_dir = common::git_in(&project, &["rev-parse", "--git-common-dir"]);
    let lock = project.join(git_dir).join("refs/heads/new.lock");
    std::fs::write(&lock, "").unwrap();

    refuse(&["branch", "rename", "new"], ws);
    assert!(record_of(ws, "new").is_some() && record_of(ws, "old").is_none());
    assert!(has_branch(&project, "old"));

    std::fs::remove_file(&lock).unwrap();
    succeed(&["branch", "rename", "new"], ws);

    assert!(has_branch(&project, "new") && !has_branch(&project, "old"));
    assert!(has_branch(&ws.join(LIB), "new"));
    assert!(!workspace_state(&project).contains("renaming"));
}

#[test]
fn a_rename_from_a_detached_project_repo_is_the_detached_state_with_its_choices() {
    let w = weave(&[LIB]);
    let project = project_dir(&w.workspace);
    common::git_in(&project, &["switch", "-q", "--detach"]);

    let out = common::cli::refused(
        &run(&["branch", "rename", "new"], &w.workspace),
        State::AlignmentProjectDetached,
    );
    assert!(out.contains("rwv switch"), "{out}");
    assert_routes_to(&out, "alignment");
}

#[test]
fn a_member_branch_an_unrecorded_live_branch_declares_protects_the_rename() {
    let w = weave(&[LIB]);
    let ws = &w.workspace;
    let project = project_dir(ws);
    succeed(&["branch", "create", "x"], ws);
    common::git_in(&project, &["switch", "-q", "-c", "line", "main"]);
    let toml = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        toml.replace("branch = \"main\"", "branch = \"x\""),
    )
    .unwrap();
    common::git_in(&project, &["commit", "-qam", "line declares x, unrecorded"]);
    common::git_in(&project, &["switch", "-q", "x"]);

    common::cli::refused(
        &run(&["branch", "rename", "y"], ws),
        State::ProtectedDeclared,
    );
    assert!(has_branch(&ws.join(LIB), "x") && !has_branch(&ws.join(LIB), "y"));
}
