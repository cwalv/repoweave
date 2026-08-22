//! The in-flight-op check `materialize` and `activate` run is advice, and the
//! window behind it is accepted.
//!
//! Checking a marker and acquiring a lease are different acts. These verbs do
//! the first: they read the marker once, at the start, and hold nothing
//! afterwards. An operation that lands between that read and the work proceeds
//! unnoticed, and that is the decided position rather than an oversight — the
//! stamp-time attestation guard is what makes the record honest regardless, so
//! buying exclusion here would cost the wedged-workspace failure mode for a
//! guarantee already covered.
//!
//! What this file pins is that both halves stay true: the check refuses the
//! common case, and it does NOT become exclusion. The refusal half lives with
//! the rest of the family in `tests/e2e_op_state_test.rs`; what is here is the
//! half a later reader is likely to "fix".
//!
//! Also here: the callers that inherit the check without naming it. `activate`
//! is not a verb but a shared entry point — `init` and `fetch` activate
//! in-process once their own work is done — so the refusal describes the act it
//! declines rather than a verb it cannot know, and each inheriting caller is
//! driven here rather than argued about.
//!
//! One test is UNIX ONLY, marked at the test. Its instrument is a `#!/bin/sh`
//! script dispatched as `cargo` off PATH, which is how a test gets a foothold
//! inside a running verb: the generator subprocess is the one moment a verb is
//! observable from outside. Windows has no executable bit and resolves by
//! PATHEXT, so a discoverable shim there is a different artifact under a
//! different name. Nothing about the boundary is platform-specific.

use std::path::{Path, PathBuf};

mod common;

const EMPTY_LOCK: &str = "{\n  \"repositories\": {}\n}\n";

