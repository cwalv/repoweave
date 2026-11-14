#![allow(dead_code)]

pub mod cli;
pub mod compile_probe;
pub mod contract;
pub mod doctor_corpus;
pub mod job;
pub mod json_schema;
pub mod pause;
pub mod records;
pub mod refusal_census;
pub mod savepoint;
pub mod scenario_invariants;
pub mod scenario_runner;
pub mod scenario_sandbox;
pub mod scenario_steps;
pub mod src_scan;

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// Announce a test that is about to return without measuring anything.
///
/// Written to the process's own stderr rather than through `eprintln!`.
/// libtest intercepts the print macros and discards what a *passing* test
/// wrote, so a skip announced that way reaches nobody unless the run already
/// passed `--nocapture` — and a reader who knew to pass it already suspected
/// the skip. A direct write is outside that interception, so the notice lands
/// beside the `test ... ok` line it contradicts.
///
/// One `write_all` of a whole line, because tests run concurrently and a
/// notice torn across two threads is worse than none.
pub fn report_skip(reason: &str) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(format!("SKIP: {reason}\n").as_bytes());
}

/// A test of this binary, by the name libtest selects it with.
///
/// A test that runs its own binary again to play a child role selects the
/// child with `--exact <name>`, and an `--exact` that matches nothing exits 0
/// having run nothing — so the role's work never happens and nothing says
/// why. The name is therefore never spelled: [`test_name`] derives it from
/// the test function itself, and checks this binary lists exactly that test
/// before handing it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestName(String);

impl TestName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The arguments that run this test alone: `[<name>, --exact,
    /// --test-threads=1]`.
    pub fn selector(&self) -> [&str; 3] {
        [&self.0, "--exact", "--test-threads=1"]
    }

    /// The path of the module the test is in, `::`-terminated: a libtest
    /// filter that selects every test beside it.
    pub fn module_filter(&self) -> &str {
        let end = self.0.rfind("::").map_or(0, |i| i + 2);
        &self.0[..end]
    }
}

/// The libtest name of `test`, a `#[test]` function of this binary.
///
/// `type_name_of_val` names a function item by its path from the crate root,
/// which is the name libtest gives the test once the crate's own name is
/// dropped. A name this binary does not list exactly once panics here,
/// before any child is started with it.
pub fn test_name<F: Fn()>(test: F) -> TestName {
    let path = std::any::type_name_of_val(&test);
    let (_, name) = path
        .split_once("::")
        .unwrap_or_else(|| panic!("{path} is not a function inside a test binary"));
    let name = TestName(name.to_string());
    assert_listed_once(&name);
    name
}

fn assert_listed_once(name: &TestName) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static CHECKED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let checked = CHECKED.get_or_init(Default::default);
    if checked.lock().unwrap().contains(name.as_str()) {
        return;
    }
    let exe = std::env::current_exe().expect("this test binary's own path");
    let out = Command::new(&exe)
        .args(["--list", "--format", "terse", "--exact", name.as_str()])
        .output()
        .unwrap_or_else(|e| panic!("listing {} failed: {e}", exe.display()));
    let listing = String::from_utf8_lossy(&out.stdout);
    let wanted = format!("{}: test", name.as_str());
    let matches = listing.lines().filter(|l| *l == wanted).count();
    assert!(
        out.status.success() && matches == 1,
        "{} does not list `{}` exactly once, so a child selected by it would run \
         nothing:\n{listing}",
        exe.display(),
        name.as_str()
    );
    checked.lock().unwrap().insert(name.as_str().to_string());
}

/// Write `contents` to `path` as a program a test will run, mode 0o755.
///
/// Exec fails with ETXTBSY while any process holds the file open for
/// writing, and a write handle this process held is inherited by every child
/// another test thread forks until that child execs — so a script written
/// here and run at once is refused at random under a parallel runner. A
/// rename into place does not help: the inherited handle is to the file, not
/// to its name. The file is written by a child process instead, which this
/// process waits on, so no write handle to it ever exists in this process.
pub fn write_executable(path: &std::path::Path, contents: impl AsRef<[u8]>) {
    write_program(path, contents.as_ref(), 0o755);
}

/// [`write_executable`] with an explicit mode, for a fixture whose subject
/// is what the OS makes of its permission bits.
pub fn write_program(path: &std::path::Path, contents: &[u8], mode: u32) {
    #[cfg(unix)]
    {
        use std::io::Write;
        let mut writer = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod \"$2\" \"$1\"", "sh"])
            .arg(path)
            .arg(format!("{mode:o}"))
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                panic!("writing {} through sh failed to start: {e}", path.display())
            });
        writer
            .stdin
            .take()
            .expect("the writer's stdin is piped")
            .write_all(contents)
            .unwrap_or_else(|e| panic!("feeding {} failed: {e}", path.display()));
        let status = writer.wait().expect("the writer is waited on");
        assert!(
            status.success(),
            "writing {} exited {status}",
            path.display()
        );
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::write(path, contents)
            .unwrap_or_else(|e| panic!("writing {} failed: {e}", path.display()));
    }
}

/// Copy `from` to `to` as a program a test will run, on the terms
/// [`write_executable`] states.
pub fn copy_executable(from: &std::path::Path, to: &std::path::Path) {
    #[cfg(unix)]
    {
        let status = Command::new("sh")
            .args(["-c", "cp -- \"$1\" \"$2\" && chmod 755 \"$2\"", "sh"])
            .arg(from)
            .arg(to)
            .status()
            .unwrap_or_else(|e| panic!("copying {} failed to start: {e}", from.display()));
        assert!(
            status.success(),
            "copying {} to {} exited {status}",
            from.display(),
            to.display()
        );
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(from, to).unwrap_or_else(|e| {
            panic!("copying {} to {} failed: {e}", from.display(), to.display())
        });
    }
}

/// How many tests of this binary libtest's `filter` selects.
pub fn tests_listed(filter: &str) -> usize {
    let exe = std::env::current_exe().expect("this test binary's own path");
    let out = Command::new(&exe)
        .args(["--list", "--format", "terse", filter])
        .output()
        .unwrap_or_else(|e| panic!("listing {} failed: {e}", exe.display()));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.ends_with(": test"))
        .count()
}

