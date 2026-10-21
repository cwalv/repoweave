//! A `static-files` entry declared `from = "primary"` surfaces the primary's
//! project copy into every workweave, so state shared across workweaves has
//! one copy, while a plain entry keeps surfacing the workweave's own.

mod common;

use std::path::{Path, PathBuf};

const MANIFEST: &str = "[repositories]\n\n\
[integrations.static-files]\n\
enabled = true\n\
files = [\"notes.md\", { path = \".shared\", from = \"primary\" }, \
{ path = \"shared.db\", from = \"primary\" }, \"local.db\"]\n\n\
[integrations.vscode-workspace]\nenabled = false\n\n\
[integrations.go-work]\nenabled = false\n";

const OTHER_MANIFEST: &str = "[repositories]\n\n\
[integrations.vscode-workspace]\nenabled = false\n\n\
[integrations.go-work]\nenabled = false\n";

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

fn rwv_output(cwd: &Path, args: &[&str]) -> String {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

struct Weaves {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    containers: PathBuf,
}

impl Weaves {
    /// What a primary-sourced `.shared` link must read from any workweave.
    fn primary_copy(&self) -> PathBuf {
        repoweave::path_spelling::resolve(&self.primary)
            .unwrap()
            .join("projects/alpha/.shared")
    }

    fn create(&self, name: &str, from: Option<&Path>) -> PathBuf {
        let mut args = vec!["workweave", "alpha", "create", name];
        let from = from.map(|p| p.to_string_lossy().into_owned());
        if let Some(from) = &from {
            args.extend(["--from", from.as_str()]);
        }
        let out = rwv_output(&self.primary, &args);
        let dir = self.containers.join(format!("alpha--{name}"));
        assert!(
            dir.join(".rwv-workweave").is_file(),
            "fixture: workweave `{name}` should exist; output:\n{out}"
        );
        dir
    }
}

/// A primary with project `alpha`, whose `.shared` directory exists only in
/// the primary's checkout (ignored, as a tracker's database is), and a
/// second project `beta` the primary can be switched to.
fn primary() -> Weaves {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let ws = root.join("ws");

    let alpha = ws.join("projects/alpha");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::write(alpha.join("rwv.toml"), MANIFEST).unwrap();
    std::fs::write(alpha.join("notes.md"), "notes\n").unwrap();
    std::fs::write(
        alpha.join(".gitignore"),
        "/.shared/\n/shared.db\n/local.db\n",
    )
    .unwrap();
    git_init_with_commit(&alpha);
    std::fs::write(alpha.join("shared.db"), "primary db\n").unwrap();
    std::fs::write(alpha.join("local.db"), "local db\n").unwrap();
    std::fs::create_dir_all(alpha.join(".shared")).unwrap();
    std::fs::write(alpha.join(".shared/state"), "primary state\n").unwrap();

    let beta = ws.join("projects/beta");
    std::fs::create_dir_all(&beta).unwrap();
    std::fs::write(beta.join("rwv.toml"), OTHER_MANIFEST).unwrap();
    git_init_with_commit(&beta);

    std::fs::write(ws.join(".rwv-active"), "alpha\n").unwrap();
    let out = rwv_output(&ws, &["activate", "alpha", "--no-materialize"]);
    assert!(
        ws.join(".shared").symlink_metadata().is_ok(),
        "fixture: activate should surface `.shared` at the primary; output:\n{out}"
    );

    let containers = root.join(".workweaves");
    std::fs::create_dir_all(&containers).unwrap();
    Weaves {
        _tmp: tmp,
        primary: ws,
        containers,
    }
}

fn link(dir: &Path, name: &str) -> PathBuf {
    std::fs::read_link(dir.join(name))
        .unwrap_or_else(|e| panic!("`{name}` should be a symlink in {}: {e}", dir.display()))
}

fn surfacing_lines(doctor: &str, name: &str) -> Vec<String> {
    doctor
        .lines()
        .filter(|line| line.contains("surfacing") && line.contains(name))
        .map(str::to_string)
        .collect()
}

/// The `rwv doctor --json` surfacing issues naming `name` that report a link
/// resolving somewhere other than expected.
fn mis_target_issues(dir: &Path, name: &str) -> Vec<serde_json::Value> {
    let output = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(dir)
        .output()
        .expect("rwv should run");
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("`--json` must emit parseable JSON");
    parsed["issues"]
        .as_array()
        .expect("`issues` must be an array")
        .iter()
        .filter(|i| {
            i["kind"] == "surfacing"
                && i["message"]
                    .as_str()
                    .is_some_and(|m| m.contains(name) && m.contains("resolves to"))
        })
        .cloned()
        .collect()
}

fn replace_link(dir: &Path, name: &str, target: &Path) {
    repoweave::symlink::remove(&dir.join(name)).unwrap();
    repoweave::symlink::create(
        target,
        &dir.join(name),
        repoweave::symlink::LinkTarget::Directory,
    )
    .unwrap();
}

#[test]
fn a_workweave_links_the_primarys_project_copy_and_keeps_plain_entries_its_own() {
    let weaves = primary();
    let ww = weaves.create("one", None);

    assert_eq!(link(&ww, ".shared"), weaves.primary_copy());
    assert_eq!(link(&ww, "notes.md"), Path::new("projects/alpha/notes.md"));
    assert_eq!(
        std::fs::read_to_string(ww.join(".shared/state")).unwrap(),
        "primary state\n"
    );

    let doctor = rwv_output(&ww, &["doctor"]);
    assert!(
        surfacing_lines(&doctor, ".shared").is_empty(),
        "a freshly created workweave must verify clean:\n{doctor}"
    );
    assert!(
        !doctor.contains("'.shared' not found"),
        "the workweave's own checkout lacking the primary's copy is not a finding:\n{doctor}"
    );
}

#[test]
fn the_primary_surfaces_a_primary_sourced_entry_as_a_plain_one() {
    let weaves = primary();
    assert_eq!(
        link(&weaves.primary, ".shared"),
        Path::new("projects/alpha/.shared")
    );
    let doctor = rwv_output(&weaves.primary, &["doctor"]);
    assert!(
        surfacing_lines(&doctor, ".shared").is_empty(),
        "the primary must verify clean:\n{doctor}"
    );
}

#[test]
fn a_workweave_forked_from_a_workweave_links_the_primary_not_its_parent() {
    let weaves = primary();
    let parent = weaves.create("parent", None);
    let child = weaves.create("child", Some(&parent));

    assert_eq!(link(&child, ".shared"), weaves.primary_copy());
}

#[test]
fn the_link_ignores_which_project_the_primary_presents() {
    let weaves = primary();
    let existing = weaves.create("before", None);
    let out = rwv_output(&weaves.primary, &["activate", "beta", "--no-materialize"]);
    assert!(
        weaves.primary.join(".shared").symlink_metadata().is_err(),
        "fixture: presenting beta takes alpha's `.shared` off the primary root:\n{out}"
    );

    let created = weaves.create("after", None);
    assert_eq!(link(&created, ".shared"), weaves.primary_copy());
    assert_ne!(
        link(&created, ".shared"),
        repoweave::path_spelling::resolve(&weaves.primary)
            .unwrap()
            .join(".shared"),
        "never the primary's weave-root slot"
    );
    let doctor = rwv_output(&existing, &["doctor"]);
    assert!(
        surfacing_lines(&doctor, ".shared").is_empty(),
        "switching the primary must not unsettle an existing workweave:\n{doctor}"
    );
}

#[test]
fn doctor_reports_a_missing_link_and_fix_restores_it() {
    let weaves = primary();
    let ww = weaves.create("one", None);
    repoweave::symlink::remove(&ww.join(".shared")).unwrap();

    let doctor = rwv_output(&ww, &["doctor"]);
    let lines = surfacing_lines(&doctor, ".shared");
    assert!(
        lines.iter().any(|l| l.contains("not surfaced")),
        "doctor should report the missing link:\n{doctor}"
    );

    let fix = rwv_output(&ww, &["doctor", "--fix"]);
    assert_eq!(link(&ww, ".shared"), weaves.primary_copy(), "{fix}");
}

#[test]
fn doctor_fix_replaces_a_link_into_the_workweaves_own_copy() {
    let weaves = primary();
    let ww = weaves.create("one", None);
    replace_link(&ww, ".shared", Path::new("projects/alpha/.shared"));

    let doctor = rwv_output(&ww, &["doctor"]);
    assert!(
        surfacing_lines(&doctor, ".shared")
            .iter()
            .any(|l| l.contains("resolves to") && l.contains("safe to --fix")),
        "doctor should report the mis-targeted link as fixable:\n{doctor}"
    );

    let issues = mis_target_issues(&ww, ".shared");
    assert_eq!(issues.len(), 1, "{issues:#?}");
    assert_eq!(issues[0]["safe_to_fix"], true, "{:#}", issues[0]);

    let fix = rwv_output(&ww, &["doctor", "--fix"]);
    assert_eq!(link(&ww, ".shared"), weaves.primary_copy(), "{fix}");
}

#[test]
fn an_occupied_slot_is_reported_and_never_clobbered() {
    let weaves = primary();
    let ww = weaves.create("one", None);
    repoweave::symlink::remove(&ww.join(".shared")).unwrap();
    std::fs::create_dir(ww.join(".shared")).unwrap();
    std::fs::write(ww.join(".shared/mine"), "operator content\n").unwrap();

    let doctor = rwv_output(&ww, &["doctor"]);
    assert!(
        surfacing_lines(&doctor, ".shared")
            .iter()
            .any(|l| l.contains("not a symlink")),
        "doctor should report the occupant:\n{doctor}"
    );

    let fix = rwv_output(&ww, &["doctor", "--fix"]);
    assert_eq!(
        std::fs::read_to_string(ww.join(".shared/mine")).unwrap(),
        "operator content\n",
        "--fix must leave the occupant alone:\n{fix}"
    );
    assert!(!ww.join(".shared").symlink_metadata().unwrap().is_symlink());
}

#[test]
fn an_absolute_link_into_another_trees_projects_is_not_offered_as_fixable() {
    let weaves = primary();
    let ww = weaves.create("one", None);
    let elsewhere = weaves.containers.join("elsewhere/projects/alpha/.shared");
    std::fs::create_dir_all(&elsewhere).unwrap();
    replace_link(&ww, ".shared", &elsewhere);

    let doctor = rwv_output(&ww, &["doctor"]);
    let lines = surfacing_lines(&doctor, ".shared");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("resolves to") && l.contains("not auto-fixed")),
        "a link rwv did not make is reported, not offered to --fix:\n{doctor}"
    );
    let issues = mis_target_issues(&ww, ".shared");
    assert_eq!(issues.len(), 1, "{issues:#?}");
    assert_eq!(
        issues[0]["safe_to_fix"], false,
        "an agent reading --json must not wait on --fix for a link --fix leaves:\n{:#}",
        issues[0]
    );

    rwv_output(&ww, &["doctor", "--fix"]);
    assert_eq!(link(&ww, ".shared"), elsewhere);
}

