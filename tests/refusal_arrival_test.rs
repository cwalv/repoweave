//! Refusal tokens as an operator reads them: the binary's stderr, not an
//! `anyhow::Error`'s contents.
//!
//! A kind can be correct in the error value and still never reach anyone — a
//! caller flattens the chain to a string, a wrapper replaces the head, a verb
//! prints before it returns. So the sample here spans the ways a kind gets
//! attached, and each drive runs the shipped binary and reads what came back:
//! an inline refusal, one minted inside a shared helper, that same helper's
//! error under a `.context()` wrap, typed errors arriving through `?` — one
//! per carrying type, because a type's arm is classified on its own variants
//! and a sibling's arm proves nothing about it — and the `--continue` resume
//! path, which is in the sample because it is the one that used to destroy the
//! kind before it could be read.
//!
//! Two silences are pinned here too, and they are the assertions most likely
//! to be weakened by someone making a failure go away: an error with no kind
//! prints no route line at all, and a wrapped refusal prints exactly one.

use std::path::{Path, PathBuf};

mod common;

const SERVER_PATH: &str = "github/example/server";
const SERVER_URL: &str = "https://github.com/example/server";

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
    common::fixture_lock(&project_dir, &[]);
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

/// The other name type reaching the terminal the same way, and the reason it
/// is drilled separately: one type's arm being right says nothing about the
/// next, because each carries its own variants and its own classification.
#[test]
fn a_project_name_error_routes_to_the_same_condition() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    for name in ["bad--name", "a+b"] {
        let stderr = refusal_stderr(&["init", name], &ws);
        assert!(
            stderr.contains("not a valid project name"),
            "precondition: this is the typed name error:\n{stderr}"
        );
        assert_routes_to(&stderr, "unrenderable-name");
    }
}

/// A repo path rejected by its own newtype, which is neither a name nor a
/// `bail!`.
#[test]
fn a_typed_repo_path_error_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    let stderr = refusal_stderr(&["add", "github/owner/re\\po"], &ws);
    assert!(
        stderr.contains("backslash not allowed"),
        "precondition: this is the typed path error:\n{stderr}"
    );
    assert_routes_to(&stderr, "backslash-in-repo-path");
}

// ---------------------------------------------------------------------------
// The rwv add --new creation surface
// ---------------------------------------------------------------------------

/// `rwv add local --new` with `owner` and `repo` but no `root` — the driving
/// input fork 7's ruling turns into a one-flag repair.
#[test]
fn a_missing_creation_param_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    let stderr = refusal_stderr(
        &[
            "add",
            "local",
            "--new",
            "--param",
            "owner=acme",
            "--param",
            "repo=fresh",
        ],
        &ws,
    );
    assert!(
        stderr.contains("root") && stderr.contains("--param root=<value>"),
        "precondition: the refusal names the missing parameter and the flag \
         to add:\n{stderr}"
    );
    assert_routes_to(&stderr, "missing-creation-param");
}

/// A `root` that does not exist — one of several conditions
/// `unusable-creation-param` covers; this is the one that needs no upstream
/// state to drive.
#[test]
fn an_unusable_creation_param_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    let missing_root = tmp.path().join("nonexistent-root");

    let stderr = refusal_stderr(
        &[
            "add",
            "local",
            "--new",
            "--param",
            &format!("root={}", missing_root.display()),
            "--param",
            "owner=acme",
            "--param",
            "repo=fresh",
        ],
        &ws,
    );
    assert!(
        stderr.contains("does not exist"),
        "precondition: this is the missing-root refusal:\n{stderr}"
    );
    assert_routes_to(&stderr, "unusable-creation-param");
}

/// AC2 row 4: a `root` inside the weave refuses — the degenerate case is an
/// upstream that is also walked, deletable, and reportable as one of the
/// weave's own members.
#[test]
fn a_root_inside_the_weave_routes_to_unusable_creation_param() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    let inside_root = ws.join("local");
    std::fs::create_dir_all(&inside_root).unwrap();

    let stderr = refusal_stderr(
        &[
            "add",
            "local",
            "--new",
            "--param",
            &format!("root={}", inside_root.display()),
            "--param",
            "owner=acme",
            "--param",
            "repo=fresh",
        ],
        &ws,
    );
    assert!(
        stderr.contains("inside the weave"),
        "precondition: this is the root-inside-weave refusal:\n{stderr}"
    );
    assert_routes_to(&stderr, "unusable-creation-param");
}

