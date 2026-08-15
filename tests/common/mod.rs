#![allow(dead_code)]

pub mod compile_probe;
pub mod contract;
pub mod doctor_corpus;
pub mod json_schema;
pub mod src_scan;

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// A temporary directory whose path is already canonical. Drop-in for
/// `tempfile::tempdir()`; use it for every fixture root in the suite.
///
/// `tempfile` hands back whatever `$TMPDIR` names, and on macOS that is under
/// `/var`, a symlink to `/private/var`. rwv canonicalizes the paths it prints,
/// so an expected path a test builds from a raw temp root is a *different
/// spelling of the same file* than the one rwv reports. Every such comparison
/// passes on Linux, where `/tmp` is a real directory, and fails on macOS —
/// which is how macOS CI stayed red through a release while Linux was green.
/// `git worktree list --porcelain` resolves the same way, so the mismatch is
/// not limited to paths rwv itself prints.
///
/// Canonicalizing the root here rather than at each comparison is the point.
/// A rule applied where paths are compared has to be remembered by every test
/// anyone adds later; rooted here there is no non-canonical path in the suite
/// to get wrong. `canonical_temp_root_test.rs` keeps it that way.
///
/// Reproduce the macOS geometry on any platform:
///
/// ```sh
/// mkdir -p $T/real && ln -s $T/real $T/link
/// TMPDIR=$T/link cargo test --release --no-fail-fast
/// ```
///
/// Pick a `$T` outside any repoweave weave: a temp root nested under one puts
/// every fixture inside it, and the suite's "outside a workspace" tests then
/// fail for that reason instead.
///
/// On Windows `canonicalize` always answers in the `\\?\` extended-length
/// form, and git refuses an argument spelled that way — so a root left in
/// that spelling fails every fixture helper that runs git against a fixture
/// path. `dunce::simplified` drops the prefix only where Windows itself
/// accepts the short form and is the identity on every other platform, the
/// same strip production applies in `src/git.rs` where a path becomes a git
/// argument.
pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    let root = ROOT.get_or_init(|| {
        let raw = std::env::temp_dir();
        let canonical = raw
            .canonicalize()
            .unwrap_or_else(|e| panic!("temp dir {} does not resolve: {e}", raw.display()));
        dunce::simplified(&canonical).to_path_buf()
    });
    tempfile::TempDir::new_in(root)
}

/// Render a fixture path as a `file://` URL git accepts on every platform.
///
/// `format!("file://{}", path.display())` breaks twice on Windows: the
/// backslashes are escape characters inside a TOML or JSON string the URL is
/// written into, and a drive-letter path pasted after `file://` puts `C:` in
/// the URL's host position. Forward slashes plus a third `/` for a rootless
/// path give the `file:///C:/…` form; on Unix the output is byte-identical
/// to the `format!` it replaces.
pub fn file_url(path: impl AsRef<std::path::Path>) -> String {
    format!("file://{}", url_path(path))
}

/// The path half of [`file_url`]: forward slashes, rooted with a leading `/`
/// so a Windows drive-letter path becomes `/C:/…`. For templates that spell
/// the `file://` prefix themselves.
pub fn url_path(path: impl AsRef<std::path::Path>) -> String {
    let p = dunce::simplified(path.as_ref())
        .to_str()
        .expect("fixture path is valid UTF-8")
        .replace('\\', "/");
    if p.starts_with('/') {
        p
    } else {
        format!("/{p}")
    }
}

/// A path's JSON string body: the serde_json encoding minus the surrounding
/// quotes, for hand-built state-file templates that spell the quotes
/// themselves. A Windows path's backslashes read as JSON escapes if pasted
/// raw. Unlike [`url_path`] the spelling is preserved, because rwv compares
/// a record's workspace paths against the live ones.
///
/// TOML basic strings take the same escape forms for everything a path can
/// contain, so this is also the rendering for a path inside a hand-built
/// `.toml` fixture.
pub fn json_escaped(path: impl AsRef<std::path::Path>) -> String {
    let quoted =
        serde_json::to_string(path.as_ref().to_str().expect("fixture path is valid UTF-8"))
            .expect("a string serializes infallibly");
    quoted[1..quoted.len() - 1].to_string()
}

