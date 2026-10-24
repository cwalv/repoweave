//! `rwv add` and `rwv fetch` give the canonical clone the submodules its
//! checkouts record, from upstream, so that creating a checkout, which never
//! reaches the network, has them to check out.

use std::path::{Path, PathBuf};

use repoweave::manifest::Manifest;

mod common;

const PROJECT: &str = "proj";
const SUB: &str = "libs/sub";
const NESTED: &str = "libs/sub/deep";

/// Every rwv and git run here may clone from a `file://` URL; git refuses
/// that to a submodule unless told otherwise.
const FILE_PROTOCOL: &[(&str, &str)] = &[("protocol.file.allow", "always")];

struct Weave {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    upstream_app: PathBuf,
    upstream_sub: PathBuf,
}

/// Upstream repositories `app`, recording `sub` at [`SUB`], which records
/// `deep`, and a primary with an empty project [`PROJECT`].
fn weave() -> Weave {
    let tmp = common::tempdir().unwrap();
    let upstream = tmp.path().join("upstream");
    let deep = upstream.join("deep");
    init_repo(&deep, &["deep.txt"]);
    let sub = upstream.join("sub");
    init_repo(&sub, &["lib.txt"]);
    git(
        &sub,
        &["submodule", "add", &common::file_url(&deep), "deep"],
    );
    git(&sub, &["commit", "-m", "add deep"]);
    let app = upstream.join("app");
    init_repo(&app, &["README"]);
    git(&app, &["submodule", "add", &common::file_url(&sub), SUB]);
    git(&app, &["commit", "-m", "add sub"]);

    let primary = tmp.path().join("primary");
    std::fs::create_dir_all(&primary).unwrap();
    rwv(&primary, &["init", PROJECT]);
    Weave {
        _tmp: tmp,
        primary,
        upstream_app: app,
        upstream_sub: sub,
    }
}

impl Weave {
    fn project_dir(&self) -> PathBuf {
        self.primary.join("projects").join(PROJECT)
    }

    /// `rwv add` the upstream app; returns its member path.
    fn add(&self) -> String {
        rwv(
            &self.primary,
            &["add", &common::file_url(&self.upstream_app)],
        );
        let manifest = Manifest::from_path(&self.project_dir().join("rwv.toml")).unwrap();
        let (member, _) = manifest.iter_entries().next().unwrap();
        member.as_str().to_owned()
    }

    fn canonical(&self, member: &str) -> PathBuf {
        self.primary.join(member)
    }

    fn lock_and_commit(&self) {
        self.lock_and_commit_with(&[]);
    }

    fn lock_and_commit_with(&self, flags: &[&str]) {
        let args: Vec<&str> = std::iter::once("lock")
            .chain(flags.iter().copied())
            .collect();
        rwv(&self.primary, &args);
        git(&self.project_dir(), &["add", "-A"]);
        git(&self.project_dir(), &["commit", "-m", "lock"]);
    }

    /// The canonical clone as an add before this behavior left it: no
    /// module data and every submodule directory empty.
    fn forget_module_data(&self, member: &str) {
        let canonical = self.canonical(member);
        git(&canonical, &["submodule", "deinit", "--all", "--force"]);
        std::fs::remove_dir_all(canonical.join(".git/modules")).unwrap();
        assert!(is_empty(&canonical.join(SUB)));
    }

    fn seat(&self, name: &str) -> PathBuf {
        rwv(&self.primary, &["workweave", PROJECT, "create", name]);
        self.primary
            .parent()
            .unwrap()
            .join(".workweaves")
            .join(format!("{PROJECT}--{name}"))
    }

    fn set_skip(&self, member: &str) {
        let path = self.project_dir().join("rwv.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        let entry = format!("[repositories.\"{member}\"]\n");
        assert!(text.contains(&entry), "{text}");
        std::fs::write(
            &path,
            text.replace(&entry, &format!("{entry}submodules = \"skip\"\n")),
        )
        .unwrap();
        git(&self.project_dir(), &["add", "-A"]);
        git(&self.project_dir(), &["commit", "-m", "skip submodules"]);
    }
}

fn init_repo(path: &Path, files: &[&str]) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-b", "main"]);
    for file in files {
        std::fs::write(path.join(file), format!("{file}\n")).unwrap();
    }
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "initial"]);
}

fn git(dir: &Path, args: &[&str]) -> String {
    common::git_in_configured(dir, FILE_PROTOCOL, args)
}

