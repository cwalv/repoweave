//! `rwv role <repo> <role>` edits a member's role in this workspace's
//! manifest, regenerates from it, and settles the member's files.
//!
//!   - both directions, in the primary and in a workweave, with the generated
//!     Cargo workspace dropping and regaining the member;
//!   - a change to `reference` refuses over uncommitted tracked changes;
//!   - an unchanged role leaves rwv.toml alone and settles the files a hand
//!     edit left behind, and an unknown role is rejected;
//!   - a change the regeneration refuses leaves the manifest and the member's
//!     files as they were;
//!   - the change lands through sync-to, the target settles on landing, and
//!     another workweave settles on its next sync.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use repoweave::manifest::{Manifest, Role};

mod common;

fn run(args: &[&str], dir: &Path) -> std::process::Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn describe(out: &std::process::Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
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

/// A bare repo holding a dependency-free library crate named `name`, whose
/// `Cargo.toml` ends with `manifest_tail`.
fn crate_origin(tmp: &Path, name: &str, manifest_tail: &str) -> String {
    let bare = tmp.join("origin").join(format!("{name}.git"));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::git_in(
        tmp,
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    let seed = tmp.join(format!("seed-{name}"));
    common::git_in(
        tmp,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            seed.to_str().unwrap(),
        ],
    );
    common::git_in(&seed, &["config", "user.email", "test@test.com"]);
    common::git_in(&seed, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(seed.join("src")).unwrap();
    std::fs::write(
        seed.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{manifest_tail}"
        ),
    )
    .unwrap();
    std::fs::write(seed.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    common::git_in(&seed, &["add", "-A"]);
    common::git_in(&seed, &["commit", "-qm", "initial"]);
    common::git_in(&seed, &["push", "-q", "origin", "main"]);
    std::fs::remove_dir_all(&seed).unwrap();
    common::file_url(&bare)
}

struct Weave {
    tmp: PathBuf,
    ws: PathBuf,
    /// Manifest-relative path of the member whose role the tests change.
    beta: String,
    alpha: String,
}

impl Weave {
    fn project_dir(&self, workspace: &Path) -> PathBuf {
        workspace.join("projects/demo")
    }

    fn role_of(&self, workspace: &Path, repo: &str) -> Role {
        Manifest::from_path(&self.project_dir(workspace).join("rwv.toml"))
            .unwrap()
            .iter_entries()
            .find(|(path, _)| path.as_str() == repo)
            .map(|(_, entry)| entry.role)
            .unwrap()
    }

    fn generated_members(&self, workspace: &Path) -> String {
        std::fs::read_to_string(self.project_dir(workspace).join("Cargo.toml")).unwrap()
    }

    fn create(&self, name: &str) -> PathBuf {
        succeed(&["workweave", "demo", "create", name], &self.ws);
        self.tmp.join(".workweaves").join(format!("demo--{name}"))
    }
}

/// A weave made by `rwv init demo` holding two owned library crates, alpha
/// and beta, with the project committed and locked.
fn make_weave(tmp: &Path) -> Weave {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    succeed(&["init", "demo"], &ws);
    let alpha_url = crate_origin(tmp, "alpha", "");
    let beta_url = crate_origin(tmp, "beta", "");
    succeed(&["add", &alpha_url], &ws);
    succeed(&["add", &beta_url], &ws);
    let project = ws.join("projects/demo");
    common::git_in(&project, &["config", "user.email", "test@test.com"]);
    common::git_in(&project, &["config", "user.name", "Test"]);
    succeed(&["lock"], &ws);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-qm", "alpha and beta"]);

    let weave = Weave {
        tmp: tmp.to_path_buf(),
        alpha: path_of(&ws, &alpha_url),
        beta: path_of(&ws, &beta_url),
        ws,
    };
    assert!(
        weave.generated_members(&weave.ws).contains(&weave.beta),
        "fixture: the generated Cargo workspace lists beta as a member"
    );
    weave
}

/// The path `rwv.toml` in `workspace` lists the repo at `url` under.
fn path_of(workspace: &Path, url: &str) -> String {
    Manifest::from_path(&workspace.join("projects/demo/rwv.toml"))
        .unwrap()
        .iter_entries()
        .find(|(_, entry)| entry.url.to_string() == url)
        .map(|(path, _)| path.as_str().to_owned())
        .unwrap()
}

fn tracked_regular_files(checkout: &Path) -> Vec<PathBuf> {
    let files: Vec<PathBuf> = common::git_in(checkout, &["ls-files", "-s"])
        .lines()
        .filter_map(|line| {
            let (meta, path) = line.split_once('\t')?;
            matches!(meta.split(' ').next(), Some("100644" | "100755")).then(|| checkout.join(path))
        })
        .collect();
    assert!(!files.is_empty(), "{} tracks no file", checkout.display());
    files
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode()
}

fn assert_read_only(checkout: &Path, when: &str) {
    for file in tracked_regular_files(checkout) {
        assert_eq!(
            mode(&file) & 0o222,
            0,
            "{when}: {} is writable",
            file.display()
        );
    }
}

fn assert_writable(checkout: &Path, when: &str) {
    for file in tracked_regular_files(checkout) {
        assert_ne!(
            mode(&file) & 0o200,
            0,
            "{when}: {} is read-only",
            file.display()
        );
    }
}

/// The lines of `stderr` rwv itself wrote, without the target banner or the
/// progress cargo prints when a regeneration's cargo waits on a lock another
/// cargo holds.
fn rwv_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| !line.starts_with("target: "))
        .filter(|line| !line.starts_with("    Blocking waiting for file lock on "))
        .collect()
}

