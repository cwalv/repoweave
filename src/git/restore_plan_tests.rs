//! A checkout's restore planned, then executed: the outcome is the one the
//! plan named, and a plan the checkout moved under writes nothing there.

use super::*;
use crate::op_state::SavepointKey;
use crate::vcs::{Attribution, ForeignTipPolicy, Vcs, VerifiedRestoreOutcome};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn repo() -> tempfile::TempDir {
    let tmp = crate::tempdir().unwrap();
    let p = tmp.path();
    git(p, &["init", "-q", "-b", "main"]);
    git(p, &["config", "user.email", "t@t"]);
    git(p, &["config", "user.name", "T"]);
    std::fs::write(p.join("f"), "1").unwrap();
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "base"]);
    tmp
}

fn commit(p: &Path, name: &str) -> CommitId {
    std::fs::write(p.join(name), name).unwrap();
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", name]);
    head(p)
}

fn head(p: &Path) -> CommitId {
    GitVcs.head_revision(p).unwrap().id().clone()
}

fn key() -> SavepointKey {
    SavepointKey::Op("1779769917405921588".parse().unwrap())
}

fn named(plan: &CheckoutRestorePlan) -> String {
    match plan {
        CheckoutRestorePlan::NoSavepoint => "NoSavepoint".to_owned(),
        CheckoutRestorePlan::LeaveStopped { .. } => "LeftStopped".to_owned(),
        CheckoutRestorePlan::Untouched { .. } => "Untouched".to_owned(),
        CheckoutRestorePlan::Reset { why, .. } => match why {
            RestoredFrom::Intent => "RestoredFromIntent",
            RestoredFrom::Converged => "RestoredFromConverged",
            RestoredFrom::Abandoned => "AbandonedForeignTip",
        }
        .to_owned(),
        CheckoutRestorePlan::Cancel { then, .. } => format!("Canceled({})", named(then)),
        CheckoutRestorePlan::SwitchBack { then, .. } => format!("SwitchedBack({})", named(then)),
        CheckoutRestorePlan::Blocked(outcome) => shape(outcome),
    }
}

fn shape(outcome: &VerifiedRestoreOutcome) -> String {
    match outcome {
        VerifiedRestoreOutcome::Kept { then, .. } => shape(then),
        VerifiedRestoreOutcome::Canceled { then, .. } => format!("Canceled({})", shape(then)),
        VerifiedRestoreOutcome::SwitchedBack { then, .. } => {
            format!("SwitchedBack({})", shape(then))
        }
        other => format!("{other:?}")
            .split([' ', '{', '('])
            .next()
            .unwrap()
            .to_owned(),
    }
}

/// Plan, then execute, and require the outcome the plan named; returns it.
fn planned_then_run(
    p: &Path,
    attribution: &Attribution<'_>,
    policy: ForeignTipPolicy,
    expect: &str,
) -> VerifiedRestoreOutcome {
    let plan = GitVcs
        .plan_checkout_restore(p, &key(), attribution, policy)
        .unwrap();
    assert_eq!(named(&plan), expect, "the plan: {plan:?}");
    let outcome = GitVcs
        .execute_checkout_restore(p, &key(), &plan, attribution.edits_here)
        .unwrap();
    assert_eq!(shape(&outcome), expect, "the outcome: {outcome:?}");
    outcome
}

#[test]
fn a_checkout_at_its_savepoint_is_untouched() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    planned_then_run(
        p.path(),
        &Attribution::default(),
        ForeignTipPolicy::Refuse,
        "Untouched",
    );
}

#[test]
fn a_tip_the_op_wrote_is_reset_to_the_savepoint() {
    let p = repo();
    let saved = GitVcs.create_savepoint(p.path(), &key()).unwrap();
    let wrote = commit(p.path(), "op");
    planned_then_run(
        p.path(),
        &Attribution {
            intent: Some(&wrote),
            ..Attribution::default()
        },
        ForeignTipPolicy::Refuse,
        "RestoredFromIntent",
    );
    assert_eq!(&head(p.path()), saved.id());
}

/// A merge stopped on a conflict: `side` and `main` both change `f`.
fn stopped_merging(p: &Path) -> CommitId {
    git(p, &["switch", "-q", "-c", "side"]);
    std::fs::write(p.join("f"), "side").unwrap();
    git(p, &["commit", "-qam", "side"]);
    let side = head(p);
    git(p, &["switch", "-q", "main"]);
    std::fs::write(p.join("f"), "main").unwrap();
    git(p, &["commit", "-qam", "main"]);
    side
}

