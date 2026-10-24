//! `rwv fetch` run in a workweave, and the primary's canonical clones it
//! shares with that workweave:
//!
//!   - it never moves the primary's branch, whether the workweave's pin is
//!     behind the primary or ahead of it;
//!   - it fetches a pinned revision the canonical clone lacks, so the advice
//!     doctor prints for `missing-unfetched` and `symlink-unfetched`, followed
//!     from the workweave, clears them;
//!   - a canonical clone it creates is born at the primary's pin, and it does
//!     not write the workweave's `rwv.lock`.

use std::path::{Path, PathBuf};

mod common;

const OWNED_PATH: &str = "github/org/owned";
const LATE_PATH: &str = "github/org/late";
const PROJECT: &str = "app";
const SEAT_BRANCH: &str = "app--seat";
const FOLLOW_THE_ADVICE: &str = "`rwv fetch` run in that workweave";

fn commit(repo: &Path, file: &str) -> String {
    std::fs::write(repo.join(file), format!("{file}\n")).unwrap();
    common::git_in(repo, &["add", "."]);
    common::git_in(repo, &["commit", "-m", file]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// A repo with one commit at `source`, published to a bare `upstream`.
fn publish(tmp: &Path, name: &str) -> (PathBuf, PathBuf, String) {
    let source = tmp.join(format!("source-{name}"));
    std::fs::create_dir_all(&source).unwrap();
    common::git_in(&source, &["init", "-b", "main"]);
    common::git_in(&source, &["config", "user.email", "test@test.com"]);
    common::git_in(&source, &["config", "user.name", "Test"]);
    let pin = commit(&source, name);
    let upstream = tmp.join(format!("upstream-{name}.git"));
    common::git_in(
        tmp,
        &[
            "clone",
            "--bare",
            &source.to_string_lossy(),
            &upstream.to_string_lossy(),
        ],
    );
    (source, upstream, pin)
}

fn clone_canonical(upstream: &Path, canonical: &Path) {
    std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
    common::git_in(
        canonical.parent().unwrap(),
        &[
            "clone",
            &upstream.to_string_lossy(),
            &canonical.to_string_lossy(),
        ],
    );
    common::git_in(canonical, &["config", "user.email", "test@test.com"]);
    common::git_in(canonical, &["config", "user.name", "Test"]);
}

fn manifest_entry(path: &str, url: &Path) -> String {
    format!(
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{url}\"\nversion = \"main\"\nrole = \"owned\"\n",
        url = common::url_path(url),
    )
}

struct Weave {
    tmp: PathBuf,
    primary: PathBuf,
    owned_upstream: PathBuf,
}

impl Weave {
    fn create_seat(&self) -> PathBuf {
        common::rwv()
            .args(["workweave", PROJECT, "create", "seat"])
            .current_dir(&self.primary)
            .assert()
            .success();
        self.tmp
            .join(".workweaves")
            .join(format!("{PROJECT}--seat"))
    }
}

/// A primary weave with one owned member cloned from a bare upstream,
/// committed and locked, and a workweave of it.
fn make_weave(tmp: &Path) -> (Weave, PathBuf) {
    let primary = tmp.join("primary");
    let (_, owned_upstream, owned_pin) = publish(tmp, "owned");
    clone_canonical(&owned_upstream, &primary.join(OWNED_PATH));
    let project_dir = primary.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project_dir).unwrap();
    common::git_in(&project_dir, &["init", "-b", "main"]);
    common::git_in(&project_dir, &["config", "user.email", "test@test.com"]);
    common::git_in(&project_dir, &["config", "user.name", "Test"]);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        manifest_entry(OWNED_PATH, &owned_upstream),
    )
    .unwrap();
    common::fixture_lock(
        &project_dir,
        &[(
            OWNED_PATH,
            common::file_url(&owned_upstream).as_str(),
            &owned_pin,
        )],
    );
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    let weave = Weave {
        tmp: tmp.to_path_buf(),
        primary,
        owned_upstream,
    };
    let seat = weave.create_seat();
    (weave, seat)
}

fn fetch_in(seat: &Path) -> std::process::Output {
    common::rwv()
        .args(["fetch"])
        .current_dir(seat)
        .output()
        .unwrap()
}

