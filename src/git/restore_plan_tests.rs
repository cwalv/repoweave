//! A checkout's restore planned, then executed: the outcome is the one the
//! plan named, and a plan the checkout moved under writes nothing there.

use super::*;
use crate::op_state::SavepointKey;
use crate::vcs::{Attribution, Vcs, VerifiedRestoreOutcome};

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
        CheckoutRestorePlan::Reset { .. } => "Restored".to_owned(),
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
    expect: &str,
) -> VerifiedRestoreOutcome {
    let plan = GitVcs
        .plan_checkout_restore(p, &key(), attribution)
        .unwrap();
    assert_eq!(named(&plan), expect, "the plan: {plan:?}");
    let outcome = GitVcs.execute_checkout_restore(p, &key(), &plan).unwrap();
    assert_eq!(shape(&outcome), expect, "the outcome: {outcome:?}");
    outcome
}

#[test]
fn a_checkout_at_its_savepoint_is_untouched() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    planned_then_run(p.path(), &Attribution::default(), "Untouched");
}

#[test]
fn a_tip_the_savepoint_reaches_is_reset_keeping_nothing() {
    let p = repo();
    commit(p.path(), "ahead");
    let saved = GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["reset", "-q", "--hard", "HEAD~1"]);
    let outcome = planned_then_run(p.path(), &Attribution::default(), "Restored");
    assert_eq!(outcome, VerifiedRestoreOutcome::Restored);
    assert_eq!(&head(p.path()), saved.id());
}

#[test]
fn a_tip_another_branch_reaches_is_reset_keeping_nothing() {
    let p = repo();
    let saved = GitVcs.create_savepoint(p.path(), &key()).unwrap();
    commit(p.path(), "also on another branch");
    git(p.path(), &["branch", "other"]);
    let outcome = planned_then_run(p.path(), &Attribution::default(), "Restored");
    assert_eq!(outcome, VerifiedRestoreOutcome::Restored);
    assert_eq!(&head(p.path()), saved.id());
}

#[test]
fn a_tip_holding_a_commit_nothing_else_reaches_is_kept_in_the_branch_slot_whoever_made_it() {
    let p = repo();
    let saved = GitVcs.create_savepoint(p.path(), &key()).unwrap();
    let tip = commit(p.path(), "resolved by hand");
    let outcome = planned_then_run(p.path(), &Attribution::default(), "Restored");
    let VerifiedRestoreOutcome::Kept { kept, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    let [slot] = kept.as_slice() else {
        panic!("{outcome:?}");
    };
    assert_eq!(slot.backup().name.as_str(), "main/aborted");
    assert_eq!(slot.backup().tip.id(), &tip);
    assert_eq!(&head(p.path()), saved.id());
}

#[test]
fn a_second_restore_overwrites_the_branch_slot() {
    let p = repo();
    let saved = GitVcs.create_savepoint(p.path(), &key()).unwrap();
    commit(p.path(), "first");
    planned_then_run(p.path(), &Attribution::default(), "Restored");
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    let second = commit(p.path(), "second");
    planned_then_run(p.path(), &Attribution::default(), "Restored");
    let listed = GitVcs
        .list_backups(p.path(), RemoteTracking::Counted)
        .unwrap();
    let slots: Vec<_> = listed
        .iter()
        .map(|b| (b.backup.name.as_str().to_owned(), b.backup.tip.id().clone()))
        .collect();
    assert_eq!(slots, vec![("main/aborted".to_owned(), second)]);
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
        "Canceled(Untouched)",
    );
    assert_eq!(GitVcs.mid_op(p.path()), None);
    assert_eq!(&head(p.path()), saved.id());
}

#[test]
fn a_merge_the_op_never_started_on_a_tip_it_never_wrote_is_canceled_and_its_tip_kept() {
    let p = repo();
    let saved = GitVcs.create_savepoint(p.path(), &key()).unwrap();
    stopped_merging(p.path());
    let at = head(p.path());
    merge_conflicting(p.path());
    let outcome = planned_then_run(
        p.path(),
        &Attribution {
            ..Attribution::default()
        },
        "Canceled(Restored)",
    );
    assert_eq!(GitVcs.mid_op(p.path()), None);
    assert_eq!(&head(p.path()), saved.id());
    let kept = GitVcs
        .list_backups(p.path(), RemoteTracking::Counted)
        .unwrap();
    assert!(kept.iter().any(|b| b.backup.tip.id() == &at), "{outcome:?}");
}

