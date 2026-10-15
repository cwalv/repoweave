//! An intent verb finishes what a hand edit to `rwv.toml` started for its own
//! entry: `rwv add <url>` over an entry already listed materializes it, and
//! `rwv remove <path>` over an entry already gone regenerates without it.

use std::path::{Path, PathBuf};

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

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A bare repo at `<tmp>/origin/<name>.git` holding a dependency-free library
/// crate named `name`, and its `file://` URL.
fn crate_origin(tmp: &Path, name: &str) -> String {
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
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .unwrap();
    std::fs::write(seed.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    common::git_in(&seed, &["add", "-A"]);
    common::git_in(&seed, &["commit", "-qm", "initial"]);
    common::git_in(&seed, &["push", "-q", "origin", "main"]);
    std::fs::remove_dir_all(&seed).unwrap();
    common::file_url(&bare)
}

/// The member path `rwv add` places a crate made by [`crate_origin`] at.
fn placed(name: &str) -> String {
    format!("local/origin/{name}")
}

struct Weave {
    tmp: tempfile::TempDir,
    ws: PathBuf,
}

impl Weave {
    /// A weave made by `rwv init demo` holding the owned crate `alpha`, with
    /// the project committed and locked.
    fn new() -> Self {
        let tmp = common::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join("projects")).unwrap();
        succeed(&["init", "demo"], &ws);
        let alpha = crate_origin(tmp.path(), "alpha");
        succeed(&["add", &alpha], &ws);
        let project = ws.join("projects/demo");
        common::git_in(&project, &["config", "user.email", "test@test.com"]);
        common::git_in(&project, &["config", "user.name", "Test"]);
        succeed(&["lock"], &ws);
        common::git_in(&project, &["add", "-A"]);
        common::git_in(&project, &["commit", "-qm", "alpha"]);
        Weave { tmp, ws }
    }

    fn origin(&self, name: &str) -> String {
        crate_origin(self.tmp.path(), name)
    }

    fn workweave(&self, name: &str) -> PathBuf {
        succeed(&["workweave", "demo", "create", name], &self.ws);
        self.tmp
            .path()
            .join(".workweaves")
            .join(format!("demo--{name}"))
    }
}

fn manifest(workspace: &Path) -> PathBuf {
    workspace.join("projects/demo/rwv.toml")
}

fn generated_members(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join("projects/demo/Cargo.toml")).unwrap()
}

fn hand_add(workspace: &Path, path: &str, url: &str) {
    let mut text = std::fs::read_to_string(manifest(workspace)).unwrap();
    text.push_str(&format!(
        "\n[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
    ));
    std::fs::write(manifest(workspace), text).unwrap();
}

