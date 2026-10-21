//! A `reference` member's tracked regular files carry no write bit, in the
//! primary and in every workweave, after every verb that writes or moves
//! them; every other member's stay writable by their owner.
//!
//!   - create, fetch, add, sync, sync-to, update and abort settle what they
//!     wrote, and sync's own move of a read-only member succeeds;
//!   - a role that changes under sync or abort is settled to the new role;
//!   - doctor reports a member whose files disagree with its role, and
//!     `--fix` settles it, giving a shared file a private copy before it
//!     restores a write bit;
//!   - a tracked symlink is never chmod'ed, and untracked files can still be
//!     created in a reference tree.
#![cfg(unix)]

use assert_cmd::Command as AssertCommand;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

mod common;

const OWNED_PATH: &str = "github/org/owned";
const REF_PATH: &str = "github/org/reference";
const PROJECT: &str = "app";

fn rwv() -> AssertCommand {
    common::rwv()
}

fn run(args: &[&str], dir: &Path) -> std::process::Output {
    rwv().args(args).current_dir(dir).output().unwrap()
}

fn describe(out: &std::process::Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn succeed(args: &[&str], dir: &Path) {
    let out = run(args, dir);
    assert!(
        out.status.success(),
        "`rwv {}` failed; {}",
        args.join(" "),
        describe(&out)
    );
}

fn commit_all(repo: &Path, msg: &str) -> String {
    common::git_in(repo, &["add", "-A"]);
    common::git_in(repo, &["commit", "-qm", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// A bare upstream holding `files`, made through a scratch clone that is then
/// removed. `link` adds a tracked symlink at `link` pointing at the given
/// absolute path.
fn upstream(tmp: &Path, name: &str, files: &[(&str, &str)], link: Option<&Path>) -> PathBuf {
    let bare = tmp.join(format!("{name}.git"));
    common::git_in(
        tmp,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            &bare.to_string_lossy(),
        ],
    );
    let seed = tmp.join(format!("seed-{name}"));
    common::git_in(
        tmp,
        &[
            "clone",
            "-q",
            &bare.to_string_lossy(),
            &seed.to_string_lossy(),
        ],
    );
    common::git_in(&seed, &["config", "user.email", "test@test.com"]);
    common::git_in(&seed, &["config", "user.name", "Test"]);
    for (path, body) in files {
        let file = seed.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, body).unwrap();
        if body.starts_with("#!") {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    if let Some(target) = link {
        std::os::unix::fs::symlink(target, seed.join("outside-link")).unwrap();
    }
    commit_all(&seed, "initial");
    common::git_in(&seed, &["push", "-q", "origin", "main"]);
    std::fs::remove_dir_all(&seed).unwrap();
    bare
}

fn manifest_entry(path: &str, url: &Path, role: &str) -> String {
    format!(
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{url}\"\nversion = \"main\"\nrole = \"{role}\"\n",
        url = common::url_path(url),
    )
}

struct Weave {
    tmp: PathBuf,
    primary: PathBuf,
    owned: PathBuf,
    reference: PathBuf,
    owned_upstream: PathBuf,
    ref_upstream: PathBuf,
    /// A writable file outside every repository, which the reference repo's
    /// tracked symlink points at.
    outside: PathBuf,
}

impl Weave {
    fn project_dir(&self) -> PathBuf {
        self.primary.join("projects").join(PROJECT)
    }

    fn workweave(&self, name: &str) -> PathBuf {
        self.tmp
            .join(".workweaves")
            .join(format!("{PROJECT}--{name}"))
    }

    fn create(&self, name: &str) -> PathBuf {
        succeed(&["workweave", PROJECT, "create", name], &self.primary);
        self.workweave(name)
    }

    /// Write the manifest with `roles` for the owned and reference members,
    /// lock it at their canonical clones' HEADs, and commit both.
    fn commit_manifest(&self, project_dir: &Path, owned_role: &str, ref_role: &str) {
        std::fs::write(
            project_dir.join("rwv.toml"),
            format!(
                "{}\n{}",
                manifest_entry(OWNED_PATH, &self.owned_upstream, owned_role),
                manifest_entry(REF_PATH, &self.ref_upstream, ref_role)
            ),
        )
        .unwrap();
        let owned_url = common::file_url(&self.owned_upstream);
        let ref_url = common::file_url(&self.ref_upstream);
        let owned_pin = common::git_in(&self.owned, &["rev-parse", "HEAD"]);
        let ref_pin = common::git_in(&self.reference, &["rev-parse", "HEAD"]);
        common::fixture_lock(
            project_dir,
            &[
                (OWNED_PATH, owned_url.as_str(), owned_pin.as_str()),
                (REF_PATH, ref_url.as_str(), ref_pin.as_str()),
            ],
        );
        commit_all(project_dir, "manifest and lock");
    }
}

/// A primary weave whose canonical clones come from bare upstreams: one
/// `owned` member and one `reference` member holding a nested file, an
/// executable and a tracked symlink to a file outside every repository.
fn make_weave(tmp: &Path) -> Weave {
    let outside = tmp.join("outside.txt");
    std::fs::write(&outside, "outside\n").unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o644)).unwrap();
    let owned_upstream = upstream(tmp, "owned", &[("OWNED", "o\n")], None);
    let ref_upstream = upstream(
        tmp,
        "reference",
        &[
            ("REF", "r\n"),
            ("dir/NESTED", "n\n"),
            ("run.sh", "#!/bin/sh\n"),
        ],
        Some(&outside),
    );

    let primary = tmp.join("primary");
    std::fs::create_dir_all(primary.join("github/org")).unwrap();
    let owned = primary.join(OWNED_PATH);
    let reference = primary.join(REF_PATH);
    for (bare, dest) in [(&owned_upstream, &owned), (&ref_upstream, &reference)] {
        common::git_in(
            tmp,
            &[
                "clone",
                "-q",
                &bare.to_string_lossy(),
                &dest.to_string_lossy(),
            ],
        );
        common::git_in(dest, &["config", "user.email", "test@test.com"]);
        common::git_in(dest, &["config", "user.name", "Test"]);
    }

    let weave = Weave {
        tmp: tmp.to_path_buf(),
        primary: primary.clone(),
        owned,
        reference,
        owned_upstream,
        ref_upstream,
        outside,
    };
    let project_dir = weave.project_dir();
    std::fs::create_dir_all(&project_dir).unwrap();
    common::git_in(&project_dir, &["init", "-q", "-b", "main"]);
    common::git_in(&project_dir, &["config", "user.email", "test@test.com"]);
    common::git_in(&project_dir, &["config", "user.name", "Test"]);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    weave.commit_manifest(&project_dir, "owned", "reference");
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    weave
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

/// The tracked regular files of the checkout at `checkout`, from git itself.
fn tracked_regular_files(checkout: &Path) -> Vec<PathBuf> {
    let listing = common::git_in(checkout, &["ls-files", "-s"]);
    let files: Vec<PathBuf> = listing
        .lines()
        .filter_map(|line| {
            let (meta, path) = line.split_once('\t')?;
            matches!(meta.split(' ').next(), Some("100644" | "100755")).then(|| checkout.join(path))
        })
        .collect();
    assert!(
        !files.is_empty(),
        "{} tracks no regular file; every assertion over them would be vacuous",
        checkout.display()
    );
    files
}

fn assert_read_only(checkout: &Path, when: &str) {
    for file in tracked_regular_files(checkout) {
        assert_eq!(
            mode(&file) & 0o222,
            0,
            "{when}: {} must carry no write bit (mode {:o})",
            file.display(),
            mode(&file)
        );
    }
}

fn assert_writable(checkout: &Path, when: &str) {
    for file in tracked_regular_files(checkout) {
        assert_ne!(
            mode(&file) & 0o200,
            0,
            "{when}: {} must be writable by its owner (mode {:o})",
            file.display(),
            mode(&file)
        );
    }
}

/// The `member-permissions` records `rwv doctor --json` emits, as
/// `(path, sub_kind, files)`.
fn permission_findings(dir: &Path) -> Vec<(String, String, u64)> {
    let out = run(&["doctor", "--json"], dir);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("doctor --json must print JSON ({e}); {}", describe(&out)));
    doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "member-permissions")
        .map(|v| {
            (
                v["path"].as_str().unwrap().to_owned(),
                v["sub_kind"].as_str().unwrap().to_owned(),
                v["files"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn create_settles_a_reference_worktree_read_only_and_leaves_owned_writable() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");

    let reference = seat.join(REF_PATH);
    assert_read_only(&reference, "after create");
    assert_writable(&seat.join(OWNED_PATH), "after create");
    assert_eq!(
        mode(&reference.join("run.sh")),
        0o555,
        "the executable bit survives; only write bits go"
    );
    assert_ne!(
        mode(&reference.join("dir")) & 0o200,
        0,
        "directories keep their write bits"
    );

    std::fs::write(reference.join("dir/UNTRACKED"), "scratch\n")
        .expect("an untracked file can still be created in a reference tree");
}

#[test]
fn a_tracked_symlink_keeps_its_targets_permissions() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");

    let link = seat.join(REF_PATH).join("outside-link");
    assert!(
        link.is_symlink(),
        "fixture: the reference repo tracks a symlink"
    );
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        weave.outside,
        "fixture: it points outside every repository"
    );
    assert_eq!(
        mode(&weave.outside),
        0o644,
        "settling the reference member must never chmod through its tracked symlink"
    );
}

#[test]
fn fetch_clones_a_reference_member_read_only_in_the_primary() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    std::fs::remove_dir_all(&weave.reference).unwrap();

    succeed(&["fetch"], &weave.primary);

    assert_read_only(&weave.reference, "after fetch cloned it");
    assert_writable(&weave.owned, "after fetch");
}

#[test]
fn add_settles_the_member_it_adds_in_the_primary_and_in_a_workweave() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");
    let extra = upstream(tmp.path(), "extra", &[("EXTRA", "e\n")], None);
    let url = common::file_url(&extra);

    succeed(&["add", &url, "--role", "reference"], &weave.primary);
    let member = find_member(&weave.primary, &extra);
    assert_read_only(&member, "after add in the primary");

    let other = upstream(tmp.path(), "other", &[("OTHER", "x\n")], None);
    succeed(
        &["add", &common::file_url(&other), "--role", "reference"],
        &seat,
    );
    let rel = find_member(&weave.primary, &other)
        .strip_prefix(&weave.primary)
        .unwrap()
        .to_path_buf();
    assert_read_only(&seat.join(rel), "after add in a workweave");
}

/// Where `rwv add` placed the clone of `upstream` under `primary`: the
/// placement depends on the URL's registry, which this file does not restate.
fn find_member(primary: &Path, upstream: &Path) -> PathBuf {
    let want = common::file_url(upstream);
    let mut stack = vec![primary.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir.join(".git").exists() && dir != primary {
            if common::git_in(&dir, &["remote", "get-url", "origin"]) == want {
                return dir;
            }
            continue;
        }
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() && !path.is_symlink() && entry.file_name() != "projects" {
                stack.push(path);
            }
        }
    }
    panic!(
        "no clone of {} under {}",
        upstream.display(),
        primary.display()
    );
}