fn rwv_output(cwd: &Path, args: &[&str]) -> (bool, String) {
    let output = common::rwv()
        .envs(common::git_config_env(FILE_PROTOCOL))
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

fn rwv(cwd: &Path, args: &[&str]) -> String {
    let (ok, text) = rwv_output(cwd, args);
    assert!(ok, "rwv {args:?} in {} failed:\n{text}", cwd.display());
    text
}

/// The `uninitialized-submodule` findings doctor reports, run in `cwd`.
fn findings(cwd: &Path) -> Vec<serde_json::Value> {
    let output = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(cwd)
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json printed no report ({e}):\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    report["violations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|v| v["kind"] == "uninitialized-submodule")
        .collect()
}

/// The one `uninitialized-submodule` finding doctor reports in `cwd`.
fn sole_finding(cwd: &Path) -> serde_json::Value {
    let found = findings(cwd);
    assert_eq!(found.len(), 1, "doctor in {}: {found:?}", cwd.display());
    found.into_iter().next().unwrap()
}

/// Doctor's text line for the finding, run in `cwd`.
fn finding_text(cwd: &Path) -> String {
    let (_, text) = rwv_output(cwd, &["doctor"]);
    text.lines()
        .find(|l| l.contains("submodules not initialized"))
        .unwrap_or_else(|| panic!("doctor in {} reported nothing:\n{text}", cwd.display()))
        .to_owned()
}

/// The `rwv fetch` doctor's text names for an `unfetched` finding on
/// `member`, run in `cwd`, as the arguments to run it with in the workweave.
fn advised_fetch(cwd: &Path, member: &str) -> Vec<String> {
    let advice = finding_text(cwd);
    let fetch = format!("rwv fetch --repo {member}");
    assert!(
        advice.contains(&format!("`{fetch}` run in that workweave")),
        "{advice}"
    );
    fetch.split(' ').skip(1).map(str::to_owned).collect()
}

fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |mut entries| entries.next().is_none())
}

/// The submodule and the one nested in it are checked out in `checkout`, at
/// the commits it records.
fn assert_populated(checkout: &Path) {
    assert_eq!(
        common::read_normalized(checkout.join(SUB).join("lib.txt")),
        "lib.txt\n",
        "{} should have its submodule checked out",
        checkout.display()
    );
    assert_eq!(
        common::read_normalized(checkout.join(NESTED).join("deep.txt")),
        "deep.txt\n",
        "{} should have its nested submodule checked out",
        checkout.display()
    );
    assert_eq!(
        git(&checkout.join(SUB), &["rev-parse", "HEAD"]),
        git(checkout, &["rev-parse", &format!("HEAD:{SUB}")]),
    );
}

#[test]
fn a_workweave_of_a_member_add_cloned_has_its_submodules_with_no_manual_step() {
    let weave = weave();
    let member = weave.add();
    assert_populated(&weave.canonical(&member));
    weave.lock_and_commit();

    let seat = weave.seat("one");

    assert_populated(&seat.join(&member));
    assert!(findings(&weave.primary).is_empty());
    assert!(findings(&seat).is_empty());
}

#[test]
fn add_leaves_a_reference_members_submodule_files_read_only() {
    let weave = weave();
    rwv(
        &weave.primary,
        &[
            "add",
            "--role",
            "reference",
            &common::file_url(&weave.upstream_app),
        ],
    );
    let manifest = Manifest::from_path(&weave.project_dir().join("rwv.toml")).unwrap();
    let (member, _) = manifest.iter_entries().next().unwrap();
    let canonical = weave.canonical(member.as_str());

    assert_populated(&canonical);
    for file in [SUB.to_owned() + "/lib.txt", NESTED.to_owned() + "/deep.txt"] {
        let readonly = std::fs::metadata(canonical.join(&file))
            .unwrap()
            .permissions()
            .readonly();
        assert!(readonly, "{file} should have no write bits");
    }
}

#[test]
fn the_advised_fetch_gives_a_canonical_clone_without_module_data_its_submodules() {
    let weave = weave();
    let member = weave.add();
    weave.forget_module_data(&member);
    weave.lock_and_commit();
    let one = weave.seat("one");
    assert!(is_empty(&one.join(&member).join(SUB)));

    for cwd in [&weave.primary, &one] {
        let finding = sole_finding(cwd);
        assert_eq!(finding["sub_kind"], "unfetched", "{finding}");
        assert_eq!(finding["workweave"], format!("{PROJECT}--one"));
        let reported = PathBuf::from(finding["absolute_path"].as_str().unwrap());
        assert_eq!(
            repoweave::path_spelling::resolve(&reported).unwrap(),
            repoweave::path_spelling::resolve(&one.join(&member)).unwrap(),
            "doctor in {} names the workweave's checkout",
            cwd.display()
        );
    }
    let fetch = advised_fetch(&weave.primary, &member);

    rwv(&one, &fetch.iter().map(String::as_str).collect::<Vec<_>>());

    assert_populated(&weave.canonical(&member));
    assert_populated(&one.join(&member));
    assert!(findings(&one).is_empty());
    let two = weave.seat("two");
    assert_populated(&two.join(&member));
    assert!(findings(&weave.primary).is_empty());
}

