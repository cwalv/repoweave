//! `.githooks/pre-push` used to hand-maintain its own clippy/fmt/test list,
//! which had already drifted from `scripts/ci-local.sh` by the time this was
//! noticed. It now delegates the whole gate to the one script, so there is
//! nothing there left to drift. It also used to run that gate against the
//! working tree the push happened from — a tree that proves nothing about
//! the commit being pushed, and whose surroundings (a weave root's
//! `[workspace]` manifest above the checkout) could red a green commit. It
//! now checks out each pushed commit into a throwaway detached worktree
//! under the temp dir and gates that.
//!
//! The first test pins the delegation at the source-text level: no bare
//! `cargo` invocation survives in the hook outside its own comments. The
//! rest drive the hook as a real subprocess — against an isolated fixture
//! carrying its own copy of `scripts/ci-local.sh`, never the live checkout —
//! with stub `cargo`/`rustup`/`gh` on `PATH`, feeding the
//! `<local_ref> <local_sha> <remote_ref> <remote_sha>` lines git writes to
//! the hook's stdin. They pin that the gate keys on the pushed commits
//! (once per distinct commit; deletions and empty pushes gate nothing) and
//! that both the gate and the version-tag check read the pushed commit's
//! tree, in both directions: a broken working tree cannot red a green
//! commit, and a green working tree cannot green a broken commit.
//!
//! `#![cfg(unix)]`: `.githooks/pre-push` is a `#!/bin/sh` script; every
//! helper here exists for a target this suite already can't run on, and a
//! per-test `#[cfg(unix)]` would strand them as dead code on the platform
//! that denies warnings on the host target.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;

const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

// Not `exit 1`: that exact body is the needle hook_fixture_portability_test
// keys its refusing-git-hook corpus on, and these fixtures are failing gate
// scripts, not git hooks — a different class that must not enter that scan.
const BROKEN_GATE: &str = "#!/bin/sh\nexit 7\n";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn pre_push_hook() -> PathBuf {
    repo_root().join(".githooks/pre-push")
}

fn non_comment_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| !l.trim_start().starts_with('#'))
}

/// True when `line` names the command `cargo` — the word, not a prefix of
/// `cargo_version` and not the `Cargo.toml` the hook legitimately reads.
fn names_the_cargo_command(line: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    line.match_indices("cargo").any(|(at, _)| {
        !word(line[..at].chars().next_back()) && !word(line[at + "cargo".len()..].chars().next())
    })
}

/// Structural license: a prohibition over an enumerable population — the
/// hook's own non-comment lines. The behavioural sibling below
/// (`stub_cargo_count == gate runs * 7`) sees only invocations on a path the
/// fixture drives whose output reaches the hook's stdout, and both of those
/// are escapable: a `cargo` call in the CI-not-green refusal arm, which the
/// `gh` stub makes unreachable, and a `cargo` call whose output is redirected
/// both leave that count unchanged. Neither is escapable here, because this
/// reads the file rather than a run of it.
///
/// Scope, and therefore the coverage boundary: `.githooks/pre-push` only,
/// lines not starting with `#`, matching `cargo` as a word. Invisible to it —
/// a cargo run reached through a variable or a shell function (`$CARGO
/// clippy`, `c() { cargo "$@"; }`), a gate script the hook delegates to that
/// itself drifts, and every other file in the repo.
///
/// The complement runs the other way: the hook can delegate to
/// `scripts/ci-local.sh --stages=check` — no bare cargo, still a subset gate —
/// and only the behavioural count reddens.
#[test]
fn contains_no_bare_cargo_invocations() {
    let text = std::fs::read_to_string(pre_push_hook()).expect("pre-push hook should exist");
    for line in non_comment_lines(&text) {
        assert!(
            !names_the_cargo_command(line),
            "pre-push hook invokes cargo directly, bypassing scripts/ci-local.sh: {line:?}"
        );
    }
    assert!(
        non_comment_lines(&text).any(|l| l.contains("scripts/ci-local.sh")),
        "pre-push hook should delegate to scripts/ci-local.sh"
    );
}

