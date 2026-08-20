//! Refusal tokens as an operator reads them: the binary's stderr, not an
//! `anyhow::Error`'s contents.
//!
//! A kind can be correct in the error value and still never reach anyone — a
//! caller flattens the chain to a string, a wrapper replaces the head, a verb
//! prints before it returns. So the sample here spans the ways a kind gets
//! attached, and each drive runs the shipped binary and reads what came back:
//! an inline refusal, one minted inside a shared helper, that same helper's
//! error under a `.context()` wrap, a typed error arriving through `?`, and
//! the `--continue` resume path, which is in the sample because it is the one
//! that used to destroy the kind before it could be read.
//!
//! Two silences are pinned here too, and they are the assertions most likely
//! to be weakened by someone making a failure go away: an error with no kind
//! prints no route line at all, and a wrapped refusal prints exactly one.

use std::path::{Path, PathBuf};

mod common;

const SERVER_PATH: &str = "github/example/server";
const SERVER_URL: &str = "https://github.com/example/server";
const EMPTY_LOCK: &str = "{\n  \"repositories\": {}\n}\n";

/// Stderr of a run that must fail.
fn refusal_stderr(args: &[&str], cwd: &Path) -> String {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "rwv {args:?} was expected to refuse, exit was {}\n{stderr}",
        output.status
    );
    stderr
}

/// The token on the route line, or `None` where no route line was printed.
///
/// Reads the whole of stderr rather than only its tail: a second route line
/// anywhere in the output is the defect the one-route-line pin exists for, and
/// a tail-only read cannot see it.
fn routed_tokens(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect()
}

/// Every error decoration in `stderr`, in the case it was spelled. Verbs write
/// progress to stderr too, so the decoration is looked for per line rather
/// than at the front of the stream.
fn decorations(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter_map(|l| {
            ["Error: ", "error: "]
                .into_iter()
                .find(|d| l.starts_with(d))
        })
        .collect()
}

fn assert_routes_to(stderr: &str, token: &str) {
    assert_eq!(
        routed_tokens(stderr),
        vec![token],
        "expected exactly one route line naming `{token}`, stderr was:\n{stderr}"
    );
    assert!(
        stderr.ends_with(&format!("\n\nrwv explain {token}\n")),
        "the route line must be the last line, after a blank one, stderr was:\n{stderr}"
    );
}

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// A weave with one active project and no repos.
fn plain_weave(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), "[repositories]\n").unwrap();
    std::fs::write(project_dir.join("rwv.lock"), EMPTY_LOCK).unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();
    ws
}

/// A v2 owner record for an op whose workspace is `ws`.
fn owner_record_json(ws: &Path) -> String {
    format!(
        "{{\"id\": \"planted-op-1234\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \
         \"project\": \"web-app\", \"source\": \"{ws}\", \"target\": \"{ws}\", \"retire\": false, \
         \"phase\": \"replay\", \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \
         \"overrides\": [], \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        ws = common::json_escaped(ws),
    )
}

// ---------------------------------------------------------------------------
// Inline refusal
// ---------------------------------------------------------------------------

/// `rwv push` from a workweave: the kind is attached at the `bail!` itself.
#[test]
fn an_inline_refusal_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let (_primary, workweave) = primary_and_workweave(tmp.path());

    let stderr = refusal_stderr(&["push"], &workweave);
    assert!(
        stderr.contains("refusing to push from workweave"),
        "precondition: this is the push-from-workweave refusal:\n{stderr}"
    );
    assert_routes_to(&stderr, "push-from-workweave");
}

// ---------------------------------------------------------------------------
// Shared helper, and the same error under a `.context()` wrap
// ---------------------------------------------------------------------------

/// `rwv materialize` over a planted op record. The kind is minted once inside
/// the in-flight helper; `materialize` then wraps it with a `.context()` that
/// replaces the headline. The route line must still name the condition that
/// fired, and there must be exactly one of it.
#[test]
fn a_wrapped_refusal_routes_once_to_the_kind_beneath_the_wrap() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    std::fs::write(ws.join(".rwv-op"), owner_record_json(&ws)).unwrap();

    let stderr = refusal_stderr(&["materialize"], &ws);
    assert!(
        stderr.contains("does not start while an operation is in flight"),
        "precondition: the wrapping context is the headline:\n{stderr}"
    );
    assert!(
        stderr.contains("Caused by:"),
        "precondition: the helper's error is a cause under the wrap:\n{stderr}"
    );
    assert_routes_to(&stderr, "op-in-progress");
}

/// The same condition reached through `activate`, which wraps it with its own
/// sentence — one kind, two headlines, one token.
#[test]
fn a_second_wrapper_over_one_condition_routes_to_the_same_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    std::fs::write(ws.join(".rwv-op"), owner_record_json(&ws)).unwrap();

    let stderr = refusal_stderr(&["activate", "web-app"], &ws);
    assert_routes_to(&stderr, "op-in-progress");
}

