//! Performance regression test for `rwv doctor` at workspace scale.
//!
//! The doctor's per-worktree drift scan is O(workweaves ×
//! repos) and was observed not to complete in 30s against a workspace with
//! ~81 workweaves × ~13 active manifest repos. This test reproduces that
//! shape at a smaller-but-still-significant scale and asserts the run
//! finishes within a generous wall-clock budget.
//!
//! Construction is intentionally side-effect light:
//!   - one bare-style repo per manifest entry, initialised with a single commit
//!   - `git worktree add` for each (workweave × repo) pair, on a distinct
//!     ephemeral branch
//!   - a `.rwv-workweave` marker per workweave
//!
//! The test exercises `repoweave::check::run_check` in-process so the time
//! budget is unaffected by binary linkage / cargo spawn overhead.

use repoweave::check;
use std::path::{Path, PathBuf};
use std::time::Instant;

mod common;

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
///   <root>/projects/app/rwv.yaml
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
    let mut manifest = String::from("repositories:\n");
    for r in 0..n_repos {
        let repo_path = format!("github/acme/repo{r}");
        let abs = root.join(&repo_path);
        std::fs::create_dir_all(&abs).unwrap();
        git(&["init", "--initial-branch=main"], &abs);
        std::fs::write(abs.join("README.md"), format!("repo{r}\n")).unwrap();
        git(&["add", "."], &abs);
        git(&["commit", "-m", "initial"], &abs);

        manifest.push_str(&format!(
            "  {repo_path}:\n    type: git\n    url: file://{}\n    version: main\n    role: owned\n",
            abs.display()
        ));
    }
    std::fs::write(root.join("projects/app/rwv.yaml"), manifest).unwrap();

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

        let marker = format!(
            "primary: {p}\nproject: app\nparent: {p}\n",
            p = primary_canon.display()
        );
        std::fs::write(ww_dir.join(".rwv-workweave"), marker).unwrap();
    }

    root
}

/// Synthetic-large-workspace perf assertion. Acceptance:
/// `rwv doctor` completes in O(seconds), not O(>30s), against a 80+
/// workweave workspace. The numbers used here are tuned to a CI-friendly
/// scale that still reproduces the original O(workweaves × repos) shape;
/// the time budget is generous so the test exists primarily as a
/// regression gate against future O(n²) regressions in the scan loop.
#[test]
fn doctor_large_workspace_completes_under_budget() {
    let n_ww: usize = 40;
    let n_repos: usize = 5;
    // Budget: with the fix in place, a 40×5 workload completes in well
    // under 2s on developer hardware. The budget is set well above
    // that — the test exists primarily as a regression gate against
    // future O(n²) blow-ups in the scan loop (the original regression
    // was >30s at 81×13), not as a tight benchmark. 8s flaked on shared
    // macOS CI runners (fo-f78ts4), hence the generous ceiling.
    let budget = std::time::Duration::from_secs(30);

    let tmp = tempfile::tempdir().unwrap();
    let root = build_large_workspace(tmp.path(), n_repos, n_ww);

    // Tell rwv to look for workweaves in the tmp scaffold, not the user's
    // real ~/weaveroot/.workweaves.
    std::env::set_var("RWV_WORKWEAVE_DIR", tmp.path().join(".workweaves"));

    let start = Instant::now();
    // Use scope_all=true so the perf test exercises the full weave-wide scan
    // (the same load it was originally benchmarking).
    let res = check::run_check(&root, false, None, true);
    let elapsed = start.elapsed();

    std::env::remove_var("RWV_WORKWEAVE_DIR");
    eprintln!("doctor_large_workspace: {n_ww} workweaves × {n_repos} repos -> {elapsed:?}");

    assert!(res.is_ok(), "run_check returned an error: {:?}", res.err());
    assert!(
        elapsed < budget,
        "doctor scan exceeded budget: {:?} > {:?} (n_ww={n_ww}, n_repos={n_repos})",
        elapsed,
        budget
    );

    // Soft floor on the workload itself: if the scan finishes in
    // microseconds, the synthetic-workspace construction silently broke
    // and the test would pass vacuously.
    assert!(
        elapsed > std::time::Duration::from_millis(10),
        "scan completed implausibly fast — check that the test fixture \
         actually built the workweaves: {:?}",
        elapsed
    );
}