/// Run `test` out of this binary again, as a subprocess under the default
/// capture, and require that it ran.
///
/// A test cannot observe its own capture: libtest installs it around whatever
/// is running, so a test reading back what it wrote sees the same bytes either
/// way. A pin on what a notice reaches has to read it from outside, and the
/// run without `--nocapture` is the one whose silence is the defect.
pub fn rerun_one_test(test: &TestName) -> std::process::Output {
    let exe = std::env::current_exe().expect("this test binary's own path");
    let out = Command::new(&exe)
        .args(test.selector())
        .output()
        .unwrap_or_else(|e| panic!("re-invoking {} failed: {e}", exe.display()));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("running 1 test\n"),
        "re-running `{}` ran no test:\n{stdout}",
        test.as_str()
    );
    out
}

/// Whether `tool` is absent from PATH, announcing the skip when it is.
///
/// ```ignore
/// if common::skip_without_tool("cargo") {
///     return;
/// }
/// ```
///
/// **This is not the question production asks, and the gap is reachable.**
/// `which` walks PATH and applies its own notion of executability; every
/// integration that needs a tool spawns it and lets the OS decide. A PATH entry
/// carrying the executable bit over a file the OS will not run — a broken
/// interpreter line, a file its owner cannot read, anything that is neither a
/// script nor a binary — is accepted here and rejected there. A test guarded on
/// this then does NOT skip: the spawn fails inside the integration and the
/// failure arrives wearing the integration's own wording, so a reader debugging
/// it starts in the wrong file.
///
/// The gap needs that entry to be the ONLY candidate. A spawn by name reaches
/// `execvp`, which reads a missing interpreter as ENOENT and keeps walking
/// PATH, so a malformed shim standing in front of a working toolchain is
/// invisible to production and visible only to a resolver that hands back the
/// first match rather than the first runnable one.
///
/// Spawning `<tool> --version` and requiring the status to succeed does not
/// close it. `go` exits 2 there — its spelling is `go version` — and so does a
/// file whose owner cannot read it, so no single status separates a healthy
/// tool that does not speak `--version` from an install the OS will not run.
///
/// `tests/source/skip_report_visibility_test.rs` plants those entries and asserts both
/// halves against the same file — that this reports it present, and that the OS
/// will not run it — so closing this gap reddens a test rather than changing
/// what the corpus's guards mean in silence.
///
/// A test that mints the tool it needs onto a PATH it owns — see
/// [`cargo_stand_in_path`] — asks neither question and is not exposed to this.
/// A test that needs the tool's LOCATION cannot take a bool at all and uses
/// [`resolve_tool_or_skip`], which spawns what it resolved and so does not
/// carry this gap.
pub fn skip_without_tool(tool: &str) -> bool {
    if which::which(tool).is_ok() {
        return false;
    }
    report_skip(&format!("`{tool}` not found on PATH"));
    true
}

/// Where `tool` is, confirmed to start, or `None` with the skip announced.
///
/// For the caller that needs the tool's LOCATION rather than whether it is
/// there: a fixture writing `exec '<resolved>' "$@"` into a shim of its own
/// spawns that exact path, so the resolver's answer is the thing that has to
/// run and [`skip_without_tool`]'s bool cannot serve it.
///
/// The extra spawn is what separates the two. `which` hands back the first
/// PATH entry it considers executable; a spawn by NAME reaches `execvp`, which
/// reads a missing interpreter as ENOENT and keeps walking. So a stale shim in
/// front of a working toolchain is invisible to everything that spawns by name
/// and fatal only to a caller holding the first match — and that caller is the
/// one here. Spawning the resolved path does not walk, so the disagreement
/// arrives as a skip instead of as the integration's own error on a host where
/// the tool was declared present.
///
/// THE PREDICATE IS WHETHER THE PROCESS STARTS, never what it exits with. No
/// exit code separates a healthy tool from a broken one: `go --version` exits 2
/// on a working install, its spelling being `go version`, and so does a script
/// whose interpreter cannot open it. That second shape is the residue — the OS
/// begins it and it fails — and this reports it present, because the reading
/// that would catch it also rejects Go.
pub fn resolve_tool_or_skip(tool: &str) -> Option<PathBuf> {
    let Ok(resolved) = which::which(tool) else {
        report_skip(&format!("`{tool}` not found on PATH"));
        return None;
    };

    let started = Command::new(&resolved)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if let Err(e) = started {
        report_skip(&format!(
            "`{tool}` resolves to {}, which this host will not start: {e}",
            resolved.display()
        ));
        return None;
    }
    Some(resolved)
}

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
/// to get wrong. `tests/source/canonical_temp_root_test.rs` keeps it that way, and
/// `scripts/ci-local.sh` runs its test stages under the geometry below.
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
/// The root is resolved by the function rwv resolves its own paths with, so
/// a fixture path and the path rwv reports for it are one spelling on every
/// platform, Windows included.
pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    let root = ROOT.get_or_init(|| {
        let raw = std::env::temp_dir();
        repoweave::path_spelling::resolve(&raw)
            .unwrap_or_else(|e| panic!("temp dir {} does not resolve: {e}", raw.display()))
    });
    tempfile::TempDir::new_in(root)
}

/// `root` joined to a `/`-separated relative path, one component at a time.
///
/// `Path::join` pushes a slash-bearing literal whole, separator and all, so
/// `root.join("projects/beta")` spells `…\ws\projects/beta` on Windows — a
/// mixed spelling nothing rwv prints. An expectation built that way disagrees
/// with correct output and reads at the failure as though the product were
/// wrong. Build any multi-component fixture path this way when it is compared
/// against what rwv printed; on Unix the result is the join it replaces.
pub fn under(root: impl AsRef<std::path::Path>, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.as_ref().to_path_buf(), |mut path, component| {
            path.push(component);
            path
        })
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
    let p = path
        .as_ref()
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
    name: &str,
    parent: impl AsRef<std::path::Path>,
) -> String {
    format!(
        "{{\"primary\":\"{}\",\"project\":\"{project}\",\"name\":\"{name}\",\"parent\":\"{}\"}}",
        json_escaped(primary),
        json_escaped(parent),
    )
}

/// The name a workweave directory `<project>--<name>` spells, as
/// `workweave create` names the directory it places.
pub fn workweave_name_of(dir: impl AsRef<std::path::Path>) -> String {
    let base = dir
        .as_ref()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    base.split_once("--")
        .map_or(base.clone(), |(_, name)| name.to_owned())
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
    register_workweave_from(primary, project, name, dir, primary);
}