fn merge_conflicting(p: &Path) {
    let out = git_command()
        .args(["merge", "side"])
        .current_dir(p)
        .output()
        .unwrap();
    assert!(!out.status.success(), "the merge was meant to conflict");
}

#[test]
fn a_merge_the_op_stopped_in_is_canceled() {
    let p = repo();
    let side = stopped_merging(p.path());
    let saved = GitVcs.create_savepoint(p.path(), &key()).unwrap();
    merge_conflicting(p.path());
    let steps = [OwnStep::Merge { source: side }];
    planned_then_run(
        p.path(),
        &Attribution {
            integrates: Integrates::Yes,
            own_steps: &steps,
            ..Attribution::default()
        },
        ForeignTipPolicy::Refuse,
        "Canceled(Untouched)",
    );
    assert_eq!(GitVcs.mid_op(p.path()), None);
    assert_eq!(&head(p.path()), saved.id());
}

#[test]
fn a_merge_the_op_never_started_on_a_tip_it_never_wrote_blocks() {
    let p = repo();
    let saved = GitVcs.create_savepoint(p.path(), &key()).unwrap();
    stopped_merging(p.path());
    let at = head(p.path());
    merge_conflicting(p.path());
    planned_then_run(
        p.path(),
        &Attribution::default(),
        ForeignTipPolicy::Refuse,
        "StoppedForeign",
    );
    assert_eq!(GitVcs.mid_op(p.path()), Some(ConflictOp::Merge));
    assert_eq!(head(p.path()), at);
    assert_ne!(&at, saved.id());
}

#[test]
fn a_checkout_switched_off_its_branch_is_switched_back() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "-c", "other"]);
    planned_then_run(
        p.path(),
        &Attribution::default(),
        ForeignTipPolicy::Refuse,
        "SwitchedBack(Untouched)",
    );
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn a_detached_checkout_is_put_back_on_its_branch() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "--detach"]);
    planned_then_run(
        p.path(),
        &Attribution::default(),
        ForeignTipPolicy::Refuse,
        "SwitchedBack(Untouched)",
    );
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn edits_here_are_kept_before_the_reset() {
    let p = repo();
    let saved = GitVcs.create_savepoint(p.path(), &key()).unwrap();
    let wrote = commit(p.path(), "op");
    std::fs::write(p.path().join("f"), "edited").unwrap();
    let outcome = planned_then_run(
        p.path(),
        &Attribution {
            intent: Some(&wrote),
            edits_here: true,
            ..Attribution::default()
        },
        ForeignTipPolicy::Refuse,
        "RestoredFromIntent",
    );
    assert!(
        matches!(outcome, VerifiedRestoreOutcome::Kept { .. }),
        "{outcome:?}"
    );
    assert_eq!(&head(p.path()), saved.id());
}

#[test]
fn edits_elsewhere_block_and_are_left() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    let wrote = commit(p.path(), "op");
    std::fs::write(p.path().join("f"), "edited").unwrap();
    planned_then_run(
        p.path(),
        &Attribution {
            intent: Some(&wrote),
            ..Attribution::default()
        },
        ForeignTipPolicy::Refuse,
        "EditsElsewhere",
    );
    assert_eq!(head(p.path()), wrote);
    assert_eq!(
        std::fs::read_to_string(p.path().join("f")).unwrap(),
        "edited"
    );
}

#[test]
fn a_branch_held_elsewhere_blocks_the_switch_back() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "-c", "other"]);
    let wt = crate::tempdir().unwrap();
    let wt = wt.path().join("wt");
    git(
        p.path(),
        &["worktree", "add", "-q", wt.to_str().unwrap(), "main"],
    );
    planned_then_run(
        p.path(),
        &Attribution::default(),
        ForeignTipPolicy::Refuse,
        "CannotSwitchBack",
    );
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "other");
}