fn describe(out: &std::process::Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `main` checked out at `tip`: the primary's branch as the test left it.
fn assert_on_main_at(canonical: &Path, tip: &str) {
    assert_eq!(
        common::git_in(canonical, &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
    assert_eq!(common::git_in(canonical, &["rev-parse", "main"]), tip);
}

#[test]
fn fetch_in_a_workweave_behind_the_primary_leaves_the_primarys_branch_alone() {
    let tmp = common::tempdir().unwrap();
    let (weave, seat) = make_weave(tmp.path());
    let canonical = weave.primary.join(OWNED_PATH);
    let seat_pin = common::git_in(seat.join(OWNED_PATH), &["rev-parse", "HEAD"]);
    let primary_tip = commit(&canonical, "PRIMARY-ONLY");

    let fetched = fetch_in(&seat);

    assert!(fetched.status.success(), "{}", describe(&fetched));
    assert_on_main_at(&canonical, &primary_tip);
    assert_eq!(
        common::git_in(seat.join(OWNED_PATH), &["rev-parse", "HEAD"]),
        seat_pin
    );
}

#[test]
fn fetch_in_a_workweave_ahead_of_the_primary_leaves_the_primarys_branch_alone() {
    let tmp = common::tempdir().unwrap();
    let (weave, seat) = make_weave(tmp.path());
    let canonical = weave.primary.join(OWNED_PATH);
    let primary_tip = common::git_in(&canonical, &["rev-parse", "main"]);
    let seat_tip = commit(&seat.join(OWNED_PATH), "SEAT-ONLY");
    common::fixture_lock(
        &seat.join("projects").join(PROJECT),
        &[(
            OWNED_PATH,
            common::file_url(&weave.owned_upstream).as_str(),
            &seat_tip,
        )],
    );

    let fetched = fetch_in(&seat);

    assert!(fetched.status.success(), "{}", describe(&fetched));
    assert_on_main_at(&canonical, &primary_tip);
}

/// The doctor findings about `path` in `workspace`, as `kind/sub_kind`, and
/// its text report.
fn doctor_about(workspace: &Path, path: &str) -> (Vec<String>, String) {
    let json = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(workspace)
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let findings = doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["path"] == path && v["kind"] == "workweave-drift")
        .map(|v| format!("workweave-drift/{}", v["sub_kind"].as_str().unwrap()))
        .collect();
    let text = common::rwv()
        .args(["doctor"])
        .current_dir(workspace)
        .output()
        .unwrap();
    (findings, String::from_utf8_lossy(&text.stdout).into_owned())
}

/// Add `LATE_PATH` from `upstream` to the manifest in `project_dir`, pinned at
/// `pin` beside the owned member's existing pin, and commit both files.
fn add_late_member(project_dir: &Path, owned_upstream: &Path, upstream: &Path, pin: Option<&str>) {
    let manifest = std::fs::read_to_string(project_dir.join("rwv.toml")).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!("{manifest}\n{}", manifest_entry(LATE_PATH, upstream)),
    )
    .unwrap();
    let lock: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap())
            .unwrap();
    let owned = lock["repositories"][OWNED_PATH]["version"]
        .as_str()
        .unwrap()
        .to_owned();
    let owned_url = common::file_url(owned_upstream);
    let late_url = common::file_url(upstream);
    let mut rows = vec![(OWNED_PATH, owned_url.as_str(), owned.as_str())];
    if let Some(pin) = pin {
        rows.push((LATE_PATH, late_url.as_str(), pin));
    }
    common::fixture_lock(project_dir, &rows);
    common::git_in(project_dir, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(project_dir, &["commit", "-m", "add late"]);
}

/// A member the workweave adds at a revision its upstream gained after the
/// primary cloned it: the canonical clone is on disk without the pin.
fn add_a_member_pinned_past_its_canonical_clone(weave: &Weave, seat: &Path) -> (String, String) {
    let (source, upstream, cloned) = publish(&weave.tmp, "late");
    clone_canonical(&upstream, &weave.primary.join(LATE_PATH));
    let pin = commit(&source, "LATER");
    common::git_in(&source, &["push", &upstream.to_string_lossy(), "main"]);
    add_late_member(
        &seat.join("projects").join(PROJECT),
        &weave.owned_upstream,
        &upstream,
        Some(&pin),
    );
    (cloned, pin)
}

fn assert_advice_clears(seat: &Path, finding: &str, canonical: &Path, cloned: &str, pin: &str) {
    let (findings, report) = doctor_about(seat, LATE_PATH);
    assert_eq!(findings, vec![finding.to_owned()]);
    assert!(report.contains(FOLLOW_THE_ADVICE), "{report}");

    let fetched = fetch_in(seat);

    assert!(fetched.status.success(), "{}", describe(&fetched));
    let checkout = seat.join(LATE_PATH);
    assert!(checkout.join(".git").is_file(), "a linked worktree");
    assert_eq!(
        common::git_in(&checkout, &["symbolic-ref", "--short", "HEAD"]),
        SEAT_BRANCH
    );
    assert_eq!(common::git_in(&checkout, &["rev-parse", "HEAD"]), pin);
    assert_eq!(doctor_about(seat, LATE_PATH).0, Vec::<String>::new());
    assert_on_main_at(canonical, cloned);
}

#[test]
fn missing_unfetched_advice_followed_from_the_workweave_clears_it() {
    let tmp = common::tempdir().unwrap();
    let (weave, seat) = make_weave(tmp.path());
    let (cloned, pin) = add_a_member_pinned_past_its_canonical_clone(&weave, &seat);
    let canonical = weave.primary.join(LATE_PATH);

    assert_advice_clears(
        &seat,
        "workweave-drift/missing-unfetched",
        &canonical,
        &cloned,
        &pin,
    );
}

#[test]
fn symlink_unfetched_advice_followed_from_the_workweave_clears_it() {
    let tmp = common::tempdir().unwrap();
    let (weave, seat) = make_weave(tmp.path());
    let (cloned, pin) = add_a_member_pinned_past_its_canonical_clone(&weave, &seat);
    let canonical = weave.primary.join(LATE_PATH);
    repoweave::symlink::create(
        &canonical,
        &seat.join(LATE_PATH),
        repoweave::symlink::LinkTarget::Directory,
    )
    .unwrap();

    assert_advice_clears(
        &seat,
        "workweave-drift/symlink-unfetched",
        &canonical,
        &cloned,
        &pin,
    );
}

#[test]
fn a_canonical_clone_fetch_creates_from_a_workweave_is_born_at_the_primarys_pin() {
    let tmp = common::tempdir().unwrap();
    let (weave, seat) = make_weave(tmp.path());
    let (source, upstream, primary_pin) = publish(&weave.tmp, "late");
    let seat_pin = commit(&source, "LATER");
    common::git_in(&source, &["push", &upstream.to_string_lossy(), "main"]);
    add_late_member(
        &weave.primary.join("projects").join(PROJECT),
        &weave.owned_upstream,
        &upstream,
        Some(&primary_pin),
    );
    add_late_member(
        &seat.join("projects").join(PROJECT),
        &weave.owned_upstream,
        &upstream,
        Some(&seat_pin),
    );

    let fetched = fetch_in(&seat);

    assert!(fetched.status.success(), "{}", describe(&fetched));
    let canonical = weave.primary.join(LATE_PATH);
    assert_on_main_at(&canonical, &primary_pin);
    assert_eq!(
        common::git_in(seat.join(LATE_PATH), &["rev-parse", "HEAD"]),
        seat_pin
    );
}

#[test]
fn an_unpinned_symlinked_member_gets_a_worktree_at_the_canonical_head_and_the_lock_is_left() {
    let tmp = common::tempdir().unwrap();
    let (weave, seat) = make_weave(tmp.path());
    let (_, upstream, head) = publish(&weave.tmp, "late");
    let project_dir = seat.join("projects").join(PROJECT);
    add_late_member(&project_dir, &weave.owned_upstream, &upstream, None);
    let canonical = weave.primary.join(LATE_PATH);
    repoweave::symlink::create(
        &canonical,
        &seat.join(LATE_PATH),
        repoweave::symlink::LinkTarget::Directory,
    )
    .unwrap();
    let lock_before = std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap();
    assert_eq!(
        doctor_about(&seat, LATE_PATH).0,
        vec!["workweave-drift/symlink-unfetched".to_owned()]
    );

    let fetched = fetch_in(&seat);

    assert!(fetched.status.success(), "{}", describe(&fetched));
    let checkout = seat.join(LATE_PATH);
    assert!(!checkout.is_symlink() && checkout.join(".git").is_file());
    assert_eq!(common::git_in(&checkout, &["rev-parse", "HEAD"]), head);
    assert_eq!(doctor_about(&seat, LATE_PATH).0, Vec::<String>::new());
    assert_eq!(
        std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap(),
        lock_before,
        "the workweave's lock records its worktrees; fetch does not write it"
    );
}