#[test]
fn sync_moves_a_read_only_reference_member_and_settles_what_it_wrote() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");
    assert_read_only(&seat.join(REF_PATH), "before sync");

    std::fs::write(weave.reference.join("REF"), "r2\n").unwrap();
    std::fs::write(weave.reference.join("dir/ADDED"), "a\n").unwrap();
    let advanced = commit_all(&weave.reference, "primary: advance reference");
    succeed(&["lock", "--commit"], &weave.primary);

    succeed(&["sync", "primary"], &seat);

    let reference = seat.join(REF_PATH);
    assert_eq!(
        common::git_in(&reference, &["rev-parse", "HEAD"]),
        advanced,
        "git must move a read-only reference member"
    );
    assert_eq!(common::read_normalized(reference.join("REF")), "r2\n");
    assert_read_only(&reference, "after sync rewrote it");
}

#[test]
fn sync_to_settles_the_member_it_lands_in_the_primary() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");
    std::fs::write(seat.join(REF_PATH).join("NEW"), "new\n").unwrap();
    commit_all(&seat.join(REF_PATH), "seat: new reference file");
    succeed(&["lock", "--commit"], &seat);

    succeed(&["sync-to", &weave.primary.to_string_lossy()], &seat);

    assert!(weave.reference.join("NEW").exists(), "the commit landed");
    assert_read_only(&weave.reference, "the primary's reference after sync-to");
    assert_read_only(
        &seat.join(REF_PATH),
        "the workweave's reference after sync-to",
    );
}

