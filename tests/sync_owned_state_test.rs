//! `rwv sync` and rwv-owned generated state: the non-carry is deliberate,
//! and the remedy note is conditional on delivered input changes.
//!
//! Sync moves committed state and never fires integration hooks, so the
//! gitignored ecosystem lock and its `.rwv-owned-digests` ledger stay
//! exactly where workweave creation left them even after the parent's
//! attested lock moves on. That silence is a decision, and a decision needs
//! a test that fails if it breaks — a sync that started carrying the state
//! (or re-resolving it) must go red here, not pass unnoticed.
//!
//! What sync owes the operator instead is a pointer at the verb whose
//! mandate materialization is: a note naming `rwv materialize`, printed
//! only when the delivered changes touched inputs the generated state is
//! derived from (the project manifest, a member's detection manifest). A
//! source-only delivery changes nothing the hooks would regenerate, so it
//! prints nothing — the fires / does-not-fire arms pin the conditional from
//! both sides.
//!
//! The locks are hand-authored and stamped with the shipped digest helper,
//! each carrying a package no resolution of this fixture's two
//! path-dependency crates could produce — and a different one per
//! generation. The child's post-sync lock content is therefore a three-way
//! discriminator: the create-time sentinel means sync left it alone, the
//! parent's sentinel means sync carried it, anything else means something
//! re-resolved it.

use std::path::{Path, PathBuf};
use std::process;

mod common;

/// The lock the child is created with.
const SOURCE_LOCK: &str = "\
version = 4

[[package]]
name = \"chatly-protocol\"
version = \"0.1.0\"

