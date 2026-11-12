//! The verbs that give or move a pin, each against a real clone with a real
//! remote: what `rwv lock` refuses and when, which verbs give a first pin and
//! to whom, and what the pruning fetch says about a pin no remote holds.

use std::path::{Path, PathBuf};

mod common;

use common::{git_in, rwv, PushWorkspace};

const APP: &str = "github/acme/app";
const LIB: &str = "github/acme/lib";

fn two_members() -> PushWorkspace {
    common::build_workspace("proj", &[(APP, "owned"), (LIB, "owned")])
}

fn member(w: &PushWorkspace, repo: &str) -> PathBuf {
    w.workspace.join(repo)
}

fn project(w: &PushWorkspace) -> PathBuf {
    w.workspace.join("projects").join(&w.project_name)
}

fn publish(dir: &Path) {
    git_in(dir, &["push", "-q", "origin", "main"]);
}

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    git_in(dir, &["add", file]);
    git_in(dir, &["commit", "-qm", file]);
    git_in(dir, &["rev-parse", "HEAD"])
}

fn lock_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("rwv.lock")).expect("the project has a lock")
}

fn pin_of(dir: &Path, repo: &str) -> Option<String> {
    let lock: serde_json::Value = serde_json::from_str(&lock_text(dir)).unwrap();
    lock["repositories"][repo]["version"]
        .as_str()
        .map(str::to_owned)
}

