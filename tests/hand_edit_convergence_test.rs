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