/// [`register_workweave`] for a workweave forked from `parent`.
pub fn register_workweave_from(
    primary: impl AsRef<std::path::Path>,
    project: &str,
    name: &str,
    dir: impl AsRef<std::path::Path>,
    parent: impl AsRef<std::path::Path>,
) {
    let (primary, dir, parent) = (primary.as_ref(), dir.as_ref(), parent.as_ref());
    let resolved = |path: &std::path::Path| {
        repoweave::path_spelling::resolve(path).unwrap_or_else(|_| path.to_path_buf())
    };
    let clone = primary.join("projects").join(project);
    std::fs::create_dir_all(&clone).unwrap();
    if !clone.join(".git").exists() {
        git_in(&clone, &["init", "-q"]);
    }
    let index_path = index_file_of(&clone);
    let mut index: serde_json::Value = match std::fs::read_to_string(&index_path) {
        Ok(raw) => serde_json::from_str(&raw).expect("fixture: index should parse"),
        Err(_) => serde_json::json!({ "workweaves": {} }),
    };
    index["workweaves"][name] = serde_json::json!({
        "path": resolved(dir),
        "parent": resolved(parent),
    });
    std::fs::write(&index_path, serde_json::to_string(&index).unwrap())
        .unwrap_or_else(|e| panic!("write {}: {e}", index_path.display()));
}

/// Where the project clone at `clone` keeps its workweave index: in the
/// directory its worktrees share.
pub fn index_file_of(clone: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    let clone = clone.as_ref();
    let common = git_in(
        clone,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    std::path::PathBuf::from(common.trim()).join("rwv-workweave-index")
}

/// Where the index an earlier release kept in the project checkout sat.
pub fn old_path_index_of(clone: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    clone.as_ref().join(".rwv-workweave-index")
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
pub const GIT_ENV_VARS: &[&str] = &[
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
    cmd.envs(git_config_env(&[]));
    cmd.env("PATH", path_finding_this_rwv());
    cmd
}

/// `PATH` with the directory of the `rwv` under test first, so a merge a test
/// runs with git finds this build's `rwv` for the lock merge driver, never an
/// installed one.
pub fn path_finding_this_rwv() -> std::ffi::OsString {
    let built = std::path::Path::new(env!("CARGO_BIN_EXE_rwv"))
        .parent()
        .expect("the binary sits in a directory")
        .to_path_buf();
    let mut paths = vec![built];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("PATH entries join")
}

/// The `GIT_CONFIG_*` environment setting `init.defaultBranch=main` and then
/// each of `settings` for every git a command runs, rwv's included.
///
/// Config given this way outranks every config file, so it also reaches a
/// repository the command creates, which a pin in a repository's own config
/// cannot.
pub fn git_config_env(settings: &[(&str, &str)]) -> Vec<(String, String)> {
    let all: Vec<(&str, &str)> = std::iter::once(("init.defaultBranch", "main"))
        .chain(settings.iter().copied())
        .collect();
    let mut env = vec![("GIT_CONFIG_COUNT".to_owned(), all.len().to_string())];
    for (i, (key, value)) in all.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), (*key).to_owned()));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), (*value).to_owned()));
    }
    env
}

/// The two line-ending configurations a checkout meets in practice, for a
/// fixture to pin so that the config of the machine running it cannot choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEndings {
    /// `core.autocrlf=false` and `core.eol=lf`: text checks out as committed.
    Verbatim,
    /// `core.autocrlf=true`, Git for Windows' default: text checks out with
    /// CRLF line endings.
    Converted,
}

impl LineEndings {
    pub fn settings(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Verbatim => &[("core.autocrlf", "false"), ("core.eol", "lf")],
            Self::Converted => &[("core.autocrlf", "true")],
        }
    }

    /// Pin in `repo`'s own config, which every worktree of it shares. A
    /// repository cloned from it does not inherit the pin; [`git_config_env`]
    /// over [`LineEndings::settings`] reaches one.
    pub fn pin(self, repo: &std::path::Path) {
        for (key, value) in self.settings() {
            git_in(repo, &["config", key, value]);
        }
    }
}