/// The `.rwv-workweave` marker JSON, built with real serialization so a
/// Windows path's backslashes arrive escaped rather than read as escapes.
/// Fixtures planted this shape as a hand-formatted template at 30+ sites;
/// build it here so no site can get the encoding wrong.
pub fn workweave_marker(
    primary: impl AsRef<std::path::Path>,
    project: &str,
    parent: impl AsRef<std::path::Path>,
) -> String {
    format!(
        "{{\"primary\":\"{}\",\"project\":\"{project}\",\"parent\":\"{}\"}}",
        json_escaped(primary),
        json_escaped(parent),
    )
}

/// Record `dir` as `project`'s workweave `name` in the primary-side index.
///
/// The companion to [`workweave_marker`]: `workweave create` writes both, and
/// a resolution reads the workweave's own name back out of this entry. A
/// fixture that plants a marker and stops there builds a directory rwv treats
/// as unregistered — a repair state, not the steady one most fixtures mean.
pub fn register_workweave(
    primary: impl AsRef<std::path::Path>,
    project: &str,
    name: &str,
    dir: impl AsRef<std::path::Path>,
) {
    let primary = primary.as_ref();
    let dir = dir.as_ref();
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let index_path = primary
        .join("projects")
        .join(project)
        .join(".rwv-workweave-index");
    let mut index: serde_json::Value = match std::fs::read_to_string(&index_path) {
        Ok(raw) => serde_json::from_str(&raw).expect("fixture: index should parse"),
        Err(_) => serde_json::json!({
            "container": canonical.parent().expect("a workweave dir has a parent"),
            "workweaves": {},
            "receipts": [],
        }),
    };
    index["workweaves"][name] = serde_json::json!(canonical);
    std::fs::create_dir_all(index_path.parent().unwrap()).unwrap();
    std::fs::write(&index_path, serde_json::to_string(&index).unwrap())
        .unwrap_or_else(|e| panic!("write {}: {e}", index_path.display()));
}

