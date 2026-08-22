//! A weave-root symlink at a name the presented project no longer declares.
//!
//! Every candidate set in the surfacing machinery is built from what a project
//! declares *now*, so a name that dropped out of the declarations is in none of
//! them: `materialize` did not recreate it, `doctor` did not report it, and
//! `doctor --fix` did not remove it. Measured against the shipped binary with
//! the link left in both states it occurs in — resolving to a real file, and
//! dangling.
//!
//! What the repair may reach is deliberately narrow, and this file pins the
//! narrowness one conjunct at a time rather than once in aggregate. A single
//! negative case tripping on the first condition would say nothing about the
//! other three, so each has its own fixture entry and its own assertion.
//!
//! The removal is reported and never auto-repaired. On disk rwv cannot tell
//! its own residue from a link an operator made at the same shape, so
//! `doctor --fix` leaves it and the operator chooses — which makes
//! "`--fix` does not remove it" a promise with a test, not an omission.

use std::path::{Path, PathBuf};

mod common;

/// The declared file that must survive everything below.
///
/// Without it every "the link is gone" assertion here would also pass against
/// a sweep that removed the entire weave root, and every "doctor is silent"
/// assertion against a doctor that stopped running.
const DECLARED: &str = "SHARED.md";

/// Declared at first, then dropped from `rwv.toml` — the subject.
const DROPPED: &str = "DROPPED.md";

/// Same, but its source never existed, so its link is dangling. The filing
/// found the class in this state; both states must be reached.
const DROPPED_DANGLING: &str = "GONE.md";

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

fn write_manifest(project_dir: &Path, files: &[&str]) {
    let list = files
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories]\n\n\
             [integrations.static-files]\nenabled = true\nfiles = [{list}]\n\n\
             [integrations.vscode-workspace]\nenabled = false\n"
        ),
    )
    .unwrap();
}

/// A weave whose project once declared three names and now declares one, with
/// rwv itself having created all three links.
///
/// The links are made by `rwv activate` rather than by hand, so what the tests
/// below act on is rwv's own residue in the shape rwv leaves it — not a
/// hand-built topology that production never writes.
fn weave_with_dropped_links() -> (tempfile::TempDir, PathBuf) {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let project_dir = ws.join("projects/alpha");
    std::fs::create_dir_all(&project_dir).unwrap();

    write_manifest(&project_dir, &[DECLARED, DROPPED, DROPPED_DANGLING]);
    std::fs::write(project_dir.join(DECLARED), "shared\n").unwrap();
    std::fs::write(project_dir.join(DROPPED), "dropped\n").unwrap();
    // DROPPED_DANGLING is deliberately never authored.
    git_init_with_commit(&project_dir);

    std::fs::write(ws.join(".rwv-active"), "alpha\n").unwrap();
    let out = rwv_output(&ws, &["activate", "alpha", "--no-materialize"]);
    for name in [DECLARED, DROPPED] {
        assert!(
            ws.join(name).symlink_metadata().is_ok(),
            "fixture: activate should have surfaced `{name}`; output:\n{out}"
        );
    }
    // The dangling one is surfaced by nothing now that a declaration says its
    // file is written at its source, so this fixture makes it the way a
    // pre-fix rwv did: the exact shape and target rwv's own surfacing writes.
    repoweave::symlink::create(
        Path::new(&format!("projects/alpha/{DROPPED_DANGLING}")),
        &ws.join(DROPPED_DANGLING),
        repoweave::symlink::LinkTarget::File,
    )
    .unwrap();

    // Drop two names from the declaration. Both links are now orphans.
    write_manifest(&project_dir, &[DECLARED]);
    (tmp, ws)
}

fn link_exists(ws: &Path, name: &str) -> bool {
    ws.join(name).symlink_metadata().is_ok()
}

