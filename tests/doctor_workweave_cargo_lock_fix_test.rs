//! Regression tests: `rwv doctor --fix` reported the
//! cargo-workspace `Cargo.lock` as regenerable and then did not regenerate
//! it, and the create-time and doctor-time reports of that finding named two
//! different paths.
//!
//! Two halves, one fixture shape:
//!
//! 1. **`--fix` performs the regeneration it advertises.** A `Cargo.lock` is
//!    `generated_files()` content, and the only thing that authors one is
//!    `cargo generate-lockfile` in the integration's activate hook. The
//!    workweave arm of doctor's content-fix path ran a hook-suppressed
//!    activation, so the fix that the warning named by verb was structurally
//!    incapable of producing the file.
//! 2. **Both reports name the canonical path.** Activation binds the
//!    integration's `output_dir` to `projects/<project>/`; `rwv doctor` binds
//!    it to the weave root, where the same files appear as surfacing
//!    symlinks. For a file that is *missing* in a workweave the root view
//!    names a path that does not exist even as a link, so one finding was
//!    reported under two paths depending on which verb ran it.
//!
//! Both tests drive the shipped binary end to end — the defect lived in the
//! seam between doctor's dispatch, the surfacing step and the hook, which a
//! unit test on either side alone cannot see.

use std::path::{Path, PathBuf};
use std::process;

mod common;

