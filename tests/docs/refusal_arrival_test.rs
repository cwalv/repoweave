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

use crate::common;

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
    common::record_trunk(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();
    ws
}

/// A v2 owner record for an op whose workspace is `ws`.
fn owner_record_json(ws: &Path) -> String {
    format!(
        "{{\"id\": \"planted-op-1234\", \"verb\": \"land\", \"history\": null, \
         \"project\": \"web-app\", \"source\": \"{ws}\", \"target\": \"{ws}\", \"retire\": false, \
         \"phase\": \"replay\", \"converged_tips\": {{}}, \
         \"started_at\": \"2026-05-27T10:00:00Z\", \"schema\": 7, \"release\": \"0.0.0\", \"take_in\": null}}",
        ws = common::json_escaped(ws),
    )
}

// ---------------------------------------------------------------------------
// Inline refusal
// ---------------------------------------------------------------------------

/// `rwv push --overwrite` naming a repo the push does not publish: the kind is
/// attached at the `bail!` itself.
#[test]
fn an_inline_refusal_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let (primary, _workweave) = primary_and_workweave(tmp.path());

    let stderr = refusal_stderr(&["push", "--overwrite", "github/acme/nothing"], &primary);
    assert!(
        stderr.contains("names no member this push publishes"),
        "precondition: this is the overwrite refusal:\n{stderr}"
    );
    assert_routes_to(&stderr, "inapplicable-flag");
}

// ---------------------------------------------------------------------------
// Shared helper
// ---------------------------------------------------------------------------

/// `rwv activate` over a planted op record: the kind is minted once inside
/// the in-flight helper that dispatch runs for every verb. A wrap above a
/// tagged error routes to the kind beneath it too; that is pinned where the
/// render is, in `src/refusal.rs`.
#[test]
fn the_in_flight_refusal_routes_to_its_token() {
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

    let stderr = refusal_stderr(&["workweave", "create", "--project", "web-app", "a/b"], &ws);
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

    let stderr = refusal_stderr(&["member", "add", "github/owner/re\\po"], &ws);
    assert!(
        stderr.contains("backslash not allowed"),
        "precondition: this is the typed path error:\n{stderr}"
    );
    assert_routes_to(&stderr, "backslash-in-repo-path");
}