/// What `rwv doctor` finds in `workspace`.
fn doctor_findings(workspace: &Path) -> String {
    let out = run(&["doctor"], workspace);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What `rwv doctor` finds after a plain `rwv add` of a fresh `beta`, in the
/// primary or, given a workweave name, in that workweave.
fn doctor_after_a_plain_add(workweave: Option<&str>) -> String {
    let control = Weave::new();
    let workspace = match workweave {
        Some(name) => control.workweave(name),
        None => control.ws.clone(),
    };
    let beta = control.origin("beta");
    succeed(&["add", &beta], &workspace);
    doctor_findings(&workspace)
}

#[test]
fn add_over_a_hand_added_entry_clones_it_and_regenerates_in_the_primary() {
    let weave = Weave::new();
    let beta = weave.origin("beta");
    hand_add(&weave.ws, &placed("beta"), &beta);
    let manifest_before = std::fs::read(manifest(&weave.ws)).unwrap();

    let out = succeed(&["add", &beta], &weave.ws);

    assert!(
        stderr_of(&out).contains("already exists in manifest"),
        "{}",
        describe(&out)
    );
    let clone = weave.ws.join(placed("beta"));
    assert!(
        clone.join("Cargo.toml").is_file(),
        "the add must clone the hand-added entry at its placement"
    );
    assert!(
        generated_members(&weave.ws).contains(&placed("beta")),
        "the regeneration must list the hand-added member"
    );
    assert_eq!(std::fs::read(manifest(&weave.ws)).unwrap(), manifest_before);
    assert_eq!(
        doctor_findings(&weave.ws),
        doctor_after_a_plain_add(None),
        "a finished hand edit must leave what a plain add leaves"
    );
}

#[test]
fn add_over_a_hand_added_entry_creates_the_workweave_worktree() {
    let weave = Weave::new();
    let workweave = weave.workweave("feat");
    let beta = weave.origin("beta");
    hand_add(&workweave, &placed("beta"), &beta);

    succeed(&["add", &beta], &workweave);

    let canonical = weave.ws.join(placed("beta"));
    assert!(
        canonical.join(".git").is_dir(),
        "the canonical clone lands in the primary"
    );
    let worktree = workweave.join(placed("beta"));
    assert!(
        worktree.join(".git").is_file() && worktree.join("Cargo.toml").is_file(),
        "the workweave gets a linked worktree of the canonical clone"
    );
    assert!(generated_members(&workweave).contains(&placed("beta")));
    assert!(
        !generated_members(&weave.ws).contains(&placed("beta")),
        "the primary's manifest never named beta, so its generation must not"
    );
    assert_eq!(
        doctor_findings(&workweave),
        doctor_after_a_plain_add(Some("feat")),
        "a finished hand edit must leave what a plain add leaves"
    );
}

#[test]
fn add_refuses_a_url_that_disagrees_with_the_hand_added_entry() {
    let weave = Weave::new();
    let beta = weave.origin("beta");
    let elsewhere = weave.tmp.path().join("elsewhere/origin/beta.git");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let recorded = common::file_url(&elsewhere);
    hand_add(&weave.ws, &placed("beta"), &recorded);
    let manifest_before = std::fs::read(manifest(&weave.ws)).unwrap();

    let out = run(&["add", &beta], &weave.ws);
    let stderr = stderr_of(&out);

    assert!(!out.status.success(), "the add must refuse; {stderr}");
    assert!(
        stderr.contains(&recorded) && stderr.contains(&beta),
        "the refusal must name both URLs; {stderr}"
    );
    assert!(
        stderr.contains("rwv explain occupied-placement"),
        "{stderr}"
    );
    assert!(
        !weave.ws.join(placed("beta")).exists(),
        "a refused add clones nothing"
    );
    assert_eq!(std::fs::read(manifest(&weave.ws)).unwrap(), manifest_before);
}

/// The weave root's top-level symlinks.
fn root_links(ws: &Path) -> Vec<String> {
    let mut links: Vec<String> = std::fs::read_dir(ws)
        .unwrap()
        .flatten()
        .filter(|e| e.path().symlink_metadata().unwrap().is_symlink())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    links.sort();
    links
}

#[test]
fn re_running_add_new_with_the_same_address_repairs_the_surfacing() {
    let weave = Weave::new();
    let root = weave.tmp.path().join("upstreams");
    std::fs::create_dir_all(&root).unwrap();
    let root_param = format!("root={}", root.display());
    let args = ["add", "local/acme/fresh", "--new", "--param", &root_param];
    succeed(&args, &weave.ws);
    assert!(
        weave.ws.join("local/acme/fresh/.git").is_dir(),
        "fixture: the creation materializes the member"
    );
    let surfaced = root_links(&weave.ws);
    assert!(
        !surfaced.is_empty(),
        "fixture: the weave must have surfaced something to lose"
    );
    for name in &surfaced {
        std::fs::remove_file(weave.ws.join(name)).unwrap();
    }

    let again = succeed(&args, &weave.ws);

    assert!(
        stderr_of(&again).contains("already exists in manifest"),
        "{}",
        describe(&again)
    );
    assert_eq!(root_links(&weave.ws), surfaced);
}

fn hand_remove(workspace: &Path, path: &str) {
    let mut listed = repoweave::manifest::Manifest::from_path(&manifest(workspace)).unwrap();
    listed
        .remove_repo(&repoweave::manifest::RepoPath::new(path).unwrap())
        .expect("fixture: the manifest lists the entry being hand-removed");
    listed.write(&manifest(workspace)).unwrap();
}

#[test]
fn remove_over_a_hand_removed_entry_regenerates_and_keeps_the_clone_in_the_primary() {
    let weave = Weave::new();
    let alpha = placed("alpha");
    hand_remove(&weave.ws, &alpha);
    assert!(
        generated_members(&weave.ws).contains(&alpha),
        "fixture: a hand edit alone regenerates nothing"
    );
    let manifest_before = std::fs::read(manifest(&weave.ws)).unwrap();

    let out = succeed(&["remove", &alpha], &weave.ws);

    assert!(
        stderr_of(&out).contains("already out of the manifest"),
        "{}",
        describe(&out)
    );
    assert!(!generated_members(&weave.ws).contains(&alpha));
    assert!(weave.ws.join(&alpha).join(".git").is_dir());
    assert_eq!(std::fs::read(manifest(&weave.ws)).unwrap(), manifest_before);
}

#[test]
fn remove_delete_over_a_hand_removed_entry_deletes_the_clone_in_the_primary() {
    let weave = Weave::new();
    let alpha = placed("alpha");
    hand_remove(&weave.ws, &alpha);

    succeed(&["remove", &alpha, "--delete"], &weave.ws);

    assert!(!weave.ws.join(&alpha).exists());
    assert!(!generated_members(&weave.ws).contains(&alpha));
}

#[test]
fn remove_over_a_hand_removed_entry_in_a_workweave_keeps_then_deletes_its_worktree() {
    let weave = Weave::new();
    let workweave = weave.workweave("feat");
    let alpha = placed("alpha");
    hand_remove(&workweave, &alpha);

    succeed(&["remove", &alpha], &workweave);
    assert!(!generated_members(&workweave).contains(&alpha));
    assert!(workweave.join(&alpha).join(".git").is_file());

    succeed(&["remove", &alpha, "--delete"], &workweave);
    assert!(!workweave.join(&alpha).exists());
    assert!(
        weave.ws.join(&alpha).join(".git").is_dir(),
        "a workweave's delete removes its own worktree, never the canonical clone"
    );
}

#[test]
fn remove_delete_in_a_workweave_removes_its_worktree_and_never_the_canonical_clone() {
    let weave = Weave::new();
    let workweave = weave.workweave("feat");
    let alpha = placed("alpha");

    succeed(&["remove", &alpha, "--delete"], &workweave);

    assert!(!workweave.join(&alpha).exists());
    assert!(weave.ws.join(&alpha).join(".git").is_dir());
    assert!(generated_members(&weave.ws).contains(&alpha));
    assert!(!generated_members(&workweave).contains(&alpha));
}

/// `rwv remove <alpha> --delete` in `workspace` refuses with `token`,
/// leaving `rwv.toml` and the checkout as they were.
fn assert_delete_refused(workspace: &Path, token: &str) {
    let alpha = placed("alpha");
    let manifest_before = std::fs::read(manifest(workspace)).unwrap();

    let out = run(&["remove", &alpha, "--delete"], workspace);
    let stderr = stderr_of(&out);

    assert!(!out.status.success(), "the delete must refuse; {stderr}");
    assert!(
        stderr.trim_end().ends_with(&format!("rwv explain {token}")),
        "{stderr}"
    );
    assert_eq!(std::fs::read(manifest(workspace)).unwrap(), manifest_before);
    assert!(workspace.join(&alpha).exists());
}

fn commit_in(checkout: &Path) {
    common::git_in(checkout, &["config", "user.email", "test@test.com"]);
    common::git_in(checkout, &["config", "user.name", "Test"]);
    std::fs::write(checkout.join("NEW"), "unique\n").unwrap();
    common::git_in(checkout, &["add", "NEW"]);
    common::git_in(checkout, &["commit", "-qm", "unique"]);
}

#[test]
fn remove_delete_refuses_a_dirty_clone_in_the_primary() {
    let weave = Weave::new();
    std::fs::write(weave.ws.join(placed("alpha")).join("src/lib.rs"), "dirty\n").unwrap();
    assert_delete_refused(&weave.ws, "dirty-checkout");
}

#[test]
fn remove_delete_refuses_a_dirty_worktree_in_a_workweave() {
    let weave = Weave::new();
    let workweave = weave.workweave("feat");
    std::fs::write(
        workweave.join(placed("alpha")).join("src/lib.rs"),
        "dirty\n",
    )
    .unwrap();
    assert_delete_refused(&workweave, "dirty-checkout");
}

#[test]
fn remove_delete_refuses_a_clone_with_unpushed_commits_in_the_primary() {
    let weave = Weave::new();
    commit_in(&weave.ws.join(placed("alpha")));
    assert_delete_refused(&weave.ws, "dropped-repo-has-unique-commits");
}

#[test]
fn remove_delete_refuses_a_worktree_with_unique_commits_in_a_workweave() {
    let weave = Weave::new();
    let workweave = weave.workweave("feat");
    commit_in(&workweave.join(placed("alpha")));
    assert_delete_refused(&workweave, "dropped-repo-has-unique-commits");
}

#[test]
fn remove_delete_over_a_hand_removed_entry_refuses_a_clone_another_project_lists() {
    let weave = Weave::new();
    let alpha = placed("alpha");
    let other = weave.ws.join("projects/other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::copy(manifest(&weave.ws), other.join("rwv.toml")).unwrap();
    hand_remove(&weave.ws, &alpha);
    assert_delete_refused(&weave.ws, "shared-clone-referenced");
}

#[test]
fn remove_refuses_a_path_whose_clone_another_project_lists() {
    let weave = Weave::new();
    let beta_url = weave.origin("beta");
    let beta = placed("beta");
    let other = weave.ws.join("projects/other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("rwv.toml"),
        format!(
            "[repositories.\"{beta}\"]\ntype = \"git\"\nurl = \"{beta_url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    let clone = weave.ws.join(&beta);
    std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
    common::git_in(
        &weave.ws,
        &["clone", "-q", &beta_url, clone.to_str().unwrap()],
    );

    let out = run(&["remove", &beta, "--delete"], &weave.ws);
    let stderr = stderr_of(&out);

    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr
            .trim_end()
            .ends_with("rwv explain repo-not-in-manifest"),
        "{stderr}"
    );
    assert!(clone.join(".git").is_dir());
}

#[test]
fn remove_refuses_a_path_that_holds_nothing() {
    let weave = Weave::new();
    let out = run(&["remove", "local/origin/ghost"], &weave.ws);
    let stderr = stderr_of(&out);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr
            .trim_end()
            .ends_with("rwv explain repo-not-in-manifest"),
        "{stderr}"
    );
}

/// Put a directory where `workspace`'s surfaced `Cargo.lock` link belongs,
/// which no regeneration may take over, so the next add refuses before its
/// install hooks.
fn occupy_cargo_lock(workspace: &Path) {
    let link = workspace.join("Cargo.lock");
    assert!(
        link.symlink_metadata().unwrap().is_symlink(),
        "fixture: the weave surfaces a Cargo.lock link"
    );
    std::fs::remove_file(&link).unwrap();
    std::fs::create_dir(&link).unwrap();
    std::fs::write(link.join("mine"), "# not rwv's\n").unwrap();
}

fn vacate_cargo_lock(workspace: &Path) {
    std::fs::remove_dir_all(workspace.join("Cargo.lock")).unwrap();
}

fn refused(args: &[&str], workspace: &Path) -> String {
    let out = run(args, workspace);
    let stderr = stderr_of(&out);
    assert!(
        !out.status.success(),
        "`rwv {}` must refuse; {stderr}",
        args.join(" ")
    );
    assert!(
        stderr
            .trim_end()
            .ends_with("rwv explain surfacing-path-occupied"),
        "{stderr}"
    );
    stderr
}

#[test]
fn a_refused_url_add_names_the_clone_it_keeps_and_the_command_that_removes_it() {
    let weave = Weave::new();
    let beta_url = weave.origin("beta");
    let beta = placed("beta");
    occupy_cargo_lock(&weave.ws);

    let stderr = refused(&["add", &beta_url], &weave.ws);

    let clone = weave.ws.join(&beta);
    assert!(
        clone.join(".git").is_dir(),
        "the clone is kept for a re-run"
    );
    assert!(
        stderr.contains(&format!("kept the clone it made at {}", clone.display()))
            && stderr.contains(&format!("`rwv add {beta_url}` reuses it"))
            && stderr.contains(&format!(
                "`rwv remove {beta} --delete` in the primary weave"
            )),
        "{stderr}"
    );

    vacate_cargo_lock(&weave.ws);
    succeed(&["remove", &beta, "--delete"], &weave.ws);
    assert!(!clone.exists(), "the named command removes what was named");
}

#[test]
fn a_refused_url_add_in_a_workweave_names_its_worktree_and_its_clone() {
    let weave = Weave::new();
    let workweave = weave.workweave("feat");
    let beta_url = weave.origin("beta");
    let beta = placed("beta");
    occupy_cargo_lock(&workweave);

    let stderr = refused(&["add", &beta_url], &workweave);

    let worktree = workweave.join(&beta);
    let clone = weave.ws.join(&beta);
    assert!(worktree.join(".git").is_file() && clone.join(".git").is_dir());
    assert!(
        stderr.contains(&format!(
            "kept the worktree it made at {}",
            worktree.display()
        )) && stderr.contains(&format!("`rwv remove {beta} --delete` in this workweave"))
            && stderr.contains(&format!("kept the clone it made at {}", clone.display())),
        "{stderr}"
    );

    vacate_cargo_lock(&workweave);
    succeed(&["remove", &beta, "--delete"], &workweave);
    assert!(!worktree.exists());
}

#[test]
fn a_refused_local_path_add_in_a_workweave_names_only_the_worktree_it_made() {
    let weave = Weave::new();
    let workweave = weave.workweave("feat");
    let beta_url = weave.origin("beta");
    let beta = placed("beta");
    let clone = weave.ws.join(&beta);
    std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
    common::git_in(
        &weave.ws,
        &["clone", "-q", &beta_url, clone.to_str().unwrap()],
    );
    occupy_cargo_lock(&workweave);

    let stderr = refused(&["add", &beta], &workweave);

    let worktree = workweave.join(&beta);
    assert!(
        stderr.contains(&format!(
            "kept the worktree it made at {}",
            worktree.display()
        )),
        "{stderr}"
    );
    assert!(
        !stderr.contains("kept the clone"),
        "the clone was there before the add; {stderr}"
    );
}

#[test]
fn a_refused_url_add_over_an_existing_clone_names_nothing() {
    let weave = Weave::new();
    let beta_url = weave.origin("beta");
    let clone = weave.ws.join(placed("beta"));
    std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
    common::git_in(
        &weave.ws,
        &["clone", "-q", &beta_url, clone.to_str().unwrap()],
    );
    occupy_cargo_lock(&weave.ws);

    let stderr = refused(&["add", &beta_url], &weave.ws);

    assert!(!stderr.contains("kept the"), "{stderr}");
    assert!(clone.join(".git").is_dir());
}

#[test]
fn a_refused_add_new_takes_back_its_upstream_and_its_member() {
    let weave = Weave::new();
    let root = weave.tmp.path().join("upstreams");
    std::fs::create_dir_all(&root).unwrap();
    let root_param = format!("root={}", root.display());
    occupy_cargo_lock(&weave.ws);

    let stderr = refused(
        &["add", "local/acme/fresh", "--new", "--param", &root_param],
        &weave.ws,
    );

    let upstream = root.join("acme/fresh");
    let member = weave.ws.join("local/acme/fresh");
    assert!(
        !upstream.exists() && !root.join("acme").exists(),
        "the bare upstream and the directory made for it are gone"
    );
    assert!(!member.exists() && !weave.ws.join("local/acme").exists());
    assert!(root.is_dir(), "the root existed before the add");
    assert!(
        stderr.contains(&format!("removed {}", upstream.display()))
            && stderr.contains(&format!("removed {}", member.display())),
        "{stderr}"
    );
}

/// A `PATH` holding `git` and a `cargo` that fails every command.
#[cfg(unix)]
fn failing_cargo_path(bin: &Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;
    let path = common::cargo_stand_in_path(bin);
    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        "#!/bin/sh\necho 'stand-in cargo: failing' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn a_failed_install_hook_leaves_the_add_standing_and_says_so() {
    let weave = Weave::new();
    let beta_url = weave.origin("beta");
    let beta = placed("beta");
    let path = failing_cargo_path(&weave.tmp.path().join("failing-bin"));

    let out = common::rwv()
        .args(["add", &beta_url])
        .current_dir(&weave.ws)
        .env("PATH", &path)
        .output()
        .unwrap();
    let stderr = stderr_of(&out);

    assert!(
        !out.status.success(),
        "the hook failure must surface; {stderr}"
    );
    assert!(
        stderr.contains("stand-in cargo: failing"),
        "fixture: the failure is the install hook's; {stderr}"
    );
    assert!(
        stderr.contains(
            "rwv.toml was updated and the integration files were regenerated from it; \
             only the install hooks did not complete"
        ),
        "{stderr}"
    );
    assert!(
        std::fs::read_to_string(manifest(&weave.ws))
            .unwrap()
            .contains(&beta),
        "the manifest edit stands"
    );
    assert!(generated_members(&weave.ws).contains(&beta));
    assert!(
        !stderr.contains("kept the"),
        "a standing member is not residue; {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn a_failed_install_hook_after_add_new_keeps_what_it_created() {
    let weave = Weave::new();
    let root = weave.tmp.path().join("upstreams");
    std::fs::create_dir_all(&root).unwrap();
    let root_param = format!("root={}", root.display());
    let path = failing_cargo_path(&weave.tmp.path().join("failing-bin"));

    let out = common::rwv()
        .args(["add", "local/acme/fresh", "--new", "--param", &root_param])
        .current_dir(&weave.ws)
        .env("PATH", &path)
        .output()
        .unwrap();
    let stderr = stderr_of(&out);

    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("only the install hooks did not complete"),
        "{stderr}"
    );
    assert!(
        root.join("acme/fresh").is_dir() && weave.ws.join("local/acme/fresh/.git").is_dir(),
        "the manifest lists the member, so nothing it created is taken back; {stderr}"
    );
}

#[test]
fn a_url_add_refused_before_its_hooks_converges_on_a_rerun_reusing_its_checkouts() {
    let weave = Weave::new();
    let workweave = weave.workweave("feat");
    let beta_url = weave.origin("beta");
    let beta = placed("beta");
    occupy_cargo_lock(&workweave);
    refused(&["add", &beta_url], &workweave);
    let clone = weave.ws.join(&beta);
    let worktree = workweave.join(&beta);
    std::fs::write(clone.join("kept-clone"), "").unwrap();
    std::fs::write(worktree.join("kept-worktree"), "").unwrap();
    vacate_cargo_lock(&workweave);

    let out = succeed(&["add", &beta_url], &workweave);

    let stderr = stderr_of(&out);
    assert!(stderr.contains("skipping clone"), "{stderr}");
    assert!(
        clone.join("kept-clone").exists() && worktree.join("kept-worktree").exists(),
        "the re-run reuses the clone and the worktree the refused run made"
    );
    assert!(std::fs::read_to_string(manifest(&workweave))
        .unwrap()
        .contains(&beta));
    assert!(generated_members(&workweave).contains(&beta));
    assert!(workweave
        .join("Cargo.lock")
        .symlink_metadata()
        .unwrap()
        .is_symlink());
}
