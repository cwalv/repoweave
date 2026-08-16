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
//! derived from — the project manifest, the rwv lock, or a member's
//! detection manifest. A delivery that touches none of them prints nothing;
//! the fires / does-not-fire arms pin the conditional from both sides.
//!
//! The lock is an input because it decides which commit of each member is on
//! disk, and that is what the ecosystem tool resolved against. It also means
//! the quiet arm cannot be a member commit: sync refuses a source whose lock
//! is behind its members, so delivering member work always moves the lock.
//!
//! The locks are hand-authored and stamped with the shipped digest helper,
//! each carrying a package no resolution of this fixture's two
//! path-dependency crates could produce — and a different one per
//! generation. The child's post-sync lock content is therefore a three-way
//! discriminator: the create-time sentinel means sync left it alone, the
//! parent's sentinel means sync carried it, anything else means something
//! re-resolved it.

use std::path::{Path, PathBuf};

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

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
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
        repoweave::owned_state::stamp_owned_digest(
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
            common::git_in(&self.source_project_dir, &["add", "-A"]);
            common::git_in(&self.source_project_dir, &["commit", "-m", "lock"]);
        }
    }

    /// `rwv sync primary` in the workweave, returning combined output.
    fn sync_from_primary(&self) -> String {
        self.rwv(&["sync", "primary"], &self.ww_dir)
    }

    /// `rwv sync primary --json` in the workweave, returning stdout parsed
    /// as JSON. Stdout only (never combined with stderr): the envelope is
    /// what a `--json` consumer actually reads, and combining streams would
    /// let stderr chatter corrupt the parse.
    fn sync_json_from_primary(&self) -> serde_json::Value {
        let output = common::rwv()
            .args(["sync", "primary", "--json"])
            .current_dir(&self.ww_dir)
            .output()
            .expect("rwv should run");
        assert!(
            output.status.success(),
            "rwv sync primary --json failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout not parseable as JSON ({e}):\n{}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    /// `rwv sync primary --json -j 2` in the workweave — NDJSON mode, one
    /// self-describing record per line, no envelope. Returns raw stdout.
    fn sync_ndjson_from_primary(&self) -> String {
        let output = common::rwv()
            .args(["sync", "primary", "--json", "-j", "2"])
            .current_dir(&self.ww_dir)
            .output()
            .expect("rwv should run");
        assert!(
            output.status.success(),
            "rwv sync primary --json -j 2 failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
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
    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "web-app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("primary intent activation should succeed");
    common::git_in(&project_dir, &["add", "-A"]);
    common::git_in(&project_dir, &["commit", "-m", "activate"]);

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
    common::git_in(f.server_dir(), &["add", "-A"]);
    common::git_in(f.server_dir(), &["commit", "-m", "server source change"]);
    f.relock_and_commit();
}

/// The parent commits project-repo content that is not an input of anything
/// generated: a note beside the manifest, with no relock.
///
/// This is what a delivery that moves no input looks like once the lock counts
/// as one. A member commit cannot play that role: sync refuses a source whose
/// lock is behind its members, so delivering member work necessarily moves the
/// lock, and the lock decides which commit of each member the generated state
/// was resolved against.
fn advance_project_note(f: &Fixture) {
    let note = f.source_project_dir.join("NOTES.md");
    std::fs::write(&note, "a note that nothing generates from\n").unwrap();
    common::git_in(&f.source_project_dir, &["add", "-A"]);
    common::git_in(&f.source_project_dir, &["commit", "-m", "note"]);
}

/// The parent commits a change to a member's `Cargo.toml` — a detection
/// manifest, so an input of the materialized state.
fn advance_member_manifest(f: &Fixture) {
    let cargo_toml = f.server_dir().join("Cargo.toml");
    let mut text = std::fs::read_to_string(&cargo_toml).unwrap();
    text.push_str("\n[features]\nadvanced = []\n");
    std::fs::write(&cargo_toml, text).unwrap();
    common::git_in(f.server_dir(), &["add", "-A"]);
    common::git_in(f.server_dir(), &["commit", "-m", "server manifest change"]);
    f.relock_and_commit();
}

/// The WORKWEAVE commits a change to a member's `Cargo.toml` and re-snapshots
/// its own lock, so a sync at the primary has an input change to pull.
fn advance_ww_member_manifest(f: &Fixture) {
    let cargo_toml = f.ww_server_dir().join("Cargo.toml");
    let mut text = std::fs::read_to_string(&cargo_toml).unwrap();
    text.push_str("\n[features]\nadvanced = []\n");
    std::fs::write(&cargo_toml, text).unwrap();
    common::git_in(f.ww_server_dir(), &["add", "-A"]);
    common::git_in(
        f.ww_server_dir(),
        &["commit", "-m", "server manifest change"],
    );

    f.rwv(&["lock"], &f.ww_dir);
    let ww_project_dir = f.ww_dir.join("projects/web-app");
    let dirty = common::git()
        .args(["status", "--porcelain"])
        .current_dir(&ww_project_dir)
        .output()
        .expect("git should be available");
    if !dirty.stdout.is_empty() {
        common::git_in(&ww_project_dir, &["add", "-A"]);
        common::git_in(&ww_project_dir, &["commit", "-m", "lock"]);
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
    common::git_in(&f.source_project_dir, &["add", "-A"]);
    common::git_in(&f.source_project_dir, &["commit", "-m", "manifest change"]);
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
        repoweave::owned_state::check_owned_digest(
            &f.ww_dir.join("projects/web-app"),
            "Cargo.lock",
            SOURCE_LOCK.as_bytes(),
        ),
        repoweave::owned_state::OwnedDigestCheck::Matches,
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

/// The rwv lock alone is enough. A delivery that carries no manifest of any
/// kind still moves which commit of each member is on disk, and the generated
/// state was resolved against those commits.
///
/// This is the arm the measured defect needed: the reported divergence was a
/// source-line edit, where no manifest moved at all.
#[test]
fn sync_names_materialize_when_delivered_changes_touch_only_the_lock() {
    let f = fixture();
    advance_member_source(&f);

    let output = f.sync_from_primary();

    assert!(
        output.contains("rwv materialize"),
        "a delivery that moved the lock moved an input.\noutput:\n{output}"
    );
    assert!(
        output.contains("(project): rwv.lock"),
        "the note should name the lock as the input that moved.\noutput:\n{output}"
    );
    assert!(
        !output.contains("Cargo.toml"),
        "and no manifest moved in this delivery, so none should be \
         named.\noutput:\n{output}"
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
fn sync_prints_no_materialize_note_for_non_input_deliveries() {
    let f = fixture();
    advance_project_note(&f);

    let output = f.sync_from_primary();

    assert!(
        f.ww_dir.join("projects/web-app/NOTES.md").is_file(),
        "control: the change must actually have been delivered — a sync with \
         nothing to deliver stays quiet for the wrong reason"
    );
    assert!(
        !output.contains("rwv materialize"),
        "no delivered input moved, so there is nothing for materialize to \
         re-derive and the note must not print.\noutput:\n{output}"
    );
}

/// The same conditional note, read through the surface a `--json` consumer
/// actually parses: an `advisories` entry, not a string a caller would have
/// to grep stderr for.
#[test]
fn sync_json_advisories_carries_the_materialize_remedy_when_a_member_manifest_arrives() {
    let f = fixture();
    advance_member_manifest(&f);

    let envelope = f.sync_json_from_primary();
    let advisories = envelope
        .get("advisories")
        .and_then(serde_json::Value::as_array)
        .expect("envelope should carry an advisories array");

    assert_eq!(
        advisories.len(),
        1,
        "expected exactly one advisory: {envelope}"
    );
    let advisory = &advisories[0];
    assert_eq!(advisory["kind"], "derived_state_stale");
    assert_eq!(advisory["remedy"], "rwv materialize");
    assert_eq!(
        advisory["inputs"],
        serde_json::json!([
            "projects/web-app/rwv.lock",
            "github/chatly/server/Cargo.toml"
        ]),
        "inputs should be the workspace-relative paths, not the 'repo: file' \
         display string the text note uses — and the lock is one of them, \
         because delivering the member commit is what moved it: {envelope}"
    );
}

/// Mirrors [`sync_prints_no_materialize_note_for_source_only_deliveries`] on
/// the `--json` surface: a source-only delivery raises no advisory, and the
/// array is present-but-empty rather than absent.
#[test]
fn sync_json_advisories_empty_for_non_input_deliveries() {
    let f = fixture();
    advance_project_note(&f);

    let envelope = f.sync_json_from_primary();
    let advisories = envelope
        .get("advisories")
        .and_then(serde_json::Value::as_array)
        .expect("envelope should carry an advisories array even when empty");

    assert!(
        advisories.is_empty(),
        "no delivered input moved, so advisories should be empty: {envelope}"
    );
}

/// NDJSON mode (`-j N` with `N > 1`) drops the advisory rather than
/// surfacing it: each line is a self-describing per-repo record with no
/// envelope for an `advisories` array to sit in, so there is nowhere for it
/// to go. Deliberate, not an oversight — pinned here rather than left an
/// unremarked absence, on the same delivery that DOES raise an advisory
/// under serial `--json` (see
/// `sync_json_advisories_carries_the_materialize_remedy_when_a_member_manifest_arrives`).
/// A change that starts leaking the advisory into an NDJSON line, or that
/// silently drops materialize's own line-emission behavior, should redden
/// this.
#[test]
fn sync_ndjson_carries_no_advisory_even_when_serial_json_would() {
    let f = fixture();
    advance_member_manifest(&f);

    let stdout = f.sync_ndjson_from_primary();
    let mut saw_a_line = false;
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        saw_a_line = true;
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("NDJSON line not parseable ({e}): {line}"));
        assert!(
            parsed.get("advisories").is_none(),
            "NDJSON has no envelope for an advisories array: {line}"
        );
        assert_ne!(
            parsed.get("kind").and_then(serde_json::Value::as_str),
            Some("derived_state_stale"),
            "NDJSON has no per-line advisory record shape: {line}"
        );
    }
    assert!(saw_a_line, "control: NDJSON must emit at least one record");
}