// ---------------------------------------------------------------------------
// Typed error, arriving through `?`
// ---------------------------------------------------------------------------

/// A workweave name spelling `/` never reaches a `bail!` — `WorkweaveName::new`
/// returns its own error type and `?` converts it. The kind rides the variant.
#[test]
fn a_typed_name_error_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    let stderr = refusal_stderr(&["workweave", "web-app", "create", "a/b"], &ws);
    assert!(
        stderr.contains("not a valid workweave name"),
        "precondition: this is the typed name error:\n{stderr}"
    );
    assert_routes_to(&stderr, "unrenderable-name");
}

// ---------------------------------------------------------------------------
// The resume path
// ---------------------------------------------------------------------------

/// Park an op at replay re-entry over a source `prepare` has made refusable,
/// and return what the operator reads.
fn parked_resume_stderr(tmp: &tempfile::TempDir, prepare: impl FnOnce(&Path)) -> String {
    let (primary, workweave) = primary_and_workweave(tmp.path());
    prepare(&primary);

    let record = format!(
        "{{\"id\": \"parked-op-1\", \"verb\": \"sync\", \"strategy\": \"rebase\", \
         \"project\": \"web-app\", \"source\": \"{src}\", \"target\": \"{tgt}\", \
         \"retire\": false, \"phase\": \"replay\", \"advanced_tips\": {{}}, \
         \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-06-10T00:00:00Z\"}}",
        src = common::json_escaped(&primary),
        tgt = common::json_escaped(&workweave),
    );
    std::fs::write(workweave.join(".rwv-op"), record).unwrap();

    let stderr = refusal_stderr(&["sync", "--continue"], &workweave);
    assert!(
        stderr.contains("still parked at its recorded phase"),
        "precondition: this is the parked-op decorator's refusal:\n{stderr}"
    );
    stderr
}

/// `--continue` into a re-gate that refuses, routed to the token of the gate
/// that actually fired — not to the parking that wrapped it.
///
/// The decorator took the refusal as a string until this design, which
/// discarded whatever kind it carried. Asserting the INNER token is what makes
/// that difference visible from outside the process: with the kind preserved
/// the operator is routed to the condition, with it discarded to `op-parked`.
///
/// **Two arms, and the second one is not redundant.** A decorator that
/// hard-coded one inner token instead of reading the error's would satisfy a
/// single-arm test. Two gates whose tokens differ can only both pass if the
/// decorator really is passing through what it was handed.
#[test]
fn the_resume_path_routes_to_the_gate_that_fired() {
    let tmp = common::tempdir().unwrap();
    let stderr = parked_resume_stderr(&tmp, |primary| {
        let source_project = primary.join("projects/web-app");
        common::fixture_lock(
            &source_project,
            &[(SERVER_PATH, SERVER_URL, &"b".repeat(40))],
        );
        common::git_in(&source_project, &["add", "rwv.lock"]);
        common::git_in(&source_project, &["commit", "-m", "lock: unresolvable"]);
    });
    assert!(
        stderr.contains("lock references unknown revisions"),
        "precondition: the unresolvable-entry gate fired:\n{stderr}"
    );

    // This drive is also the sample's only multi-line headline reaching a
    // route line, and a fixture that quietly became single-line would take
    // that coverage with it while still passing everything below.
    let headline = stderr
        .split_once("Error: ")
        .expect("the refusal is decorated")
        .1
        .rsplit_once("\n\nrwv explain")
        .expect("the route line is last")
        .0;
    assert!(
        headline.contains('\n'),
        "the headline must span several lines here, got:\n{headline}"
    );

    assert_routes_to(&stderr, "unresolvable-lock-entry");
}

#[test]
fn the_resume_path_routes_to_a_second_gates_own_token() {
    let tmp = common::tempdir().unwrap();
    // Lock a commit, then move HEAD off it backwards. The lock now records a
    // commit HEAD lacks, which is `behind` — anomalous, where `ahead` is the
    // benign case this gate deliberately lets through.
    let stderr = parked_resume_stderr(&tmp, |primary| {
        let server = primary.join(SERVER_PATH);
        std::fs::write(server.join("second.txt"), "second\n").unwrap();
        common::git_in(&server, &["add", "-A"]);
        common::git_in(&server, &["commit", "-m", "second"]);
        let locked = common::git_in(&server, &["rev-parse", "HEAD"]);

        let source_project = primary.join("projects/web-app");
        common::fixture_lock(&source_project, &[(SERVER_PATH, SERVER_URL, &locked)]);
        common::git_in(&source_project, &["add", "rwv.lock"]);
        common::git_in(&source_project, &["commit", "-m", "lock: at second"]);

        common::git_in(&server, &["reset", "--hard", "HEAD~1"]);
    });
    assert!(
        stderr.contains("has a stale lock"),
        "precondition: the stale-relation gate fired:\n{stderr}"
    );
    assert_routes_to(&stderr, "stale-lock");
}

