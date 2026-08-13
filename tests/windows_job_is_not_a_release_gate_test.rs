//! Pins the one property that keeps the advisory Windows test job from
//! blocking releases, which is a property of WHERE it is declared and nothing
//! else.
//!
//! `.githooks/pre-push` refuses to push a version tag, and
//! `.github/workflows/require-green-ci.yml` refuses to build release artifacts,
//! unless the run of `ci.yml` on that exact commit concluded `success`. Both
//! ask by workflow file name. `.github/workflows/ci.yml` is a thin caller of
//! `.github/workflows/ci-checks.yml`, so every job reachable from it is inside
//! the run those two gates read — which is deliberate, so that adding a
//! platform there is covered without either gate hearing about it.
//!
//! The Windows test job is the one job that must NOT be covered, because it is
//! expected to be red until the suite is portable, and a red job inside that
//! run blocks every release until someone fixes it. It therefore lives in a
//! workflow of its own. Nothing enforces that but this test: moving the job
//! into `ci-checks.yml` is a one-line edit, it is what someone tidying the
//! workflow directory would naturally do, and the damage does not appear until
//! the next release is refused.
//!
//! Residue. This reads YAML as indented text rather than parsing it, so a job
//! written with unusual indentation, or a reusable workflow pulled from another
//! repository rather than by `./` path, is invisible to the reachability walk.
//! It checks where a job is declared, never whether GitHub would let a check
//! block a merge: required status checks are a repository setting that is not
//! in this tree, so a red run reaching a branch-protection rule is a path
//! neither this test nor any file here can see.

use std::path::{Path, PathBuf};

/// The query both release gates make, as it is spelled in each of them.
const GATE_QUERY: &str = "workflows/ci.yml/runs";

/// The workflow whose run conclusion the two gates read.
const GATED_WORKFLOW: &str = "ci.yml";

/// The advisory workflow, which must stay outside that run.
const ADVISORY_WORKFLOW: &str = "windows-tests.yml";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workflow_dir() -> PathBuf {
    repo_root().join(".github").join("workflows")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// One job block: its name and the lines beneath it, split on the two-space
/// keys directly under `jobs:`.
struct Job {
    name: String,
    body: String,
}

fn jobs_in(text: &str) -> Vec<Job> {
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim_end() == "jobs:") else {
        return Vec::new();
    };

    let is_key = |l: &str| {
        l.starts_with("  ")
            && !l.starts_with("   ")
            && l.trim_end().ends_with(':')
            && !l.trim_start().starts_with('#')
    };

    let mut out: Vec<Job> = Vec::new();
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if !is_key(line) {
            continue;
        }
        let end = lines[i + 1..]
            .iter()
            .position(|l| is_key(l))
            .map(|p| i + 1 + p)
            .unwrap_or(lines.len());
        out.push(Job {
            name: line.trim().trim_end_matches(':').to_string(),
            body: lines[i..end].join("\n"),
        });
    }
    out
}

/// A job that runs the test suite on a Windows runner.
fn runs_the_suite_on_windows(job: &Job) -> bool {
    let runs_on_windows = job
        .body
        .lines()
        .any(|l| l.contains("runs-on:") && l.contains("windows"));
    runs_on_windows && job.body.contains("cargo test")
}

/// Every workflow file inside the run `ci.yml` produces: itself, plus the
/// closure of the reusable workflows it calls by `./` path.
fn workflows_inside_the_gated_run() -> Vec<String> {
    let mut seen = vec![GATED_WORKFLOW.to_string()];
    let mut i = 0;
    while i < seen.len() {
        let text = read(&workflow_dir().join(&seen[i]));
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("uses:") else {
                continue;
            };
            let target = rest.trim();
            let Some(name) = target.strip_prefix("./.github/workflows/") else {
                continue;
            };
            let name = name.to_string();
            if !seen.contains(&name) {
                seen.push(name);
            }
        }
        i += 1;
    }
    seen
}

#[test]
fn both_release_gates_key_on_the_ci_workflow_file() {
    for (label, path) in [
        (
            "pre-push hook",
            repo_root().join(".githooks").join("pre-push"),
        ),
        (
            "release-artifact gate",
            workflow_dir().join("require-green-ci.yml"),
        ),
    ] {
        let text = read(&path);
        assert!(
            text.contains(GATE_QUERY),
            "the {label} at {} no longer asks for `{GATE_QUERY}`. Everything \
             this suite pins rests on both gates naming that one workflow file, \
             so if the question changed, the answer to 'is the advisory Windows \
             job out of reach' has to be re-derived rather than assumed",
            path.display()
        );
    }
}

#[test]
fn the_advisory_workflow_is_what_this_suite_thinks_it_is() {
    let path = workflow_dir().join(ADVISORY_WORKFLOW);
    let jobs = jobs_in(&read(&path));
    assert!(
        jobs.iter().any(runs_the_suite_on_windows),
        "{ADVISORY_WORKFLOW} has no job that runs the suite on Windows, so the \
         predicate used below matches nothing and the absence it asserts would \
         hold over any repository at all. Jobs found: {:?}",
        jobs.iter().map(|j| &j.name).collect::<Vec<_>>()
    );
}

#[test]
fn no_windows_test_job_is_reachable_from_the_gated_workflow() {
    let inside = workflows_inside_the_gated_run();

    assert!(
        inside.len() >= 2 && inside.iter().any(|w| w == "ci-checks.yml"),
        "the reachability walk from {GATED_WORKFLOW} found {inside:?}, which \
         does not include the reusable workflow it calls. The walk broke, and \
         an empty reach makes the assertion below vacuous"
    );
    assert!(
        !inside.iter().any(|w| w == ADVISORY_WORKFLOW),
        "{ADVISORY_WORKFLOW} is reachable from {GATED_WORKFLOW} ({inside:?}), \
         so its runs are part of the run both release gates read"
    );

    let offenders: Vec<String> = inside
        .iter()
        .flat_map(|w| {
            jobs_in(&read(&workflow_dir().join(w)))
                .into_iter()
                .filter(runs_the_suite_on_windows)
                .map(move |j| format!("{w}:{}", j.name))
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "these jobs run the test suite on Windows from inside the run that \
         .githooks/pre-push and .github/workflows/require-green-ci.yml both \
         require to conclude `success`: {}. Until the suite is portable that \
         job is expected to be red, and a red job there refuses every release \
         tag — the exact failure the pre-push hook was written to prevent. Keep \
         it in a workflow of its own",
        offenders.join(", ")
    );
}