/// `(project)` names the project repo wherever a repo is a key, so a clone
/// adopted at that path is refused, and a manifest keying a repo on it fails
/// to parse.
#[test]
fn a_repo_path_of_project_routes_to_reserved_repo_path() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    std::fs::create_dir_all(ws.join("(project)")).unwrap();
    std::fs::write(ws.join("(project)/README"), "x\n").unwrap();
    git_init_with_commit(&ws.join("(project)"));

    let stderr = refusal_stderr(&["member", "add", "--adopt", "(project)"], &ws);
    assert_routes_to(&stderr, "reserved-repo-path");

    std::fs::write(
        ws.join("projects/web-app/rwv.toml"),
        "[repositories.\"(project)\"]\ntype = \"git\"\n\
         url = \"https://github.com/example/server\"\nbranch = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    let stderr = refusal_stderr(&["lock"], &ws);
    assert!(
        stderr.contains("failed to parse rwv.toml")
            && stderr
                .contains("`(project)` names the project repo, so no member can live at that path"),
        "the manifest must fail to load, naming why the key is refused:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// The rwv member add --new creation surface
// ---------------------------------------------------------------------------

/// `rwv member add local --new` with `owner` and `repo` but no `root` — the driving
/// input fork 7's ruling turns into a one-flag repair.
#[test]
fn a_missing_creation_param_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    let stderr = refusal_stderr(
        &[
            "member",
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
            "member",
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

/// docs/reference/refusals.md, `unusable-creation-param`: an empty `root`
/// refuses, and the message says what it got.
#[test]
fn an_empty_creation_root_refuses_naming_what_it_got() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    let stderr = refusal_stderr(
        &[
            "member",
            "add",
            "local",
            "--new",
            "--param",
            "root=",
            "--param",
            "owner=acme",
            "--param",
            "repo=fresh",
        ],
        &ws,
    );
    assert!(
        stderr.contains("creation parameter 'root' must name a path") && stderr.contains("got ''"),
        "{stderr}"
    );
    assert_routes_to(&stderr, "unusable-creation-param");
}

/// docs/reference/refusals.md, `unusable-creation-param`: a relative `root`
/// "resolves against the directory rwv runs in (the `-C` or `-w` directory
/// when one is given), and the messages name the absolute path it resolved
/// to".
#[test]
fn a_relative_creation_root_is_named_by_the_absolute_path_under_c() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(outside.join("repos")).unwrap();

    let stderr = refusal_stderr(
        &[
            "-C",
            ws.to_str().unwrap(),
            "member",
            "add",
            "local",
            "--new",
            "--param",
            "root=repos",
            "--param",
            "owner=acme",
            "--param",
            "repo=fresh",
        ],
        &outside,
    );
    assert!(
        stderr.contains(&format!(
            "the root '{}' does not exist",
            ws.join("repos").display()
        )),
        "{stderr}"
    );
}

/// A Windows-looking `root` on a Unix host is a relative path whose first
/// component happens to be `C:\x`, so it is read from the directory rwv runs
/// in and not as an absolute path.
#[cfg(unix)]
#[test]
fn a_windows_looking_creation_root_is_relative_on_unix() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(outside.join(r"C:\x")).unwrap();

    let stderr = refusal_stderr(
        &[
            "-C",
            ws.to_str().unwrap(),
            "member",
            "add",
            "local",
            "--new",
            "--param",
            r"root=C:\x",
            "--param",
            "owner=acme",
            "--param",
            "repo=fresh",
        ],
        &outside,
    );
    assert!(
        stderr.contains(&format!(
            "the root '{}' does not exist",
            ws.join(r"C:\x").display()
        )),
        "{stderr}"
    );
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
            "member",
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

/// Two `rwv member add local --new` invocations naming the same owner/repo under
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
            "member",
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
            "member",
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

/// B5: `rwv member add --adopt <path>` on a clone with no remote — nothing to
/// read a URL from, so there is nothing to register it under.
#[test]
fn an_adopt_of_a_clone_with_no_remote_routes_to_no_remote_url() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    let clone_dir = ws.join(SERVER_PATH);
    std::fs::create_dir_all(&clone_dir).unwrap();
    std::fs::write(clone_dir.join("README.md"), "init\n").unwrap();
    git_init_with_commit(&clone_dir);

    let stderr = refusal_stderr(&["member", "add", "--adopt", SERVER_PATH], &ws);
    assert!(
        stderr.contains("has no remote to read a URL from"),
        "precondition: this is the no-remote-url refusal:\n{stderr}"
    );
    assert_routes_to(&stderr, "no-remote-url");
}

/// `rwv member add --adopt <path>` on a clone whose `origin` is a relative path:
/// git reads it from each checkout's own directory, so it names no one URL
/// the manifest could record.
#[test]
fn an_adopt_of_a_relative_remote_routes_to_no_remote_url() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    let up = up_remote(tmp.path());
    let clone_dir = ws.join(SERVER_PATH);
    std::fs::create_dir_all(clone_dir.parent().unwrap()).unwrap();
    common::git_in(
        tmp.path(),
        &["clone", &up.to_string_lossy(), &clone_dir.to_string_lossy()],
    );
    common::git_in(&clone_dir, &["remote", "set-url", "origin", "../../up.git"]);

    let stderr = refusal_stderr(&["member", "add", "--adopt", SERVER_PATH], &ws);
    assert!(
        stderr.contains("relative path '../../up.git'"),
        "the refusal names the recorded path:\n{stderr}"
    );
    assert_routes_to(&stderr, "no-remote-url");

    let set_url =
        common::advised_command(&stderr, "git -C").replace("<url>", &common::file_url(&up));
    let rerun = common::advised_command(&stderr, "rwv member add --adopt");
    let findings_before = common::doctor_findings(&ws);
    let set = common::run_advised(&set_url, &ws);
    assert!(set.status.success(), "`{set_url}`");
    let adopted = common::run_advised(&rerun, &ws);
    assert!(
        adopted.status.success(),
        "`{rerun}` after `{set_url}`:\n{}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    let manifest = std::fs::read_to_string(ws.join("projects/web-app/rwv.toml")).unwrap();
    assert!(
        manifest.contains(&format!("url = \"{}\"", common::file_url(&up))),
        "the manifest records the URL the clone was given:\n{manifest}"
    );
    common::assert_no_new_findings(&ws, &findings_before);
}

fn up_remote(tmp: &Path) -> PathBuf {
    let bare = tmp.join("remotes/up.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    bare
}

/// The selector errors, which are three conditions under one type. A single
/// drive would pass on an arm that answered the same kind for all three.
#[test]
fn each_selector_condition_routes_to_its_own_token() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    for (args, token) in [
        (["update", "--remote", "--role", "bogus"], "unknown-role"),
        (
            ["update", "--remote", "--repo", "re:"],
            "empty-selector-pattern",
        ),
        (
            ["update", "--remote", "--repo", "re:["],
            "uncompilable-selector",
        ),
    ] {
        let stderr = refusal_stderr(&args, &ws);
        assert_routes_to(&stderr, token);
    }
}

// ---------------------------------------------------------------------------
// The resume path
// ---------------------------------------------------------------------------

/// `--continue` with nothing recorded. The op-state module's own refusal,
/// reached without a sync engine in the way.
#[test]
fn a_resume_with_no_recorded_op_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let (_primary, workweave) = primary_and_workweave(tmp.path());

    let stderr = refusal_stderr(&["land", "--continue"], &workweave);
    assert!(
        stderr.contains("no sync/land op in progress")
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
        stderr.contains(&format!("rwv lock: {SERVER_PATH}: edits dirty\n")),
        "precondition: the dirt preflight fired:\n{stderr}"
    );
    assert_routes_to(&stderr, "edits");
}