fn run(w: &PushWorkspace, args: &[&str]) -> std::process::Output {
    rwv()
        .args(args)
        .current_dir(&w.workspace)
        .output()
        .expect("rwv runs")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn succeeded(out: &std::process::Output) -> String {
    let text = stderr_of(out);
    assert!(out.status.success(), "expected success:\n{text}");
    text
}

fn refused(out: &std::process::Output) -> String {
    let text = stderr_of(out);
    assert!(!out.status.success(), "expected a refusal:\n{text}");
    text
}

fn both_behind_their_pins() -> PushWorkspace {
    let w = two_members();
    commit_in(&member(&w, LIB), "lib-one");
    commit_in(&member(&w, APP), "app-one");
    publish(&member(&w, LIB));
    publish(&member(&w, APP));
    succeeded(&run(&w, &["lock"]));
    git_in(member(&w, LIB), &["reset", "-q", "--hard", "HEAD~1"]);
    git_in(member(&w, APP), &["reset", "-q", "--hard", "HEAD~1"]);
    w
}

#[test]
fn plain_lock_refuses_everything_it_finds_before_it_writes_a_pin() {
    let w = two_members();
    commit_in(&member(&w, LIB), "lib-one");
    publish(&member(&w, LIB));
    succeeded(&run(&w, &["lock"]));
    git_in(member(&w, LIB), &["reset", "-q", "--hard", "HEAD~1"]);
    commit_in(&member(&w, APP), "app-one");
    let before = lock_text(&project(&w));

    let text = refused(&run(&w, &["lock"]));
    assert!(text.contains("github/acme/lib: pin backwards"), "{text}");
    assert_eq!(
        lock_text(&project(&w)),
        before,
        "the refusal came after app's forward pin was already written"
    );

    std::fs::write(project(&w).join("README"), "edited\n").unwrap();
    let text = refused(&run(&w, &["lock", "--commit"]));
    assert!(text.contains("github/acme/lib: pin backwards"), "{text}");
    assert!(!text.contains("projects/proj: edits"), "{text}");
    assert_eq!(lock_text(&project(&w)), before);
}

#[test]
fn lock_repo_moves_no_pin_backward_and_member_pin_moves_only_the_named_one() {
    let w = both_behind_their_pins();
    let app_pin = pin_of(&project(&w), APP).unwrap();
    let lib_pin = pin_of(&project(&w), LIB).unwrap();
    let lib_head = git_in(member(&w, LIB), &["rev-parse", "HEAD"]);
    assert_ne!(lib_pin, lib_head);

    let text = refused(&run(&w, &["lock", "--repo", LIB]));
    assert!(text.contains("github/acme/lib: pin backwards"), "{text}");
    assert_eq!(pin_of(&project(&w), LIB).unwrap(), lib_pin);

    let text = succeeded(&run(&w, &["member", "pin", LIB, "HEAD"]));
    assert!(text.contains("re-pinned"), "{text}");
    assert_eq!(pin_of(&project(&w), LIB).unwrap(), lib_head);
    assert_eq!(
        pin_of(&project(&w), APP).unwrap(),
        app_pin,
        "naming lib moved app's pin too"
    );

    let text = refused(&run(&w, &["lock"]));
    assert!(text.contains("github/acme/app: pin backwards"), "{text}");
    assert!(!text.contains("github/acme/lib: pin"), "{text}");
}

#[test]
fn plain_lock_gives_a_member_with_no_pin_none() {
    let w = two_members();
    common::fixture_lock(&project(&w), &[]);
    commit_in(&member(&w, APP), "app-one");

    let text = succeeded(&run(&w, &["lock"]));
    assert!(text.contains("github/acme/app: pin none"), "{text}");
    assert_eq!(pin_of(&project(&w), APP), None);
    assert_eq!(pin_of(&project(&w), LIB), None);
}

fn assert_committed_with_its_first_pin(w: &PushWorkspace, repo: &str, clone: &Path, before: usize) {
    let dir = project(w);
    let after: usize = git_in(&dir, &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap();
    assert_eq!(after, before + 1, "the add is one commit");
    assert_eq!(
        files_of_head(&dir),
        ["proj.code-workspace", "rwv.lock", "rwv.toml"],
        "the manifest, what the integrations regenerate from it, and the lock"
    );
    assert_eq!(
        pin_of(&dir, repo).as_deref(),
        Some(git_in(clone, &["rev-parse", "HEAD"]).as_str()),
        "the commit holds the pin of the commit the clone is at"
    );
    assert_eq!(git_in(&dir, &["status", "--porcelain"]), "");
}

fn commits(w: &PushWorkspace) -> usize {
    git_in(project(w), &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap()
}

fn upstream(w: &PushWorkspace, owner_repo: &str) -> PathBuf {
    let bare = w._tmp.path().join(owner_repo);
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    bare
}

fn files_of_head(dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = git_in(dir, &["show", "--name-only", "--format=", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect();
    files.sort_unstable();
    files
}

#[test]
fn member_add_by_url_commits_the_entry_with_the_commit_it_cloned() {
    let w = common::build_workspace("proj", &[]);
    let bare = upstream(&w, "acme/added.git");
    let before = commits(&w);

    succeeded(&run(&w, &["member", "add", &common::file_url(&bare)]));
    assert_committed_with_its_first_pin(
        &w,
        "local/acme/added",
        &member(&w, "local/acme/added"),
        before,
    );
}

#[test]
fn member_add_adopt_commits_the_entry_with_the_commit_the_clone_is_at() {
    let w = common::build_workspace("proj", &[]);
    let bare = upstream(&w, "acme/adopted.git");
    let clone = member(&w, "local/acme/adopted");
    std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
    git_in(
        clone.parent().unwrap(),
        &["clone", "-q", &common::file_url(&bare), "adopted"],
    );
    git_in(&clone, &["remote", "set-head", "origin", "main"]);
    let before = commits(&w);

    succeeded(&run(
        &w,
        &["member", "add", "--adopt", "local/acme/adopted"],
    ));
    assert_committed_with_its_first_pin(&w, "local/acme/adopted", &clone, before);
}

fn add_new(w: &PushWorkspace, lab: &Path) -> std::process::Output {
    run(
        w,
        &[
            "member",
            "add",
            "local",
            "--new",
            "--param",
            &format!("root={}", lab.display()),
            "--param",
            "owner=acme",
            "--param",
            "repo=fresh",
        ],
    )
}

#[test]
fn member_add_new_over_an_upstream_with_commits_pins_the_commit_it_cloned() {
    let w = common::build_workspace("proj", &[]);
    let lab = w._tmp.path().join("lab");
    std::fs::create_dir_all(lab.join("acme")).unwrap();
    common::init_bare_repo_with_commit(&lab.join("acme/fresh"));
    let before = commits(&w);

    succeeded(&add_new(&w, &lab));
    assert_committed_with_its_first_pin(
        &w,
        "local/acme/fresh",
        &member(&w, "local/acme/fresh"),
        before,
    );
}

#[test]
fn member_add_new_of_an_empty_upstream_commits_the_entry_and_says_it_has_no_pin() {
    let w = common::build_workspace("proj", &[]);
    let lab = w._tmp.path().join("lab");
    std::fs::create_dir_all(&lab).unwrap();
    let before = commits(&w);

    let text = succeeded(&add_new(&w, &lab));
    assert!(text.contains("local/acme/fresh: pin none"), "{text}");
    assert_eq!(commits(&w), before + 1);
    assert_eq!(pin_of(&project(&w), "local/acme/fresh"), None);
    assert!(std::fs::read_to_string(project(&w).join("rwv.toml"))
        .unwrap()
        .contains("local/acme/fresh"));
    assert_eq!(git_in(project(&w), &["status", "--porcelain"]), "");
}

fn advance_remote(w: &PushWorkspace, repo: &str, file: &str) -> String {
    let bare = &w
        .manifest_bares
        .iter()
        .find(|(path, _)| path == repo)
        .expect("the repo has a remote")
        .1;
    let work = w._tmp.path().join(format!("work-{file}"));
    git_in(
        w._tmp.path(),
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    let tip = commit_in(&work, file);
    git_in(&work, &["push", "-q", "origin", "main"]);
    tip
}

fn pinless(w: &PushWorkspace) {
    common::fixture_lock(&project(w), &[]);
    git_in(project(w), &["add", "rwv.lock"]);
    git_in(project(w), &["commit", "-qm", "no pins"]);
}

#[test]
fn update_remote_gives_a_pinless_member_its_own_upstream_tip_and_no_other() {
    let w = two_members();
    pinless(&w);
    let app_tip = advance_remote(&w, APP, "app-upstream");
    advance_remote(&w, LIB, "lib-upstream");

    succeeded(&run(&w, &["update", "--remote", "--repo", APP]));

    assert_eq!(pin_of(&project(&w), APP).as_deref(), Some(app_tip.as_str()));
    assert_eq!(git_in(member(&w, APP), &["rev-parse", "HEAD"]), app_tip);
    assert_eq!(
        pin_of(&project(&w), LIB),
        None,
        "lib was not named and has no pin to be given"
    );
}

#[test]
fn update_remote_without_a_selector_gives_every_pinless_member_its_upstream_tip() {
    let w = two_members();
    pinless(&w);
    let app_tip = advance_remote(&w, APP, "app-upstream");
    let lib_tip = advance_remote(&w, LIB, "lib-upstream");

    succeeded(&run(&w, &["update", "--remote"]));

    assert_eq!(pin_of(&project(&w), APP).as_deref(), Some(app_tip.as_str()));
    assert_eq!(pin_of(&project(&w), LIB).as_deref(), Some(lib_tip.as_str()));
}

#[test]
fn the_pruning_fetch_reports_a_pin_off_the_remote_and_moves_nothing() {
    let w = two_members();
    let app = member(&w, APP);
    let published = commit_in(&app, "published");
    git_in(&app, &["push", "-q", "origin", "main"]);
    succeeded(&run(&w, &["lock", "--commit"]));
    let bare = &w.manifest_bares[0].1;
    git_in(
        bare,
        &["update-ref", "refs/heads/main", &format!("{published}~1")],
    );

    let heads = |dir: &Path| {
        git_in(
            dir,
            &[
                "for-each-ref",
                "refs/heads",
                "--format=%(refname) %(objectname)",
            ],
        )
    };
    let (head, branches, lock) = (
        git_in(&app, &["rev-parse", "HEAD"]),
        heads(&app),
        lock_text(&project(&w)),
    );

    let out = run(&w, &["fetch"]);
    succeeded(&out);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("github/acme/app: pin off remote"), "{text}");
    assert!(text.contains(&published[..12]), "{text}");
    assert!(text.contains("proj main"), "{text}");

    assert_eq!(git_in(&app, &["rev-parse", "HEAD"]), head);
    assert_eq!(heads(&app), branches);
    assert_eq!(lock_text(&project(&w)), lock);
    assert_eq!(git_in(project(&w), &["status", "--porcelain"]), "");
}

#[test]
fn member_pin_records_a_commit_by_name_in_either_direction() {
    let w = two_members();
    let app = member(&w, APP);
    let first = git_in(&app, &["rev-parse", "HEAD"]);
    let second = commit_in(&app, "app-one");
    git_in(&app, &["push", "-q", "origin", "main"]);

    let text = succeeded(&run(&w, &["member", "pin", APP, "HEAD", "--commit"]));
    assert!(
        text.contains(&format!("pinned at {}", &second[..12])),
        "{text}"
    );
    assert_eq!(pin_of(&project(&w), APP).as_deref(), Some(second.as_str()));

    let text = succeeded(&run(&w, &["member", "pin", APP, &first, "--commit"]));
    assert!(text.contains("by name"), "{text}");
    assert_eq!(pin_of(&project(&w), APP).as_deref(), Some(first.as_str()));
    assert_eq!(git_in(project(&w), &["status", "--porcelain"]), "");
    assert_eq!(files_of_head(&project(&w)), ["rwv.lock"]);
}

#[test]
fn member_pin_refuses_before_it_writes_or_moves_anything() {
    let w = two_members();
    let app = member(&w, APP);
    git_in(&app, &["switch", "-q", "-c", "side"]);
    let off_branch = commit_in(&app, "side-only");
    git_in(&app, &["switch", "-q", "main"]);
    let before = (
        lock_text(&project(&w)),
        git_in(&app, &["rev-parse", "HEAD"]),
    );

    let text = refused(&run(&w, &["member", "pin", APP, &off_branch]));
    assert!(
        text.contains("github/acme/app: pin not on declared branch"),
        "{text}"
    );
    let text = refused(&run(&w, &["member", "pin", APP, "no-such-rev"]));
    assert!(text.contains("name no-such-branch"), "{text}");
    let text = refused(&run(&w, &["member", "pin", "github/acme/other", "HEAD"]));
    assert!(text.contains("not found in manifest"), "{text}");

    assert_eq!(
        (
            lock_text(&project(&w)),
            git_in(&app, &["rev-parse", "HEAD"])
        ),
        before
    );
}