/// The predicate above is the instrument, so it gets fed a known red and a
/// known green: the hook's own `cargo_version=` assignment and the
/// `Cargo.toml` blob it reads are what a naive substring match trips on.
#[test]
fn the_cargo_command_predicate_separates_invocations_from_mentions() {
    for invocation in [
        "cargo build --release",
        "  cargo fmt --all -- --check >/dev/null 2>&1",
        "CARGO_INCREMENTAL=0 cargo test",
        "(cd \"$dir\" && cargo xtask)",
    ] {
        assert!(
            names_the_cargo_command(invocation),
            "should read as a cargo invocation: {invocation:?}"
        );
    }
    for mention in [
        "      cargo_version=\"v$(git cat-file blob \"$local_sha:Cargo.toml\")\"",
        "if [ \"$tag\" != \"$cargo_version\" ]; then",
        "CARGO_INCREMENTAL=0 RUSTC_WRAPPER=\"$rustc_wrapper\" ./scripts/ci-local.sh",
    ] {
        assert!(
            !names_the_cargo_command(mention),
            "should not read as a cargo invocation: {mention:?}"
        );
    }
}

const STUB_CARGO: &str = "#!/bin/sh\nprintf 'STUB_CARGO: %s\\n' \"$*\"\nexit 0\n";

// Always reports the target installed, so the delegated gate's windows stage
// runs the check (via the stub cargo above) rather than skipping — these tests
// count one cargo invocation per stage and a skip would silently drop one.
const STUB_RUSTUP: &str = "#!/bin/sh\n\
if [ \"$*\" = \"target list --installed\" ]; then\n\
    echo x86_64-pc-windows-msvc\n\
    exit 0\n\
fi\n\
exit 0\n";

// Answers the two queries the version-tag stanza makes: a repo slug for
// `gh repo view` and a green conclusion for `gh api`. The fixture has no
// remote, so the real gh would fail before either answer.
const STUB_GH: &str = "#!/bin/sh\n\
case \"$1\" in\n\
    repo) echo stub/stub ;;\n\
    api) echo success ;;\n\
esac\n\
exit 0\n";