/// `rwv land` from a workweave carrying uncommitted tracked changes. A
/// different producer from the lock preflight above and the same token: this
/// one assembles its body from two conditions and picks the kind itself.
#[test]
fn a_dirty_workweave_at_sync_to_time_routes_to_its_token() {
    let tmp = common::tempdir().unwrap();
    let (_primary, workweave) = primary_and_workweave(tmp.path());

    let manifest = workweave.join("projects/web-app/rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}# edited, uncommitted\n")).unwrap();

    let stderr = refusal_stderr(&["land", "--history", "linear"], &workweave);
    assert!(
        stderr.contains("rwv land: projects/web-app: edits dirty\n    rwv.toml\n"),
        "precondition: the land dirt preflight fired:\n{stderr}"
    );
    assert_routes_to(&stderr, "edits");
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
/// UNIX ONLY, and for the same reason `tests/doctor/doctor_unreadable_projects_dir_test.rs`
/// is: mode `0o000` denies the read on Unix, where the Windows read-only
/// attribute does not deny one at all and the repo would simply scan clean.
#[test]
#[cfg(unix)]
fn a_dirty_repo_outranks_an_unreadable_one_in_the_same_refusal() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = common::tempdir().unwrap();
    let (_primary, workweave) = primary_and_workweave(tmp.path());

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
        .args(["land", "--history", "linear"])
        .current_dir(&workweave)
        .output()
        .expect("rwv should run");
    std::fs::set_permissions(&server, original).unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "land was expected to refuse:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv land: projects/web-app: edits dirty\n    .gitattributes\n"),
        "precondition: the actionable half is present:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "rwv land: {SERVER_PATH}: edits dirty\n    (status unreadable)\n"
        )),
        "precondition: the unreadable half is present too:\n{stderr}"
    );
    assert_eq!(
        routed_tokens(&stderr).first(),
        Some(&"edits"),
        "the actionable half routes first:\n{stderr}"
    );
}

/// `rwv member remove --delete` of a clone a workweave's worktree derives from: the
/// clone is judged on the `checkout` kind, the workweave named and its delete
/// offered, and nothing else is refused first.
#[test]
fn a_clone_a_workweave_derives_from_routes_to_checkout() {
    let tmp = common::tempdir().unwrap();
    let (primary, _workweave) = primary_and_workweave(tmp.path());

    let stderr = refusal_stderr(&["member", "remove", SERVER_PATH, "--delete"], &primary);
    assert!(
        stderr.contains(&format!(
            "rwv member remove: {SERVER_PATH}: checkout in another workspace (web-app--ww)\n"
        )) && stderr.contains("  free the other checkout\n    rwv -C ")
            && stderr.contains("    rwv workweave delete ww --project web-app\n"),
        "precondition: the store's checkouts were judged:\n{stderr}"
    );
    assert_routes_to(&stderr, "checkout");
    assert!(primary.join(SERVER_PATH).join(".git").exists());
}

/// A workweave name the index already records at another path is the `name`
/// kind's `taken`, refused before anything is created.
#[test]
fn a_workweave_name_recorded_elsewhere_routes_to_name() {
    let tmp = common::tempdir().unwrap();
    let (primary, _workweave) = primary_and_workweave(tmp.path());
    let elsewhere = tmp.path().join("elsewhere");

    let stderr = refusal_stderr(
        &[
            "workweave",
            "create",
            "--project",
            "web-app",
            "ww",
            "--dir",
            &elsewhere.to_string_lossy(),
        ],
        &primary,
    );
    assert!(
        stderr.contains("rwv workweave create: projects/web-app: name taken (ww)\n")
            && stderr.contains("  use another name\n"),
        "precondition: the index's record was judged:\n{stderr}"
    );
    assert_routes_to(&stderr, "name");
    assert!(!elsewhere.exists(), "a refused create makes nothing");
}

