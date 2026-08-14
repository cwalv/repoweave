//! Regression test for the *cost* of `rwv doctor`'s per-worktree drift scan,
//! at a workspace shape that reproduces the scan's O(workweaves x repos)
//! access pattern: many workweaves, each materializing every manifest repo.
//!
//! Asserts on `git` subprocess invocation COUNT rather than elapsed time. A
//! count is a property of the code path an input takes; it does not change
//! with how fast or how loaded the host is, so doubling the workspace
//! doubles the count on a saturated CI runner exactly as it does on an idle
//! laptop. Wall-clock has no such invariance — runner speed scales the one
//! thing it measures directly — which is why this test does not time
//! anything it asserts on.
//!
//! SKIPPED ON WINDOWS. The counting instrument is a `#!/bin/sh` `git` shim
//! placed on `PATH` and made discoverable with a mode bit; Windows has
//! neither `sh` nor the executable-bit convention `which` needs to find it
//! there. `windows-check` still compiles this file — it is absent from that
//! job the same way any `#[cfg(unix)]`-skipped test is, which is not a gap
//! particular to this one.

#![cfg(unix)]

mod common;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn git(args: &[&str], dir: &Path) {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Create a primary workspace with `n_repos` manifest entries and `n_ww`
/// workweaves that each materialize every repo as a worktree.
///
/// Workspace layout:
///   <root>/projects/app/rwv.toml
///   <root>/github/acme/repoN          (N = 0..n_repos)
///   <root>/.workweaves/app--wkK/      (K = 0..n_ww)
///       └─ github/acme/repoN          (worktree on branch wkK/main)
///       └─ .rwv-workweave             (marker)
fn build_large_workspace(parent: &Path, n_repos: usize, n_ww: usize) -> PathBuf {
    let root = parent.join("ws");
    std::fs::create_dir_all(root.join("projects/app")).unwrap();
    std::fs::create_dir_all(root.join("github/acme")).unwrap();
    std::fs::create_dir_all(parent.join(".workweaves")).unwrap();

    // --- Real source repos under the primary registry ---
    let mut manifest = String::from("[repositories]\n");
    for r in 0..n_repos {
        let repo_path = format!("github/acme/repo{r}");
        let abs = root.join(&repo_path);
        std::fs::create_dir_all(&abs).unwrap();
        git(&["init", "--initial-branch=main"], &abs);
        std::fs::write(abs.join("README.md"), format!("repo{r}\n")).unwrap();
        git(&["add", "."], &abs);
        git(&["commit", "-m", "initial"], &abs);

        manifest.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"file://{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::url_path(&abs)
        ));
    }
    std::fs::write(root.join("projects/app/rwv.toml"), manifest).unwrap();

    // --- Workweaves, each materialising every repo via `git worktree add` ---
    let primary_canon = root.canonicalize().unwrap();
    for k in 0..n_ww {
        let ww_dir = parent.join(".workweaves").join(format!("app--wk{k}"));
        std::fs::create_dir_all(ww_dir.join("github/acme")).unwrap();

        for r in 0..n_repos {
            let repo_path = format!("github/acme/repo{r}");
            let src = root.join(&repo_path);
            let dst = ww_dir.join(&repo_path);
            git(
                &[
                    "worktree",
                    "add",
                    "-b",
                    &format!("wk{k}/main"),
                    &dst.to_string_lossy(),
                ],
                &src,
            );
        }

        let marker = common::workweave_marker(&primary_canon, "app", &primary_canon);
        std::fs::write(ww_dir.join(".rwv-workweave"), marker).unwrap();
    }

    root
}

/// A `PATH` entry providing a `git` that records one marker file per
/// invocation, then execs the real binary at an absolute path baked into the
/// shim script — so it never has to re-search `PATH` and can never recurse
/// into itself. Each invocation's marker gets a name from `mktemp`, which
/// hands out distinct names under concurrent callers without any locking on
/// this side.
struct CountingGitShim {
    dir: tempfile::TempDir,
    markers: PathBuf,
}