/// Return early (skip) if `cargo` is not on PATH — the fix under test IS
/// `cargo generate-lockfile`, so without cargo there is nothing to observe.
macro_rules! require_cargo {
    () => {
        if which::which("cargo").is_err() {
            eprintln!("skipping test: `cargo` not found on PATH");
            return;
        }
    };
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

fn git_init_with_commit(dir: &Path) {
    git(&["init", "--initial-branch=main"], dir);
    git(&["config", "user.email", "test@test.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "-A"], dir);
    git(&["commit", "-m", "init"], dir);
}

/// The paths a fixture hands back to a test.
struct Fixture {
    _tmp: tempfile::TempDir,
    ww_dir: PathBuf,
    /// stderr+stdout of the `rwv workweave ... create` that made `ww_dir`.
    create_output: String,
}

impl Fixture {
    /// The canonical (committed-location) lock inside the workweave — where
    /// the generated file belongs and where `--fix` must write it.
    fn ww_canonical_lock(&self) -> PathBuf {
        self.ww_dir.join("projects/web-app/Cargo.lock")
    }

    /// The surfacing path at the workweave root — a symlink to
    /// [`Self::ww_canonical_lock`] once the lock exists.
    fn ww_surfaced_lock(&self) -> PathBuf {
        self.ww_dir.join("Cargo.lock")
    }

    fn rwv(&self, args: &[&str], cwd: &Path) -> String {
        let output = common::rwv()
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("rwv should run");
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

/// Build a primary weave with two path-dependency crates and a project repo,
/// author the managed `Cargo.toml`, then create a workweave off it.
///
/// The project repo gitignores `/Cargo.lock` — the default policy for an
/// aggregated workspace, and the reason a fresh workweave never inherits
/// one: the lock is regenerable, so it is not committed, so the workweave's
/// worktree of the project repo does not carry it.
///
/// Install hooks are suppressed for the primary-side authoring pass, so
/// primary has no lock either and the workweave's missing lock cannot be an
/// artifact of a copy that did not happen.
fn fixture() -> Fixture {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    // ---- two crates, protocol <- server by path dependency ----
    let protocol = ws.join("github/chatly/protocol");
    std::fs::create_dir_all(protocol.join("src")).unwrap();
    std::fs::write(
        protocol.join("Cargo.toml"),
        "[package]\nname = \"chatly-protocol\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        protocol.join("src/lib.rs"),
        "pub fn version() -> &'static str { \"1.0\" }\n",
    )
    .unwrap();
    git_init_with_commit(&protocol);

    let server = ws.join("github/chatly/server");
    std::fs::create_dir_all(server.join("src")).unwrap();
    std::fs::write(
        server.join("Cargo.toml"),
        "[package]\nname = \"chatly-server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nchatly-protocol = { path = \"../protocol\" }\n",
    )
    .unwrap();
    std::fs::write(
        server.join("src/main.rs"),
        "fn main() { println!(\"{}\", chatly_protocol::version()); }\n",
    )
    .unwrap();
    git_init_with_commit(&server);

    // ---- project repo ----
    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/chatly/protocol\"]\ntype = \"git\"\nurl = \"https://github.com/chatly/protocol.git\"\nversion = \"main\"\nrole = \"owned\"\n\n[repositories.\"github/chatly/server\"]\ntype = \"git\"\nurl = \"https://github.com/chatly/server.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    // The generated lock is regenerable, so it is not committed.
    std::fs::write(project_dir.join(".gitignore"), "/Cargo.lock\n").unwrap();
    git_init_with_commit(&project_dir);

    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    // ---- author the managed Cargo.toml (no hooks: primary gets no lock) ----
    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "web-app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("primary intent activation should succeed");
    assert!(
        project_dir.join("Cargo.toml").exists(),
        "fixture: the managed Cargo.toml should have been authored at {}",
        project_dir.join("Cargo.toml").display()
    );
    assert!(
        !project_dir.join("Cargo.lock").exists(),
        "fixture: primary must have no lock, or the workweave's missing lock proves nothing"
    );
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "activate"], &project_dir);

    // ---- workweave ----
    let weaveroot = root.join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    let create_output = {
        let output = common::rwv()
            .args(["workweave", "web-app", "create", "agent-1"])
            .current_dir(&ws)
            .output()
            .expect("rwv workweave create should run");
        assert!(
            output.status.success(),
            "fixture: workweave create failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };

    let ww_dir = weaveroot.join("web-app--agent-1");
    assert!(
        ww_dir.join("projects/web-app/Cargo.toml").exists(),
        "fixture: the workweave should carry the committed Cargo.toml"
    );

    Fixture {
        _tmp: tmp,
        ww_dir,
        create_output,
    }
}

/// Pull the path out of a `<name> managed file missing: <path>; run rwv
/// doctor --fix to regenerate` line naming `Cargo.lock`.
///
/// Returns `None` when no such finding is present, which is itself an
/// assertable outcome (after the fix, doctor stops reporting one).
fn missing_lock_path(haystack: &str) -> Option<String> {
    haystack
        .lines()
        .filter_map(|line| line.split_once("managed file missing: "))
        .filter_map(|(_, rest)| rest.split_once(';'))
        .map(|(path, _)| path.trim().to_string())
        .find(|path| path.ends_with("Cargo.lock"))
}

/// Half 1 — the fix a warning names by verb is the fix it performs.
#[test]
fn doctor_fix_in_a_workweave_generates_the_missing_cargo_lock() {
    require_cargo!();
    let f = fixture();

    assert!(
        !f.ww_canonical_lock().exists(),
        "precondition: a fresh workweave has no lock at {}",
        f.ww_canonical_lock().display()
    );

    let fix_output = f.rwv(&["doctor", "--fix"], &f.ww_dir);

    assert!(
        f.ww_canonical_lock().is_file(),
        "`doctor --fix` advertises regeneration of {}, so it must produce it.\n\
         doctor --fix output:\n{fix_output}",
        f.ww_canonical_lock().display()
    );

    // The generation flowed back through the surfacing link rather than
    // landing as a root-level file no repo tracks.
    let surfaced = f.ww_surfaced_lock();
    assert!(
        surfaced
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false),
        "{} should be the surfacing symlink, not a real file.\ndoctor --fix output:\n{fix_output}",
        surfaced.display()
    );
    assert_eq!(
        std::fs::read_link(&surfaced).unwrap(),
        Path::new("projects/web-app/Cargo.lock"),
        "the surfacing link should point at the canonical lock"
    );

    // Both crates resolved into it — the lock is the aggregated workspace's,
    // not an empty shell.
    let lock = std::fs::read_to_string(f.ww_canonical_lock()).unwrap();
    for crate_name in ["chatly-protocol", "chatly-server"] {
        assert!(
            lock.contains(crate_name),
            "generated lock should resolve `{crate_name}`; got:\n{lock}"
        );
    }

    // And the finding is gone: detector and fixer agree afterwards.
    let after = f.rwv(&["doctor"], &f.ww_dir);
    assert_eq!(
        missing_lock_path(&after),
        None,
        "doctor should report no missing lock after --fix regenerated it.\n\
         doctor output:\n{after}"
    );
}

/// Half 2 — one finding, one path, whichever verb reports it.
#[test]
fn create_and_doctor_name_the_same_missing_lock_path() {
    require_cargo!();
    let f = fixture();

    let create_path = missing_lock_path(&f.create_output).unwrap_or_else(|| {
        panic!(
            "workweave create should warn that the lock is missing; got:\n{}",
            f.create_output
        )
    });

    let doctor_output = f.rwv(&["doctor"], &f.ww_dir);
    let doctor_path = missing_lock_path(&doctor_output).unwrap_or_else(|| {
        panic!("doctor should warn that the lock is missing; got:\n{doctor_output}")
    });

    assert_eq!(
        create_path, doctor_path,
        "create-time and doctor-time must name the same file for the same finding"
    );
    assert_eq!(
        doctor_path,
        repoweave::path_spelling::operator_path(&f.ww_canonical_lock()),
        "and that file is the canonical one `--fix` writes, not the weave-root view \
         (which a workweave does not even surface while the source is missing)"
    );
}

/// A real file sitting on the surfacing path is user-held: rwv will not
/// overwrite it, so the generation cannot reach the canonical location. That
/// state is reachable by anyone who runs a bare `cargo build` in a workweave
/// before `doctor --fix` — cargo finds the workspace through the root
/// `Cargo.toml` symlink and drops a real lock beside it.
///
/// Pinned because the honest failure is the whole point: `--fix`
/// must not report success when the file it names is still missing.
///
/// The refusal arrives from link creation, which reaches the orphan before
/// the generator does. What is pinned is that it arrives at all, with the
/// path and the repair in it — not which site minted it.
#[test]
fn doctor_fix_names_the_orphan_when_a_real_file_blocks_the_surfacing_path() {
    require_cargo!();
    let f = fixture();

    std::fs::write(f.ww_surfaced_lock(), "# not a symlink\n").unwrap();

    let fix_output = f.rwv(&["doctor", "--fix"], &f.ww_dir);

    assert!(
        !f.ww_canonical_lock().exists(),
        "precondition for this arm: the canonical lock stays missing"
    );
    assert!(
        fix_output.contains("does not overwrite what is already at"),
        "the failure must name why the generation could not land.\noutput:\n{fix_output}"
    );
    assert!(
        fix_output.contains("remove it and re-run"),
        "the failure must name the repair, not just the obstruction.\noutput:\n{fix_output}"
    );
    assert!(
        fix_output.contains(&f.ww_surfaced_lock().display().to_string()),
        "the failure must name the orphan to remove.\noutput:\n{fix_output}"
    );

    // The named remedy works.
    std::fs::remove_file(f.ww_surfaced_lock()).unwrap();
    let retry = f.rwv(&["doctor", "--fix"], &f.ww_dir);
    assert!(
        f.ww_canonical_lock().is_file(),
        "removing the orphan and re-running --fix should produce the lock.\noutput:\n{retry}"
    );
}