#[test]
fn fetch_brings_a_commit_the_canonical_copy_lacks_without_moving_its_checkout() {
    let weave = weave();
    let member = weave.add();
    let canonical = weave.canonical(&member);
    let checked_out = git(&canonical.join(SUB), &["rev-parse", "HEAD"]);
    std::fs::write(weave.upstream_sub.join("lib.txt"), "moved\n").unwrap();
    git(&weave.upstream_sub, &["commit", "-am", "move"]);
    let app_sub = weave.upstream_app.join(SUB);
    git(&app_sub, &["pull", "--quiet"]);
    git(&weave.upstream_app, &["commit", "-am", "bump sub"]);
    git(&canonical, &["pull", "--quiet", "--no-recurse-submodules"]);
    weave.lock_and_commit_with(&["--dirty"]);
    let seat = weave.seat("one");
    assert_eq!(sole_finding(&seat)["sub_kind"], "unfetched");

    let out = rwv(&weave.primary, &["fetch", "--repo", &member]);

    assert!(
        out.contains(&format!("{member}/{SUB}: submodule fetched")),
        "{out}"
    );
    assert_eq!(
        git(&canonical.join(SUB), &["rev-parse", "HEAD"]),
        checked_out,
        "the canonical clone's own checkout of the submodule is not moved"
    );
    rwv(&seat, &["doctor", "--fix"]);
    assert_eq!(
        common::read_normalized(seat.join(&member).join(SUB).join("lib.txt")),
        "moved\n"
    );
}

#[test]
fn a_member_that_skips_submodules_is_not_fetched_for() {
    let weave = weave();
    let member = weave.add();
    weave.forget_module_data(&member);
    weave.set_skip(&member);

    rwv(&weave.primary, &["fetch"]);
    rwv(
        &weave.primary,
        &["add", &common::file_url(&weave.upstream_app)],
    );

    let canonical = weave.canonical(&member);
    assert!(is_empty(&canonical.join(SUB)));
    assert!(!canonical.join(".git/modules").exists());
}

#[test]
fn a_submodule_git_declines_is_neither_fetched_nor_reported() {
    let weave = weave();
    let member = weave.add();
    weave.forget_module_data(&member);
    let canonical = weave.canonical(&member);
    git(
        &canonical,
        &["config", &format!("submodule.{SUB}.update"), "none"],
    );
    weave.lock_and_commit();

    rwv(&weave.primary, &["fetch"]);
    let seat = weave.seat("one");

    assert!(is_empty(&canonical.join(SUB)));
    assert!(!canonical.join(".git/modules").exists());
    assert!(is_empty(&seat.join(&member).join(SUB)));
    assert!(findings(&weave.primary).is_empty());
    assert!(findings(&seat).is_empty());
}

/// A weave whose seat `one` records, at an empty [`SUB`], a commit of the
/// upstream sub that the canonical clone's copy lacks; returns the seat, the
/// member path and the commit.
fn seat_records_a_newer_sub(weave: &Weave) -> (PathBuf, String, String) {
    let member = weave.add();
    weave.lock_and_commit();
    let seat = weave.seat("one");
    std::fs::write(weave.upstream_sub.join("lib.txt"), "moved\n").unwrap();
    git(&weave.upstream_sub, &["commit", "-am", "move"]);
    let moved = git(&weave.upstream_sub, &["rev-parse", "HEAD"]);
    let checkout = seat.join(&member);
    let repository = git(
        &checkout.join(SUB),
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    );
    std::fs::remove_dir_all(checkout.join(SUB)).unwrap();
    std::fs::create_dir(checkout.join(SUB)).unwrap();
    std::fs::remove_dir_all(repository).unwrap();
    git(
        &checkout,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{moved},{SUB}"),
        ],
    );
    git(&checkout, &["commit", "-m", "bump sub in the seat only"]);
    rwv(&seat, &["lock", "--commit"]);
    (seat, member, moved)
}

