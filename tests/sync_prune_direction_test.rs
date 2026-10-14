//! What replay does with a member CWD's lock names and the source's lock does
//! not. It prunes the checkout only when the op ends with CWD's lock no longer
//! naming the member, and keeps one CWD added that the source has not taken
//! yet. Role plays no part.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const PROJECT: &str = "app";
const LIB: &str = "local/owner/lib";
const EXTRA: &str = "local/owner/extra";

struct Weave {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    extra_url: String,
}

fn project_dir(workspace: &Path) -> PathBuf {
    workspace.join("projects").join(PROJECT)
}

fn upstream_repo(upstream: &Path, name: &str) -> PathBuf {
    let repo = upstream.join("owner").join(name);
    std::fs::create_dir_all(&repo).unwrap();
    common::git_in(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "init\n").unwrap();
    common::git_in(&repo, &["add", "."]);
    common::git_in(&repo, &["commit", "-m", "initial"]);
    repo
}

/// A primary weave whose project names one owned member. A second upstream
/// repo, `extra`, is there for a seat to add.
fn weave() -> Weave {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    let upstream = tmp.path().join("upstream");
    let lib = upstream_repo(&upstream, "lib");
    let extra = upstream_repo(&upstream, "extra");
    let lib_clone = common::under(&root, LIB);
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &common::file_url(&lib),
            &lib_clone.to_string_lossy(),
        ],
    );

    let project = project_dir(&root);
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    std::fs::write(project.join(".gitignore"), "*.code-workspace\ngita/\n").unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::file_url(&lib)
        ),
    )
    .unwrap();
    std::fs::write(root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    succeed(&root, &["lock"]);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);

    Weave {
        _tmp: tmp,
        root,
        extra_url: common::file_url(&extra),
    }
}