#[test]
fn a_checkout_switched_off_its_branch_is_switched_back() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "-c", "other"]);
    planned_then_run(p.path(), &Attribution::default(), "SwitchedBack(Untouched)");
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn a_detached_checkout_is_put_back_on_its_branch() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "--detach"]);
    planned_then_run(p.path(), &Attribution::default(), "SwitchedBack(Untouched)");
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn edits_are_kept_before_the_reset() {
    let p = repo();
    let saved = GitVcs.create_savepoint(p.path(), &key()).unwrap();
    commit(p.path(), "op");
    std::fs::write(p.path().join("f"), "edited").unwrap();
    let outcome = planned_then_run(p.path(), &Attribution::default(), "Restored");
    assert!(
        matches!(outcome, VerifiedRestoreOutcome::Kept { .. }),
        "{outcome:?}"
    );
    assert_eq!(&head(p.path()), saved.id());
}

#[test]
fn a_submodule_left_dirty_elsewhere_does_not_block_the_reset_that_never_touches_it() {
    let sub = repo();
    let p = repo();
    git(
        p.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.path().to_str().unwrap(),
            "sub",
        ],
    );
    git(p.path(), &["commit", "-qm", "sub"]);
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    commit(p.path(), "op");
    std::fs::write(p.path().join("sub/f"), "dirt in the submodule").unwrap();
    planned_then_run(p.path(), &Attribution::default(), "Restored");
    assert_eq!(
        std::fs::read_to_string(p.path().join("sub/f")).unwrap(),
        "dirt in the submodule"
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
    planned_then_run(p.path(), &Attribution::default(), "CannotSwitchBack");
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "other");
}

#[test]
fn a_branch_gone_blocks_the_switch_back() {
    let p = repo();
    git(p.path(), &["switch", "-q", "-c", "topic"]);
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "main"]);
    git(p.path(), &["branch", "-q", "-d", "topic"]);
    planned_then_run(p.path(), &Attribution::default(), "CannotSwitchBack");
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn a_plan_the_checkout_moved_under_writes_nothing_and_reports_what_it_holds() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    commit(p.path(), "op");
    let plan = GitVcs
        .plan_checkout_restore(p.path(), &key(), &Attribution::default())
        .unwrap();
    let moved = commit(p.path(), "meanwhile");

    let outcome = GitVcs
        .execute_checkout_restore(p.path(), &key(), &plan)
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
    let plan = GitVcs.plan_ref_restore(p.path(), &saved, &[]).unwrap();
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
    let predicted = GitVcs::edits_block_switch(p, to).unwrap().is_some();
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
    assert_eq!(
        GitVcs::edits_block_switch(p, &main).unwrap(),
        Some(InTheWay::Unresolved)
    );
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
    assert_eq!(
        GitVcs::edits_block_switch(p, &base).unwrap(),
        Some(InTheWay::Edits)
    );
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
    assert_eq!(
        GitVcs::edits_block_switch(p, &to).unwrap(),
        Some(InTheWay::Ignored)
    );
}

fn set_aside_as_hinted(p: &Path, in_the_way: InTheWay) {
    let action = match in_the_way {
        InTheWay::Edits => VcsAction::StashUntracked,
        InTheWay::Ignored => VcsAction::StashIgnored,
        InTheWay::Unresolved => panic!("no stash sets aside an unresolved index"),
    };
    let line = GitVcs
        .hint(action)
        .replace("<checkout>", &p.display().to_string());
    let words: Vec<&str> = line.split_whitespace().skip(1).collect();
    git(p, &words);
}