/// The live control every confinement case below needs.
///
/// Each of those asserts that something SURVIVED a sweep, and a sweep that
/// never ran — a predicate blocked upstream, a flag that stopped being parsed,
/// a scan whose walk returns nothing — satisfies all of them at once. So each
/// asserts in the same run that the two known orphans did go away. Without
/// this the confinement suite is green exactly when the feature is dead.
fn assert_the_sweep_actually_ran(ws: &Path, out: &str) {
    for name in [DROPPED, DROPPED_DANGLING] {
        assert!(
            !link_exists(ws, name),
            "control: the sweep did not remove the known orphan `{name}`, so \
             every survival assertion in this test would pass against a sweep \
             that does nothing:\n{out}"
        );
    }
}

/// P5 — the finding exists, names the link AND its target, and names the verb
/// that removes it.
#[test]
fn doctor_reports_each_undeclared_link_with_its_target_and_the_remedy() {
    let (_tmp, ws) = weave_with_dropped_links();

    let doctor = rwv_output(&ws, &["doctor"]);

    for name in [DROPPED, DROPPED_DANGLING] {
        assert!(
            doctor.contains(name),
            "doctor must name the orphan `{name}`:\n{doctor}"
        );
        // The target, not just the name: a report naming only the name cannot
        // be told from one naming the wrong link, and the target is what makes
        // the link reconstructable after it is removed.
        assert!(
            doctor.contains(&format!("projects/alpha/{name}")),
            "doctor must name what `{name}` resolves to, or the operator cannot \
             put it back:\n{doctor}"
        );
    }
    // Discoverability: the operator meets this finding while running doctor,
    // and the command that acts on it is a different verb. A finding that
    // reported the problem and left the remedy to be guessed would be worse
    // than not splitting the verbs at all.
    assert!(
        doctor.contains("rwv materialize --remove-undeclared-links"),
        "the finding must name the verb that fixes it:\n{doctor}"
    );
    assert!(
        !doctor.contains(DECLARED),
        "the still-declared name is not an orphan and must not be reported:\n{doctor}"
    );
}

/// P6, half one — the wire says this finding is not waiting on `--fix`.
///
/// **Asserted on the published field, not only on the outcome, and that is a
/// measured correction rather than belt-and-braces.** The outcome assertion
/// below was written first and a mutation flipping `safe_to_fix` to `true` left
/// it GREEN: doctor's repair arm re-runs surfacing, whose removal set is built
/// from declarations, so it would not remove an undeclared link even when told
/// the finding was fixable. Two independent things were holding the promise up
/// and the test could only see one of them, so the flip that breaks the
/// contract — an agent reading `--json` is told to wait for `--fix` — was
/// invisible. This half closes that: it reddens on the one-line flip.
#[test]
fn the_finding_is_published_as_not_auto_fixable() {
    let (_tmp, ws) = weave_with_dropped_links();

    let output = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("`--json` must emit parseable JSON");

    let issues = parsed["issues"]
        .as_array()
        .expect("`issues` must be an array");
    let found: Vec<&serde_json::Value> = issues
        .iter()
        .filter(|i| {
            i["message"]
                .as_str()
                .is_some_and(|m| m.contains("no longer declares"))
        })
        .collect();
    assert_eq!(
        found.len(),
        2,
        "both orphans must arrive on --json; a count that drifted means this \
         test is asserting about a set it did not expect:\n{parsed:#}"
    );
    for issue in found {
        assert_eq!(
            issue["safe_to_fix"], false,
            "an agent reading the report must be told this one is NOT waiting \
             on `--fix`, or it will wait forever:\n{issue:#}"
        );
        assert_eq!(issue["kind"], "surfacing");
    }
}

/// P6, half two — and the link really does survive `doctor --fix`.
///
/// The behavioural half. Kept alongside the wire assertion above because they
/// fail to different defects: this one catches a repair arm that grows a
/// removal, that one catches a finding that starts claiming to be repairable.
/// Neither implies the other — proven, not assumed: the mutation flipping
/// `safe_to_fix` reddens only the first, and a mutation adding the removal to
/// the repair arm would leave the first green.
#[test]
fn doctor_fix_does_not_remove_them() {
    let (_tmp, ws) = weave_with_dropped_links();

    let fix = rwv_output(&ws, &["doctor", "--fix"]);

    for name in [DROPPED, DROPPED_DANGLING] {
        assert!(
            link_exists(&ws, name),
            "`doctor --fix` removed `{name}`. It must not: rwv cannot tell its own \
             residue from a link made by hand at the same shape, so the choice is \
             the operator's.\noutput:\n{fix}"
        );
    }
    // And it is still reported afterwards — a finding that vanished without the
    // link going away would be the worse failure of the two.
    let after = rwv_output(&ws, &["doctor"]);
    assert!(
        after.contains(DROPPED),
        "the finding must still stand after --fix declined to act:\n{after}"
    );
}