/// Two `rwv add local --new` invocations naming the same owner/repo under
/// different roots: the second placement collision refuses rather than
/// silently discarding the operator's new root.
#[test]
fn an_occupied_placement_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    let root_a = tmp.path().join("root-a");
    let root_b = tmp.path().join("root-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();

    let first = common::rwv()
        .args([
            "add",
            "local",
            "--new",
            "--param",
            &format!("root={}", root_a.display()),
            "--param",
            "owner=acme",
            "--param",
            "repo=fresh",
        ])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    assert!(
        first.status.success(),
        "precondition: the first creation must succeed:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );

    let stderr = refusal_stderr(
        &[
            "add",
            "local",
            "--new",
            "--param",
            &format!("root={}", root_b.display()),
            "--param",
            "owner=acme",
            "--param",
            "repo=fresh",
        ],
        &ws,
    );
    assert!(
        stderr.contains("already maps to"),
        "precondition: this is the occupied-placement refusal:\n{stderr}"
    );
    assert_routes_to(&stderr, "occupied-placement");
}

/// `rwv init --provider local/owner`: `local` cannot mint a clone URL from
/// an owner and a project name alone — it has no `--root` to draw one from.
#[test]
fn a_provider_cannot_mint_url_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    let stderr = refusal_stderr(&["init", "newproj", "--provider", "local/acme"], &ws);
    assert!(
        stderr.contains("cannot name a repository"),
        "precondition: this is the provider-cannot-mint-url refusal:\n{stderr}"
    );
    assert_routes_to(&stderr, "provider-cannot-mint-url");
}

/// The selector errors, which are three conditions under one type. A single
/// drive would pass on an arm that answered the same kind for all three.
#[test]
fn each_selector_condition_routes_to_its_own_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    for (args, token) in [
        (["update", "--role", "bogus"], "unknown-role"),
        (["update", "--repo", "re:"], "empty-selector-pattern"),
        (["update", "--repo", "re:["], "uncompilable-selector"),
    ] {
        let stderr = refusal_stderr(&args, &ws);
        assert_routes_to(&stderr, token);
    }
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

/// `rwv sync` over a destination carrying uncommitted tracked changes. A
/// different producer from the lock preflight above and the same token: this
/// one assembles its body from two conditions and picks the kind itself.
#[test]
fn a_dirty_destination_at_sync_time_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let (primary, workweave) = primary_and_workweave(tmp.path());

    std::fs::write(
        workweave.join("projects/web-app/rwv.toml"),
        "[repositories]\n# edited, uncommitted\n",
    )
    .unwrap();

    let stderr = refusal_stderr(&["sync", &primary.to_string_lossy()], &workweave);
    assert!(
        stderr.contains("uncommitted tracked changes"),
        "precondition: the sync dirt preflight fired:\n{stderr}"
    );
    assert_routes_to(&stderr, "dirty-checkout");
}

/// Both halves of that same body at once: one repo whose dirt cannot be read,
/// and one carrying changes the operator can act on.
///
/// The ratified rule is that the actionable half wins — an operator told
/// "commit or stash" has something to do, where "a repo could not be read" is
/// something to go and look at. Nothing distinguishes the two orderings unless
/// a fixture makes both halves non-empty at once, which is what this one is
/// for: with only one half present, either ordering yields the same token.
///
/// UNIX ONLY, and for the same reason `tests/doctor_unreadable_projects_dir_test.rs`
/// is: mode `0o000` denies the read on Unix, where the Windows read-only
/// attribute does not deny one at all and the repo would simply scan clean.
#[test]
#[cfg(unix)]
fn a_dirty_repo_outranks_an_unreadable_one_in_the_same_refusal() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = common::tempdir().unwrap();
    let (primary, workweave) = primary_and_workweave(tmp.path());

    // Under root every permission check is a no-op, so the unreadable half of
    // the precondition cannot be built and the test would assert against a
    // fixture it did not get.
    let probe = tmp.path().join(".rwv-permission-probe");
    std::fs::create_dir(&probe).unwrap();
    let probe_perms = std::fs::metadata(&probe).unwrap().permissions();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000)).unwrap();
    let enforced = std::fs::read_dir(&probe).is_err();
    std::fs::set_permissions(&probe, probe_perms).unwrap();
    std::fs::remove_dir(&probe).unwrap();
    if !enforced {
        common::report_skip("permission bits are not enforced for this user (root?)");
        return;
    }

    // Dirty a tracked file that is NOT the manifest: rewriting `rwv.toml`
    // would empty the repository table the scan iterates, and the unreadable
    // half would then have nothing to be unreadable about.
    std::fs::write(
        workweave.join("projects/web-app/.gitattributes"),
        "rwv.lock merge=rwv-ours\n# edited, uncommitted\n",
    )
    .unwrap();

    // Restored before any assertion runs, so a red does not leave the fixture
    // locked against whatever cleans the temp dir up.
    let server = workweave.join(SERVER_PATH);
    let original = std::fs::metadata(&server).unwrap().permissions();
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o000)).unwrap();
    let output = common::rwv()
        .args(["sync", &primary.to_string_lossy()])
        .current_dir(&workweave)
        .output()
        .expect("rwv should run");
    std::fs::set_permissions(&server, original).unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "sync was expected to refuse:\n{stderr}"
    );
    assert!(
        stderr.contains("uncommitted tracked changes"),
        "precondition: the actionable half is present:\n{stderr}"
    );
    assert!(
        stderr.contains("git status could not be read"),
        "precondition: the unreadable half is present too:\n{stderr}"
    );
    assert_routes_to(&stderr, "dirty-checkout");
}