[[package]]
name = \"chatly-server\"
version = \"0.1.0\"
dependencies = [\"chatly-protocol\"]

[[package]]
name = \"pinned-only-at-the-source\"
version = \"0.0.1\"
";

/// The lock the parent moves to after the fork.
const ADVANCED_LOCK: &str = "\
version = 4

[[package]]
name = \"chatly-protocol\"
version = \"0.1.0\"

[[package]]
name = \"chatly-server\"
version = \"0.1.0\"
dependencies = [\"chatly-protocol\"]

[[package]]
name = \"pinned-only-at-parent-after-advance\"
version = \"0.0.2\"
";

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

fn git_head(dir: &Path) -> String {
    let output = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git should be available");
    assert!(output.status.success(), "rev-parse in {}", dir.display());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    source_project_dir: PathBuf,
    ww_dir: PathBuf,
}

impl Fixture {
    fn source_lock(&self) -> PathBuf {
        self.source_project_dir.join("Cargo.lock")
    }

    fn ww_lock(&self) -> PathBuf {
        self.ww_dir.join("projects/web-app/Cargo.lock")
    }

    fn ww_digest_state(&self) -> PathBuf {
        self.ww_dir.join("projects/web-app/.rwv-owned-digests")
    }

    fn server_dir(&self) -> PathBuf {
        self.ws.join("github/chatly/server")
    }

    fn ww_server_dir(&self) -> PathBuf {
        self.ww_dir.join("github/chatly/server")
    }

    /// Run `rwv` and hand back combined stdout+stderr, asserting success.
    fn rwv(&self, args: &[&str], cwd: &Path) -> String {
        let output = common::rwv()
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("rwv should run");
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "rwv {args:?} in {} failed:\n{combined}",
            cwd.display()
        );
        combined
    }

    /// Write `content` as the parent's lock and stamp it as the accepted
    /// generation, the way an activation hook's acceptance would.
    fn accept_source_lock(&self, content: &str) {
        std::fs::write(self.source_lock(), content).unwrap();
        repoweave::integrations::merge::stamp_owned_digest(
            &self.source_project_dir,
            "Cargo.lock",
            content.as_bytes(),
        )
        .expect("stamping the source lock should succeed");
    }

    /// Re-snapshot the parent's rwv.lock and commit whatever changed.
    fn relock_and_commit(&self) {
        self.rwv(&["lock"], &self.ws);
        let dirty = common::git()
            .args(["status", "--porcelain"])
            .current_dir(&self.source_project_dir)
            .output()
            .expect("git should be available");
        if !dirty.stdout.is_empty() {
            git(&["add", "-A"], &self.source_project_dir);
            git(&["commit", "-m", "lock"], &self.source_project_dir);
        }
    }

    /// `rwv sync primary` in the workweave, returning combined output.
    fn sync_from_primary(&self) -> String {
        self.rwv(&["sync", "primary"], &self.ww_dir)
    }
}

/// Read the `Cargo.lock` entry out of a `.rwv-owned-digests` state file.
fn recorded_lock_digest(state_file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(state_file).ok()?;
    let map: std::collections::BTreeMap<String, String> = serde_json::from_str(&text).ok()?;
    map.get("Cargo.lock").cloned()
}

/// Build a primary weave of two path-dependency crates plus a project repo
/// that gitignores its generated lock, leave an attested lock in the project
/// dir, create a workweave off it, then move the parent's lock to
/// [`ADVANCED_LOCK`]. The parent's member/manifest advance is each arm's own
/// step — which files it delivers is the variable under test.
fn fixture() -> Fixture {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();

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

    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/chatly/protocol\"]\ntype = \"git\"\nurl = \"https://github.com/chatly/protocol.git\"\nversion = \"main\"\nrole = \"owned\"\n\n[repositories.\"github/chatly/server\"]\ntype = \"git\"\nurl = \"https://github.com/chatly/server.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    std::fs::write(project_dir.join(".gitignore"), "/Cargo.lock\n").unwrap();
    git_init_with_commit(&project_dir);

    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    // Author the managed Cargo.toml with hooks suppressed: the source's lock
    // is this fixture's own, not whatever a resolver would produce here.
    let ctx = repoweave::workspace::WorkspaceContext::resolve(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "web-app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("primary intent activation should succeed");
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "activate"], &project_dir);

    let f = Fixture {
        ww_dir: root.join(".workweaves/web-app--agent-1"),
        _tmp: tmp,
        ws,
        source_project_dir: project_dir,
    };

    f.accept_source_lock(SOURCE_LOCK);

    // The carry, not the worktree checkout, is the only route this lock can
    // reach a workweave by.
    let ignored = common::git()
        .args(["check-ignore", "-q", "Cargo.lock"])
        .current_dir(&f.source_project_dir)
        .status()
        .expect("git should be available");
    assert!(
        ignored.success(),
        "fixture: the source lock must be gitignored, or a workweave would \
         inherit it through git and prove nothing"
    );

    f.relock_and_commit();
    std::fs::create_dir_all(f.ws.parent().unwrap().join(".workweaves")).unwrap();
    f.rwv(&["workweave", "web-app", "create", "agent-1"], &f.ws);

    assert_eq!(
        std::fs::read(f.ww_lock()).unwrap().as_slice(),
        SOURCE_LOCK.as_bytes(),
        "fixture: the workweave must start on the source's attested lock, or \
         'sync left it alone' and 'it was never there' are indistinguishable"
    );

    // The parent's lock moves AFTER the fork; from here the child and the
    // parent hold different attested generations.
    f.accept_source_lock(ADVANCED_LOCK);

    f
}

/// The parent commits a source-only change to a member — nothing the
/// generated ecosystem state is derived from.
fn advance_member_source(f: &Fixture) {
    let main_rs = f.server_dir().join("src/main.rs");
    let mut text = std::fs::read_to_string(&main_rs).unwrap();
    text.push_str("pub fn advanced() -> bool { true }\n");
    std::fs::write(&main_rs, text).unwrap();
    git(&["add", "-A"], &f.server_dir());
    git(&["commit", "-m", "server source change"], &f.server_dir());
    f.relock_and_commit();
}