fn head_sha(root: &Path) -> String {
    common::git_in(root, &["rev-parse", "HEAD"])
}

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fixture carrying its own `scripts/ci-local.sh` (copied from this
/// checkout), a `Cargo.toml` at version 0.0.1 for the version-tag stanza, and
/// the three artifact directories the drift stage diffs, so driving the real
/// hook never touches this checkout's working tree.
fn fixture_repo() -> tempfile::TempDir {
    let dir = common::tempdir().expect("tempdir");
    let root = dir.path();

    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::copy(
        repo_root().join("scripts/ci-local.sh"),
        root.join("scripts/ci-local.sh"),
    )
    .unwrap();
    std::fs::set_permissions(
        root.join("scripts/ci-local.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.1\"\n",
    )
    .unwrap();

    for sub in [
        "docs/reference/explain",
        "docs/reference/schemas",
        "docs/reference/prime",
    ] {
        std::fs::create_dir_all(root.join(sub)).unwrap();
        std::fs::write(root.join(sub).join("placeholder.txt"), "generated\n").unwrap();
    }

    common::git_in(root, &["init", "-q", "--initial-branch=main"]);
    common::git_in(root, &["config", "user.email", "test@test.com"]);
    common::git_in(root, &["config", "user.name", "Test"]);
    common::git_in(root, &["add", "-A"]);
    common::git_in(root, &["commit", "-q", "-m", "init"]);
    dir
}

/// Run the real hook in `cwd` with the given stdin lines and the stub
/// toolchain first on PATH.
fn run_hook(cwd: &Path, stdin: &str) -> std::process::Output {
    let stub_dir = common::tempdir().expect("tempdir");
    for (name, body) in [
        ("cargo", STUB_CARGO),
        ("rustup", STUB_RUSTUP),
        ("gh", STUB_GH),
    ] {
        write_executable(&stub_dir.path().join(name), body);
    }

    let real_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{real_path}", stub_dir.path().display());

    let mut child = Command::new(pre_push_hook())
        .current_dir(cwd)
        .env("PATH", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("pre-push hook should run");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(stdin.as_bytes())
        .expect("write hook stdin");
    child.wait_with_output().expect("hook should exit")
}

fn stub_cargo_count(out: &std::process::Output) -> usize {
    String::from_utf8_lossy(&out.stdout)
        .matches("STUB_CARGO:")
        .count()
}

fn assert_green(out: &std::process::Output, expected_gate_runs: usize) {
    assert!(
        out.status.success(),
        "hook should pass:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stub_cargo_count(out),
        expected_gate_runs * 7,
        "expected one cargo invocation per ci-local.sh stage per gated commit:\n{stdout}"
    );
    assert!(stdout.contains("pre-push: all checks passed"));
}

#[test]
fn a_branch_push_gates_the_pushed_commit_once() {
    let fixture = fixture_repo();
    let head = head_sha(fixture.path());

    let out = run_hook(
        fixture.path(),
        &format!("refs/heads/main {head} refs/heads/main {ZERO_SHA}\n"),
    );

    assert_green(&out, 1);
    assert!(String::from_utf8_lossy(&out.stdout).contains("All checks passed."));
}

#[test]
fn an_empty_push_gates_nothing() {
    let fixture = fixture_repo();
    let out = run_hook(fixture.path(), "");
    assert_green(&out, 0);
}

#[test]
fn a_deletion_push_gates_nothing() {
    let fixture = fixture_repo();
    let head = head_sha(fixture.path());

    let out = run_hook(
        fixture.path(),
        &format!("refs/heads/gone {ZERO_SHA} refs/heads/gone {head}\n"),
    );

    assert_green(&out, 0);
}

#[test]
fn two_refs_at_one_commit_gate_it_once() {
    let fixture = fixture_repo();
    let head = head_sha(fixture.path());

    let out = run_hook(
        fixture.path(),
        &format!(
            "refs/heads/main {head} refs/heads/main {ZERO_SHA}\n\
             refs/heads/other {head} refs/heads/other {ZERO_SHA}\n"
        ),
    );

    assert_green(&out, 1);
}

#[test]
fn a_broken_working_tree_cannot_red_a_green_commit() {
    let fixture = fixture_repo();
    let head = head_sha(fixture.path());

    // Uncommitted breakage: the old hook ran this and failed; the new hook
    // gates the committed copy in a worktree and never reads it.
    write_executable(&fixture.path().join("scripts/ci-local.sh"), BROKEN_GATE);

    let out = run_hook(
        fixture.path(),
        &format!("refs/heads/main {head} refs/heads/main {ZERO_SHA}\n"),
    );

    assert_green(&out, 1);
}

#[test]
fn a_green_working_tree_cannot_green_a_red_commit() {
    let fixture = fixture_repo();
    let good = std::fs::read_to_string(fixture.path().join("scripts/ci-local.sh")).unwrap();

    write_executable(&fixture.path().join("scripts/ci-local.sh"), BROKEN_GATE);
    common::git_in(fixture.path(), &["commit", "-q", "-am", "break the gate"]);
    let red = head_sha(fixture.path());

    // Working tree restored to green, red commit pushed: the hook must
    // believe the commit.
    write_executable(&fixture.path().join("scripts/ci-local.sh"), &good);

    let out = run_hook(
        fixture.path(),
        &format!("refs/heads/main {red} refs/heads/main {ZERO_SHA}\n"),
    );

    assert!(
        !out.status.success(),
        "hook must fail on a commit whose gate script fails:\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("pre-push: all checks passed"));
}

#[test]
fn a_mismatched_version_tag_is_refused_before_the_gate_runs() {
    let fixture = fixture_repo();
    let head = head_sha(fixture.path());

    let out = run_hook(
        fixture.path(),
        &format!("refs/tags/v9.9.9 {head} refs/tags/v9.9.9 {ZERO_SHA}\n"),
    );

    assert!(
        !out.status.success(),
        "hook must refuse a tag that does not match the tagged Cargo.toml:\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("does not match Cargo.toml version v0.0.1"));
    assert_eq!(
        stub_cargo_count(&out),
        0,
        "the cheap tag refusal must come before any build:\n{stdout}"
    );
}

#[test]
fn the_tag_version_check_reads_the_pushed_commit_not_the_working_tree() {
    let fixture = fixture_repo();
    let head = head_sha(fixture.path());

    // Uncommitted version bump: the committed manifest still says 0.0.1, and
    // that is the version the tag must match.
    std::fs::write(
        fixture.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"9.9.9\"\n",
    )
    .unwrap();

    let out = run_hook(
        fixture.path(),
        &format!("refs/tags/v0.0.1 {head} refs/tags/v0.0.1 {ZERO_SHA}\n"),
    );

    assert_green(&out, 1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tag v0.0.1 matches Cargo.toml"));
    assert!(stdout.contains("CI is green on"));
}