#[test]
fn update_settles_what_it_advances() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let pusher = tmp.path().join("pusher");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            "-q",
            &weave.ref_upstream.to_string_lossy(),
            &pusher.to_string_lossy(),
        ],
    );
    common::git_in(&pusher, &["config", "user.email", "test@test.com"]);
    common::git_in(&pusher, &["config", "user.name", "Test"]);
    std::fs::write(pusher.join("UPSTREAM"), "u\n").unwrap();
    commit_all(&pusher, "upstream advance");
    common::git_in(&pusher, &["push", "-q", "origin", "main"]);

    succeed(&["update"], &weave.primary);

    assert!(
        weave.reference.join("UPSTREAM").exists(),
        "update advanced it"
    );
    assert_read_only(&weave.reference, "after update");
}

#[test]
fn a_reference_member_arriving_by_sync_is_read_only() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");
    let late_upstream = upstream(tmp.path(), "late", &[("LATE", "l\n")], None);
    let late = weave.primary.join("github/org/late");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            "-q",
            &late_upstream.to_string_lossy(),
            &late.to_string_lossy(),
        ],
    );
    let manifest = weave.project_dir().join("rwv.toml");
    let body = common::read_normalized(&manifest);
    std::fs::write(
        &manifest,
        format!(
            "{body}\n{}",
            manifest_entry("github/org/late", &late_upstream, "reference")
        ),
    )
    .unwrap();
    commit_all(&weave.project_dir(), "add late");
    succeed(&["lock", "--commit"], &weave.primary);

    succeed(&["sync", "primary"], &seat);

    assert_read_only(&seat.join("github/org/late"), "after sync materialized it");
}