#[test]
fn dropping_the_entry_reports_the_primary_sourced_link_as_undeclared() {
    let weaves = primary();
    let ww = weaves.create("one", None);
    std::fs::write(
        ww.join("projects/alpha/rwv.toml"),
        MANIFEST.replace(", { path = \".shared\", from = \"primary\" }", ""),
    )
    .unwrap();

    let doctor = rwv_output(&ww, &["doctor"]);
    assert!(
        surfacing_lines(&doctor, ".shared")
            .iter()
            .any(|l| l.contains("no longer declares")),
        "the stranded absolute link is rwv's, and reported:\n{doctor}"
    );

    let out = rwv_output(&ww, &["materialize", "--remove-undeclared-links"]);
    assert!(
        ww.join(".shared").symlink_metadata().is_err(),
        "the reported link is removed:\n{out}"
    );
    assert_eq!(
        std::fs::read_to_string(weaves.primary.join("projects/alpha/.shared/state")).unwrap(),
        "primary state\n",
        "and the primary's copy behind it is untouched"
    );
}

#[test]
fn switching_the_entry_back_to_plain_replaces_the_absolute_link() {
    let weaves = primary();
    let ww = weaves.create("one", None);
    std::fs::create_dir_all(ww.join("projects/alpha/.shared")).unwrap();
    std::fs::write(
        ww.join("projects/alpha/rwv.toml"),
        MANIFEST.replace("{ path = \".shared\", from = \"primary\" }", "\".shared\""),
    )
    .unwrap();

    let fix = rwv_output(&ww, &["doctor", "--fix"]);
    assert_eq!(
        link(&ww, ".shared"),
        Path::new("projects/alpha/.shared"),
        "{fix}"
    );
}