// ---------------------------------------------------------------------------
// Two more of this slice's producers, in two other files
// ---------------------------------------------------------------------------

/// `--continue` with nothing recorded. The op-state module's own refusal,
/// reached without a sync engine in the way.
#[test]
fn a_resume_with_no_recorded_op_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let (_primary, workweave) = primary_and_workweave(tmp.path());

    let stderr = refusal_stderr(&["sync", "--continue"], &workweave);
    assert!(
        stderr.contains("no sync/sync-to op in progress")
            || stderr.contains("no operation in progress"),
        "precondition: nothing is recorded here:\n{stderr}"
    );
    assert_routes_to(&stderr, "no-op-recorded");
}

/// `rwv lock` over a manifest repo carrying uncommitted tracked changes —
/// the lock module's own preflight, and the most-shared token in this slice.
#[test]
fn a_dirty_repo_at_lock_time_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let (primary, _workweave) = primary_and_workweave(tmp.path());

    let server = primary.join(SERVER_PATH);
    std::fs::write(server.join("README.md"), "edited, uncommitted\n").unwrap();

    let stderr = refusal_stderr(&["lock"], &primary);
    assert!(
        stderr.contains("uncommitted changes"),
        "precondition: the dirty-checkout preflight fired:\n{stderr}"
    );
    assert_routes_to(&stderr, "dirty-checkout");
}

// ---------------------------------------------------------------------------
// The silence
// ---------------------------------------------------------------------------

/// An error that is not a refusal gains nothing. Nothing was declined on
/// purpose here, so there is no condition to route to and no line to print.
#[test]
fn an_error_with_no_kind_prints_no_route_line() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    let stderr = refusal_stderr(&["remove", "nonexistent/path/repo"], &ws);
    assert!(
        stderr.contains("not found in manifest"),
        "precondition: the verb refused:\n{stderr}"
    );
    assert!(
        routed_tokens(&stderr).is_empty(),
        "an unkinded error must print no route line, stderr was:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// Exit codes and the case of the E
// ---------------------------------------------------------------------------

/// The two classes stay told apart by the same two signals as before: a
/// refusal is `Error:` and exit 1, an argv rejection is `error:` and exit 2.
/// The funnel is the only producer of the first pair.
#[test]
fn the_refusal_class_and_the_argv_class_stay_distinct() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    let refusal = common::rwv()
        .args(["remove", "nonexistent/path/repo"])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    let refusal_stderr = String::from_utf8_lossy(&refusal.stderr).into_owned();
    assert_eq!(refusal.status.code(), Some(1), "{refusal_stderr}");
    assert!(
        decorations(&refusal_stderr) == vec!["Error: "],
        "a refusal carries the capitalized decoration, once:\n{refusal_stderr}"
    );

    let argv = common::rwv()
        .args(["sync", "--retire"])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    let argv_stderr = String::from_utf8_lossy(&argv.stderr).into_owned();
    assert_eq!(argv.status.code(), Some(2), "{argv_stderr}");
    assert!(
        decorations(&argv_stderr) == vec!["error: "],
        "an argv rejection carries the lowercase one, once:\n{argv_stderr}"
    );

    common::rwv()
        .args(["status"])
        .current_dir(&ws)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A primary weave and a registered workweave forked from it, both holding one
/// manifest repo and a committed lock. Returns `(primary_root, workweave_root)`.
fn primary_and_workweave(parent: &Path) -> (PathBuf, PathBuf) {
    let primary = parent.join("primary");
    std::fs::create_dir_all(primary.join("github/example")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let server = primary.join(SERVER_PATH);
    std::fs::create_dir_all(&server).unwrap();
    std::fs::write(server.join("README.md"), "init\n").unwrap();
    git_init_with_commit(&server);
    let sha = common::git_in(&server, &["rev-parse", "HEAD"]);

    let project_dir = primary.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"{SERVER_PATH}\"]\ntype = \"git\"\nurl = \"{SERVER_URL}\"\n\
             version = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    common::fixture_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &sha)]);
    git_init_with_commit(&project_dir);
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    let ww_root = parent.join(".workweaves/web-app--ww");
    std::fs::create_dir_all(ww_root.join("github/example")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();
    common::git_in(
        &server,
        &[
            "worktree",
            "add",
            &ww_root.join(SERVER_PATH).to_string_lossy(),
            "-b",
            "web-app--ww",
        ],
    );
    common::git_in(
        &project_dir,
        &[
            "worktree",
            "add",
            &ww_root.join("projects/web-app").to_string_lossy(),
            "-b",
            "web-app--ww",
        ],
    );
    common::register_workweave(&primary, "web-app", "ww", &ww_root);
    std::fs::write(
        ww_root.join(".rwv-workweave"),
        common::workweave_marker(&primary, "web-app", &primary),
    )
    .unwrap();

    (primary, ww_root)
}