/// `read_to_string` modulo git's eol filter: under `core.autocrlf` a
/// checkout spells text content CRLF, which is the same content to git and
/// not what a content assertion is about.
pub fn read_normalized(path: impl AsRef<std::path::Path>) -> String {
    let path = path.as_ref();
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// `GIT_*` environment variables that git itself sets for hooks and that
/// would silently misdirect any subprocess `git` invocation if inherited.
const GIT_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// Build a `git` command with all inherited `GIT_*` environment variables
/// stripped. The returned `Command` has no cwd set; callers add
/// `current_dir(...)` (and any args) themselves.
///
/// Tests create temp git repos and run subprocess `git` against them. If
/// the outer process has any of `GIT_DIR`, `GIT_WORK_TREE`,
/// `GIT_INDEX_FILE`, etc. set (as is the case under a `pre-push` hook,
/// where `git` exports these for the hook), every subprocess `git` call
/// inherits them and silently operates on the *outer* repo regardless of
/// `current_dir`. That has historically corrupted the source repo's
/// `.git/config` (writing `core.bare = true`, the test `[user]` block,
/// etc.) when the test suite ran from a hook context.
pub fn git() -> Command {
    let mut cmd = Command::new("git");
    for var in GIT_ENV_VARS {
        cmd.env_remove(var);
    }
    // Make `git` non-interactive. `git rebase --continue` and any other
    // commit-completing path invoke `$EDITOR` for the commit message. In CI
    // there is no editor and no TTY, so git aborts with "Terminal is dumb,
    // but EDITOR unset". `GIT_EDITOR=true` substitutes the `true` command,
    // which exits 0 without modifying the prepared message — git uses
    // whatever it already has.
    cmd.env("GIT_EDITOR", "true");
    cmd.env("GIT_SEQUENCE_EDITOR", "true");
    // Pin `init.defaultBranch=main` for every subprocess git call. CI runners
    // don't ship a user-level `init.defaultBranch` config, so `git init`
    // falls back to `master` and tests that later do `git rev-parse main`
    // explode. Locally this is invisible because most dev machines have
    // `init.defaultBranch = main` set globally. Injecting via
    // `GIT_CONFIG_*` env vars (see git-config(1)) stacks on top of any
    // existing config without touching files.
    cmd.env("GIT_CONFIG_COUNT", "1");
    cmd.env("GIT_CONFIG_KEY_0", "init.defaultBranch");
    cmd.env("GIT_CONFIG_VALUE_0", "main");
    cmd
}

/// Assert that `commit_messages` appear in top-down order (newest-first) in
/// the log of `repo`.
///
/// This is the canonical "history shape" helper for the silent-fallback
/// elimination suite. Use it whenever a sync test must verify that
/// CWD's commits land *on top of* a target's prior tip — not below it.
///
/// `commit_messages` is a slice of substrings; each element must match exactly
/// one line in `git log --oneline --no-decorate` output, and the *position* of
/// the first match must be in strictly ascending order (i.e. earlier elements
/// appear higher / newer in the log).
///
/// Panics with a diagnostic showing the full log and the expected ordering if
/// any element is not found or the ordering is violated.
///
/// # Example
/// ```ignore
/// assert_log_ordering(
///     &project_dir,
///     &["feat: ww unique commit", "feat: primary unique commit"],
/// );
/// ```
pub fn assert_log_ordering(repo: &std::path::Path, commit_messages: &[&str]) {
    let out = git()
        .args(["log", "--oneline", "--no-decorate"])
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git log failed to start");
    assert!(
        out.status.success(),
        "git log failed in {}:\n{}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8(out.stdout).unwrap();

    let positions: Vec<(usize, &str)> = commit_messages
        .iter()
        .map(|msg| {
            let pos = log
                .lines()
                .position(|l| l.contains(msg))
                .unwrap_or_else(|| {
                    panic!(
                        "commit message {:?} not found in log of {}.\nLog:\n{log}",
                        msg,
                        repo.display()
                    )
                });
            (pos, *msg)
        })
        .collect();

    for window in positions.windows(2) {
        let (pos_a, msg_a) = window[0];
        let (pos_b, msg_b) = window[1];
        assert!(
            pos_a < pos_b,
            "History shape violation in {}:\n\
             Expected {:?} (pos {pos_a}) to appear ABOVE {:?} (pos {pos_b}) in the log.\n\
             (Lower position number = newer commit = higher in `git log` output.)\n\
             Full log:\n{log}",
            repo.display(),
            msg_a,
            msg_b
        );
    }
}

// ---------------------------------------------------------------------------
// "Which ref is this checkout on"
// ---------------------------------------------------------------------------
//
// This is the enforcement primitive the suite was missing: a fetch detach
// survived because the test that should have caught it asserted only
// `rev-parse HEAD` equality, against a fixture that had pre-detached the repo.
// A tip comparison cannot see a detach — HEAD points at the same commit either
// way. The question has to be asked about the *ref*.
//
// Two things make this a primitive rather than another local helper:
//
//  1. **It asks the production classifier.** `Vcs::head_attachment` is the
//     code under test's own answer, so a test cannot pass because the test
//     and the product disagree about what "on a branch" means.
//  2. **It does not ask git for a short name.** Every hand-rolled version of
//     this in the suite ran `git symbolic-ref --short HEAD`, and `--short`
//     answers the shortest *unambiguous* name: with a tag named `main` in the
//     repo it returns `heads/main`, which does not round-trip through
//     `refs/heads/<name>`. `observe_head` avoids `--short` deliberately, and
//     a test that reintroduces it is asserting against a different function
//     than the one that ships.
//
// The four states `current_ref`'s `Ok(None)` used to collapse stay apart
// here: `Attached` and `Unborn` answer with a name, `Detached` answers
// `None`, and a directory that is not a repo — or a ref database that cannot
// be read — panics rather than quietly reading as "no branch".

/// The name of the ref `repo`'s checkout is on, or `None` when HEAD is
/// detached.
///
/// Panics when `repo` is not a repository or its ref database is unreadable:
/// in a test those are fixture bugs, and letting them read as "detached"
/// would conflate a fixture bug with a real detached HEAD.
pub fn checkout_ref(repo: &std::path::Path) -> Option<String> {
    use repoweave::vcs::HeadAttachment;
    match repoweave::git::git_vcs().head_attachment(repo) {
        Ok(HeadAttachment::Attached(a)) => Some(a.to_string()),
        Ok(HeadAttachment::Unborn(u)) => Some(u.name().as_str().to_owned()),
        Ok(HeadAttachment::Detached(_)) => None,
        Err(e) => panic!(
            "head_attachment failed for {}: {e} — this is a fixture bug, not a \
             detached HEAD",
            repo.display()
        ),
    }
}

/// Assert that `repo`'s checkout is on the branch named `branch`.
pub fn assert_on_branch(repo: &std::path::Path, branch: &str) {
    match checkout_ref(repo) {
        Some(actual) => assert_eq!(actual, branch, "{} should be on '{branch}'", repo.display()),
        None => panic!(
            "{} should be on '{branch}' but HEAD is detached",
            repo.display()
        ),
    }
}

/// Assert that `repo`'s HEAD names no branch.
///
/// Use where a detach is the *specified* outcome (`--detach-checkouts`, a
/// lock-pinned materialization). Everywhere else, prefer [`assert_on_branch`]:
/// asserting the positive is what makes an unintended detach a failure.
pub fn assert_detached(repo: &std::path::Path) {
    if let Some(branch) = checkout_ref(repo) {
        panic!("{} should be detached but is on '{branch}'", repo.display());
    }
}

/// Build an `assert_cmd::Command` for the `rwv` binary with inherited
/// `GIT_*` environment variables stripped.
///
/// `rwv` shells out to `git` internally; if it inherits a polluted
/// `GIT_*` env from the test process, those subprocesses operate on the
/// wrong repo. See [`git`] for context.
pub fn rwv() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("rwv").expect("rwv binary should be buildable");
    for var in GIT_ENV_VARS {
        cmd.env_remove(var);
    }
    // Mirror the `init.defaultBranch=main` pin from [`git`] — rwv shells out
    // to git internally and those subprocesses inherit this env, so any
    // `git init` rwv runs on behalf of a test gets `main` as the default
    // branch regardless of CI runner config.
    cmd.env("GIT_CONFIG_COUNT", "1");
    cmd.env("GIT_CONFIG_KEY_0", "init.defaultBranch");
    cmd.env("GIT_CONFIG_VALUE_0", "main");
    cmd
}

/// The spelling `CreateProcess` can execute — the same fact
/// `integrations::node_tool` states for production: npm-family tools install
/// `.cmd` shims on Windows, and `Command` runs a script through the
/// interpreter only when the name spells its extension.
pub fn node_tool(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.to_string()
    }
}