// ---------------------------------------------------------------------------
// The verbs that refuse on their arguments
// ---------------------------------------------------------------------------

/// One drive per condition an operator reaches by typing something the verb
/// will not take. Each gets its own weave: several of these refuse partway
/// through a verb that has already written to the one it ran in, and a shared
/// fixture would let an earlier row decide a later row's answer.
///
/// The table is the assertion. A site that loses its kind stops routing and
/// its row reddens by name, where a suite that drove only one of them would
/// report a single failure whatever went wrong.
#[test]
fn each_argument_refusal_routes_to_its_own_token() {
    let tmp = common::tempdir().unwrap();

    let cases: [(&[&str], &str); 14] = [
        (&["add", "foo"], "no-matching-registry"),
        (&["add", "--new", "github/owner"], "malformed-repo-path"),
        (
            &["init", "elsewhere", "--provider", "nosuch/owner"],
            "unknown-registry",
        ),
        (
            &["init", "elsewhere", "--provider", "noslash"],
            "malformed-provider",
        ),
        (&["remove", "nonexistent/path/repo"], "repo-not-in-manifest"),
        (&["init", "web-app"], "project-dir-occupied"),
        (&["init", "web-app/sub"], "nested-project-name"),
        (&["-w", "a/b", "status"], "wrong-address-flag"),
        (
            &["-w", "noseparator", "status"],
            "malformed-workweave-address",
        ),
        (&["fetch", "--allow-non-empty-dir"], "inapplicable-flag"),
        (&["nosuchverb"], "unknown-verb"),
        (&["explain", "nosuchverb"], "no-explain-entry"),
        (&["doctor", "--kind", "nosuchkind"], "unknown-finding-kind"),
        (&["update", "--repo", "glob:"], "empty-selector-pattern"),
    ];

    for (index, (args, token)) in cases.into_iter().enumerate() {
        let ws = plain_weave(&tmp.path().join(format!("case{index}")));
        let stderr = refusal_stderr(args, &ws);
        assert!(
            routed_tokens(&stderr) == vec![token],
            "rwv {args:?} must route to `{token}`, stderr was:\n{stderr}"
        );
    }
}

/// The lock preconditions, which need a weave shaped against them rather than
/// an argument the verb rejects.
#[test]
fn the_frozen_lock_gates_route_to_their_tokens() {
    let tmp = common::tempdir().unwrap();

    let absent = plain_weave(&tmp.path().join("absent"));
    std::fs::remove_file(absent.join("projects/web-app/rwv.lock")).unwrap();
    assert_routes_to(
        &refusal_stderr(&["fetch", "--frozen"], &absent),
        "missing-lock",
    );

    let partial = plain_weave(&tmp.path().join("partial"));
    std::fs::write(
        partial.join("projects/web-app/rwv.toml"),
        format!(
            "[repositories.\"{SERVER_PATH}\"]\ntype = \"git\"\nurl = \"{SERVER_URL}\"\n\
             version = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    assert_routes_to(
        &refusal_stderr(&["fetch", "--frozen"], &partial),
        "incomplete-lock",
    );
}

/// A pre-TOML manifest, whose refusal reaches the operator under the load
/// step's own `.context()` — so the route line has to survive a wrap the
/// refusing code never sees.
#[test]
fn the_legacy_manifest_refusal_routes_from_under_its_wrap() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    let project = ws.join("projects/web-app");
    std::fs::remove_file(project.join("rwv.toml")).unwrap();
    std::fs::write(project.join("rwv.yaml"), "repositories: {}\n").unwrap();

    let stderr = refusal_stderr(&["remove", "nonexistent/path/repo"], &ws);
    assert!(
        stderr.contains("failed to load manifest"),
        "precondition: the wrapping context is the headline:\n{stderr}"
    );
    assert_routes_to(&stderr, "legacy-manifest-format");
}

/// A run that got partway and withheld the artifact it would have written.
/// The tally itself is not what earns the token — the unwritten lock is.
#[test]
fn a_run_that_withheld_its_artifact_routes_to_a_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    std::fs::write(
        ws.join("projects/web-app/rwv.toml"),
        "[repositories.\"github/example/absent\"]\ntype = \"git\"\n\
         url = \"file:///nonexistent/absent\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();

    let stderr = refusal_stderr(&["fetch"], &ws);
    assert_routes_to(&stderr, "partial-run-aborted");
}

// ---------------------------------------------------------------------------
// The silence
// ---------------------------------------------------------------------------

/// An error that is not a refusal gains nothing. Nothing was declined on
/// purpose here: the manifest parser said no, and its answer reaches the
/// operator relabelled rather than classified.
#[test]
fn an_error_with_no_kind_prints_no_route_line() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    std::fs::write(ws.join("projects/web-app/rwv.toml"), "not = = toml\n").unwrap();

    let stderr = refusal_stderr(&["remove", "nonexistent/path/repo"], &ws);
    assert!(
        stderr.contains("TOML parse error"),
        "precondition: the failure is the parser's, not a refusal:\n{stderr}"
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