/// P5 — the flag removes exactly the reported set, and the declared link and
/// the pointed-at files survive.
#[test]
fn the_named_flag_removes_exactly_the_reported_links() {
    let (_tmp, ws) = weave_with_dropped_links();

    let out = rwv_output(&ws, &["materialize", "--remove-undeclared-links"]);

    for name in [DROPPED, DROPPED_DANGLING] {
        assert!(
            !link_exists(&ws, name),
            "`--remove-undeclared-links` should have unlinked `{name}`:\n{out}"
        );
        assert!(
            out.contains(name),
            "each removal must be announced as it happens, so an operator who \
             never ran doctor still gets the list:\n{out}"
        );
    }
    assert!(
        link_exists(&ws, DECLARED),
        "the still-declared link must survive:\n{out}"
    );
    // Unlinking destroys no content. This is the whole reason the flag is
    // narrow enough to exist.
    assert!(
        ws.join("projects/alpha").join(DROPPED).is_file(),
        "the file the removed link pointed at must be untouched:\n{out}"
    );

    let after = rwv_output(&ws, &["doctor"]);
    for name in [DROPPED, DROPPED_DANGLING] {
        assert!(
            !after.contains(name),
            "the finding must clear once the link is gone:\n{after}"
        );
    }
}

/// Bare `materialize` — without the flag — changes nothing.
#[test]
fn materialize_without_the_flag_leaves_them() {
    let (_tmp, ws) = weave_with_dropped_links();

    let out = rwv_output(&ws, &["materialize"]);

    for name in [DROPPED, DROPPED_DANGLING] {
        assert!(
            link_exists(&ws, name),
            "bare `materialize` must not remove `{name}` — the consent is the flag:\n{out}"
        );
    }
}

// ---------------------------------------------------------------------------
// P7 — confinement, one case per conjunct of the predicate.
//
// Each case is built to satisfy every conjunct except the one it targets, so a
// case that survives proves that conjunct is load-bearing. Asserted against the
// sweep AND against the report: a link the sweep spares but doctor names would
// be a finding with no remedy.
// ---------------------------------------------------------------------------

/// Conjunct 1 — the entry is a symlink, read without following.
#[test]
fn a_real_file_at_an_undeclared_name_is_never_touched() {
    let (_tmp, ws) = weave_with_dropped_links();
    let victim = ws.join("hand-written.md");
    std::fs::write(&victim, "operator content\n").unwrap();

    let doctor = rwv_output(&ws, &["doctor"]);
    let out = rwv_output(&ws, &["materialize", "--remove-undeclared-links"]);

    assert_the_sweep_actually_ran(&ws, &out);
    assert!(
        victim.is_file(),
        "a REGULAR FILE at an undeclared name was removed. This is the one \
         outcome that destroys content:\n{out}"
    );
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "operator content\n",
        "and its bytes must be untouched"
    );
    assert!(
        !doctor.contains("hand-written.md"),
        "nor may it be reported:\n{doctor}"
    );
}

/// Conjunct 2 — the target has rwv's own surfacing shape.
#[test]
fn a_link_pointing_outside_the_surfacing_shape_is_never_touched() {
    let (_tmp, ws) = weave_with_dropped_links();
    std::fs::write(ws.join("elsewhere.txt"), "target\n").unwrap();
    // An operator's own link, at an undeclared name, resolving to something
    // that is not `projects/<p>/<same name>` — the shape `workweave.link`
    // entries and any hand-made link have.
    repoweave::symlink::create(
        Path::new("elsewhere.txt"),
        &ws.join("mine.md"),
        repoweave::symlink::LinkTarget::File,
    )
    .unwrap();

    let doctor = rwv_output(&ws, &["doctor"]);
    let out = rwv_output(&ws, &["materialize", "--remove-undeclared-links"]);

    assert_the_sweep_actually_ran(&ws, &out);
    assert!(
        link_exists(&ws, "mine.md"),
        "a link whose target is not rwv's surfacing shape must survive:\n{out}"
    );
    assert!(
        !doctor.contains("mine.md"),
        "nor may it be reported:\n{doctor}"
    );
}