#[test]
fn a_role_changed_to_reference_by_sync_is_settled_read_only() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");
    assert_writable(&seat.join(OWNED_PATH), "before the role change");

    weave.commit_manifest(&weave.project_dir(), "reference", "reference");
    succeed(&["sync", "primary"], &seat);

    assert_read_only(
        &seat.join(OWNED_PATH),
        "after sync brought role = reference",
    );
}

#[test]
fn abort_restoring_an_older_manifest_resettles_the_target() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");
    weave.commit_manifest(
        &seat.join("projects").join(PROJECT),
        "reference",
        "reference",
    );
    succeed(&["lock", "--commit"], &seat);
    std::fs::write(seat.join(OWNED_PATH).join("UNTRACKED"), "u\n").unwrap();

    let retired = run(
        &["sync-to", &weave.primary.to_string_lossy(), "--retire"],
        &seat,
    );
    assert!(
        !retired.status.success() && seat.join(".rwv-op").exists(),
        "fixture: retire must refuse on the untracked file and park; {}",
        describe(&retired)
    );
    assert_read_only(
        &weave.owned,
        "the primary's member once the landed manifest made it a reference",
    );

    succeed(&["abort"], &seat);

    assert_writable(
        &weave.owned,
        "the primary's member once abort restored the manifest naming it owned",
    );
}