#[test]
fn rwv_lines_drops_cargo_lock_waits_and_keeps_rwv_output() {
    let stderr = "target: workspace /w · project demo (.rwv-active)\n    \
                  Blocking waiting for file lock on package cache\n\
                  'a/b/c' is already owned; nothing to change\n    \
                  Blocking waiting for file lock on build directory\n";
    assert_eq!(
        rwv_lines(stderr),
        vec!["'a/b/c' is already owned; nothing to change"]
    );
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, vec![token], "stderr was:\n{stderr}");
}

/// Both directions in `workspace`: the manifest, the generated Cargo
/// workspace and the member's files follow the role each way.
fn round_trip(weave: &Weave, workspace: &Path) {
    let beta = workspace.join(&weave.beta);
    assert_writable(&beta, "before");

    succeed(&["role", &weave.beta, "reference"], workspace);
    assert_eq!(weave.role_of(workspace, &weave.beta), Role::Reference);
    assert_read_only(&beta, "after `rwv role ... reference`");
    let generated = weave.generated_members(workspace);
    assert!(
        !generated.contains(&weave.beta) && generated.contains(&weave.alpha),
        "a reference member leaves the generated Cargo workspace:\n{generated}"
    );

    succeed(&["role", &weave.beta, "owned"], workspace);
    assert_eq!(weave.role_of(workspace, &weave.beta), Role::Owned);
    assert_writable(&beta, "after `rwv role ... owned`");
    assert!(
        weave.generated_members(workspace).contains(&weave.beta),
        "an owned member rejoins the generated Cargo workspace"
    );
}

#[test]
fn role_changes_both_ways_in_the_primary() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    round_trip(&weave, &weave.ws);
}

#[test]
fn role_changes_both_ways_in_a_workweave_and_stays_local() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");

    succeed(&["role", &weave.beta, "reference"], &seat);
    assert_read_only(&seat.join(&weave.beta), "in the workweave");
    assert_writable(
        &weave.ws.join(&weave.beta),
        "the primary, which has not seen the change",
    );
    assert_eq!(weave.role_of(&weave.ws, &weave.beta), Role::Owned);
    succeed(&["role", &weave.beta, "owned"], &seat);

    round_trip(&weave, &seat);
}

#[test]
fn a_change_to_reference_refuses_over_uncommitted_changes() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let beta = weave.ws.join(&weave.beta);
    std::fs::write(beta.join("src/lib.rs"), "pub fn edited() {}\n").unwrap();
    let manifest = std::fs::read(weave.project_dir(&weave.ws).join("rwv.toml")).unwrap();

    let out = run(&["role", &weave.beta, "reference"], &weave.ws);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    assert!(
        stderr.contains("src/lib.rs")
            && stderr.contains("Commit them, stash them, or discard them"),
        "the refusal names the file and the remedies: {stderr}"
    );
    assert_routes_to(&stderr, "dirty-checkout");
    assert_eq!(
        std::fs::read(weave.project_dir(&weave.ws).join("rwv.toml")).unwrap(),
        manifest,
        "nothing is written"
    );
    assert_writable(&beta, "after the refusal");
}

#[test]
fn an_unchanged_role_changes_nothing() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let project = weave.project_dir(&weave.ws);
    let before = common::git_in(&project, &["status", "--porcelain"]);
    let manifest = std::fs::read(project.join("rwv.toml")).unwrap();
    let written_at = std::fs::metadata(project.join("rwv.toml"))
        .unwrap()
        .modified()
        .unwrap();

    let out = succeed(&["role", &weave.alpha, "owned"], &weave.ws);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        rwv_lines(&stderr),
        vec![format!(
            "'{}' is already owned; nothing to change",
            weave.alpha
        )],
        "{}",
        describe(&out)
    );
    assert_eq!(std::fs::read(project.join("rwv.toml")).unwrap(), manifest);
    assert_eq!(
        std::fs::metadata(project.join("rwv.toml"))
            .unwrap()
            .modified()
            .unwrap(),
        written_at,
        "rwv.toml is not rewritten"
    );
    assert_eq!(common::git_in(&project, &["status", "--porcelain"]), before);
}

fn manifest_mtime(weave: &Weave) -> std::time::SystemTime {
    std::fs::metadata(weave.project_dir(&weave.ws).join("rwv.toml"))
        .unwrap()
        .modified()
        .unwrap()
}

