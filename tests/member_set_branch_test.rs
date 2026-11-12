//! `rwv member set-branch`: both meanings of the branch an entry declares, the
//! commit that carries the entry and its pin, and what the verb refuses before
//! it changes anything.

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

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    git_in(dir, &["add", file]);
    git_in(dir, &["commit", "-qm", file]);
    git_in(dir, &["rev-parse", "HEAD"])
}

fn pin_of(w: &PushWorkspace, repo: &str) -> Option<String> {
    let text = std::fs::read_to_string(project(w).join("rwv.lock")).unwrap();
    let lock: serde_json::Value = serde_json::from_str(&text).unwrap();
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

fn succeeded(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "expected success:\n{text}");
    text
}

fn refused(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "expected a refusal:\n{text}");
    text
}

fn commits(w: &PushWorkspace) -> usize {
    git_in(project(w), &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap()
}

fn files_of_head(dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = git_in(dir, &["show", "--name-only", "--format=", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect();
    files.sort_unstable();
    files
}

fn manifest_text(w: &PushWorkspace) -> String {
    std::fs::read_to_string(project(w).join("rwv.toml")).unwrap()
}

fn branch_of_entry(w: &PushWorkspace, repo: &str) -> String {
    let manifest: toml::Value = toml::from_str(&manifest_text(w)).unwrap();
    manifest["repositories"][repo]["branch"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn committed_manifest(w: &PushWorkspace) -> String {
    git_in(project(w), &["show", "HEAD:rwv.toml"])
}

fn publish_branch(dir: &Path, branch: &str) {
    git_in(dir, &["push", "-q", "origin", branch]);
}

fn lib_with_dev(w: &PushWorkspace) -> String {
    let lib = member(w, LIB);
    git_in(&lib, &["switch", "-q", "-c", "dev"]);
    let tip = commit_in(&lib, "dev-one");
    publish_branch(&lib, "dev");
    git_in(&lib, &["switch", "-q", "main"]);
    tip
}

#[test]
fn set_branch_declares_an_existing_branch_pins_its_tip_and_commits_the_entry_and_the_pin() {
    let w = two_members();
    let tip = lib_with_dev(&w);
    let before = commits(&w);

    let text = succeeded(&run(&w, &["member", "set-branch", LIB, "dev"]));

    assert_eq!(branch_of_entry(&w, LIB), "dev", "{text}");
    assert_eq!(pin_of(&w, LIB).as_deref(), Some(tip.as_str()));
    common::assert_on_branch(&member(&w, LIB), "dev");
    assert_eq!(commits(&w), before + 1, "the setter is one commit");
    assert_eq!(
        files_of_head(&project(&w)),
        ["proj.code-workspace", "rwv.lock", "rwv.toml"],
        "the entry, the pin, and what the integrations regenerate from the entry"
    );
    assert_eq!(git_in(project(&w), &["status", "--porcelain"]), "");
}

#[test]
fn a_branch_that_exists_nowhere_is_refused_naming_the_fetch_and_the_new_spelling_and_nothing_changes(
) {
    let w = two_members();
    let before = (commits(&w), manifest_text(&w), pin_of(&w, LIB));

    let text = refused(&run(&w, &["member", "set-branch", LIB, "dve"]));

    assert!(text.contains("name no-such-branch (dve)"), "{text}");
    assert!(text.contains("rwv fetch"), "{text}");
    assert!(
        text.contains("rwv member set-branch") && text.contains("--new"),
        "{text}"
    );
    assert_eq!(
        (commits(&w), manifest_text(&w), pin_of(&w, LIB)),
        before,
        "the refusal came after something changed"
    );
    assert!(
        git_in(member(&w, LIB), &["branch", "--list", "dve"]).is_empty(),
        "the typo was created"
    );
}

#[test]
fn new_creates_the_branch_at_the_pin_and_pins_it_there() {
    let w = two_members();
    let pin = pin_of(&w, LIB).unwrap();
    let before = commits(&w);

    succeeded(&run(&w, &["member", "set-branch", LIB, "hold", "--new"]));

    assert_eq!(branch_of_entry(&w, LIB), "hold");
    assert_eq!(pin_of(&w, LIB).as_deref(), Some(pin.as_str()));
    assert_eq!(git_in(member(&w, LIB), &["rev-parse", "hold"]), pin);
    common::assert_on_branch(&member(&w, LIB), "hold");
    assert_eq!(commits(&w), before + 1);
}

#[test]
fn new_at_creates_the_branch_at_the_revision_and_pins_it_there() {
    let w = two_members();
    let lib = member(&w, LIB);
    let older = git_in(&lib, &["rev-parse", "HEAD"]);
    commit_in(&lib, "ahead");
    publish_branch(&lib, "main");

    succeeded(&run(
        &w,
        &["member", "set-branch", LIB, "hold", "--new", "--at", &older],
    ));

    assert_eq!(git_in(&lib, &["rev-parse", "hold"]), older);
    assert_eq!(pin_of(&w, LIB).as_deref(), Some(older.as_str()));
}

#[test]
fn new_on_a_branch_that_exists_is_refused_naming_the_spelling_without_it() {
    let w = two_members();
    lib_with_dev(&w);
    let before = (commits(&w), manifest_text(&w));

    let text = refused(&run(&w, &["member", "set-branch", LIB, "dev", "--new"]));

    assert!(text.contains("name exists (dev)"), "{text}");
    assert!(text.contains("rwv member set-branch"), "{text}");
    assert_eq!((commits(&w), manifest_text(&w)), before);
}

#[test]
fn at_a_commit_the_branch_does_not_reach_is_refused_naming_the_pin_choice() {
    let w = two_members();
    let lib = member(&w, LIB);
    lib_with_dev(&w);
    let on_main_only = commit_in(&lib, "main-only");
    let before = (commits(&w), manifest_text(&w));

    let text = refused(&run(
        &w,
        &["member", "set-branch", LIB, "dev", "--at", &on_main_only],
    ));

    assert!(text.contains("pin not on declared branch"), "{text}");
    assert!(text.contains("rwv member pin"), "{text}");
    assert_eq!((commits(&w), manifest_text(&w)), before);
}

#[test]
fn the_commit_holds_only_the_entry_and_the_pin_and_other_edits_to_the_file_stay() {
    let w = two_members();
    lib_with_dev(&w);
    let app_role_edit = manifest_text(&w).replace(
        &format!("branch = \"main\"\nrole = \"owned\"\n[repositories.\"{LIB}\"]"),
        &format!("branch = \"main\"\nrole = \"dependency\"\n[repositories.\"{LIB}\"]"),
    );
    assert_ne!(app_role_edit, manifest_text(&w), "the hand edit applies");
    std::fs::write(project(&w).join("rwv.toml"), &app_role_edit).unwrap();

    succeeded(&run(&w, &["member", "set-branch", LIB, "dev"]));

    let committed = committed_manifest(&w);
    assert!(committed.contains("branch = \"dev\""), "{committed}");
    assert!(
        !committed.contains("role = \"dependency\""),
        "the hand edit of app's role was committed:\n{committed}"
    );
    let on_disk = manifest_text(&w);
    assert!(on_disk.contains("role = \"dependency\""), "{on_disk}");
    assert!(on_disk.contains("branch = \"dev\""), "{on_disk}");
}

fn mirror_of_lib(w: &PushWorkspace) -> PathBuf {
    let bare = w._tmp.path().join("lib-mirror.git");
    git_in(
        w._tmp.path(),
        &[
            "clone",
            "-q",
            "--bare",
            w.manifest_bares
                .iter()
                .find(|(p, _)| p == LIB)
                .unwrap()
                .1
                .to_str()
                .unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git_in(
        member(w, LIB),
        &["remote", "add", "mirror", &common::file_url(&bare)],
    );
    git_in(member(w, LIB), &["fetch", "-q", "mirror"]);
    bare
}

#[test]
fn set_url_relinks_only_the_branch_the_member_sits_on_and_commits_the_entry_and_the_lock_url() {
    let w = two_members();
    let lib = member(&w, LIB);
    let bare = mirror_of_lib(&w);
    git_in(&lib, &["branch", "other", "--track", "origin/main"]);
    let before = commits(&w);

    let text = succeeded(&run(
        &w,
        &["member", "set-url", LIB, &common::file_url(&bare)],
    ));

    assert_eq!(
        git_in(&lib, &["config", "branch.main.remote"]),
        "mirror",
        "the branch the member sits on takes in from the new url's remote:\n{text}"
    );
    assert_eq!(
        git_in(&lib, &["config", "branch.other.remote"]),
        "origin",
        "a branch the member does not sit on was relinked"
    );
    assert!(manifest_text(&w).contains(&common::file_url(&bare)));
    assert_eq!(commits(&w), before + 1);
    let lock = std::fs::read_to_string(project(&w).join("rwv.lock")).unwrap();
    assert!(lock.contains(&common::file_url(&bare)), "{lock}");
    assert_eq!(git_in(project(&w), &["status", "--porcelain"]), "");
}

#[test]
fn set_url_leaves_a_branch_with_a_push_remote_of_its_own() {
    let w = two_members();
    let lib = member(&w, LIB);
    let bare = mirror_of_lib(&w);
    git_in(&lib, &["config", "branch.main.pushRemote", "origin"]);

    succeeded(&run(
        &w,
        &["member", "set-url", LIB, &common::file_url(&bare)],
    ));

    assert_eq!(git_in(&lib, &["config", "branch.main.remote"]), "origin");
}

#[test]
fn set_url_refuses_a_diverged_branch_and_changes_nothing() {
    let w = two_members();
    let lib = member(&w, LIB);
    let bare = mirror_of_lib(&w);
    let seed = w._tmp.path().join("seed");
    git_in(
        w._tmp.path(),
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            seed.to_str().unwrap(),
        ],
    );
    git_in(&seed, &["config", "user.email", "t@t"]);
    git_in(&seed, &["config", "user.name", "T"]);
    commit_in(&seed, "theirs");
    git_in(&seed, &["push", "-q", "origin", "main"]);
    git_in(&lib, &["fetch", "-q", "mirror"]);
    commit_in(&lib, "ours");
    let before = (commits(&w), manifest_text(&w));

    let text = refused(&run(
        &w,
        &["member", "set-url", LIB, &common::file_url(&bare)],
    ));

    assert!(text.contains("push-branch diverged"), "{text}");
    assert_eq!((commits(&w), manifest_text(&w)), before);
    assert_eq!(git_in(&lib, &["config", "branch.main.remote"]), "origin");
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn an_entry_change_is_reported_with_both_values_verb_after_verb_until_the_push_branch_carries_it() {
    let w = two_members();
    lib_with_dev(&w);
    succeeded(&run(&w, &["update"]));
    let lib = member(&w, LIB);
    commit_in(&lib, "main-unpushed");
    git_in(&lib, &["switch", "-q", "main"]);
    succeeded(&run(&w, &["member", "set-branch", LIB, "dev"]));
    git_in(&lib, &["switch", "-q", "dev"]);

    let update = succeeded(&run(&w, &["update"]));
    assert!(
        update.contains(&format!("{LIB}: member-changed branch: main -> dev")),
        "{update}"
    );
    assert!(update.contains("not yet published"), "{update}");
    assert!(update.contains("unpushed on main"), "{update}");
    assert!(update.contains("main-unpushed"), "{update}");
    let (keep, restore) = (
        update.find("rwv member pin").expect("C25 is printed"),
        update
            .find(&format!("rwv member set-branch {LIB} main"))
            .expect("C11 restores the previous value"),
    );
    assert!(keep < restore, "C25 is listed before C11:\n{update}");

    for verb in [&["update"][..], &["lock"][..]] {
        let again = succeeded(&run(&w, verb));
        assert!(
            again.contains("member-changed branch: main -> dev"),
            "{verb:?} stopped reporting before the change was published:\n{again}"
        );
    }
    let status = stdout_of(&run(&w, &["status"]));
    assert!(
        status.contains("member-changed branch: main -> dev"),
        "{status}"
    );

    git_in(&lib, &["push", "-q", "origin", "main"]);
    succeeded(&run(&w, &["lock", "--commit"]));
    succeeded(&run(&w, &["push"]));
    let after = succeeded(&run(&w, &["update"]));
    assert!(
        !after.contains("member-changed"),
        "still reported once the push branch carries it:\n{after}"
    );
}

#[test]
fn a_change_restored_is_no_longer_reported() {
    let w = two_members();
    lib_with_dev(&w);
    succeeded(&run(&w, &["update"]));
    succeeded(&run(&w, &["member", "set-branch", LIB, "dev"]));
    assert!(succeeded(&run(&w, &["update"])).contains("member-changed"));

    succeeded(&run(&w, &["member", "set-branch", LIB, "main"]));

    let after = succeeded(&run(&w, &["update"]));
    assert!(!after.contains("member-changed"), "{after}");
}

#[test]
fn a_url_change_a_raw_merge_carried_is_reported_and_relinks_at_the_next_aligning_verb() {
    let w = two_members();
    let lib = member(&w, LIB);
    let proj = project(&w);
    succeeded(&run(&w, &["update"]));
    let bare = mirror_of_lib(&w);
    let mirror = common::file_url(&bare);
    let original = w
        .manifest_bares
        .iter()
        .find(|(p, _)| p == LIB)
        .map(|(_, b)| common::file_url(b))
        .unwrap();

    git_in(&proj, &["switch", "-q", "-c", "mirror-url"]);
    let moved = manifest_text(&w).replace(&original, &mirror);
    std::fs::write(proj.join("rwv.toml"), moved).unwrap();
    git_in(&proj, &["commit", "-qam", "lib: mirror url"]);
    git_in(&proj, &["switch", "-q", "main"]);
    commit_in(&proj, "elsewhere");
    git_in(
        &proj,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "merge mirror-url",
            "mirror-url",
        ],
    );
    assert_eq!(git_in(&lib, &["config", "branch.main.remote"]), "origin");

    let update = succeeded(&run(&w, &["update"]));

    assert!(update.contains("member-changed url:"), "{update}");
    assert_eq!(
        git_in(&lib, &["config", "branch.main.remote"]),
        "mirror",
        "the merge carried the url change but nothing relinked:\n{update}"
    );
    let again = succeeded(&run(&w, &["update"]));
    assert!(
        again.contains("member-changed url:"),
        "reported until published:\n{again}"
    );
}

#[test]
fn placement_disagreement_compares_with_the_url_on_the_default_branch() {
    let w = two_members();
    let proj = project(&w);
    let on_default = manifest_text(&w).replace(
        &w.manifest_bares
            .iter()
            .find(|(p, _)| p == LIB)
            .map(|(_, b)| common::file_url(b))
            .unwrap(),
        "github/acme/lib",
    );
    std::fs::write(proj.join("rwv.toml"), on_default).unwrap();
    git_in(&proj, &["commit", "-qam", "lib by shorthand"]);
    git_in(&proj, &["push", "-q", "origin", "main"]);
    git_in(&proj, &["remote", "set-head", "origin", "main"]);
    git_in(&proj, &["switch", "-q", "-c", "stable"]);
    let moved = manifest_text(&w).replacen(
        "url = \"github/acme/lib\"",
        "url = \"github/mirror/lib\"",
        1,
    );
    assert_ne!(moved, manifest_text(&w));
    std::fs::write(proj.join("rwv.toml"), moved).unwrap();
    git_in(&proj, &["commit", "-qam", "stable: mirror url"]);
    common::record_feature(&proj, "stable", "main");

    let findings = common::doctor_findings(&w.workspace);

    assert!(
        !findings
            .iter()
            .any(|f| f.contains("placement-disagreement") && f.contains(LIB)),
        "a mirror url on another branch tripped it: {findings:?}"
    );
}

fn declare_by_hand(w: &PushWorkspace, repo: &str, branch: &str) {
    let mut manifest: toml::Value = toml::from_str(&manifest_text(w)).unwrap();
    manifest["repositories"][repo]["branch"] = toml::Value::String(branch.to_owned());
    std::fs::write(
        project(w).join("rwv.toml"),
        toml::to_string(&manifest).unwrap(),
    )
    .unwrap();
}

fn head_of(dir: &Path) -> String {
    git_in(dir, &["rev-parse", "HEAD"])
}

#[test]
fn update_remote_refuses_a_declared_branch_that_exists_nowhere_as_lock_does_and_moves_nothing() {
    let w = two_members();
    let app = member(&w, APP);
    commit_in(&app, "app-ahead");
    publish_branch(&app, "main");
    git_in(&app, &["reset", "-q", "--hard", "HEAD~1"]);
    declare_by_hand(&w, LIB, "nowhere");
    let before = (
        head_of(&app),
        head_of(&member(&w, LIB)),
        std::fs::read_to_string(project(&w).join("rwv.lock")).unwrap(),
    );

    let update = refused(&run(&w, &["update", "--remote"]));
    let lock = refused(&run(&w, &["lock", "--commit"]));

    let state = format!("{LIB}: name no-such-branch (nowhere)");
    assert!(lock.contains(&state), "{lock}");
    assert!(update.contains(&state), "{update}");
    assert!(update.contains("rwv fetch"), "{update}");
    assert!(update.contains("rwv explain name"), "{update}");
    assert!(!update.contains("update aborted"), "{update}");
    assert_eq!(
        (
            head_of(&app),
            head_of(&member(&w, LIB)),
            std::fs::read_to_string(project(&w).join("rwv.lock")).unwrap(),
        ),
        before,
        "the refusal came after something moved"
    );
}

#[test]
fn update_reports_a_declared_branch_that_exists_nowhere_by_the_same_judgment() {
    let w = two_members();
    declare_by_hand(&w, LIB, "nowhere");

    let out = run(&w, &["update"]);
    let update = format!("{}{}", stdout_of(&out), succeeded(&out));

    assert!(
        update.contains(&format!("{LIB}: name no-such-branch (nowhere)")),
        "{update}"
    );
    assert!(update.contains("rwv fetch"), "{update}");
}

#[test]
fn set_url_with_no_remote_at_the_new_url_reports_the_relink_waiting_for_a_fetch_until_it_relinks() {
    let w = two_members();
    let lib = member(&w, LIB);
    succeeded(&run(&w, &["update"]));
    let bare = w._tmp.path().join("lib-mirror.git");
    git_in(
        w._tmp.path(),
        &[
            "clone",
            "-q",
            "--bare",
            w.manifest_bares
                .iter()
                .find(|(p, _)| p == LIB)
                .unwrap()
                .1
                .to_str()
                .unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let mirror = common::file_url(&bare);
    let remotes = git_in(&lib, &["remote"]);

    let set = succeeded(&run(&w, &["member", "set-url", LIB, &mirror]));

    let waiting = format!("{LIB}: 'main' relinks to {mirror} once a fetch finds it there");
    assert!(set.contains(&waiting), "{set}");
    assert!(set.contains("rwv fetch"), "{set}");
    assert_eq!(git_in(&lib, &["remote"]), remotes, "set-url added a remote");
    assert_eq!(git_in(&lib, &["config", "branch.main.remote"]), "origin");

    let update = succeeded(&run(&w, &["update"]));
    assert!(update.contains(&waiting), "{update}");
    assert!(update.contains("rwv fetch"), "{update}");
    assert_eq!(git_in(&lib, &["config", "branch.main.remote"]), "origin");

    succeeded(&run(&w, &["fetch"]));
    let relinked = succeeded(&run(&w, &["update"]));
    assert!(!relinked.contains(&waiting), "{relinked}");
    assert_ne!(
        git_in(&lib, &["config", "branch.main.remote"]),
        "origin",
        "{relinked}"
    );
}

#[test]
fn lock_commit_over_a_hand_declared_branch_behind_the_pin_reports_the_re_pin_as_member_pin_does() {
    let w = two_members();
    let lib = member(&w, LIB);
    git_in(&lib, &["branch", "dev"]);
    publish_branch(&lib, "dev");
    commit_in(&lib, "main-ahead");
    publish_branch(&lib, "main");
    succeeded(&run(&w, &["lock", "--commit"]));
    let was = pin_of(&w, LIB).unwrap();
    let dev = git_in(&lib, &["rev-parse", "dev"]);
    declare_by_hand(&w, LIB, "dev");

    let text = succeeded(&run(&w, &["lock", "--commit"]));

    assert_eq!(pin_of(&w, LIB).as_deref(), Some(dev.as_str()), "{text}");
    assert!(
        text.contains(&format!(
            "rwv lock: {LIB}: re-pinned {} to {}, by name",
            &was[..12],
            &dev[..12]
        )),
        "{text}"
    );
}