#[test]
fn the_stash_each_obstacle_prints_clears_it() {
    let tmp = repo();
    let p = tmp.path();
    let base = head(p);
    let to = commit(p, "u");
    git(p, &["switch", "-q", "-c", "other", base.as_str()]);
    std::fs::write(p.join("u"), "untracked").unwrap();
    let in_the_way = GitVcs::edits_block_switch(p, &to).unwrap().unwrap();
    set_aside_as_hinted(p, in_the_way);
    assert_eq!(GitVcs::edits_block_switch(p, &to).unwrap(), None);
    std::fs::write(p.join(".git/info/exclude"), "u\n").unwrap();
    std::fs::write(p.join("u"), "ignored").unwrap();
    let in_the_way = GitVcs::edits_block_switch(p, &to).unwrap().unwrap();
    set_aside_as_hinted(p, in_the_way);
    assert_eq!(GitVcs::edits_block_switch(p, &to).unwrap(), None);
    assert_eq!(
        GitVcs::hard_move_discards(p, &ResolvedRevisionId::from(to.clone())).unwrap(),
        None
    );
}

#[test]
fn a_switch_back_here_keeps_the_ignored_file_it_overwrites() {
    let tmp = repo();
    let p = tmp.path();
    let base = head(p);
    commit(p, "u");
    GitVcs.create_savepoint(p, &key()).unwrap();
    git(p, &["switch", "-q", "-c", "other", base.as_str()]);
    std::fs::write(p.join(".git/info/exclude"), "u\n").unwrap();
    std::fs::write(p.join("u"), "mine").unwrap();
    let attribution = Attribution {
        ..Attribution::default()
    };
    let plan = GitVcs
        .plan_checkout_restore(p, &key(), &attribution)
        .unwrap();
    let outcome = GitVcs.execute_checkout_restore(p, &key(), &plan).unwrap();
    assert_eq!(git(p, &["branch", "--show-current"]), "main", "{outcome:?}");
    assert!(
        !git(p, &["log", "--all", "--format=%H", "-Smine"]).is_empty(),
        "the ignored file's content is kept as a backup: {outcome:?}"
    );
}

#[test]
fn a_held_index_lock_is_named_by_the_plan_not_read_as_edits() {
    let tmp = repo();
    let p = tmp.path();
    let base = head(p);
    let to = commit(p, "u");
    git(p, &["switch", "-q", "-c", "other", base.as_str()]);
    std::fs::write(p.join(".git/index.lock"), "").unwrap();
    let err = GitVcs::edits_block_switch(p, &to).unwrap_err();
    assert!(format!("{err:#}").contains("index.lock"), "{err:#}");
}

fn savepoint_survives(p: &Path) -> bool {
    GitVcs.resolve_savepoint(p, &key()).is_some()
}

#[test]
fn a_leave_stopped_plan_whose_merge_was_given_up_leaves_nothing_to_report() {
    let p = repo();
    let side = stopped_merging(p.path());
    merge_conflicting(p.path());
    let plan = GitVcs
        .plan_checkout_restore(p.path(), &key(), &Attribution::default())
        .unwrap();
    assert_eq!(named(&plan), "LeftStopped");
    git(p.path(), &["merge", "--abort"]);
    let outcome = GitVcs
        .execute_checkout_restore(p.path(), &key(), &plan)
        .unwrap();
    assert_eq!(shape(&outcome), "NoSavepoint", "{outcome:?}");
    assert_eq!(GitVcs.mid_op(p.path()), None);
    assert_ne!(head(p.path()), side);
}

#[test]
fn an_untouched_plan_the_checkout_moved_under_keeps_its_savepoint() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    let plan = GitVcs
        .plan_checkout_restore(p.path(), &key(), &Attribution::default())
        .unwrap();
    assert_eq!(named(&plan), "Untouched");
    let moved = commit(p.path(), "meanwhile");
    let outcome = GitVcs
        .execute_checkout_restore(p.path(), &key(), &plan)
        .unwrap();
    assert_eq!(shape(&outcome), "ForeignTip", "{outcome:?}");
    assert_eq!(head(p.path()), moved);
    assert!(savepoint_survives(p.path()));
}

#[test]
fn a_cancel_plan_whose_merge_was_given_up_cancels_nothing_and_keeps_its_savepoint() {
    let p = repo();
    let side = stopped_merging(p.path());
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    merge_conflicting(p.path());
    let steps = [OwnStep::Merge { source: side }];
    let attribution = Attribution {
        integrates: Integrates::Yes,
        own_steps: &steps,
        ..Attribution::default()
    };
    let plan = GitVcs
        .plan_checkout_restore(p.path(), &key(), &attribution)
        .unwrap();
    assert_eq!(named(&plan), "Canceled(Untouched)");
    git(p.path(), &["merge", "--abort"]);
    let outcome = GitVcs
        .execute_checkout_restore(p.path(), &key(), &plan)
        .unwrap();
    assert_eq!(shape(&outcome), "ForeignTip", "{outcome:?}");
    assert!(savepoint_survives(p.path()));
}