#[test]
fn a_branch_gone_blocks_the_switch_back() {
    let p = repo();
    git(p.path(), &["switch", "-q", "-c", "topic"]);
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "main"]);
    git(p.path(), &["branch", "-q", "-d", "topic"]);
    planned_then_run(
        p.path(),
        &Attribution::default(),
        ForeignTipPolicy::Refuse,
        "CannotSwitchBack",
    );
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn a_plan_the_checkout_moved_under_writes_nothing_and_reports_what_it_holds() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    let wrote = commit(p.path(), "op");
    let attribution = Attribution {
        intent: Some(&wrote),
        ..Attribution::default()
    };
    let plan = GitVcs
        .plan_checkout_restore(p.path(), &key(), &attribution, ForeignTipPolicy::Refuse)
        .unwrap();
    let moved = commit(p.path(), "meanwhile");

    let outcome = GitVcs
        .execute_checkout_restore(p.path(), &key(), &plan, false)
        .unwrap();

    let VerifiedRestoreOutcome::ForeignTip { observed_tip, .. } = outcome else {
        panic!("expected the moved checkout reported, got {outcome:?}");
    };
    assert_eq!(observed_tip, moved);
    assert_eq!(head(p.path()), moved);
    assert!(GitVcs.resolve_savepoint(p.path(), &key()).is_some());
}

#[test]
fn a_ref_plan_the_branch_moved_under_writes_nothing() {
    let p = repo();
    let at = head(p.path());
    git(p.path(), &["branch", "topic"]);
    git(p.path(), &["switch", "-q", "-c", "work"]);
    let wrote = commit(p.path(), "op");
    git(p.path(), &["branch", "-qf", "topic", wrote.as_str()]);
    let saved = RefSavepoint {
        branch: RawRefName::new("topic"),
        at,
    };
    let plan = GitVcs
        .plan_ref_restore(
            p.path(),
            &saved,
            Some(&wrote),
            ForeignTipPolicy::Refuse,
            &[],
        )
        .unwrap();
    assert!(matches!(plan, RefRestorePlan::Restore { .. }), "{plan:?}");
    let moved = commit(p.path(), "meanwhile");
    git(p.path(), &["branch", "-qf", "topic", moved.as_str()]);

    let outcome = GitVcs
        .execute_ref_restore(p.path(), &key(), &saved, &plan)
        .unwrap();

    assert!(
        matches!(&outcome, crate::vcs::RefRestoreOutcome::Foreign { observed: Some(o), .. } if *o == moved),
        "{outcome:?}"
    );
    assert_eq!(git(p.path(), &["rev-parse", "topic"]), moved.as_str());
}

fn switch_refused(p: &Path, to: &str) -> bool {
    let refused = !git_command()
        .args(["switch", "-q", "--detach", to])
        .current_dir(p)
        .status()
        .unwrap()
        .success();
    if !refused {
        git(p, &["switch", "-q", "-"]);
    }
    refused
}

fn agrees_with_git(p: &Path, to: &CommitId) {
    let predicted = GitVcs::edits_block_switch(p, to).unwrap();
    assert_eq!(predicted, switch_refused(p, to.as_str()));
}

#[test]
fn an_unresolved_index_blocks_a_switch_as_git_does() {
    let tmp = repo();
    let p = tmp.path();
    git(p, &["switch", "-qc", "side"]);
    std::fs::write(p.join("f"), "side").unwrap();
    git(p, &["commit", "-qam", "side"]);
    git(p, &["switch", "-q", "main"]);
    std::fs::write(p.join("f"), "main").unwrap();
    git(p, &["commit", "-qam", "main"]);
    let main = head(p);
    git(p, &["switch", "-q", "side"]);
    let _ = git_command()
        .args(["rebase", "main"])
        .current_dir(p)
        .output()
        .unwrap();
    git(p, &["rebase", "--quit"]);
    assert!(GitVcs::edits_block_switch(p, &main).unwrap());
    agrees_with_git(p, &main);
}

#[test]
fn a_renamed_file_with_edits_blocks_a_switch_as_git_does() {
    let tmp = repo();
    let p = tmp.path();
    std::fs::write(p.join("f"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    git(p, &["commit", "-qam", "f long"]);
    let base = head(p);
    git(p, &["mv", "f", "g"]);
    git(p, &["commit", "-qm", "f renamed g"]);
    std::fs::write(p.join("g"), "one\ntwo\nthree\nfour\nfive\nsix\n").unwrap();
    assert!(GitVcs::edits_block_switch(p, &base).unwrap());
    agrees_with_git(p, &base);
}

#[test]
fn an_ignored_file_a_switch_would_overwrite_blocks_it() {
    let tmp = repo();
    let p = tmp.path();
    let base = head(p);
    let to = commit(p, "u");
    git(p, &["switch", "-q", "--detach", base.as_str()]);
    std::fs::write(p.join(".git/info/exclude"), "u\n").unwrap();
    std::fs::write(p.join("u"), "mine").unwrap();
    assert!(GitVcs::edits_block_switch(p, &to).unwrap());
}