#[test]
fn a_workweave_link_into_the_primarys_projects_is_left_to_workweave_link() {
    let weaves = primary();
    let alpha = weaves.primary.join("projects/alpha");
    std::fs::write(alpha.join("linked.md"), "linked\n").unwrap();
    repoweave::symlink::create(
        Path::new("projects/alpha/linked.md"),
        &weaves.primary.join("linked.md"),
        repoweave::symlink::LinkTarget::File,
    )
    .unwrap();
    std::fs::write(
        alpha.join("rwv.toml"),
        format!("{MANIFEST}\n[workweave]\nlink = [\"linked.md\"]\n"),
    )
    .unwrap();
    common::git_in(&alpha, &["add", "-A"]);
    common::git_in(&alpha, &["commit", "-m", "link"]);

    let ww = weaves.create("one", None);
    let made = link(&ww, "linked.md");
    assert_eq!(
        made,
        repoweave::path_spelling::resolve(&weaves.primary)
            .unwrap()
            .join("projects/alpha/linked.md"),
        "fixture: workweave.link canonicalizes into the primary's project tree"
    );

    let doctor = rwv_output(&ww, &["doctor"]);
    assert!(
        surfacing_lines(&doctor, "linked.md").is_empty(),
        "a workweave.link link is not rwv surfacing:\n{doctor}"
    );
    rwv_output(&ww, &["materialize", "--remove-undeclared-links"]);
    assert_eq!(link(&ww, "linked.md"), made);
}