fn rwv(args: &[&str], cwd: &Path) -> (bool, String) {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    (
        output.status.success(),
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[cfg(unix)]
fn rwv_with_path_prefix(args: &[&str], cwd: &Path, prepend: &Path) -> (bool, String) {
    let inherited = std::env::var("PATH").unwrap_or_default();
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .env("PATH", format!("{}:{inherited}", prepend.display()))
        .output()
        .expect("rwv should run");
    (
        output.status.success(),
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// A weave whose `materialize` really runs a generator: one Rust member and
/// cargo-workspace enabled, so there is a subprocess to stand inside.
fn weave_with_a_generator(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let member = ws.join("github/acme/lib");
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(member.join("src/lib.rs"), "").unwrap();
    git_init_with_commit(&member);

    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/lib\"]\ntype = \"git\"\nurl = \"https://github.com/acme/lib.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    std::fs::write(project_dir.join("rwv.lock"), EMPTY_LOCK).unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation should author the workspace manifest");
    ws
}

/// A v2 owner record for an op the check must refuse.
fn owner_record_json(ws: &Path) -> String {
    format!(
        "{{\"id\": \"planted-op-1234\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \
         \"project\": \"app\", \"source\": \"{ws}\", \"target\": \"{ws}\", \"retire\": false, \
         \"phase\": \"replay\", \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \
         \"overrides\": [], \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        ws = common::json_escaped(ws),
    )
}

/// An op that lands after the check has passed does not stop the verb, and the
/// verb holds nothing while it runs.
///
/// Two claims, one drive. The shim records which op-state files exist at the
/// one moment a lease taken at startup would still be held — inside the
/// generator subprocess — and then plants a record in the window the check has
/// already passed through.
///
/// The refusal at the end is what makes the success in the middle mean
/// something: the planted record is one this check does refuse, so the verb
/// completing over it is the accepted window and not a record too malformed to
/// see.
#[test]
#[cfg(unix)]
fn an_op_landing_after_the_check_does_not_stop_the_verb() {
    use std::os::unix::fs::PermissionsExt;

    let Some(real_cargo) = common::resolve_tool_or_skip("cargo") else {
        return;
    };
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_a_generator(tmp.path());

    let (ok, first) = rwv(&["materialize"], &ws);
    assert!(ok, "precondition: the weave materializes cleanly:\n{first}");

    let record = tmp.path().join("planted-op.json");
    std::fs::write(&record, owner_record_json(&ws)).unwrap();
    let held = tmp.path().join("held-mid-verb.txt");

    let shim_dir = tmp.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("cargo");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             {{ [ -e '{ws}/.rwv-op' ] && echo owner:present || echo owner:absent\n\
             [ -e '{ws}/.rwv-op-lease' ] && echo lease:present || echo lease:absent\n\
             }} > '{held}'\n\
             cp '{record}' '{ws}/.rwv-op'\n\
             exec '{real}' \"$@\"\n",
            ws = ws.display(),
            held = held.display(),
            record = record.display(),
            real = real_cargo.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (ok, materialized) = rwv_with_path_prefix(&["materialize"], &ws, &shim_dir);
    assert!(
        ok,
        "an op landing inside the window must not fail the verb — the check is \
         read once at the start and nothing re-reads it:\n{materialized}"
    );

    assert_eq!(
        std::fs::read_to_string(&held).unwrap(),
        "owner:absent\nlease:absent\n",
        "the verb must hold no op-state of its own while it runs: checking the \
         marker and claiming it are different acts, and only the first is in \
         budget here"
    );

    let (ok, refused) = rwv(&["materialize"], &ws);
    assert!(
        !ok,
        "control: the planted record IS one this check refuses, so the success \
         above was the accepted window and not an unreadable record:\n{refused}"
    );
    assert!(
        refused.contains("sync-to in progress"),
        "and it refuses by naming that op:\n{refused}"
    );

    std::fs::remove_file(ws.join(".rwv-op")).unwrap();
    let (ok, cleared) = rwv(&["materialize"], &ws);
    assert!(ok, "{cleared}");
}

/// A workspace an op record can sit in.
///
/// Bootstrap mode is not that workspace, and the difference is the whole
/// reachable population for the two callers below: `init` and `fetch` refuse an
/// occupied directory before they get anywhere near activation, and an op record
/// is itself an occupant. So these paths reach the check only where the
/// workspace already exists — which is also the only place an op could have been
/// started.
fn bootstrapped_workspace(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let (ok, out) = rwv(&["init", "base"], &ws);
    assert!(ok, "fixture: bootstrapping the workspace:\n{out}");
    ws
}

/// `rwv init` into an existing workspace inherits the refusal, and it names the
/// act rather than `activate`, which is not the verb the operator ran.
///
/// The refusal lands AFTER init has minted the project: the check sits in the
/// in-process activation, which is the last thing init does. That is the shape
/// an activate-hook failure already has — the verb's own work stands, the
/// activation is what is declined — and it is asserted so the ordering is on the
/// record rather than incidental.
#[test]
fn init_inherits_the_refusal_and_it_names_the_act() {
    let tmp = common::tempdir().unwrap();
    let ws = bootstrapped_workspace(tmp.path());
    std::fs::write(ws.join(".rwv-op"), owner_record_json(&ws)).unwrap();

    let (ok, refused) = rwv(&["init", "app"], &ws);
    assert!(!ok, "{refused}");
    assert!(
        refused.contains("running its install hooks does not start"),
        "the refusal must name the act it declines: an operator who ran `init` \
         never ran `activate` and must not be told activate refused:\n{refused}"
    );
    assert!(
        !refused.contains("activate does not start"),
        "and must not name a verb it cannot know:\n{refused}"
    );
    assert!(
        refused.contains("sync-to in progress"),
        "the op that caused it is still named:\n{refused}"
    );
    assert!(
        ws.join("projects/app/rwv.toml").is_file(),
        "init's own work stands; it is the activation that was declined"
    );
}

/// `rwv fetch` inherits it on the same terms. This is the operator-visible
/// widening: a fetch into a workspace carrying an op record now exits non-zero
/// where it used to succeed, and clears when the record does.
#[test]
fn fetch_inherits_the_refusal_and_it_names_the_act() {
    let tmp = common::tempdir().unwrap();
    let ws = bootstrapped_workspace(tmp.path());

    // `fetch <source>` clones the source as a PROJECT, so each source carries a
    // manifest; a bare repo without one fails before reaching activation.
    let project_source = |name: &str| {
        let bare = tmp.path().join(format!("{name}.git"));
        common::git_in(
            tmp.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                &bare.to_string_lossy(),
            ],
        );
        let work = tmp.path().join(format!("work-{name}"));
        common::git_in(
            tmp.path(),
            &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        );
        std::fs::write(work.join("rwv.toml"), "[repositories]\n").unwrap();
        common::git_in(&work, &["add", "-A"]);
        common::git_in(&work, &["commit", "-m", "initial"]);
        common::git_in(&work, &["push", "origin", "main"]);
        common::file_url(&bare)
    };
    let refused_source = project_source("dep");
    let cleared_source = project_source("dep2");

    // `fetch` auto-activates only where no project is selected yet; with one
    // selected it says so and skips, and the check is never reached. That is
    // the narrow shape this inherits, so it is the shape driven here.
    std::fs::remove_file(ws.join(".rwv-active")).unwrap();
    std::fs::write(ws.join(".rwv-op"), owner_record_json(&ws)).unwrap();

    let (ok, refused) = rwv(&["fetch", &refused_source], &ws);
    assert!(
        !ok,
        "a fetch into a workspace with an op in flight must refuse:\n{refused}"
    );
    assert!(
        refused.contains("running its install hooks does not start"),
        "naming the act, not `activate`:\n{refused}"
    );
    assert!(
        refused.contains("sync-to in progress"),
        "and naming the op:\n{refused}"
    );

    // A second source, because the refused fetch had already cloned the first:
    // re-fetching it would report the clone, not the check.
    std::fs::remove_file(ws.join(".rwv-op")).unwrap();
    let (ok, cleared) = rwv(&["fetch", &cleared_source], &ws);
    assert!(
        ok,
        "and the refusal clears with the record, so the widening is a refusal \
         and not a break:\n{cleared}"
    );
}