impl CountingGitShim {
    fn install(real_git: &Path) -> Self {
        let dir = common::tempdir().unwrap();
        let markers = dir.path().join("markers");
        fs::create_dir_all(&markers).unwrap();

        let script_path = dir.path().join("git");
        let mut f = fs::File::create(&script_path).unwrap();
        write!(
            f,
            "#!/bin/sh\nmktemp '{markers}/call.XXXXXX' >/dev/null\nexec '{real_git}' \"$@\"\n",
            markers = markers.display(),
            real_git = real_git.display(),
        )
        .unwrap();
        drop(f);
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        Self { dir, markers }
    }

    fn path_entry(&self) -> &Path {
        self.dir.path()
    }

    fn call_count(&self) -> usize {
        fs::read_dir(&self.markers).unwrap().count()
    }
}

/// Run `rwv doctor --all` against `root` with every `git` subprocess it
/// spawns routed through a fresh [`CountingGitShim`], and return the call
/// count alongside elapsed wall time (reported, not asserted on — see the
/// module doc for why).
fn scan_and_count_git_calls(root: &Path, real_git: &Path) -> (usize, std::time::Duration) {
    let shim = CountingGitShim::install(real_git);
    // The shim itself is a `#!/bin/sh` script that calls `mktemp`, so it
    // needs a real PATH behind it — only prepended, so `Command::new("git")`
    // still finds the shim first.
    let outer_path = std::env::var("PATH").unwrap_or_default();
    let shimmed_path = format!("{}:{outer_path}", shim.path_entry().display());

    let start = std::time::Instant::now();
    common::rwv()
        .args(["-C", &root.to_string_lossy(), "doctor", "--all"])
        .env("PATH", shimmed_path)
        .output()
        .expect("rwv doctor should run to completion");
    let elapsed = start.elapsed();

    (shim.call_count(), elapsed)
}

/// `rwv doctor --all`'s `git` invocation count grows linearly, not
/// quadratically, with workspace size.
///
/// Measured at two scales related by a clean doubling of workweave count;
/// their ratio stays well under the ~4x a quadratic scan would produce. The
/// floor on the smaller scale's count is what keeps a fixture that silently
/// failed to build (zero repos on disk) from passing the ratio check
/// vacuously.
#[test]
fn doctor_scan_git_call_count_stays_linear_in_workspace_size() {
    let n_repos: usize = 5;
    let n1_ww: usize = 20;
    let n2_ww: usize = 40;

    let real_git = which::which("git").expect("git must be resolvable on PATH for this test");

    let tmp1 = common::tempdir().unwrap();
    let root1 = build_large_workspace(tmp1.path(), n_repos, n1_ww);
    let tmp2 = common::tempdir().unwrap();
    let root2 = build_large_workspace(tmp2.path(), n_repos, n2_ww);

    let (count1, elapsed1) = scan_and_count_git_calls(&root1, &real_git);
    let (count2, elapsed2) = scan_and_count_git_calls(&root2, &real_git);

    eprintln!(
        "doctor scan git calls: {n1_ww}x{n_repos} -> {count1} calls in {elapsed1:?}; \
         {n2_ww}x{n_repos} -> {count2} calls in {elapsed2:?}"
    );

    let floor = n1_ww * n_repos;
    assert!(
        count1 >= floor,
        "only {count1} git calls scanning {n1_ww}x{n_repos} — fewer than one per \
         repo-on-disk entry, which means the fixture likely didn't build as intended"
    );

    let ratio = count2 as f64 / count1 as f64;
    assert!(
        ratio < 3.0,
        "git call count more than tripled ({count1} -> {count2}, ratio {ratio:.2}) when \
         workweave count only doubled ({n1_ww} -> {n2_ww}); the scan is no longer linear \
         in (workweaves x repos)"
    );
}