#[test]
fn deleting_a_workweave_leaves_the_primarys_copy_intact() {
    let weaves = primary();
    weaves.create("one", None);

    let out = rwv_output(
        &weaves.primary,
        &[
            "workweave",
            "alpha",
            "delete",
            "one",
            "--discard-uncommitted",
        ],
    );
    assert!(
        !weaves.containers.join("alpha--one").exists(),
        "fixture: the workweave is gone:\n{out}"
    );
    assert_eq!(
        std::fs::read_to_string(weaves.primary.join("projects/alpha/.shared/state")).unwrap(),
        "primary state\n"
    );
}

#[test]
fn an_untracked_primary_sourced_file_is_not_copied_into_a_workweave() {
    let weaves = primary();
    let ww = weaves.create("one", None);

    assert!(
        !ww.join("projects/alpha/shared.db").exists(),
        "the workweave links the primary's copy, so its own checkout gets none"
    );
    assert_eq!(
        link(&ww, "shared.db"),
        repoweave::path_spelling::resolve(&weaves.primary)
            .unwrap()
            .join("projects/alpha/shared.db")
    );
    assert_eq!(
        std::fs::read_to_string(ww.join("projects/alpha/local.db")).unwrap(),
        "local db\n",
        "control: an untracked plain entry is still carried"
    );
}
