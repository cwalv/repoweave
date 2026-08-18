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
//! **Scope.** The Go-free fixtures below pin the declaration gate. The second
//! half — a name that stays in the union while its source does not exist — is
//! pinned by [`a_declared_file_nothing_produces_converges_in_a_workweave`] and
//! its primary twin, through `static-files`, which reaches the state with no
//! ecosystem tool involved at all: one `rwv.toml` line naming a file that is
//! not in the checkout. Which of the two readings a link over an absent source
//! gets is now the declaration's own answer, so the case that used to
//! oscillate has a fixed point.
//!
//! What is NOT here: the same convergence for a path an ecosystem tool writes
//! through its link, where the fixed point is the link SURVIVING rather than
//! staying absent. That arm is unit-level, in `src/activate.rs`
//! (`a_missing_source_means_opposite_things_for_the_two_provenances`), because
//! reaching it end to end costs a real ecosystem toolchain on PATH.

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

/// A name `static-files` declares that is NOT in the project checkout.
///
/// The whole "declared, and nothing produces it" class in one `rwv.toml`
/// entry: no ecosystem tool is involved, so there is no toolchain to install
/// and no hook that might quietly fill the file in and hide the state under
/// test.
const DECLARED_ABSENT: &str = "NOT-IN-CHECKOUT.md";

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
             [integrations.static-files]\nenabled = true\n\
             files = [\"{SURFACED_CONTROL}\", \"{DECLARED_ABSENT}\"]\n\n\
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

/// The declared-but-absent name must reach a fixed point: no link, and nothing
/// for doctor to say about one.
///
/// Asserted per round rather than once at the end, because the failure this
/// guards is an oscillation — a pair that ends where it started still passes a
/// check taken only at the end.
fn assert_absent_name_converges(root: &Path, where_: &str) {
    assert!(
        root.join(SURFACED_CONTROL).symlink_metadata().is_ok(),
        "{where_}: the control link `{SURFACED_CONTROL}` is gone, so the \
         assertions below would pass against a surfacing pass that stopped \
         running"
    );
    let absent = root.join(DECLARED_ABSENT);
    assert!(
        absent.symlink_metadata().is_err(),
        "{where_}: surfaced `{DECLARED_ABSENT}`, whose source is not in the \
         checkout; it resolves to {} and nothing will ever write it",
        absent
            .read_link()
            .map_or_else(|e| e.to_string(), |target| target.display().to_string())
    );
}

/// `doctor`'s surfacing channel must be silent about the absent name — and the
/// silence has to be checked against the finding TEXT, not against a clean
/// exit, because the declaring integration legitimately reports the missing
/// file on its own channel in the same run.
fn assert_no_surfacing_finding(doctor: &str, where_: &str) {
    for phrase in ["stale symlink", "no longer exists", "is not surfaced"] {
        assert!(
            !doctor.contains(phrase),
            "{where_}: doctor raised a surfacing finding (`{phrase}`) for a \
             declared file that has no source and no link:\n{doctor}"
        );
    }
    assert!(
        doctor.contains(DECLARED_ABSENT),
        "{where_}: doctor said nothing at all about `{DECLARED_ABSENT}`. The \
         file IS missing and the declaring integration owns that finding — a \
         silent doctor here would mean this test is passing because the \
         declaration went away, not because the surfacing channel got it \
         right:\n{doctor}"
    );
}

/// The workweave arm: the root where the loop was measured.
#[test]
fn a_declared_file_nothing_produces_converges_in_a_workweave() {
    let (_tmp, ww_dir) = workweave();

    for round in 1..=3 {
        let fix = rwv_output(&ww_dir, &["doctor", "--fix"]);
        assert_absent_name_converges(&ww_dir, &format!("round {round} --fix:\n{fix}"));

        let materialize = rwv_output(&ww_dir, &["materialize"]);
        assert_absent_name_converges(
            &ww_dir,
            &format!("round {round} materialize:\n{materialize}"),
        );

        let doctor = rwv_output(&ww_dir, &["doctor"]);
        assert_no_surfacing_finding(&doctor, &format!("round {round}"));
    }
}

/// The primary arm, and not a duplicate of the one above.
///
/// Primary never reported a stale link at all: the check took its
/// missing-source flag from "am I in a workweave", which is false here, so the
/// arm that fires on the link could not be reached from this root. The loop was
/// the workweave symptom of that; a link standing over a source that had gone
/// away, forever unreported, was the primary one. Both are the same flag, so
/// both have to be pinned or half the fix is untested.
#[test]
fn a_declared_file_nothing_produces_converges_at_primary() {
    let (_tmp, ws) = primary_weave();

    for round in 1..=3 {
        let fix = rwv_output(&ws, &["doctor", "--fix"]);
        assert_absent_name_converges(&ws, &format!("round {round} --fix:\n{fix}"));

        let materialize = rwv_output(&ws, &["materialize"]);
        assert_absent_name_converges(&ws, &format!("round {round} materialize:\n{materialize}"));

        let doctor = rwv_output(&ws, &["doctor"]);
        assert_no_surfacing_finding(&doctor, &format!("round {round}"));
    }
}

/// A link left behind at a declared name whose source went away IS stale, and
/// at primary too. The complement of the two tests above: they pin that a link
/// is never created, this pins that one already there is reported and removed.
#[test]
fn a_link_over_a_vanished_source_is_reported_and_removed_at_primary() {
    let (_tmp, ws) = primary_weave();

    // Through rwv's own seam, not `std::os::unix`: it is what production uses,
    // and it is the spelling that compiles for the Windows target the gate
    // cross-checks.
    repoweave::symlink::create(
        Path::new(&format!("projects/alpha/{DECLARED_ABSENT}")),
        &ws.join(DECLARED_ABSENT),
        repoweave::symlink::LinkTarget::File,
    )
    .unwrap();

    let doctor = rwv_output(&ws, &["doctor"]);
    assert!(
        doctor.contains("stale symlink") && doctor.contains(DECLARED_ABSENT),
        "a link standing over an absent source is stale, and primary used to \
         be unable to say so:\n{doctor}"
    );

    let fix = rwv_output(&ws, &["doctor", "--fix"]);
    assert!(
        ws.join(DECLARED_ABSENT).symlink_metadata().is_err(),
        "--fix should have removed the stale link:\n{fix}"
    );
    assert_absent_name_converges(&ws, "after --fix");
}