/// A member whose push the remote rejects withholds the project repo: the
/// member is judged `publish` `withheld`, in the terminal and in the `--json`
/// envelope, which also still records what was pushed.
#[test]
fn a_rejected_member_push_routes_to_publish() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let member = ws.workspace.join("local/org/a");
    std::fs::write(member.join("advance.txt"), "advance").unwrap();
    common::git_in(&member, &["add", "."]);
    common::git_in(&member, &["commit", "-m", "advance"]);
    let lock = common::rwv()
        .args(["lock", "--commit"])
        .current_dir(&ws.workspace)
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    common::refuse_pushes_to(&ws.manifest_bares[0].1, "refs/heads/main");

    let stderr = refusal_stderr(&["push"], &ws.workspace);
    assert!(
        stderr.contains("rwv push: local/org/a: publish withheld\n")
            && stderr.contains("projects/alpha was not pushed")
            && stderr.contains("  publish it (again)\n    rwv push\n"),
        "precondition: the rejected push was judged:\n{stderr}"
    );
    assert_routes_to(&stderr, "publish");

    let out = common::rwv()
        .args(["push", "--json"])
        .current_dir(&ws.workspace)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "one JSON document ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(envelope["refused"][0]["axis"], "publish", "{envelope:#}");
    assert_eq!(envelope["refused"][0]["state"], "withheld", "{envelope:#}");
    assert_eq!(
        envelope["refused"][0]["path"], "local/org/a",
        "{envelope:#}"
    );
    assert_eq!(envelope["outcomes"][0]["kind"], "failed", "{envelope:#}");
}

/// `rwv land` judges both sides it would move before refusing on either:
/// the source's dirty member and the target's, each named in its own line,
/// the target's with the workspace it is in, routed once. The member holds
/// work to land, so the landing moves it on both sides.
#[test]
fn a_landing_judges_the_edits_of_both_sides_in_one_refusal() {
    let tmp = common::tempdir().unwrap();
    let (primary, workweave) = primary_and_workweave(tmp.path());
    let source = workweave.join(SERVER_PATH);
    std::fs::write(source.join("work.txt"), "work\n").unwrap();
    common::git_in(&source, &["add", "work.txt"]);
    common::commit_staged(&source, "work to land");
    for side in [&primary, &workweave] {
        std::fs::write(side.join(SERVER_PATH).join("README.md"), "edited\n").unwrap();
    }

    let stderr = refusal_stderr(&["land", "--history", "linear"], &workweave);
    for line in [
        format!("rwv land: {SERVER_PATH}: edits dirty\n    README.md\n"),
        format!(
            "rwv land: {SERVER_PATH} in {}: edits dirty\n    README.md\n",
            primary.display()
        ),
    ] {
        assert!(stderr.contains(&line), "expected {line:?}:\n{stderr}");
    }
    assert_routes_to(&stderr, "edits");
}

/// A verb refused on the repos it judged before printing anything prints
/// the refusal envelope under `--json`, carrying each judged repo.
#[test]
fn a_judged_refusal_before_any_output_prints_the_refusal_envelope() {
    let tmp = common::tempdir().unwrap();
    let (_primary, workweave) = primary_and_workweave(tmp.path());
    let manifest = workweave.join("projects/web-app/rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}# edited, uncommitted\n")).unwrap();

    let out = common::rwv()
        .args(["land", "--history", "linear", "--json"])
        .current_dir(&workweave)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "one JSON document ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(
        envelope["$schema"],
        repoweave::diagnosis::REFUSAL_SCHEMA_URL,
        "{envelope:#}"
    );
    assert_eq!(
        envelope["refused"][0]["path"], "projects/web-app",
        "{envelope:#}"
    );
    assert_eq!(envelope["refused"][0]["axis"], "edits", "{envelope:#}");
    assert_eq!(
        envelope["refused"][0]["choices"][0]["id"], "C13",
        "{envelope:#}"
    );
    assert_routes_to(&stderr, "edits");
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

    let cases: [(&[&str], &str); 16] = [
        (
            &["member", "add", "https://example.com"],
            "no-matching-registry",
        ),
        (&["member", "add", "foo"], "unresolvable-repo-source"),
        (
            &["clone", "/srv/repos/web-app.git"],
            "unresolvable-repo-source",
        ),
        (
            &["member", "add", "--new", "github/owner"],
            "malformed-repo-path",
        ),
        (
            &["init", "elsewhere", "--provider", "nosuch/owner"],
            "unknown-registry",
        ),
        (
            &["init", "elsewhere", "--provider", "noslash"],
            "malformed-provider",
        ),
        (
            &["member", "remove", "nonexistent/path/repo"],
            "repo-not-in-manifest",
        ),
        (&["init", "web-app"], "project-dir-occupied"),
        (&["init", "web-app/sub"], "nested-project-name"),
        (&["-w", "a/b", "status"], "wrong-address-flag"),
        (
            &["-w", "noseparator", "status"],
            "malformed-workweave-address",
        ),
        (&["update", "--force"], "inapplicable-flag"),
        (&["nosuchverb"], "unknown-verb"),
        (&["explain", "nosuchverb"], "no-explain-entry"),
        (&["doctor", "--kind", "nosuchkind"], "unknown-finding-kind"),
        (
            &["update", "--remote", "--repo", "glob:"],
            "empty-selector-pattern",
        ),
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

    let stderr = refusal_stderr(&["member", "remove", "nonexistent/path/repo"], &ws);
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
         url = \"file:///nonexistent/absent\"\nbranch = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();

    let stderr = refusal_stderr(&["update", "--remote"], &ws);
    assert_routes_to(&stderr, "partial-run-aborted");
}

