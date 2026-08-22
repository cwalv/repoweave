#![allow(dead_code)]

pub mod compile_probe;
pub mod contract;
pub mod doctor_corpus;
pub mod json_schema;
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

/// Run one test out of the calling binary again, as a subprocess under the
/// default capture.
///
/// A test cannot observe its own capture: libtest installs it around whatever
/// is running, so a test reading back what it wrote sees the same bytes either
/// way. A pin on what a notice reaches has to read it from outside, and the
/// run without `--nocapture` is the one whose silence is the defect.
pub fn rerun_one_test(name: &str) -> std::process::Output {
    let exe = std::env::current_exe().expect("this test binary's own path");
    Command::new(&exe)
        .args([name, "--exact", "--test-threads=1"])
        .output()
        .unwrap_or_else(|e| panic!("re-invoking {} failed: {e}", exe.display()))
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
/// `tests/skip_report_visibility_test.rs` plants those entries and asserts both
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

/// `ETXTBSY`. Not worth a dependency for one number, and it is the same on
/// every unix this suite builds for.
#[cfg(unix)]
pub fn etxtbsy() -> i32 {
    26
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
///
/// `ETXTBSY` is retried rather than believed. It says the file could not be
/// TRIED, which is not an answer about whether the OS would run it, and a
/// sibling test forking while a freshly written fixture is still open provokes
/// it.
pub fn resolve_tool_or_skip(tool: &str) -> Option<PathBuf> {
    let Ok(resolved) = which::which(tool) else {
        report_skip(&format!("`{tool}` not found on PATH"));
        return None;
    };

    for _ in 0..50 {
        let started = Command::new(&resolved)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        #[cfg(unix)]
        if started
            .as_ref()
            .is_err_and(|e| e.raw_os_error() == Some(etxtbsy()))
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
        }
        if let Err(e) = started {
            report_skip(&format!(
                "`{tool}` resolves to {}, which this host will not start: {e}",
                resolved.display()
            ));
            return None;
        }
        return Some(resolved);
    }
    report_skip(&format!(
        "`{tool}` resolves to {}, which stayed unrunnable for a second",
        resolved.display()
    ));
    None
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
    let dir = dir.as_ref();
    let out = git()
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
/// Goes through `LockFile::from_json_str` and `lock::write_lock` rather than
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
/// No consumer of this builder samples the malformed, legacy, or
/// non-canonical-bytes plane — a test whose subject IS those bytes (an empty
/// lock, a stale-format one, one that must not parse) builds them locally
/// and says why at the site; `advisory_op_check_test.rs`'s hand-spelled
/// `EMPTY_LOCK` is the worked example of that boundary.
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
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock"))
        .expect("a fixture lock must be writable");
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
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
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
/// is why the link is tested for rather than created unconditionally.
///
/// Two callers can also race that test on a cold target: both see the link
/// absent, one create fails on the winner's identical, already-usable link.
/// A failed create is therefore only a failure while the link is still
/// absent.
pub fn tool_only_bin(name: &str) -> PathBuf {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("only-bin-{name}"));
    std::fs::create_dir_all(&dir).expect("shim bin directory should be creatable");

    let tool = which::which(name)
        .unwrap_or_else(|e| panic!("{name} must be resolvable to run these tests: {e}"));
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
/// Unix only, for the reason `tests/integrations_test.rs` states at
/// `write_exit_code_shim`: the stand-in is a `#!/bin/sh` script the child
/// resolves off `PATH` and spawns itself, and Windows has no executable bit
/// and selects candidates on `PATHEXT`. A caller is therefore `#[cfg(unix)]`
/// too.
#[cfg(unix)]
pub fn cargo_stand_in_path(bin: &std::path::Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(bin).expect("the stand-in bin directory should be creatable");
    let git = which::which("git").expect("git must be resolvable to run these tests");
    let linked = bin.join("git");
    if linked.symlink_metadata().is_err() {
        std::os::unix::fs::symlink(&git, &linked).expect("git should link into the stand-in bin");
    }

    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        r#"#!/bin/sh
case "$1" in
  generate-lockfile|fetch|update)
    [ -s Cargo.lock ] || printf 'version = 4\n' > Cargo.lock ;;
esac
exit 0
"#,
    )
    .expect("the cargo stand-in should be writable");
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755))
        .expect("the cargo stand-in should be executable");

    bin.as_os_str().to_owned()
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

/// Flatten path spelling inside prose: separators to `/`, the verbatim
/// prefix dropped. Compare both sides through this, never one.
///
/// Green whether or not the message it reads carries the spelling that message
/// is owed, so it cannot be the pin for a surface that mints. Two kinds of
/// call site keep it. One reads a render that still
/// stringifies its own path, and gives the call up when that render moves onto
/// `operator_path`. The other reads text this repository does not author:
/// `git worktree list --porcelain` answers in git's spelling, which no decision
/// here governs, and flattening is the only honest comparison available.
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

/// The whole stdout of a command that prints one path and nothing else, in the
/// spelling the operator seam mints for it.
///
/// Equality for the reason [`assert_weave_line`] gives: containment cannot pin
/// a spelling whose correct form is a substring of the wrong one.
pub fn operator_path_stdout(path: impl AsRef<std::path::Path>) -> String {
    format!(
        "{}\n",
        repoweave::path_spelling::operator_path(path.as_ref())
    )
}

/// Assert `text` names `path` where the path sits inside a sentence, so
/// whole-line equality is not available.
///
/// Two assertions, because containment alone is not a pin here either: the
/// minted spelling is a substring of the internal one, so `contains(minted)`
/// is satisfied by text that still carries the `\\?\` prefix — green exactly
/// when the leak is present. The second assertion is what closes that, and it
/// names only THIS path, so an unrelated unminted path elsewhere in the same
/// message is somebody else's finding rather than a false red here.
///
/// Off Windows the two spellings are one string and the second assertion is
/// provably vacuous — the `minted == internal` arm says so rather than leaving
/// a reader to work it out. The pin it provides is a Windows pin; on Unix
/// nothing here can distinguish the two, which is why the seam also carries a
/// structural pin (`tests/operator_path_seam_test.rs`).
pub fn assert_names_operator_path(text: &str, path: impl AsRef<std::path::Path>) {
    let path = path.as_ref();
    let minted = repoweave::path_spelling::operator_path(path);
    assert!(
        text.contains(&minted),
        "the message must name {minted} in the operator spelling; got:\n{text}"
    );
    let internal = path.display().to_string();
    assert!(
        minted == internal || !text.contains(&internal),
        "the message still carries the internal spelling {internal}; it is owed \
         {minted}, which is what `crate::path_spelling::operator_path` mints. Got:\n{text}"
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
