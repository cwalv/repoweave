//! Regression test: `rwv materialize` and `rwv doctor --fix` disagreed about a
//! declared file whose source does not exist, and neither would give way.
//!
//! `go-work` declared `go.sum` from `generated_files()` with no gate on a
//! member carrying a `go.mod`, so every weave that left the integration on its
//! default — a pure-Rust one included — held `go.sum` in the surfacing union.
//! `materialize` creates a symlink for a declared file whether or not the
//! source exists, which is how a lock an ecosystem tool writes at the weave
//! root lands in `projects/<project>/`. Inside a workweave, doctor then read
//! the resulting `go.sum -> projects/<project>/go.sum` as a stale symlink and
//! `--fix` removed it, and the next `materialize` put it back. Nothing rwv runs
//! has ever written a `go.sum`, so the cycle had no end state.
//!
//! What this pins is the convergence, not one absent link: `doctor --fix`
//! followed by `materialize` has to leave doctor with nothing to say, over
//! repeated rounds, in a workweave and at primary alike.
//!
//! **Scope.** Every fixture here is Go-free, which is the half of the defect
//! the declaration gate closes. The other half was measured and is open: give a
//! member a `go.mod` and `go.sum` re-enters the union legitimately, the create
//! path leaves the dangling link `materialize` is supposed to leave, and doctor
//! reads it as stale again — one `--fix`/`materialize` round in that shape
//! still oscillates. Distinguishing "a link a tool is about to write through"
//! from "a link whose source went away" needs a per-file signal the surfacing
//! union does not carry, so nothing in this file covers it.

use std::path::{Path, PathBuf};

mod common;

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

/// The file `static-files` declares. It exists in the checkout, so its own
/// surfacing link is the control: every assertion about `go.sum` being absent
/// would also pass against a surfacing pass that had silently stopped running,
/// and this is what tells the two apart.
const SURFACED_CONTROL: &str = "SHARED.md";

/// A weave whose project has no Go in it at all: no member declares a `go.mod`,
/// and `rwv.toml` carries no `go-work` stanza, so the integration sits at its
/// `default_enabled()`. `static-files` declares one real file so the surfacing
/// union is non-empty either way.
fn primary_weave() -> (tempfile::TempDir, PathBuf) {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    let project_dir = ws.join("projects").join("alpha");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories]\n\n\
             [integrations.static-files]\nenabled = true\nfiles = [\"{SURFACED_CONTROL}\"]\n\n\
             [integrations.vscode-workspace]\nenabled = false\n"
        ),
    )
    .unwrap();
    std::fs::write(project_dir.join(SURFACED_CONTROL), "shared\n").unwrap();
    git_init_with_commit(&project_dir);

    std::fs::write(ws.join(".rwv-active"), "alpha\n").unwrap();
    let out = rwv_output(&ws, &["activate", "alpha", "--no-materialize"]);
    assert!(
        ws.join(SURFACED_CONTROL).symlink_metadata().is_ok(),
        "fixture: activate should surface `{SURFACED_CONTROL}`; output:\n{out}"
    );

    std::fs::create_dir_all(root.join(".workweaves")).unwrap();
    (tmp, ws)
}

/// A workweave forked off [`primary_weave`], which is the only root where
/// doctor reads a link over a missing source as stale.
fn workweave() -> (tempfile::TempDir, PathBuf) {
    let (tmp, ws) = primary_weave();
    let out = rwv_output(&ws, &["workweave", "alpha", "create", "agent-1"]);
    let ww_dir = tmp.path().join(".workweaves").join("alpha--agent-1");
    assert!(
        ww_dir.join(SURFACED_CONTROL).symlink_metadata().is_ok(),
        "fixture: workweave create should surface `{SURFACED_CONTROL}`; output:\n{out}"
    );
    (tmp, ww_dir)
}

/// Assert the surfacing pass ran and claimed nothing for an ecosystem the
/// weave does not have. `where_` names the step for the failure message.
fn assert_surfacing_is_go_free(root: &Path, where_: &str) {
    assert!(
        root.join(SURFACED_CONTROL).symlink_metadata().is_ok(),
        "{where_}: the control link `{SURFACED_CONTROL}` is gone, so a `go.sum` \
         assertion below would pass against a surfacing pass that stopped running"
    );
    let go_sum = root.join("go.sum");
    assert!(
        go_sum.symlink_metadata().is_err(),
        "{where_}: surfaced `go.sum` into a weave whose members declare no \
         go.mod; it resolves to {} which nothing will ever write",
        go_sum
            .read_link()
            .map_or_else(|e| e.to_string(), |target| target.display().to_string())
    );
}

#[test]
fn materialize_surfaces_no_go_sum_into_a_weave_with_no_go_member() {
    let (_tmp, ws) = primary_weave();

    let out = rwv_output(&ws, &["materialize"]);
    assert_surfacing_is_go_free(&ws, &format!("primary `rwv materialize`; output:\n{out}"));
}

#[test]
fn workweave_materialize_surfaces_no_go_sum_and_doctor_stays_silent() {
    let (_tmp, ww_dir) = workweave();

    let materialize = rwv_output(&ww_dir, &["materialize"]);
    assert_surfacing_is_go_free(
        &ww_dir,
        &format!("workweave `rwv materialize`; output:\n{materialize}"),
    );

    let doctor = rwv_output(&ww_dir, &["doctor"]);
    assert!(
        !doctor.contains("go.sum"),
        "doctor named `go.sum` after materialize, so the declaration is still \
         in the surfacing union:\n{doctor}"
    );
}

/// The loop itself. `--fix` removing the link and `materialize` restoring it
/// each looked correct in isolation; only running them against each other shows
/// the workspace never reaching a state both agree on.
#[test]
fn doctor_fix_then_materialize_reaches_a_fixed_point() {
    let (_tmp, ww_dir) = workweave();

    for round in 1..=3 {
        let fix = rwv_output(&ww_dir, &["doctor", "--fix"]);
        assert_surfacing_is_go_free(&ww_dir, &format!("round {round} --fix; output:\n{fix}"));

        let materialize = rwv_output(&ww_dir, &["materialize"]);
        assert_surfacing_is_go_free(
            &ww_dir,
            &format!("round {round} materialize; output:\n{materialize}"),
        );

        let doctor = rwv_output(&ww_dir, &["doctor"]);
        assert!(
            !doctor.contains("stale symlink") && !doctor.contains("no longer exists"),
            "round {round}: `doctor --fix` then `materialize` left doctor with a \
             stale-surfacing finding again, so the pair has no fixed point:\n{doctor}"
        );
    }
}
