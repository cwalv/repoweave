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

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

mod common;

/// Return early (skip) if `cargo` is not on PATH — the fix under test IS
/// `cargo generate-lockfile`, so without cargo there is nothing to observe.
macro_rules! require_cargo {
    () => {
        if common::skip_without_tool("cargo") {
            return;
        }
    };
}

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// The paths a fixture hands back to a test.
struct Fixture {
    tmp: tempfile::TempDir,
    ww_dir: PathBuf,
    /// stderr+stdout of the `rwv workweave ... create` that made `ww_dir`.
    create_output: String,
}

impl Fixture {
    /// A path beside the weave rather than inside it, for fixture apparatus
    /// that must not read as workspace content.
    fn beside_the_weave(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

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
        self.rwv_with_path(args, cwd, None)
    }

    /// `Some(path)` replaces the child's whole `PATH`, so which tools the run
    /// can reach is the caller's decision rather than the machine's; `None`
    /// inherits.
    fn rwv_with_path(&self, args: &[&str], cwd: &Path, path: Option<&OsStr>) -> String {
        let mut cmd = common::rwv();
        cmd.args(args).current_dir(cwd);
        if let Some(path) = path {
            cmd.env("PATH", path);
        }
        let output = cmd.output().expect("rwv should run");
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
    common::git_in(&project_dir, &["add", "-A"]);
    common::git_in(&project_dir, &["commit", "-m", "activate"]);

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
        tmp,
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
///
/// Both reports are rwv's own text about a file that is MISSING, so nothing
/// here needs the generator to have run — only for `doctor` to reach the
/// content axis, which it declines to do with no cargo reachable at all. The
/// run therefore gets a stand-in `cargo` and no other tool, and the two paths
/// are compared the same way on a machine with no toolchain installed.
///
/// Unix only, since the stand-in is a script resolved off `PATH` —
/// `common::cargo_stand_in_path` states why that does not port.
#[cfg(unix)]
#[test]
fn create_and_doctor_name_the_same_missing_lock_path() {
    let f = fixture();
    let path = common::cargo_stand_in_path(&f.beside_the_weave("stand-in-bin"));
    let path = Some(path.as_os_str());

    let create_path = missing_lock_path(&f.create_output).unwrap_or_else(|| {
        panic!(
            "workweave create should warn that the lock is missing; got:\n{}",
            f.create_output
        )
    });

    let doctor_output = f.rwv_with_path(&["doctor"], &f.ww_dir, path);
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
         (which is a link to it, and names nothing a report could send anyone to)"
    );
}

/// A real file sitting on the surfacing path is user-held: rwv will not
/// overwrite it, so the generation cannot reach the canonical location.
///
/// Reaching that state now takes a deliberate unlink. A fresh workweave
/// carries the lock's surfacing link from creation, and a `cargo build`
/// through it writes to the canonical file rather than beside it — which is
/// the point of declaring the lock as written through its link. What stays
/// reachable, and is what this pins, is the same weave after someone removes
/// the link by hand and then builds.
///
/// Pinned because the honest failure is the whole point: `--fix`
/// must not report success when the file it names is still missing.
///
/// The refusal arrives from link creation, which reaches the orphan before
/// the generator does. What is pinned is that it arrives at all, with the
/// path and the repair in it — not which site minted it.
///
/// What is pinned is rwv's refusal text and the path in it, so the generator's
/// output is not the subject and the run gets a stand-in `cargo` and no other
/// tool. The final arm asserts only that removing the orphan lets the
/// generation LAND — that the write reaches the canonical path — which is
/// rwv's half of it; what the resolve contains is the neighbouring test's
/// subject and it keeps the real tool.
///
/// Unix only, since the stand-in is a script resolved off `PATH` —
/// `common::cargo_stand_in_path` states why that does not port.
#[cfg(unix)]
#[test]
fn doctor_fix_names_the_orphan_when_a_real_file_blocks_the_surfacing_path() {
    let f = fixture();
    let path = common::cargo_stand_in_path(&f.beside_the_weave("stand-in-bin"));
    let path = Some(path.as_os_str());

    // The link create leaves is the route the generation takes, so the orphan
    // has to be built by removing it first — writing over it would write
    // THROUGH it and produce the canonical lock this arm needs absent. That
    // unlink is what a `cargo build` after a hand `rm` of the link amounts to.
    std::fs::remove_file(f.ww_surfaced_lock()).unwrap();
    std::fs::write(f.ww_surfaced_lock(), "# not a symlink\n").unwrap();

    let fix_output = f.rwv_with_path(&["doctor", "--fix"], &f.ww_dir, path);

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
    let retry = f.rwv_with_path(&["doctor", "--fix"], &f.ww_dir, path);
    assert!(
        f.ww_canonical_lock().is_file(),
        "removing the orphan and re-running --fix should produce the lock.\noutput:\n{retry}"
    );
}