#[test]
fn a_switch_back_plan_the_checkout_moved_under_switches_nothing() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "-c", "other"]);
    let plan = GitVcs
        .plan_checkout_restore(p.path(), &key(), &Attribution::default())
        .unwrap();
    assert_eq!(named(&plan), "SwitchedBack(Untouched)");
    git(p.path(), &["switch", "-q", "-c", "third"]);
    let outcome = GitVcs
        .execute_checkout_restore(p.path(), &key(), &plan)
        .unwrap();
    assert_eq!(shape(&outcome), "ForeignTip", "{outcome:?}");
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "third");
    assert!(savepoint_survives(p.path()));
}

fn ref_at_a_tip_the_op_wrote(p: &Path) -> (RefSavepoint, CommitId) {
    let at = head(p);
    git(p, &["branch", "topic"]);
    git(p, &["switch", "-q", "-c", "work"]);
    let wrote = commit(p, "op");
    git(p, &["branch", "-qf", "topic", wrote.as_str()]);
    let saved = RefSavepoint {
        branch: RawRefName::new("topic"),
        at,
    };
    GitVcs
        .create_ref_savepoint(p, &key(), &saved.branch, &saved.at)
        .unwrap();
    (saved, wrote)
}

#[test]
fn a_ref_untouched_plan_the_branch_moved_under_keeps_its_savepoint() {
    let p = repo();
    let at = head(p.path());
    git(p.path(), &["branch", "topic"]);
    let saved = RefSavepoint {
        branch: RawRefName::new("topic"),
        at,
    };
    GitVcs
        .create_ref_savepoint(p.path(), &key(), &saved.branch, &saved.at)
        .unwrap();
    let plan = GitVcs.plan_ref_restore(p.path(), &saved, &[]).unwrap();
    assert!(matches!(plan, RefRestorePlan::Untouched), "{plan:?}");
    git(p.path(), &["switch", "-q", "topic"]);
    let moved = commit(p.path(), "meanwhile");
    git(p.path(), &["switch", "-q", "main"]);

    let outcome = GitVcs
        .execute_ref_restore(p.path(), &key(), &saved, &plan)
        .unwrap();

    assert!(
        matches!(&outcome, crate::vcs::RefRestoreOutcome::Foreign { observed: Some(o), .. } if *o == moved),
        "{outcome:?}"
    );
    assert_eq!(git(p.path(), &["rev-parse", "topic"]), moved.as_str());
    assert_eq!(GitVcs.ref_savepoints(p.path(), &key()).unwrap().len(), 1);
}

#[test]
fn a_ref_restore_plan_the_branch_was_checked_out_under_writes_nothing() {
    let p = repo();
    let (saved, wrote) = ref_at_a_tip_the_op_wrote(p.path());
    let plan = GitVcs.plan_ref_restore(p.path(), &saved, &[]).unwrap();
    assert!(matches!(plan, RefRestorePlan::Restore { .. }), "{plan:?}");
    git(p.path(), &["switch", "-q", "topic"]);

    let outcome = GitVcs
        .execute_ref_restore(p.path(), &key(), &saved, &plan)
        .unwrap();

    assert!(
        matches!(&outcome, crate::vcs::RefRestoreOutcome::Foreign { why, .. } if why.contains("checked out")),
        "{outcome:?}"
    );
    assert_eq!(git(p.path(), &["rev-parse", "topic"]), wrote.as_str());
    assert_eq!(GitVcs.ref_savepoints(p.path(), &key()).unwrap().len(), 1);
}