/// Conjunct 2 again, at the sharpest edge the shape rule has: the target is
/// under `projects/` and names the right project, but a DIFFERENT file.
///
/// This is the case the owner-scoped tail comparison exists for. Without it the
/// predicate would be "points into the project", which is satisfied by a link
/// an operator made to some other file in the project dir.
#[test]
fn a_link_into_the_project_at_a_mismatched_tail_is_never_touched() {
    let (_tmp, ws) = weave_with_dropped_links();
    repoweave::symlink::create(
        Path::new("projects/alpha/SHARED.md"),
        &ws.join("alias.md"),
        repoweave::symlink::LinkTarget::File,
    )
    .unwrap();

    let doctor = rwv_output(&ws, &["doctor"]);
    let out = rwv_output(&ws, &["materialize", "--remove-undeclared-links"]);

    assert_the_sweep_actually_ran(&ws, &out);
    assert!(
        link_exists(&ws, "alias.md"),
        "`alias.md -> projects/alpha/SHARED.md` is not rwv's surfacing of \
         `alias.md`; only `projects/alpha/alias.md` would be:\n{out}"
    );
    assert!(
        !doctor.contains("alias.md"),
        "nor may it be reported:\n{doctor}"
    );
}

/// Conjunct 3 — it resolves into the project the root presents.
///
/// A link into ANOTHER project is the existing foreign-shared-name sweeper's
/// remit. The two must partition rather than overlap, so this one asserts the
/// new predicate does not claim it.
#[test]
fn a_link_into_another_project_is_left_to_the_existing_sweeper() {
    let (_tmp, ws) = weave_with_dropped_links();
    let other = ws.join("projects/beta");
    std::fs::create_dir_all(&other).unwrap();
    write_manifest(&other, &["BETA.md"]);
    std::fs::write(other.join("BETA.md"), "beta\n").unwrap();
    repoweave::symlink::create(
        Path::new("projects/beta/BETA.md"),
        &ws.join("BETA.md"),
        repoweave::symlink::LinkTarget::File,
    )
    .unwrap();

    let out = rwv_output(&ws, &["materialize", "--remove-undeclared-links"]);

    assert_the_sweep_actually_ran(&ws, &out);
    assert!(
        !out.contains("BETA.md"),
        "a link into another project is not this predicate's business; the \
         foreign-shared-name rule owns it:\n{out}"
    );
}

/// Conjunct 4 — the name is not declared.
///
/// The widening is exactly this conjunct, so a still-declared link surviving is
/// what says the sweep did not simply become "remove every surfacing link".
/// The other three cases would all pass against such a sweep.
#[test]
fn a_declared_name_is_never_swept() {
    let (_tmp, ws) = weave_with_dropped_links();

    let out = rwv_output(&ws, &["materialize", "--remove-undeclared-links"]);

    assert_the_sweep_actually_ran(&ws, &out);
    assert!(
        link_exists(&ws, DECLARED),
        "the declared link must survive the sweep:\n{out}"
    );
    assert_eq!(
        std::fs::read_link(ws.join(DECLARED)).unwrap(),
        Path::new(&format!("projects/alpha/{DECLARED}")),
        "and still resolve where it did"
    );
}

/// The name `vscode-workspace` declares for `alpha` while the fixture's
/// manifest carries it disabled.
const DISABLED_INTEGRATION_NAME: &str = "alpha.code-workspace";