/// Give every member of the project active at the weave root `root` its first
/// pin at the commit its clone is at, through `rwv member pin`, the verb that
/// gives a pin by name. `rwv lock` records a pin forward only and gives none.
pub fn pin_every_member(root: &std::path::Path) {
    let active = std::fs::read_to_string(root.join(".rwv-active"))
        .expect("a fixture weave has an active project")
        .trim()
        .to_owned();
    let manifest = repoweave::manifest::Manifest::from_path(
        &root.join("projects").join(&active).join("rwv.toml"),
    )
    .expect("a fixture project's manifest parses");
    for (path, _) in manifest.iter_entries() {
        let out = rwv()
            .args(["member", "pin", path.as_str(), "HEAD"])
            .current_dir(root)
            .output()
            .expect("rwv member pin should run");
        assert!(
            out.status.success(),
            "rwv member pin {path} HEAD in {} failed:\n{}",
            root.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Commit what is staged in `dir`, unless nothing is: a verb that commits its
/// own change leaves nothing for the fixture to commit after it.
pub fn commit_staged(dir: impl AsRef<std::path::Path>, message: &str) {
    let dir = dir.as_ref();
    let nothing_staged = git()
        .args(["diff", "--cached", "--quiet"])
        .current_dir(dir)
        .status()
        .expect("git diff --cached should run")
        .success();
    if !nothing_staged {
        git_in(dir, &["commit", "-q", "-m", message]);
    }
}

/// Run `git args` in `dir`, panic on a non-zero exit, and hand back trimmed
/// stdout.
///
/// The suite's one subprocess-git wrapper. Fixtures hand-rolled this at 155
/// sites under 13 different names, and the copies drifted: 94 of them set no
/// author or committer identity, so whether a commit could be made at all
/// depended on the ambient `user.email` of the machine running the tests —
/// present on a developer's box, absent on a CI runner. Baking the identity
/// here is what makes a fixture's commit independent of who runs it.
///
/// Reads and writes share one wrapper deliberately: the drift was between a
/// file's own `git` and `git_out`, one of which set the identity and the other
/// of which did not.
pub fn git_in(dir: impl AsRef<std::path::Path>, args: &[&str]) -> String {
    git_in_configured(dir, &[], args)
}

/// The `.gitattributes` text assigning `rwv.lock` the lock merge driver.
pub const LOCK_DRIVER_ATTRIBUTE: &str = "rwv.lock merge=rwv-lock\n";

/// The command the lock merge driver's config names.
pub const LOCK_DRIVER_COMMAND: &str = "rwv lock --merge-driver %O %A %B";

/// Set the lock merge driver up in a fixture's project repo as `rwv init`
/// does: [`LOCK_DRIVER_ATTRIBUTE`]'s line in `.gitattributes`, any line an
/// earlier release wrote for the lock dropped and every other line kept,
/// staged for the caller's next commit; and the driver's command in the
/// clone's config. A directory that is not a repo yet gets the file alone.
pub fn set_up_lock_driver(project_dir: impl AsRef<std::path::Path>) {
    let project_dir = project_dir.as_ref();
    let path = project_dir.join(".gitattributes");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut text: String = existing
        .lines()
        .filter(|line| {
            !matches!(
                line.trim(),
                "rwv.lock merge=rwv-ours" | "rwv.lock merge=ours" | "rwv.lock merge=rwv-lock"
            )
        })
        .map(|line| format!("{line}\n"))
        .collect();
    text.push_str(LOCK_DRIVER_ATTRIBUTE);
    std::fs::write(&path, text).unwrap();
    if !project_dir.join(".git").exists() {
        return;
    }
    git_in(project_dir, &["add", ".gitattributes"]);
    git_in(
        project_dir,
        &["config", "merge.rwv-lock.driver", LOCK_DRIVER_COMMAND],
    );
}

/// [`set_up_lock_driver`], with the line folded into the checked-out commit
/// when there is one, so the fixture's project holds what a project `rwv
/// init` made holds without a commit of its own.
pub fn set_up_lock_driver_committed(project_dir: impl AsRef<std::path::Path>) {
    let project_dir = project_dir.as_ref();
    if !project_dir.join(".git").exists() {
        return;
    }
    let committed = git()
        .args(["show", "HEAD:.gitattributes"])
        .current_dir(project_dir)
        .output()
        .unwrap();
    if committed.status.success()
        && String::from_utf8_lossy(&committed.stdout)
            .lines()
            .any(|line| line.trim() == LOCK_DRIVER_ATTRIBUTE.trim())
    {
        git_in(
            project_dir,
            &["config", "merge.rwv-lock.driver", LOCK_DRIVER_COMMAND],
        );
        return;
    }
    set_up_lock_driver(project_dir);
    let born = git()
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .current_dir(project_dir)
        .output()
        .unwrap()
        .status
        .success();
    if born {
        git_in(project_dir, &["commit", "-q", "--amend", "--no-edit"]);
    }
}

/// [`git_in`], with each of `settings` given as [`git_config_env`] gives it.
pub fn git_in_configured(
    dir: impl AsRef<std::path::Path>,
    settings: &[(&str, &str)],
    args: &[&str],
) -> String {
    let dir = dir.as_ref();
    let out = git()
        .envs(git_config_env(settings))
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {} failed to start: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed:\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("git output is valid UTF-8")
        .trim()
        .to_string()
}

/// Record the branch the project repo at `project_dir` is on as `declared`,
/// and set up the lock merge driver ([`set_up_lock_driver_committed`]), as
/// `rwv init` and `rwv clone` do for a project they make.
///
/// A fixture that builds its project repo with git has no record until it
/// says so, and the verbs that align, lock, land or publish refuse a branch
/// with none. The record is written by the code those verbs run, so the
/// fixture holds exactly what a weave made by `rwv init` holds.
pub fn record_trunk(project_dir: &std::path::Path) {
    set_up_lock_driver_committed(project_dir);
    let name = project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let project = repoweave::manifest::ProjectName::new(name)
        .expect("a fixture project directory names a project");
    repoweave::branch::record_trunk(repoweave::git::git_vcs().as_ref(), &project, project_dir)
        .unwrap_or_else(|e| panic!("cannot record {}: {e:#}", project_dir.display()));
}

/// Make `project_dir` the repository a weave's project always is, holding what
/// it holds now: the clone that keeps the workweave index, and that a
/// workweave's project checkout is a worktree of. Commits everything in it and
/// records the trunk, as `rwv init` leaves a project it makes.
pub fn make_project_repo(project_dir: impl AsRef<std::path::Path>) {
    let project_dir = project_dir.as_ref();
    if !project_dir.join(".git").exists() {
        git_in(project_dir, &["init", "-q"]);
    }
    git_in(project_dir, &["add", "-A"]);
    commit_staged(project_dir, "project");
    record_trunk(project_dir);
}

/// Record `branch` of the project repo at `project_dir` as a feature onto
/// `target`, which must itself be recorded: the record `rwv workweave create`
/// writes for the branch it makes, for a fixture that makes such a branch
/// with git.
pub fn record_feature(project_dir: &std::path::Path, branch: &str, target: &str) {
    let name = project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let project = repoweave::manifest::ProjectName::new(name)
        .expect("a fixture project directory names a project");
    let vcs = repoweave::git::git_vcs();
    let target = repoweave::records::BranchName::parse(vcs.as_ref(), target)
        .unwrap_or_else(|e| panic!("{target:?} is no weave branch name: {e:?}"));
    repoweave::branch::record_workweave(project_dir, &project, &target, branch)
        .unwrap_or_else(|e| panic!("cannot record {branch}: {e:#}"));
}

/// An `update` hook on the bare repo `bare` that refuses a push of `refname`
/// and accepts every other ref, so a push that carries both lands the rest.
/// How a test makes one remote's push fail while it stays reachable.
pub fn refuse_pushes_to(bare: &std::path::Path, refname: &str) {
    let hooks = bare.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("update");
    write_executable(&hook, format!(
            "#!/bin/sh\nif [ \"$1\" = {refname} ]; then\n    echo refused >&2\n    exit 1\nfi\nexit 0\n"
        ));
}

/// Initialize a bare repo and seed it with one commit on `main` so it can
/// be cloned by `--origin` consumers and act as a push target.
pub fn init_bare_repo_with_commit(bare: &std::path::Path) {
    let parent = bare.parent().expect("bare repo path needs a parent");
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    git_in(
        parent,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    let seed = parent.join(format!("__seed_{stem}"));
    git_in(
        parent,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    git_in(&seed, &["config", "user.email", "test@test.com"]);
    git_in(&seed, &["config", "user.name", "Test"]);
    std::fs::write(seed.join("README"), "seed").unwrap();
    git_in(&seed, &["add", "."]);
    git_in(&seed, &["commit", "-m", "initial"]);
    git_in(&seed, &["push", "origin", "main"]);
}

/// A test workspace ready to be driven by `rwv push`.
///
/// Holds the workspace root and the bare-remote paths so tests can both
/// invoke `rwv push` against it and inspect the bare remotes to verify
/// what was pushed.
pub struct PushWorkspace {
    pub _tmp: tempfile::TempDir,
    pub workspace: PathBuf,
    pub project_name: String,
    pub project_bare: PathBuf,
    pub manifest_bares: Vec<(String, PathBuf)>,
}

/// Write an `rwv.lock` into `project_dir` pinning each
/// `(repo_path, url, version)`.
///
/// Goes through `LockFile::from_json_str` and `lock::write_lock_in` rather than
/// formatting the on-disk JSON, for two reasons a hand-written fixture cannot
/// give you. The parse means a fixture that has drifted to an unparseable
/// shape fails loudly here instead of being written and silently skipped past
/// by whatever reads it. And the serializer means the bytes are the ones
/// `rwv lock` itself emits for equal content, so nothing that stages or
/// commits the lock sees a phantom diff.
///
/// What this builder holds constant for every consumer, stated here because
/// a silence stated only at a consuming site is invisible from the
/// instrument (`docs/internals/testing.md`, shared fixtures): every entry is
/// `type: "git"` — the only `VcsType` today, so the constant is temporary by
/// that enum's own admission; the file is always `rwv.lock` directly under
/// `project_dir`; and the bytes are always well-formed and current-shaped.
/// A version is whatever the parser accepts, tag forms included, so pinning
/// something that is not a SHA is inside this and not outside it. So is an
/// entry count of zero: `&[]` writes the empty lock.
///
/// What is outside, and is written raw at the site behind a
/// `// raw lock bytes: …` line naming which of these it is: bytes that must
/// differ from canonical while still parsing; bytes that must not parse at
/// all; a payload that is an arbitrary distinguishable marker because the
/// path rather than the content is what the test drives; and an append onto
/// a lock already written here, whose subject is the file reading as
/// tracked-dirty. `tests/source/lock_write_route_census_test.rs` holds every such
/// site to that line, and reports any raw write that carries none.
pub fn fixture_lock(project_dir: &std::path::Path, repos: &[(&str, &str, &str)]) {
    let entries: Vec<String> = repos
        .iter()
        .map(|(path, url, version)| {
            format!("{path:?}: {{\"type\": \"git\", \"url\": {url:?}, \"version\": {version:?}}}")
        })
        .collect();
    let raw = format!("{{\"repositories\": {{{}}}}}", entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw)
        .expect("a fixture lock must parse before it is written");
    repoweave::lock::write_lock_in(&lock, project_dir).expect("a fixture lock must be writable");
}

/// [`fixture_lock`] pinning each `(repo_path, url)` at the tip of its remote's
/// `main`, read from the remote the way a clone reads it.
///
/// For a project source whose members already sit on their remotes: a clone
/// of it places every member at its pin, so the lock it carries is the
/// complete, reachable one a published project has.
pub fn fixture_lock_at_remote_tips(project_dir: &std::path::Path, repos: &[(&str, &str)]) {
    let pinned: Vec<(String, String, String)> = repos
        .iter()
        .map(|(path, url)| {
            let out = git()
                .args(["ls-remote", url, "refs/heads/main"])
                .output()
                .expect("git ls-remote runs");
            assert!(
                out.status.success(),
                "git ls-remote {url} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let line = String::from_utf8_lossy(&out.stdout).into_owned();
            let tip = line
                .split_whitespace()
                .next()
                .unwrap_or_else(|| panic!("{url} has no main branch to pin"))
                .to_owned();
            ((*path).to_owned(), (*url).to_owned(), tip)
        })
        .collect();
    let borrowed: Vec<(&str, &str, &str)> = pinned
        .iter()
        .map(|(p, u, v)| (p.as_str(), u.as_str(), v.as_str()))
        .collect();
    fixture_lock(project_dir, &borrowed);
}

/// The bytes [`fixture_lock`] writes, for a caller that needs them as a value
/// — committing through a helper that takes file contents, or asserting
/// against what a lock write must produce.
///
/// Produced by writing one and reading it back, so the bytes are the shipped
/// serializer's by construction. A caller that instead spells
/// `to_string_pretty` plus a newline is byte-correct only while `write_lock`
/// is that and nothing else.
pub fn fixture_lock_bytes(repos: &[(&str, &str, &str)]) -> String {
    let tmp = crate::common::tempdir().expect("a tempdir for the lock bytes");
    fixture_lock(tmp.path(), repos);
    std::fs::read_to_string(tmp.path().join("rwv.lock"))
        .expect("the lock this just wrote must be readable")
}

/// Build a workspace with `repos.len()` manifest repos plus a project repo.
///
/// Each manifest repo gets a bare remote, a canonical-path local clone, and
/// is referenced by `rwv.toml`. The project repo gets a bare remote and a
/// clone under `projects/<project_name>/`. `rwv.lock` is generated to match
/// the manifest repos' local HEAD SHAs. Returns the workspace handle.
pub fn build_workspace(project_name: &str, repos: &[(&str, &str)]) -> PushWorkspace {
    // repos is &[(canonical_path, role)]
    let tmp = crate::common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

    // Build manifest bare remotes and local clones.
    let mut manifest_bares: Vec<(String, PathBuf)> = Vec::new();
    let mut manifest_shas: Vec<(String, String)> = Vec::new();
    let mut manifest_yaml = String::from("[repositories]\n");
    for (repo_path, role) in repos {
        let bare = tmp
            .path()
            .join(format!("{}.git", repo_path.replace('/', "_")));
        init_bare_repo_with_commit(&bare);

        let canonical = workspace.join(repo_path);
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        git_in(
            workspace.parent().unwrap(),
            &[
                "clone",
                "--origin",
                "origin",
                bare.to_str().unwrap(),
                canonical.to_str().unwrap(),
            ],
        );
        git_in(&canonical, &["config", "user.email", "test@test.com"]);
        git_in(&canonical, &["config", "user.name", "Test"]);
        let head = git_in(&canonical, &["rev-parse", "HEAD"]);
        manifest_shas.push(((*repo_path).to_string(), head));
        manifest_bares.push(((*repo_path).to_string(), bare.clone()));
        let bare_url = file_url(&bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nbranch = \"main\"\nrole = \"{role}\"\n"
        ));
    }

    // Build a project bare and a `projects/<name>/` clone, then commit
    // rwv.toml + rwv.lock and push back to the bare.
    let project_bare = tmp.path().join("project.git");
    init_bare_repo_with_commit(&project_bare);
    let project_dir = workspace.join("projects").join(project_name);
    git_in(
        workspace.parent().unwrap(),
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_dir.to_str().unwrap(),
        ],
    );
    git_in(&project_dir, &["config", "user.email", "test@test.com"]);
    git_in(&project_dir, &["config", "user.name", "Test"]);

    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();

    let mut lock_entries: Vec<(String, String, String)> = Vec::new();
    for (rp, sha) in &manifest_shas {
        let (_, bare) = manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let bare_url = file_url(bare);
        lock_entries.push((rp.clone(), bare_url, sha.clone()));
    }
    let entries: Vec<(&str, &str, &str)> = lock_entries
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    fixture_lock(&project_dir, &entries);

    git_in(&project_dir, &["add", "."]);
    git_in(&project_dir, &["commit", "-m", "manifest + lock"]);
    record_trunk(&project_dir);

    // Mark this project active.
    std::fs::write(workspace.join(".rwv-active"), format!("{project_name}\n")).unwrap();

    PushWorkspace {
        _tmp: tmp,
        workspace,
        project_name: project_name.to_string(),
        project_bare,
        manifest_bares,
    }
}

/// A vendored crate source holding `pinnable` at each of `versions`, plus the
/// `.cargo/config.toml` that redirects crates-io at it.
///
/// Lets a fixture drive a real `cargo` resolution — a version pin surviving a
/// hook, a lock materializing — without reaching the network.
pub fn write_local_crate_source(
    source_dir: &std::path::Path,
    weave_root: &std::path::Path,
    versions: &[&str],
) {
    for version in versions {
        let pkg = source_dir.join(format!("pinnable-{version}"));
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(
            pkg.join("Cargo.toml"),
            format!(
                "[package]\nname = \"pinnable\"\nversion = \"{version}\"\nedition = \"2021\"\n"
            ),
        )
        .unwrap();
        std::fs::write(pkg.join("src/lib.rs"), "pub fn pinned() {}\n").unwrap();
        std::fs::write(pkg.join(".cargo-checksum.json"), r#"{"files":{}}"#).unwrap();
    }
    std::fs::create_dir_all(weave_root.join(".cargo")).unwrap();
    std::fs::write(
        weave_root.join(".cargo/config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"local\"\n\n[source.local]\ndirectory = \"{}\"\n",
            json_escaped(source_dir)
        ),
    )
    .unwrap();
}

/// A directory holding a link to `name` and nothing else, to be used as the
/// whole `PATH` of a child that must find that tool and no other.
///
/// Two fixtures minted this separately — one to prove a verb works with no Go
/// toolchain reachable, one to prove `doctor` works with no plugin reachable —
/// and the copies disagreed about Windows: only one appended `.exe`, so the
/// other's shim was invisible to a child's lookup there and the fixture handed
/// out a directory that resolved nothing.
///
/// Not a `TempDir`: one held in a `static` never drops, so it would leave a
/// directory behind on every run. The directory is reused across runs, which
/// is why the link is tested for rather than created unconditionally, and why
/// the directory is named for the tool the lookup found: a run with another
/// `git` first on `PATH` (the git-floor stage's) must not reuse a link to the
/// last run's.
///
/// Two callers can also race that test on a cold target: both see the link
/// absent, one create fails on the winner's identical, already-usable link.
/// A failed create is therefore only a failure while the link is still
/// absent.
pub fn tool_only_bin(name: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let tool = which::which(name)
        .unwrap_or_else(|e| panic!("{name} must be resolvable to run these tests: {e}"));
    let mut found = std::collections::hash_map::DefaultHasher::new();
    tool.hash(&mut found);
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("only-bin-{name}-{:016x}", found.finish()));
    std::fs::create_dir_all(&dir).expect("shim bin directory should be creatable");

    // The shim must carry the name a child's lookup resolves: Windows appends
    // `.exe`, so a link named bare `git` is invisible there.
    let link = dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    if link.symlink_metadata().is_err() {
        if let Err(e) =
            repoweave::symlink::create(&tool, &link, repoweave::symlink::LinkTarget::File)
        {
            if link.symlink_metadata().is_err() {
                panic!("linking {name} into {}: {e}", dir.display());
            }
        }
    }
    dir
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
/// Use where a detach is the *specified* outcome (a member of a feature that
/// is not owned, a lock-pinned materialization). Everywhere else, prefer [`assert_on_branch`]:
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
    cmd.env_remove(pause::ARMING_VARIABLE);
    // Mirror the `init.defaultBranch=main` pin from [`git`] — rwv shells out
    // to git internally and those subprocesses inherit this env, so any
    // `git init` rwv runs on behalf of a test gets `main` as the default
    // branch regardless of CI runner config.
    cmd.envs(git_config_env(&[]));
    cmd
}

/// The first backtick span of `text` that begins with `prefix`: the command a
/// refusal or finding tells the operator to run.
pub fn advised_command(text: &str, prefix: &str) -> String {
    text.split('`')
        .skip(1)
        .step_by(2)
        .find(|span| span.starts_with(prefix))
        .unwrap_or_else(|| panic!("no `{prefix}...` command is advised in:\n{text}"))
        .to_owned()
}

/// Run `command` as advised, from `cwd`: `rwv ...` through [`rwv`], `git ...`
/// through [`git`]. The argv is split on whitespace, so a path the command
/// carries must hold none.
pub fn run_advised(command: &str, cwd: impl AsRef<std::path::Path>) -> std::process::Output {
    let mut argv = command.split_whitespace();
    let program = argv.next().expect("an advised command is not empty");
    match program {
        "rwv" => rwv().args(argv).current_dir(cwd).output().unwrap(),
        "git" => git().args(argv).current_dir(cwd).output().unwrap(),
        other => panic!("no runner for the advised program `{other}`"),
    }
}

/// Every finding `rwv doctor --all --json` reports from `cwd`, as its kind,
/// its sub-kind token where it has one, and the path it names.
pub fn doctor_findings(cwd: impl AsRef<std::path::Path>) -> Vec<String> {
    let out = rwv()
        .args(["doctor", "--all", "--json"])
        .current_dir(cwd)
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json produced invalid JSON: {e}\noutput: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    json["violations"]
        .as_array()
        .expect("violations is an array")
        .iter()
        .map(|v| {
            let sub_kind = match &v["sub_kind"] {
                serde_json::Value::String(tag) => tag.clone(),
                serde_json::Value::Object(fields) => {
                    fields.keys().next().cloned().unwrap_or_default()
                }
                _ => String::new(),
            };
            format!(
                "{} {sub_kind} {}",
                v["kind"].as_str().unwrap_or_default(),
                v["path"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

/// Panics if `rwv doctor --all` from `cwd` reports a finding that was not in
/// `before` (from [`doctor_findings`]), other than the two a fixture
/// explains: an entry just added is not in `rwv.lock` yet, and a clone whose
/// remote is a temp path places elsewhere than where the fixture put it. Neither is what the advice
/// under test is about; any other new finding is damage.
pub fn assert_no_new_findings(cwd: &std::path::Path, before: &[String]) {
    let new: Vec<String> = doctor_findings(cwd)
        .into_iter()
        .filter(|f| {
            !before.contains(f)
                && !f.starts_with("pin none ")
                && !f.starts_with("provenance placement-disagreement")
        })
        .collect();
    assert_eq!(new, Vec::<String>::new());
}

/// Strip the `rwv <version>` banner every `rwv explain` invocation opens
/// with, returning the bundle or entry text a committed doc can be compared
/// against. Panics if `stdout` does not open with exactly that banner, so a
/// caller that reaches for this has that much already pinned for free.
pub fn strip_version_banner(stdout: &str) -> &str {
    let banner = format!("rwv {}\n\n", repoweave::rwv_version_with_build());
    stdout
        .strip_prefix(&banner)
        .unwrap_or_else(|| panic!("expected stdout to open with {banner:?}; got:\n{stdout}"))
}

/// What a `--json` run printed, read as the one envelope `--json` promises:
/// its top-level keys, and the `path` of each record under `records_key` in
/// the order printed. Panics, quoting stdout, when stdout is not a single JSON
/// document whose `$schema` is `schema_url`.
pub fn envelope_shape(
    stdout: &[u8],
    schema_url: &str,
    records_key: &str,
) -> (std::collections::BTreeSet<String>, Vec<String>) {
    let text = String::from_utf8_lossy(stdout);
    let doc: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}):\n{text}"));
    assert_eq!(
        doc["$schema"].as_str(),
        Some(schema_url),
        "the envelope must name its schema:\n{text}"
    );
    let keys = doc
        .as_object()
        .unwrap_or_else(|| panic!("the envelope must be an object:\n{text}"))
        .keys()
        .cloned()
        .collect();
    let paths = doc[records_key]
        .as_array()
        .unwrap_or_else(|| panic!("`{records_key}` must be an array:\n{text}"))
        .iter()
        .map(|record| {
            record["path"]
                .as_str()
                .unwrap_or_else(|| panic!("a record without `path`: {record}"))
                .to_owned()
        })
        .collect();
    (keys, paths)
}

/// [`rwv`] as a `std::process::Command`, for a test that spawns the binary
/// and acts on it while it runs, which `assert_cmd` cannot.
#[allow(dead_code)]
pub fn rwv_std() -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("rwv"));
    for var in GIT_ENV_VARS {
        cmd.env_remove(var);
    }
    cmd.env_remove(pause::ARMING_VARIABLE);
    cmd.envs(git_config_env(&[]));
    cmd
}

/// The whole `PATH` for a child that must find `git` and a stand-in `cargo`
/// and nothing else — so whether the child's cargo step completes is an input
/// the caller sets rather than a property of the machine.
///
/// `bin` is created if absent and is the caller's to own; give it a directory
/// inside the fixture's own temporary root, not a shared one, because the
/// stand-in is rewritten on every call.
///
/// The stand-in writes a `Cargo.lock` in its working directory. That is not
/// decoration: cargo-workspace's activate hook reads the lock back to stamp
/// the generation it has just accepted, so a stand-in that only exits leaves
/// the hook failing on a file that was never written. What it writes records
/// no resolve, which is the boundary on this helper — a test that reads a
/// version, a package name or a digest out of that lock is measuring the
/// stand-in, and belongs on the real tool behind `skip_without_tool` instead.
///
/// Unix only, for the reason `tests/integrations/integrations_test.rs` states at
/// `write_exit_code_shim`: the stand-in is a `#!/bin/sh` script the child
/// resolves off `PATH` and spawns itself, and Windows has no executable bit
/// and selects candidates on `PATHEXT`. A caller is therefore `#[cfg(unix)]`
/// too.
#[cfg(unix)]
pub fn cargo_stand_in_path(bin: &std::path::Path) -> std::ffi::OsString {
    std::fs::create_dir_all(bin).expect("the stand-in bin directory should be creatable");
    let git = which::which("git").expect("git must be resolvable to run these tests");
    let linked = bin.join("git");
    if linked.symlink_metadata().is_err() {
        std::os::unix::fs::symlink(&git, &linked).expect("git should link into the stand-in bin");
    }

    let cargo = bin.join("cargo");
    write_executable(
        &cargo,
        r#"#!/bin/sh
case "$1" in
  generate-lockfile|fetch|update)
    [ -s Cargo.lock ] || printf 'version = 4\n' > Cargo.lock ;;
esac
exit 0
"#,
    );

    bin.as_os_str().to_owned()
}

/// Write a plugin fixture at `dir/<name>` that `rwv` discovers and dispatches
/// as the verb after `rwv-`, and return its path.
///
/// On Unix, `sh` follows a `#!/bin/sh` line and the file gets the exec bit. On
/// Windows there is no exec bit — `PATHEXT` decides what runs — so the file is
/// `<name>.cmd` holding `cmd`, a batch body run with echo off; `Command` runs
/// it through `cmd.exe`. Both bodies are written with `\n` line ends and the
/// batch one is converted to `\r\n`.
pub fn write_plugin(dir: &std::path::Path, name: &str, sh: &str, cmd: &str) -> PathBuf {
    #[cfg(unix)]
    {
        let _ = cmd;
        let path = dir.join(name);
        write_executable(&path, format!("#!/bin/sh\n{sh}"));

        path
    }
    #[cfg(windows)]
    {
        let _ = sh;
        let path = dir.join(format!("{name}.cmd"));
        let body = format!("@echo off\n{cmd}").replace('\n', "\r\n");
        std::fs::write(&path, body).unwrap();
        path
    }
}

/// Write a file at `dir/<name>` that is named like a plugin but that the host
/// would not execute: mode 0o644 on Unix, `<name>.txt` on Windows.
pub fn write_non_plugin(dir: &std::path::Path, name: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, "data\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    #[cfg(windows)]
    std::fs::write(dir.join(format!("{name}.txt")), "data\r\n").unwrap();
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

/// Assert the context display's `Weave:` line names `root`, as `Display`
/// renders it.
///
/// Whole-line equality rather than containment: the plain spelling is a
/// substring of the verbatim one, so `stdout.contains(plain)` is satisfied by
/// a line that still carries the Windows `\\?\` prefix — it is green exactly
/// when the leak it would catch is present.
pub fn assert_weave_line(stdout: &str, root: impl AsRef<std::path::Path>) {
    let named = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Weave: "))
        .unwrap_or_else(|| panic!("context display has no `Weave:` line:\n{stdout}"));
    assert_eq!(
        named,
        root.as_ref().display().to_string(),
        "the `Weave:` line must name the weave root in the operator spelling"
    );
}

/// The whole stdout of a command that prints one path and nothing else, as
/// `Display` renders it.
///
/// Equality for the reason [`assert_weave_line`] gives: containment cannot pin
/// a spelling whose correct form is a substring of the wrong one.
pub fn operator_path_stdout(path: impl AsRef<std::path::Path>) -> String {
    format!("{}\n", path.as_ref().display())
}

/// Assert `text` names `path` where the path sits inside a sentence, so
/// whole-line equality is not available.
///
/// Two assertions, because containment alone is not a pin here either: the
/// plain spelling is a substring of the verbatim one, so `contains(plain)` is
/// satisfied by text that still carries the `\\?\` prefix — green exactly
/// when the leak is present. The second assertion is what closes that, and it
/// names only THIS path, so an unrelated path elsewhere in the same message is
/// somebody else's finding rather than a false red here. Off Windows no path
/// carries the prefix, and the second assertion cannot fail.
pub fn assert_names_operator_path(text: &str, path: impl AsRef<std::path::Path>) {
    let plain = path.as_ref().display().to_string();
    assert!(
        text.contains(&plain),
        "the message must name {plain} in the operator spelling; got:\n{text}"
    );
    let verbatim = format!(r"\\?\{plain}");
    assert!(
        !text.contains(&verbatim),
        "the message names {plain} in the verbatim spelling {verbatim}; got:\n{text}"
    );
}

/// Every `--long-flag` clap offers, read out of `--help` output as whole
/// tokens.
///
/// A documented flag is checked by asking whether this list holds it, never
/// by asking whether the help text contains its spelling: `--frozen` is a
/// substring of `--frozen-lockfile`, so containment answers yes for a flag
/// clap would reject, and answers yes for a flag named only in a paragraph
/// about some other verb.
///
/// Scans whitespace-separated tokens, strips surrounding punctuation
/// (backticks, parens, commas) while keeping hyphens in the stem, truncates
/// at `=` or `<`, and lowercases. Callers comparing a documented spelling
/// lowercase it too.
pub fn extract_long_flags_from_help(text: &str) -> Vec<String> {
    let mut flags = Vec::new();
    for word in text.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
        if trimmed.starts_with("--") {
            let stem: String = trimmed
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-')
                .collect::<String>()
                .to_lowercase();
            if stem.len() > 2 {
                flags.push(stem);
            }
        }
    }
    flags.sort();
    flags.dedup();
    flags
}

/// The plan a landing from `source` into the branch `target` has checked out
/// records on its op record, as JSON, read from the workspaces as they stand:
/// the branch the target's project repo and each member there has checked
/// out, with its tip, and the source's project tip. A hand-written op record carries it so `--continue` has the
/// plan the landing would have recorded.
pub fn land_plan_json(source: &std::path::Path, target: &std::path::Path, project: &str) -> String {
    let target_project = target.join("projects").join(project);
    let source_project = source.join("projects").join(project);
    let branch = git_in(&target_project, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let receiving = |dir: &std::path::Path| repoweave::op_state::Receiving {
        branch: git_in(dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
        tip: repoweave::vcs::CommitId::parse(&git_in(dir, &["rev-parse", "HEAD"])).unwrap(),
        moves: repoweave::op_state::Moves::InCheckout,
    };
    let mut plan = repoweave::op_state::TakeInRecord {
        sync_from: Some(repoweave::op_state::SyncFrom::Target(branch)),
        source_tip: Some(
            repoweave::vcs::CommitId::parse(&git_in(&source_project, &["rev-parse", "HEAD"]))
                .unwrap(),
        ),
        ..Default::default()
    };
    plan.receiving.insert(
        repoweave::manifest::RepoKey::Project,
        receiving(&target_project),
    );
    let manifest =
        repoweave::manifest::Manifest::from_path(&target_project.join("rwv.toml")).unwrap();
    for (path, entry) in manifest.iter_entries() {
        if !matches!(
            entry.role,
            repoweave::manifest::Role::Owned | repoweave::manifest::Role::Fork
        ) {
            continue;
        }
        let checkout = target.join(path.as_path());
        if checkout.exists() {
            plan.receiving.insert(
                repoweave::manifest::RepoKey::Member(path.clone()),
                receiving(&checkout),
            );
        }
    }
    serde_json::to_string(&plan).unwrap()
}

/// [`land_plan_json`] for a landing that has advanced the target: every repo
/// it plans is journaled as moved there, as a landing journals each before
/// it moves it.
pub fn land_plan_json_after_advancing(
    source: &std::path::Path,
    target: &std::path::Path,
    project: &str,
) -> String {
    let mut plan: repoweave::op_state::TakeInRecord =
        serde_json::from_str(&land_plan_json(source, target, project)).unwrap();
    plan.moved_in_target = plan.receiving.keys().cloned().collect();
    serde_json::to_string(&plan).unwrap()
}
