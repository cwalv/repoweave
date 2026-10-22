//! Every matrix job in `.github/workflows/` runs its legs to completion.
//!
//! With `fail-fast` left at GitHub's default of true, the first leg to fail
//! cancels the legs still running. A canceled leg is not a verdict: a red
//! macOS leg can mark a Linux leg canceled mid-suite, and the run then cannot
//! say whether Linux is green. Each platform's result is independent
//! evidence, so every matrix declares `fail-fast: false`.
//!
//! Structural, because no test here can make GitHub fail one leg and observe
//! another: the property is a prohibition over the jobs that declare a
//! matrix, which the workflow files enumerate. The population is derived from
//! them, so a new matrix job is held to the rule without this file hearing
//! about it.
//!
//! Scope: every `*.yml` directly under `.github/workflows/` except
//! `release.yml`, which dist regenerates and owns. A matrix job in a
//! composite action or another repository's reusable workflow is invisible
//! here, and so is a `strategy` written as an expression instead of a
//! mapping, which is reported rather than evaluated.

use std::path::{Path, PathBuf};

use saphyr::{LoadableYamlNode, YamlOwned};

const DIST_OWNED: &str = "release.yml";

fn workflow_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows")
}

fn workflow_files() -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(workflow_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".yml") && name != DIST_OWNED)
        .collect();
    files.sort();
    files
}

fn load(file: &str) -> YamlOwned {
    let path = workflow_dir().join(file);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    YamlOwned::load_from_str(&text)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{} holds no YAML document", path.display()))
}

struct MatrixJob {
    label: String,
    fail_fast: Option<bool>,
}

fn matrix_jobs() -> Vec<MatrixJob> {
    workflow_files()
        .into_iter()
        .flat_map(|file| {
            let workflow = load(&file);
            let jobs = workflow
                .as_mapping_get("jobs")
                .and_then(YamlOwned::as_mapping)
                .unwrap_or_else(|| panic!("{file} has no jobs mapping"));
            jobs.iter()
                .filter_map(|(name, job)| {
                    let strategy = job.as_mapping_get("strategy")?;
                    let declares_matrix = strategy.as_mapping().is_none()
                        || strategy.as_mapping_get("matrix").is_some();
                    declares_matrix.then(|| MatrixJob {
                        label: format!("{file}:{}", name.as_str().unwrap()),
                        fail_fast: strategy
                            .as_mapping_get("fail-fast")
                            .and_then(YamlOwned::as_bool),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn the_walk_finds_the_matrix_jobs_this_suite_polices() {
    let found: Vec<String> = matrix_jobs().into_iter().map(|j| j.label).collect();
    assert!(
        found.iter().any(|l| l.starts_with("ci-checks.yml:")),
        "no matrix job was found in ci-checks.yml (found: {found:?}), so either the \
         walk broke or the platform matrix moved. An empty population makes the \
         assertion below hold over any repository at all"
    );
}

#[test]
fn every_matrix_job_declares_fail_fast_false() {
    let offenders: Vec<String> = matrix_jobs()
        .into_iter()
        .filter(|j| j.fail_fast != Some(false))
        .map(|j| j.label)
        .collect();
    assert!(
        offenders.is_empty(),
        "these matrix jobs do not declare `fail-fast: false`: {}. Under the default, \
         one leg's failure cancels the legs still running, and a canceled leg reports \
         no verdict for its platform",
        offenders.join(", ")
    );
}