#[test]
fn an_unchanged_reference_role_makes_the_files_read_only() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let beta = weave.ws.join(&weave.beta);
    let toml = weave.project_dir(&weave.ws).join("rwv.toml");
    let edited = std::fs::read_to_string(&toml).unwrap();
    let section = edited.find(&weave.beta).unwrap();
    let (head, tail) = edited.split_at(section);
    let tail = tail.replacen("role = \"owned\"", "role = \"reference\"", 1);
    std::fs::write(&toml, format!("{head}{tail}")).unwrap();
    assert_eq!(weave.role_of(&weave.ws, &weave.beta), Role::Reference);
    assert_writable(&beta, "fixture: the hand edit left the files writable");
    let written_at = manifest_mtime(&weave);
    let manifest = std::fs::read(&toml).unwrap();

    let out = succeed(&["role", &weave.beta, "reference"], &weave.ws);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains(&format!("'{}' is already reference; settled ", weave.beta)),
        "{}",
        describe(&out)
    );
    assert_read_only(&beta, "after re-running the role");
    let generated = weave.generated_members(&weave.ws);
    assert!(
        !generated.contains(&weave.beta) && generated.contains(&weave.alpha),
        "the generated Cargo workspace drops the reference member:\n{generated}"
    );
    assert_eq!(std::fs::read(&toml).unwrap(), manifest);
    assert_eq!(
        manifest_mtime(&weave),
        written_at,
        "rwv.toml is not rewritten"
    );
}

#[test]
fn an_unchanged_owned_role_makes_a_shared_read_only_file_a_private_writable_copy() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let beta = weave.ws.join(&weave.beta);
    let file = beta.join("src/lib.rs");
    let twin = tmp.path().join("twin-of-lib.rs");
    std::fs::hard_link(&file, &twin).unwrap();
    for f in tracked_regular_files(&beta) {
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    assert_read_only(&beta, "fixture: a read-only owned member");
    let written_at = manifest_mtime(&weave);

    let out = succeed(&["role", &weave.beta, "owned"], &weave.ws);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains(&format!("'{}' is already owned; settled ", weave.beta)),
        "{}",
        describe(&out)
    );
    assert_writable(&beta, "after re-running the role");
    assert_eq!(
        std::os::unix::fs::MetadataExt::nlink(&std::fs::metadata(&file).unwrap()),
        1,
        "the file is a private copy"
    );
    assert_eq!(mode(&twin) & 0o222, 0, "the other link stays read-only");
    assert_eq!(
        manifest_mtime(&weave),
        written_at,
        "rwv.toml is not rewritten"
    );
}

#[test]
fn a_change_the_regeneration_refuses_leaves_the_member_as_it_was() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let url = crate_origin(tmp.path(), "nested", "\n[workspace]\nmembers = []\n");
    succeed(&["add", &url, "--role", "reference"], &weave.ws);
    let nested = path_of(&weave.ws, &url);
    let member = weave.ws.join(&nested);
    assert_read_only(&member, "fixture: a reference member");
    let manifest = std::fs::read(weave.project_dir(&weave.ws).join("rwv.toml")).unwrap();

    let out = run(&["role", &nested, "owned"], &weave.ws);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{}", describe(&out));
    assert!(
        stderr.contains("declares its own [workspace]") && stderr.contains("rwv.toml is unchanged"),
        "the refusal names its cause and says nothing landed: {stderr}"
    );
    assert_eq!(
        std::fs::read(weave.project_dir(&weave.ws).join("rwv.toml")).unwrap(),
        manifest
    );
    assert_eq!(weave.role_of(&weave.ws, &nested), Role::Reference);
    assert_read_only(&member, "after the refused change");
}

#[test]
fn an_unknown_role_or_repo_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());

    let out = run(&["role", &weave.alpha, "upstream"], &weave.ws);
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("reference"),
        "clap lists the roles it accepts: {}",
        describe(&out)
    );

    let out = run(&["role", "github/nobody/nothing", "reference"], &weave.ws);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_routes_to(&stderr, "repo-not-in-manifest");
}

#[test]
fn a_role_change_lands_through_sync_to_and_reaches_other_workweaves() {
    let tmp = common::tempdir().unwrap();
    let weave = make_weave(tmp.path());
    let seat = weave.create("seat");
    let other = weave.create("other");

    succeed(&["role", &weave.beta, "reference"], &seat);
    let seat_project = weave.project_dir(&seat);
    common::git_in(&seat_project, &["add", "-A"]);
    common::git_in(
        &seat_project,
        &["commit", "-qm", "beta becomes a reference"],
    );
    succeed(&["lock", "--commit"], &seat);
    succeed(&["sync-to", &weave.ws.to_string_lossy()], &seat);

    assert_eq!(weave.role_of(&weave.ws, &weave.beta), Role::Reference);
    assert_read_only(&weave.ws.join(&weave.beta), "the target, on landing");

    assert_writable(
        &other.join(&weave.beta),
        "another workweave, before its sync",
    );
    succeed(&["sync", "primary"], &other);
    assert_read_only(
        &other.join(&weave.beta),
        "another workweave, on its next sync",
    );
}