/// The disabled-integration hold-out: a name a disabled integration declares
/// is exempt from this sweep even though nothing here ever authored the file
/// it would point at.
///
/// Minted by hand, at rwv's own surfacing shape, the way `DROPPED_DANGLING`
/// is above: the pass that actually retires a disabled integration's links
/// runs only inside `rwv materialize`, which this fixture never calls, so the
/// link stands the way an earlier `rwv materialize` — back when
/// vscode-workspace was still enabled — would have left it once the
/// integration was turned off afterward.
#[test]
fn a_disabled_integrations_declared_name_is_exempt() {
    let (_tmp, ws) = weave_with_dropped_links();
    repoweave::symlink::create(
        Path::new(&format!("projects/alpha/{DISABLED_INTEGRATION_NAME}")),
        &ws.join(DISABLED_INTEGRATION_NAME),
        repoweave::symlink::LinkTarget::File,
    )
    .unwrap();

    let doctor = rwv_output(&ws, &["doctor"]);

    assert!(
        !doctor.contains(DISABLED_INTEGRATION_NAME),
        "a name a disabled integration declares is not this sweep's business; \
         reporting it here would duplicate the disabled-integration pass's \
         own remedy under this one's different wording:\n{doctor}"
    );
    assert!(
        link_exists(&ws, DISABLED_INTEGRATION_NAME),
        "doctor must not have acted on it either way"
    );
}

/// Declared one directory down, then dropped the same way `DROPPED` is.
const NESTED_DROPPED: &str = "notes/NESTED.md";

/// Same shape as `weave_with_dropped_links`, except the dropped name sits a
/// directory down — the recursion into a subdirectory and the pruning of the
/// directory a removal empties are otherwise never driven, since every other
/// fixture in this file sits at the weave root.
fn weave_with_nested_dropped_link() -> (tempfile::TempDir, PathBuf) {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let project_dir = ws.join("projects/alpha");
    std::fs::create_dir_all(project_dir.join("notes")).unwrap();

    write_manifest(&project_dir, &[DECLARED, NESTED_DROPPED]);
    std::fs::write(project_dir.join(DECLARED), "shared\n").unwrap();
    std::fs::write(project_dir.join(NESTED_DROPPED), "nested\n").unwrap();
    git_init_with_commit(&project_dir);

    std::fs::write(ws.join(".rwv-active"), "alpha\n").unwrap();
    let out = rwv_output(&ws, &["activate", "alpha", "--no-materialize"]);
    for name in [DECLARED, NESTED_DROPPED] {
        assert!(
            ws.join(name).symlink_metadata().is_ok(),
            "fixture: activate should have surfaced `{name}`; output:\n{out}"
        );
    }

    write_manifest(&project_dir, &[DECLARED]);
    (tmp, ws)
}

/// One directory down: the walk must recurse to find the orphan, and
/// removing it must leave no empty directory behind.
///
/// The doctor half is a clean pin on recursion: `doctor` never repairs, so
/// nothing but the walk under test can be why the nested orphan is found.
/// The directory-survives half is not as sharp a pin on `unsurface_undeclared`
/// specifically: `rwv materialize` always runs its own surfacing repair right
/// after removing undeclared links, and that repair's directory walk prunes
/// any directory an activation left empty, on its own, regardless of this
/// sweep. Measured — reverting `unsurface_undeclared`'s own prune alone still
/// leaves this assertion green, because that repair's independent pruning
/// (`remove_activation_symlinks_in`) satisfies it. So this half pins the
/// end-to-end promise an operator sees ("removing the orphan leaves no
/// litter"), not a mutation exclusive to this file's own removal path.
#[test]
fn a_nested_dropped_link_is_found_and_its_emptied_directory_is_pruned() {
    let (_tmp, ws) = weave_with_nested_dropped_link();
    let nested_dir = ws.join("notes");

    let doctor = rwv_output(&ws, &["doctor"]);
    assert!(
        doctor.contains(NESTED_DROPPED),
        "doctor must find the orphan one directory down:\n{doctor}"
    );

    let out = rwv_output(&ws, &["materialize", "--remove-undeclared-links"]);
    assert!(
        !link_exists(&ws, NESTED_DROPPED),
        "the nested link should have been removed:\n{out}"
    );
    assert!(
        !nested_dir.exists(),
        "removing the only link `notes/` held should have pruned the \
         now-empty directory:\n{out}"
    );
}