#[test]
fn fetch_brings_a_commit_only_a_workweave_records() {
    let weave = weave();
    let (seat, member, moved) = seat_records_a_newer_sub(&weave);
    let canonical = weave.canonical(&member);
    let canonical_head = git(&canonical, &["rev-parse", "HEAD"]);
    assert_eq!(sole_finding(&seat)["sub_kind"], "unfetched");

    let out = rwv(
        &weave.primary,
        &["fetch", "--project", PROJECT, "--repo", &member],
    );

    assert!(
        out.contains(&format!("{member}/{SUB}: submodule fetched")),
        "{out}"
    );
    assert_eq!(git(&canonical, &["rev-parse", "HEAD"]), canonical_head);
    assert_eq!(sole_finding(&seat)["sub_kind"], "fetched");
    rwv(&seat, &["doctor", "--fix"]);
    let checkout = seat.join(&member);
    assert_eq!(git(&checkout.join(SUB), &["rev-parse", "HEAD"]), moved);
    assert_eq!(
        common::read_normalized(checkout.join(SUB).join("lib.txt")),
        "moved\n"
    );
}

#[test]
fn fetch_brings_nothing_for_a_submodule_git_declines() {
    let weave = weave();
    let (seat, member, moved) = seat_records_a_newer_sub(&weave);
    let canonical = weave.canonical(&member);
    git(
        &canonical,
        &["config", &format!("submodule.{SUB}.update"), "none"],
    );

    let out = rwv(
        &weave.primary,
        &["fetch", "--project", PROJECT, "--repo", &member],
    );

    assert!(!out.contains("submodule fetched"), "{out}");
    let copy = canonical.join(".git/modules").join(SUB);
    let held = common::git()
        .args(["cat-file", "-e", &format!("{moved}^{{commit}}")])
        .current_dir(&copy)
        .status()
        .unwrap()
        .success();
    assert!(
        !held,
        "the declined submodule's copy should not have been fetched into"
    );
    assert!(findings(&seat).is_empty());
}

/// Seats `one` and `two` from an empty project, with `one` having added the
/// upstream app and committed it; returns them and the member path.
fn one_adds_two_lacks(weave: &Weave) -> (PathBuf, PathBuf, String) {
    weave.lock_and_commit();
    let one = weave.seat("one");
    let two = weave.seat("two");
    rwv(&one, &["add", &common::file_url(&weave.upstream_app)]);
    rwv(&one, &["lock", "--commit"]);
    let manifest =
        Manifest::from_path(&one.join("projects").join(PROJECT).join("rwv.toml")).unwrap();
    let (member, _) = manifest.iter_entries().next().unwrap();
    let member = member.as_str().to_owned();
    assert!(!two.join(&member).exists());
    (one, two, member)
}

#[test]
fn add_in_a_workweave_creates_a_checkout_with_its_submodules() {
    let weave = weave();
    let (one, _, member) = one_adds_two_lacks(&weave);

    assert_populated(&one.join(&member));
}

#[test]
fn sync_creates_a_checkout_with_its_submodules() {
    let weave = weave();
    let (_, two, member) = one_adds_two_lacks(&weave);

    rwv(&two, &["sync", "one"]);

    assert_populated(&two.join(&member));
}

#[test]
fn sync_to_creates_a_checkout_with_its_submodules() {
    let weave = weave();
    let (one, two, member) = one_adds_two_lacks(&weave);

    rwv(&one, &["sync-to", "two"]);

    assert_populated(&two.join(&member));
}

#[test]
fn fetch_in_a_workweave_checks_out_its_members_empty_submodules() {
    let weave = weave();
    let member = weave.add();
    weave.forget_module_data(&member);
    weave.lock_and_commit();
    let seat = weave.seat("one");
    assert!(is_empty(&seat.join(&member).join(SUB)));

    rwv(&seat, &["fetch"]);

    assert_populated(&weave.canonical(&member));
    assert_populated(&seat.join(&member));
}

#[test]
fn a_submodule_git_declines_is_not_checked_out_from_local_data() {
    let weave = weave();
    let member = weave.add();
    weave.lock_and_commit();
    git(
        &weave.canonical(&member),
        &["config", &format!("submodule.{SUB}.update"), "none"],
    );

    let seat = weave.seat("one");

    assert!(is_empty(&seat.join(&member).join(SUB)));
    assert!(findings(&seat).is_empty());
}

#[test]
fn the_advised_fetch_in_the_workweave_brings_and_checks_out_a_commit_only_it_records() {
    let weave = weave();
    let (seat, member, moved) = seat_records_a_newer_sub(&weave);
    let canonical = weave.canonical(&member);
    let canonical_head = git(&canonical, &["rev-parse", "HEAD"]);
    assert_eq!(sole_finding(&seat)["sub_kind"], "unfetched");
    let fetch = advised_fetch(&seat, &member);

    rwv(&seat, &fetch.iter().map(String::as_str).collect::<Vec<_>>());

    assert!(findings(&seat).is_empty());
    let checkout = seat.join(&member);
    assert_eq!(git(&checkout.join(SUB), &["rev-parse", "HEAD"]), moved);
    assert_eq!(git(&canonical, &["rev-parse", "HEAD"]), canonical_head);
}