/// Whether `path`'s trailing components equal `suffix`'s, whatever separator
/// or absolute prefix the platform spelled the path with. The suffix is an
/// identity written with `/`; the path under test is whatever a surface
/// printed. Pinning arrival of the right components without blessing any one
/// spelling is what keeps these assertions platform-honest while the spelling
/// itself stays an open design question.
pub fn path_ends_with(path: impl AsRef<str>, suffix: &str) -> bool {
    let path = path.as_ref();
    let hay: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let want: Vec<&str> = suffix.split('/').filter(|s| !s.is_empty()).collect();
    hay.len() >= want.len() && hay[hay.len() - want.len()..] == want[..]
}

/// Whether two spellings denote one path: compared by components with the
/// Windows verbatim prefix simplified away, so `\\?\C:\ws\x`, `C:\ws/x` and
/// `C:/ws/x` are one path and a genuinely different file never is. For
/// asserting a surface named the right file without pinning which spelling
/// the surface uses — the spelling itself is an open design question.
pub fn same_path(a: impl AsRef<std::path::Path>, b: impl AsRef<std::path::Path>) -> bool {
    dunce::simplified(a.as_ref()) == dunce::simplified(b.as_ref())
}

/// Flatten path spelling inside prose: separators to `/`, the verbatim
/// prefix dropped. For asserting a message names a file when the message
/// holds whatever spelling a surface printed — compare both sides through
/// this, never one.
pub fn flatten_path_spelling(s: &str) -> String {
    s.replace('\\', "/").replace("//?/", "")
}

/// Assert the context display's `Weave:` line names `root`, in the spelling
/// the operator seam mints for it.
///
/// Whole-line equality rather than containment: the simplified spelling is a
/// substring of the verbatim one, so `stdout.contains(simplified)` is
/// satisfied by a line that still carries the Windows `\\?\` prefix — it is
/// green exactly when the leak it would catch is present.
pub fn assert_weave_line(stdout: &str, root: impl AsRef<std::path::Path>) {
    let named = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Weave: "))
        .unwrap_or_else(|| panic!("context display has no `Weave:` line:\n{stdout}"));
    assert_eq!(
        named,
        repoweave::path_spelling::operator_path(root.as_ref()),
        "the `Weave:` line must name the weave root in the operator spelling"
    );
}