#[test]
fn a_branch_held_by_a_checkout_the_plan_switches_off_does_not_block_its_restore() {
    let p = repo();
    let (saved, _) = ref_at_a_tip_the_op_wrote(p.path());
    git(p.path(), &["switch", "-q", "topic"]);
    let held = vec![p.path().to_path_buf()];
    let blocked = GitVcs.plan_ref_restore(p.path(), &saved, &[]).unwrap();
    assert!(
        matches!(blocked, RefRestorePlan::Blocked { .. }),
        "{blocked:?}"
    );
    let freed = GitVcs.plan_ref_restore(p.path(), &saved, &held).unwrap();
    assert!(matches!(freed, RefRestorePlan::Restore { .. }), "{freed:?}");
}

#[test]
fn edits_that_collide_with_the_switch_back_are_kept_and_it_runs() {
    let p = repo();
    let base = head(p.path());
    std::fs::write(p.path().join("f"), "2").unwrap();
    git(p.path(), &["commit", "-qam", "op"]);
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "-c", "other", base.as_str()]);
    std::fs::write(p.path().join("f"), "mine").unwrap();
    let plan = GitVcs
        .plan_checkout_restore(p.path(), &key(), &Attribution::default())
        .unwrap();
    let outcome = GitVcs
        .execute_checkout_restore(p.path(), &key(), &plan)
        .unwrap();
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "main");
    assert!(
        !git(p.path(), &["log", "--all", "--format=%H", "-Smine"]).is_empty(),
        "the edit is kept as a backup: {outcome:?}"
    );
}

#[test]
fn a_switch_back_from_a_detached_head_keeps_its_commits() {
    let p = repo();
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "--detach"]);
    let stranded = commit(p.path(), "stranded");
    let plan = GitVcs
        .plan_checkout_restore(p.path(), &key(), &Attribution::default())
        .unwrap();
    let outcome = GitVcs
        .execute_checkout_restore(p.path(), &key(), &plan)
        .unwrap();
    assert_eq!(git(p.path(), &["branch", "--show-current"]), "main");
    let reached = git(
        p.path(),
        &[
            "for-each-ref",
            "--contains",
            stranded.as_str(),
            "--format=%(refname)",
        ],
    );
    assert!(
        !reached.is_empty(),
        "the commit stays on a ref: {outcome:?}"
    );
}

#[test]
fn an_untracked_file_in_a_cancels_way_is_kept_before_it_is_cleared() {
    let p = repo();
    git(p.path(), &["switch", "-q", "-c", "topic"]);
    std::fs::write(p.path().join("f"), "topic").unwrap();
    git(p.path(), &["commit", "-qam", "topic"]);
    commit(p.path(), "g");
    GitVcs.create_savepoint(p.path(), &key()).unwrap();
    git(p.path(), &["switch", "-q", "main"]);
    std::fs::write(p.path().join("f"), "main").unwrap();
    git(p.path(), &["commit", "-qam", "main"]);
    git(p.path(), &["switch", "-q", "topic"]);
    let rebase = git_command()
        .args(["rebase", "main"])
        .current_dir(p.path())
        .output()
        .unwrap();
    assert!(!rebase.status.success(), "the rebase was meant to stop");
    std::fs::write(p.path().join("g"), "mine").unwrap();
    std::fs::write(p.path().join("f"), "resolving").unwrap();

    let plan = GitVcs
        .plan_checkout_restore(
            p.path(),
            &key(),
            &Attribution {
                ..Attribution::default()
            },
        )
        .unwrap();
    assert!(
        matches!(&plan, CheckoutRestorePlan::Cancel { clear_first: Some(paths), .. }
            if paths.iter().any(|path| path == "g")),
        "{plan:?}"
    );
    let outcome = GitVcs
        .execute_checkout_restore(p.path(), &key(), &plan)
        .unwrap();

    assert_eq!(GitVcs.mid_op(p.path()), None, "{outcome:?}");
    let VerifiedRestoreOutcome::Kept { kept, .. } = &outcome else {
        panic!("expected the working copy kept, got {outcome:?}");
    };
    let Some(edits) = kept
        .iter()
        .find(|k| k.backup().mark == Some(BackupMark::AbortedWorkingCopy))
    else {
        panic!("expected the working copy kept, got {outcome:?}");
    };
    let tip = edits.backup().tip.id().as_str().to_owned();
    assert_eq!(git(p.path(), &["show", &format!("{tip}:g")]), "mine");
    assert_eq!(git(p.path(), &["show", &format!("{tip}:f")]), "resolving");
}