#[test]
fn doctor_fix_settles_a_reference_member_made_writable_by_hand() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");
    let file = seat.join(REF_PATH).join("REF");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(
        permission_findings(&seat),
        vec![(REF_PATH.to_owned(), "writable-reference".to_owned(), 1)]
    );
    let fixed = run(&["doctor", "--fix"], &seat);
    assert!(
        String::from_utf8_lossy(&fixed.stdout).contains(&format!(
            "[fixed] core: settled 1 tracked file(s) of {PROJECT}--seat/{REF_PATH} for its role (reference)"
        )),
        "{}",
        describe(&fixed)
    );
    assert_eq!(mode(&file), 0o444);
    assert_eq!(permission_findings(&seat), vec![]);
}

#[test]
fn a_hand_edit_to_owned_unshares_before_restoring_write_bits() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let wa = weave.create("wa");
    let wb = weave.create("wb");
    let mine = wa.join(REF_PATH).join("REF");
    let sibling = wb.join(REF_PATH).join("REF");
    std::fs::remove_file(&mine).unwrap();
    std::fs::hard_link(&sibling, &mine).unwrap();
    assert_eq!(
        std::fs::metadata(&mine).unwrap().ino(),
        std::fs::metadata(&sibling).unwrap().ino(),
        "fixture: the two workspaces share one read-only file"
    );

    let manifest = wa.join("projects").join(PROJECT).join("rwv.toml");
    let body = common::read_normalized(&manifest);
    let ref_block = manifest_entry(REF_PATH, &weave.ref_upstream, "reference");
    assert!(body.contains(&ref_block), "fixture: manifest layout");
    std::fs::write(
        &manifest,
        body.replace(
            &ref_block,
            &manifest_entry(REF_PATH, &weave.ref_upstream, "owned"),
        ),
    )
    .unwrap();

    assert!(
        permission_findings(&wa).contains(&(
            REF_PATH.to_owned(),
            "read-only-member".to_owned(),
            tracked_regular_files(&wa.join(REF_PATH)).len() as u64
        )),
        "doctor reports every read-only file of the no-longer-reference member"
    );
    succeed(&["doctor", "--fix"], &wa);

    assert_writable(&wa.join(REF_PATH), "after the role left reference");
    assert_ne!(
        std::fs::metadata(&mine).unwrap().ino(),
        std::fs::metadata(&sibling).unwrap().ino(),
        "the shared file must get a private copy before it is made writable"
    );
    assert_eq!(common::read_normalized(&mine), "r\n");
    assert_eq!(
        mode(&sibling) & 0o222,
        0,
        "the sibling workspace's copy must stay read-only"
    );
    assert_eq!(std::fs::metadata(&sibling).unwrap().nlink(), 1);
}

const OTHER_PROJECT: &str = "lib";

/// A second project in the primary, listing the reference member's canonical
/// clone as `ref_role`.
fn add_project(weave: &Weave, name: &str, ref_role: &str) {
    let project_dir = weave.primary.join("projects").join(name);
    std::fs::create_dir_all(&project_dir).unwrap();
    common::git_in(&project_dir, &["init", "-q", "-b", "main"]);
    common::git_in(&project_dir, &["config", "user.email", "test@test.com"]);
    common::git_in(&project_dir, &["config", "user.name", "Test"]);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    weave.commit_manifest(&project_dir, "owned", ref_role);
}

fn activate(weave: &Weave, project: &str) {
    std::fs::write(weave.primary.join(".rwv-active"), format!("{project}\n")).unwrap();
}

fn make_read_only(checkout: &Path) {
    for file in tracked_regular_files(checkout) {
        let mode = mode(&file);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode & !0o222)).unwrap();
    }
}