/// The parent commits a change to a member's `Cargo.toml` — a detection
/// manifest, so an input of the materialized state.
fn advance_member_manifest(f: &Fixture) {
    let cargo_toml = f.server_dir().join("Cargo.toml");
    let mut text = std::fs::read_to_string(&cargo_toml).unwrap();
    text.push_str("\n[features]\nadvanced = []\n");
    std::fs::write(&cargo_toml, text).unwrap();
    git(&["add", "-A"], &f.server_dir());
    git(&["commit", "-m", "server manifest change"], &f.server_dir());
    f.relock_and_commit();
}

/// The WORKWEAVE commits a change to a member's `Cargo.toml` and re-snapshots
/// its own lock, so a sync at the primary has an input change to pull.
fn advance_ww_member_manifest(f: &Fixture) {
    let cargo_toml = f.ww_server_dir().join("Cargo.toml");
    let mut text = std::fs::read_to_string(&cargo_toml).unwrap();
    text.push_str("\n[features]\nadvanced = []\n");
    std::fs::write(&cargo_toml, text).unwrap();
    git(&["add", "-A"], &f.ww_server_dir());
    git(
        &["commit", "-m", "server manifest change"],
        &f.ww_server_dir(),
    );

    f.rwv(&["lock"], &f.ww_dir);
    let ww_project_dir = f.ww_dir.join("projects/web-app");
    let dirty = common::git()
        .args(["status", "--porcelain"])
        .current_dir(&ww_project_dir)
        .output()
        .expect("git should be available");
    if !dirty.stdout.is_empty() {
        git(&["add", "-A"], &ww_project_dir);
        git(&["commit", "-m", "lock"], &ww_project_dir);
    }
}

/// Give the primary a second project and point `.rwv-active` at it, so the
/// root no longer presents `web-app`.
fn present_other_project(f: &Fixture) {
    let other = f.ws.join("projects/other-app");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("rwv.toml"), "[repositories]\n").unwrap();
    git_init_with_commit(&other);
    std::fs::write(f.ws.join(".rwv-active"), "other-app\n").unwrap();
}

/// The parent commits a content change to the project manifest itself.
fn advance_project_manifest(f: &Fixture) {
    let rwv_toml = f.source_project_dir.join("rwv.toml");
    let mut text = std::fs::read_to_string(&rwv_toml).unwrap();
    text.push_str("\n# advanced at the parent\n");
    std::fs::write(&rwv_toml, text).unwrap();
    git(&["add", "-A"], &f.source_project_dir);
    git(&["commit", "-m", "manifest change"], &f.source_project_dir);
    f.relock_and_commit();
}

/// Sync delivers committed state and leaves the rwv-owned generated state
/// exactly where creation put it — deliberately. A sync that starts carrying
/// the parent's lock (or re-resolving the child's) goes red here.
#[test]
fn sync_leaves_the_child_on_its_create_time_attested_lock() {
    let f = fixture();
    advance_member_source(&f);

    let parent_tip = git_head(&f.server_dir());
    assert_ne!(
        git_head(&f.ww_server_dir()),
        parent_tip,
        "fixture: the parent must be ahead, or the sync below has nothing to deliver"
    );

    f.sync_from_primary();

    assert_eq!(
        git_head(&f.ww_server_dir()),
        parent_tip,
        "control: sync must have delivered the member advance — a no-op sync \
         leaving the lock alone would prove nothing"
    );
    assert_eq!(
        std::fs::read(f.ww_lock()).unwrap().as_slice(),
        SOURCE_LOCK.as_bytes(),
        "the child's lock must still be its create-time generation, byte for \
         byte: sync moves committed state only, and this lock is neither \
         committed nor sync's to regenerate"
    );
    assert_eq!(
        std::fs::read(f.source_lock()).unwrap().as_slice(),
        ADVANCED_LOCK.as_bytes(),
        "fixture: the parent must hold the advanced generation, or the child \
         'keeping' its lock is indistinguishable from the two never diverging"
    );
    let child_recorded = recorded_lock_digest(&f.ww_digest_state())
        .expect("the child should still record a digest for its lock");
    assert_eq!(
        repoweave::integrations::merge::check_owned_digest(
            &f.ww_dir.join("projects/web-app"),
            "Cargo.lock",
            SOURCE_LOCK.as_bytes(),
        ),
        repoweave::integrations::merge::OwnedDigestCheck::Matches,
        "the child's ledger must still attest the lock it holds (recorded: \
         {child_recorded}) — lock and ledger stay stale TOGETHER, which is \
         what keeps the checkout self-consistent"
    );
}