// ---------------------------------------------------------------------------
// The silence
// ---------------------------------------------------------------------------

/// An error that is not a refusal gains nothing. Nothing was declined on
/// purpose here: the manifest parser said no, and its answer reaches the
/// operator relabeled rather than classified.
#[test]
fn an_error_with_no_kind_prints_no_route_line() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    std::fs::write(ws.join("projects/web-app/rwv.toml"), "not = = toml\n").unwrap();

    let stderr = refusal_stderr(&["member", "remove", "nonexistent/path/repo"], &ws);
    assert!(
        stderr.contains("TOML parse error"),
        "precondition: the failure is the parser's, not a refusal:\n{stderr}"
    );
    assert!(
        routed_tokens(&stderr).is_empty(),
        "an unkinded error must print no route line, stderr was:\n{stderr}"
    );
}

/// Stderr of a failing run with backtraces requested the way an operator
/// requests them.
fn stderr_under_rust_backtrace(args: &[&str], cwd: &Path) -> String {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .env("RUST_BACKTRACE", "1")
        .env_remove("RUST_LIB_BACKTRACE")
        .output()
        .expect("rwv should run");
    assert!(
        !output.status.success(),
        "rwv {args:?} was expected to fail"
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A refusal was declined on purpose; the stack it was declined from is not
/// the operator's to read, whatever `RUST_BACKTRACE` says.
#[test]
fn a_refusal_prints_no_backtrace_under_rust_backtrace() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());

    let stderr = stderr_under_rust_backtrace(&["abort"], &ws);
    assert_routes_to(&stderr, "no-op-recorded");
    assert!(
        !stderr.contains("backtrace"),
        "a refusal must print no backtrace, stderr was:\n{stderr}"
    );
}

/// The counterpart: an error rwv did not decline on purpose keeps the
/// backtrace `RUST_BACKTRACE` asks for, since where it came from is the
/// report's whole value.
#[test]
fn an_error_with_no_kind_keeps_its_backtrace_under_rust_backtrace() {
    let tmp = common::tempdir().unwrap();
    let ws = plain_weave(tmp.path());
    std::fs::write(ws.join("projects/web-app/rwv.toml"), "not = = toml\n").unwrap();

    let stderr = stderr_under_rust_backtrace(&["member", "remove", "nonexistent/path/repo"], &ws);
    assert!(
        stderr.contains("TOML parse error") && routed_tokens(&stderr).is_empty(),
        "precondition: the failure is the parser's, not a refusal:\n{stderr}"
    );
    assert!(
        stderr.contains("\n\nStack backtrace:\n"),
        "an unkinded error must keep its backtrace, stderr was:\n{stderr}"
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
        .args(["member", "remove", "nonexistent/path/repo"])
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
             branch = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    common::fixture_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &sha)]);
    git_init_with_commit(&project_dir);
    common::set_up_lock_driver_committed(&project_dir);
    common::record_trunk(&project_dir);
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
    common::record_feature(&project_dir, "web-app--ww", "main");
    common::register_workweave(&primary, "web-app", "ww", &ww_root);
    std::fs::write(
        ww_root.join(".rwv-workweave"),
        common::workweave_marker(&primary, "web-app", &primary),
    )
    .unwrap();

    (primary, ww_root)
}