fn run(dir: &Path, args: &[&str]) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn succeed(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "rwv {args:?} in {} failed:\nstdout:\n{}\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `rwv workweave create` run from `from`, which forks the seat from it.
fn seat(weave: &Weave, from: &Path, name: &str) -> PathBuf {
    succeed(from, &["workweave", PROJECT, "create", name]);
    weave
        .root
        .parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{PROJECT}--{name}"))
}

/// `rwv add`, `rwv lock`, then commit both files in the project repo.
fn add_and_commit(workspace: &Path, url: &str, role: &str) {
    succeed(workspace, &["add", url, "--role", role]);
    succeed(workspace, &["lock"]);
    let project = project_dir(workspace);
    common::git_in(&project, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&project, &["commit", "-m", "add extra"]);
}

fn names_extra(workspace: &Path) -> bool {
    let project = project_dir(workspace);
    ["rwv.toml", "rwv.lock"].iter().all(|file| {
        std::fs::read_to_string(project.join(file))
            .unwrap()
            .contains(EXTRA)
    })
}

fn assert_no_prune_line(stdout: &str) {
    assert!(
        !stdout.contains(&format!("{EXTRA}: pruned")),
        "replay pruned a member this side added:\n{stdout}"
    );
}

fn assert_not_pruned(stdout: &str, workspace: &Path) {
    assert_no_prune_line(stdout);
    assert!(
        common::under(workspace, EXTRA).is_dir(),
        "{EXTRA} must still be checked out in {}",
        workspace.display()
    );
}

#[test]
fn a_seat_that_adds_a_reference_member_lands_and_retires_with_it() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    add_and_commit(&s1, &weave.extra_url, "reference");

    let out = run(&s1, &["sync-to", "--retire"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_no_prune_line(&stdout);
    assert!(
        stdout.contains(&format!(
            "{EXTRA}: kept (added here; not in the source's lock yet)"
        )),
        "the replay must say it kept the member and why:\n{stdout}"
    );
    assert!(
        out.status.success(),
        "sync-to --retire must land and retire:\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!s1.exists(), "--retire must delete the seat");
    assert!(names_extra(&weave.root), "the target must name the member");
    assert!(
        common::under(&weave.root, EXTRA).is_dir(),
        "the target must keep its checkout of the member"
    );
}

/// The parent seat adds a member; a child forked before the add lands into
/// it, and then the parent lands into primary. Neither landing may take the
/// parent's checkout.
#[test]
fn a_parent_seat_keeps_a_member_it_added_through_its_childs_landing_and_its_own() {
    let weave = weave();
    let home = seat(&weave, &weave.root, "home");
    let child = seat(&weave, &home, "child");
    add_and_commit(&home, &weave.extra_url, "owned");

    let lib = common::under(&child, LIB);
    std::fs::write(lib.join("work.txt"), "work\n").unwrap();
    common::git_in(&lib, &["add", "work.txt"]);
    common::git_in(&lib, &["commit", "-m", "work"]);
    succeed(&child, &["lock", "--commit"]);

    let child_landing = succeed(&child, &["sync-to"]);
    assert_not_pruned(&child_landing, &home);

    let home_landing = succeed(&home, &["sync-to"]);
    assert_not_pruned(&home_landing, &home);
    assert!(
        names_extra(&home),
        "the parent seat must still name the member"
    );
    assert!(names_extra(&weave.root), "primary must name the member");
}

#[test]
fn a_pull_from_a_seat_forked_before_the_add_keeps_the_added_member() {
    let weave = weave();
    let home = seat(&weave, &weave.root, "home");
    let child = seat(&weave, &home, "child");
    add_and_commit(&home, &weave.extra_url, "owned");

    let child_dir = child.to_string_lossy().into_owned();
    let stdout = succeed(&home, &["sync", &child_dir, "--strategy", "rebase"]);
    assert_not_pruned(&stdout, &home);
    assert!(
        names_extra(&home),
        "the pulling seat must still name the member"
    );
}

#[test]
fn a_member_the_source_dropped_is_pruned() {
    let weave = weave();
    add_and_commit(&weave.root, &weave.extra_url, "owned");
    let s1 = seat(&weave, &weave.root, "s1");
    assert!(
        common::under(&s1, EXTRA).is_dir(),
        "the seat starts with the member"
    );

    succeed(&weave.root, &["remove", EXTRA]);
    succeed(&weave.root, &["lock"]);
    let project = project_dir(&weave.root);
    common::git_in(&project, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&project, &["commit", "-m", "drop extra"]);

    let stdout = succeed(&s1, &["sync", "primary"]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "a member the source dropped must be pruned:\n{stdout}"
    );
    assert!(
        !common::under(&s1, EXTRA).exists(),
        "the seat's checkout of the dropped member must be gone"
    );
}

#[test]
fn discarding_local_commits_prunes_the_member_they_added() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    add_and_commit(&s1, &weave.extra_url, "owned");

    let stdout = succeed(&s1, &["sync", "primary", "--discard-local-commits"]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "a discarded add leaves the member in no lock, so its checkout goes:\n{stdout}"
    );
    assert!(!common::under(&s1, EXTRA).exists());
    assert!(!names_extra(&s1) && !names_extra(&weave.root));
}

/// With no commit in common there is no lock both sides shared, so nothing
/// shows the source dropped the member. Replaying CWD's whole history onto
/// the source keeps CWD's member in CWD's lock, so the checkout stays.
#[test]
fn a_seat_with_no_history_in_common_with_its_target_keeps_its_members() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    let project = project_dir(&s1);
    let root = common::git_in(
        &project,
        &["commit-tree", "HEAD^{tree}", "-m", "unrelated root"],
    );
    common::git_in(&project, &["reset", "--soft", &root]);
    add_and_commit(&s1, &weave.extra_url, "owned");

    let stdout = succeed(&s1, &["sync-to"]);
    assert_not_pruned(&stdout, &s1);
    assert!(
        stdout.contains(&format!(
            "{EXTRA}: kept (not in the source's lock, and nothing shows the source dropped \
             it: the two project histories share no commit)"
        )),
        "the replay must say it kept the member and why:\n{stdout}"
    );
    assert!(names_extra(&weave.root), "the target must name the member");
}