/// A delivered change to a member's detection manifest means the generated
/// state's inputs moved under this checkout, and sync says which verb
/// re-derives it.
#[test]
fn sync_names_materialize_when_delivered_changes_touch_a_member_manifest() {
    let f = fixture();
    advance_member_manifest(&f);

    let output = f.sync_from_primary();

    assert!(
        output.contains("rwv materialize"),
        "sync should name the materialize verb when a member manifest \
         arrived.\noutput:\n{output}"
    );
    assert!(
        output.contains("github/chatly/server: Cargo.toml"),
        "the note should say which delivered input moved.\noutput:\n{output}"
    );
}

/// A delivered change to the project manifest is equally an input: membership
/// is what the generated state declares.
#[test]
fn sync_names_materialize_when_delivered_changes_touch_the_project_manifest() {
    let f = fixture();
    advance_project_manifest(&f);

    let output = f.sync_from_primary();

    assert!(
        output.contains("rwv materialize"),
        "sync should name the materialize verb when the project manifest \
         moved.\noutput:\n{output}"
    );
    assert!(
        output.contains("(project): rwv.toml"),
        "the note should attribute the hit to the project repo.\noutput:\n{output}"
    );
}

/// The receiver can be the primary too: pulling a workweave's input change
/// into a primary that presents the same project notes the same remedy.
#[test]
fn sync_at_primary_names_materialize_for_the_presented_project() {
    let f = fixture();
    advance_ww_member_manifest(&f);

    let ww_arg = f.ww_dir.to_string_lossy().to_string();
    let output = f.rwv(&["sync", &ww_arg], &f.ws);

    assert!(
        output.contains("rwv materialize"),
        "the primary presents web-app and web-app's inputs arrived, so the \
         note should print here too.\noutput:\n{output}"
    );
    assert!(
        output.contains("github/chatly/server: Cargo.toml"),
        "the note should say which delivered input moved.\noutput:\n{output}"
    );
}

/// A primary receiving changes to a project its pointer does not present has
/// nothing materialized to go stale, so the same delivery prints no note.
#[test]
fn sync_at_primary_stays_quiet_for_a_project_the_root_does_not_present() {
    let f = fixture();
    advance_ww_member_manifest(&f);
    present_other_project(&f);

    let ww_tip = git_head(&f.ww_server_dir());
    let ww_arg = f.ww_dir.to_string_lossy().to_string();
    let output = f.rwv(&["sync", &ww_arg, "--project", "web-app"], &f.ws);

    assert_eq!(
        git_head(&f.server_dir()),
        ww_tip,
        "control: the input change must actually have been delivered — a \
         refused or no-op sync stays quiet for the wrong reason"
    );
    assert!(
        !output.contains("rwv materialize"),
        "the root presents other-app; web-app's generated state is not \
         materialized here, so there is nothing for the note to point \
         at.\noutput:\n{output}"
    );
}

/// The note is conditional, not a banner: a source-only delivery touches
/// nothing the hooks would regenerate, so sync stays quiet about them.
#[test]
fn sync_prints_no_materialize_note_for_source_only_deliveries() {
    let f = fixture();
    advance_member_source(&f);

    let parent_tip = git_head(&f.server_dir());
    let output = f.sync_from_primary();

    assert_eq!(
        git_head(&f.ww_server_dir()),
        parent_tip,
        "control: the source-only change must actually have been delivered — \
         a sync with nothing to deliver stays quiet for the wrong reason"
    );
    assert!(
        !output.contains("rwv materialize"),
        "no delivered input moved, so there is nothing for materialize to \
         re-derive and the note must not print.\noutput:\n{output}"
    );
}