/// `rwv doctor --json` run in `dir` with `scope`, as the `(kind, path)` of
/// every `member-permissions` and `role-disagreement` record, plus the
/// `project: role` pairs of the latter.
fn role_findings(dir: &Path, scope: &[&str]) -> Vec<(String, String, Vec<String>)> {
    let mut args = vec!["doctor", "--json"];
    args.extend_from_slice(scope);
    let out = run(&args, dir);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("doctor --json must print JSON ({e}); {}", describe(&out)));
    doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "member-permissions" || v["kind"] == "role-disagreement")
        .map(|v| {
            let roles = v["roles"]
                .as_array()
                .map(|roles| {
                    roles
                        .iter()
                        .map(|r| {
                            format!(
                                "{}: {}",
                                r["project"].as_str().unwrap(),
                                r["role"].as_str().unwrap()
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            (
                v["kind"].as_str().unwrap().to_owned(),
                v["path"].as_str().unwrap().to_owned(),
                roles,
            )
        })
        .collect()
}

#[test]
fn a_clone_projects_disagree_on_stays_writable_whichever_project_settles_it() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    add_project(&weave, OTHER_PROJECT, "owned");

    for project in [PROJECT, OTHER_PROJECT] {
        activate(&weave, project);
        make_read_only(&weave.reference);
        succeed(&["update"], &weave.primary);
        assert_writable(
            &weave.reference,
            &format!("after update with {project} active, while {OTHER_PROJECT} declares it owned"),
        );
    }
}

#[test]
fn a_clone_every_project_declares_reference_is_read_only() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    add_project(&weave, OTHER_PROJECT, "reference");

    activate(&weave, OTHER_PROJECT);
    succeed(&["update"], &weave.primary);
    assert_read_only(
        &weave.reference,
        "after update, every project declaring reference",
    );
    assert_eq!(role_findings(&weave.primary, &["--all"]), vec![]);
}

#[test]
fn doctor_reports_the_disagreement_and_agrees_on_permissions_whichever_project_is_in_scope() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    add_project(&weave, OTHER_PROJECT, "owned");
    let disagreement = (
        "role-disagreement".to_owned(),
        REF_PATH.to_owned(),
        vec![
            format!("{PROJECT}: reference"),
            format!("{OTHER_PROJECT}: owned"),
        ],
    );
    let scopes: [&[&str]; 3] = [&[], &["--project", OTHER_PROJECT], &["--all"]];

    for scope in scopes {
        assert_eq!(
            role_findings(&weave.primary, scope),
            vec![disagreement.clone()],
            "doctor {scope:?} on a writable clone the projects disagree on"
        );
    }

    make_read_only(&weave.reference);
    let read_only = (
        "member-permissions".to_owned(),
        REF_PATH.to_owned(),
        Vec::new(),
    );
    for scope in scopes {
        let mut found = role_findings(&weave.primary, scope);
        found.sort();
        assert_eq!(
            found,
            vec![read_only.clone(), disagreement.clone()],
            "doctor {scope:?} on a read-only clone the projects disagree on"
        );
    }

    succeed(&["doctor", "--fix"], &weave.primary);
    assert_writable(
        &weave.reference,
        &format!("after doctor --fix with {PROJECT} active"),
    );
    for scope in scopes {
        assert_eq!(
            role_findings(&weave.primary, scope),
            vec![disagreement.clone()],
            "doctor {scope:?} after --fix"
        );
    }
}

#[test]
fn a_role_made_reference_in_one_project_leaves_a_clone_another_declares_owned_writable() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    weave.commit_manifest(&weave.project_dir(), "owned", "owned");
    add_project(&weave, OTHER_PROJECT, "owned");
    activate(&weave, OTHER_PROJECT);

    succeed(
        &["role", "--project", PROJECT, REF_PATH, "reference"],
        &weave.primary,
    );

    assert_writable(
        &weave.reference,
        &format!("after {PROJECT} made it reference, while {OTHER_PROJECT} declares it owned"),
    );
}
