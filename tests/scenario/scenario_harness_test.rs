//! Branching scenarios run step by step in hermetic sandbox weaves, with
//! invariants checked after every step.
//!
//! A scenario is a sequence of plain git commands, rwv verbs, and what a forge
//! or colleague does to a remote. Each step carries its expected outcome as
//! data; a step left undecided has only its invariants held, and a green run
//! announces it on stderr, as it does a step meeting a known defect. A step
//! that needs a verb rwv does not have is recorded as pending and ends the
//! scenario there, announced on stderr.
//!
//! The invariants and what they cannot see are stated in
//! `tests/scenario/scenario_harness_test/invariants.rs`; the sandbox's fixed shape in
//! `tests/scenario/scenario_harness_test/sandbox.rs`. A test that holds rwv at a pause
//! point runs only in a build with the `test-pause` feature, and announces
//! its skip in any other.

use crate::common;

#[path = "scenario_harness_test/invariants.rs"]
mod invariants;
#[path = "scenario_harness_test/runner.rs"]
mod runner;
#[path = "scenario_harness_test/sandbox.rs"]
mod sandbox;
#[path = "scenario_harness_test/scenarios.rs"]
mod scenarios;

use invariants::{
    ChangeKind, Declaration, Invariant, LockSite, Mode, Record, RefChange, RemoteWrite, Snapshot,
    Violation,
};
use repoweave::diagnosis::State;
use repoweave::manifest::Role;
use repoweave::refusal::RefusalKind;
use runner::{Claim, Exit, Forge, MissingVerb, Outcome, Report, Scenario, Step};
use sandbox::{Machine, Place, Remote, RemoteKind, Repo, Sandbox, Seed, Store};

fn fired(report: &Report) -> Vec<(&str, Invariant)> {
    report
        .violations
        .iter()
        .map(|(label, v)| (label.as_str(), v.invariant()))
        .collect()
}

fn bad_steps(name: &'static str, steps: Vec<Step>) -> Report {
    runner::run(Scenario {
        name,
        seed: Seed::default(),
        steps,
    })
}

#[test]
fn a_seat_with_no_work_on_a_member_follows_its_new_declared_branch_after_landing() {
    runner::run(scenarios::seat_follows_after_landing()).assert_fully_decided_green();
}

#[test]
fn a_landing_lists_what_a_declared_branch_change_leaves_behind_and_status_keeps_it() {
    runner::run(scenarios::member_change_race_listing()).assert_fully_decided_green();
}

#[test]
fn a_diverged_main_left_by_a_member_change_publishes_through_a_workspace_on_it() {
    runner::run(scenarios::main_diverged_after_member_change()).assert_fully_decided_green();
}

#[test]
fn stray_commits_copied_onto_their_branch_checked_out_elsewhere_clear_the_state() {
    runner::run(scenarios::stray_commits_copied_onto_the_checked_out_branch())
        .assert_fully_decided_green();
}

#[test]
fn a_diverged_main_left_by_a_member_change_resets_to_its_upstream_keeping_a_backup() {
    runner::run(scenarios::main_diverged_after_member_change_reset()).assert_fully_decided_green();
}

#[test]
fn a_diverged_main_resolves_a_derived_path_to_its_upstream() {
    runner::run(scenarios::main_diverged_resolves_derived_to_the_upstream())
        .assert_fully_decided_green();
}

#[test]
fn a_landing_stops_at_alignment_where_the_landed_lock_does_not_read_and_continues() {
    runner::run(scenarios::landing_stops_where_the_landed_lock_does_not_read())
        .assert_fully_decided_green();
}

#[test]
fn the_printed_reset_of_a_diverged_main_leaves_the_expected_branch_and_its_pin() {
    runner::run(scenarios::main_diverged_reset_leaves_the_expected_branch())
        .assert_fully_decided_green();
}

#[test]
fn a_diverged_main_still_stops_on_an_undeclared_path_both_sides_changed() {
    runner::run(scenarios::main_diverged_still_stops_on_an_undeclared_path())
        .assert_fully_decided_green();
}

#[test]
fn a_diverged_main_whose_merge_conflicts_and_whose_commits_the_declared_branch_holds_is_reset() {
    runner::run(scenarios::main_diverged_conflicting_held()).assert_fully_decided_green();
}

#[test]
fn a_diverged_main_whose_merge_conflicts_is_copied_onto_the_declared_branch_then_reset() {
    runner::run(scenarios::main_diverged_conflicting_copied()).assert_fully_decided_green();
}

#[test]
fn a_diverged_main_whose_merge_conflicts_is_published_under_another_name_then_reset() {
    runner::run(scenarios::main_diverged_conflicting_published()).assert_fully_decided_green();
}

#[test]
fn a_landing_fast_forwards_a_receiving_branch_the_primary_holds_in_place() {
    runner::run(scenarios::shared_checkout_detached()).assert_fully_decided_green();
}

#[test]
fn abort_restores_a_member_moved_in_the_target_and_in_place() {
    runner::run(scenarios::abort_restores_a_member_moved_in_the_target_and_in_place())
        .assert_fully_decided_green();
}

#[test]
fn an_in_place_fast_forward_is_leased_and_aborted_from_either_side_or_refused_on_edits() {
    for end in [
        scenarios::InPlaceEnd::AbortedFromTheSource,
        scenarios::InPlaceEnd::AbortedFromThePrimary,
        scenarios::InPlaceEnd::Dirty,
        scenarios::InPlaceEnd::Drifted,
        scenarios::InPlaceEnd::KilledBeforeThePlan,
        scenarios::InPlaceEnd::Rewound,
        scenarios::InPlaceEnd::ContinuedAfterRelease,
    ] {
        runner::run(scenarios::in_place_fast_forward(end)).assert_fully_decided_green();
    }
}

#[test]
fn a_branch_named_by_an_update_operand_is_refused_under_a_landing_claim() {
    for merge in [false, true] {
        runner::run(
            scenarios::a_branch_named_by_an_update_operand_is_refused_under_a_landing_claim(merge),
        )
        .assert_fully_decided_green();
    }
}

#[test]
fn a_landing_judges_the_pin_of_a_shared_member_it_fast_forwards() {
    runner::run(scenarios::a_landing_judges_the_pin_of_a_shared_member_it_fast_forwards())
        .assert_fully_decided_green();
}

#[test]
fn an_inactive_projects_landing_leaves_a_dropped_member_the_presented_project_lists() {
    runner::run(
        scenarios::an_inactive_projects_landing_leaves_a_dropped_member_the_presented_project_lists(
        ),
    )
    .assert_fully_decided_green();
}

#[test]
fn an_inactive_landing_spells_its_refusals_for_its_project() {
    runner::run(scenarios::inactive_landing_refusal_spelled_for_its_project())
        .assert_fully_decided_green();
}

#[test]
fn unpresented_project_choices_carry_the_project() {
    runner::run(scenarios::unpresented_project_choices_carry_the_project())
        .assert_fully_decided_green();
}

#[test]
fn abort_after_aligning_leaves_a_member_the_landing_never_moved() {
    runner::run(scenarios::abort_leaves_a_target_member_the_alignment_never_moved())
        .assert_fully_decided_green();
}

#[test]
fn a_member_change_refusal_prints_the_rebind_then_the_setter_then_the_consent() {
    runner::run(scenarios::member_change_choice_order()).assert_fully_decided_green();
}

#[test]
fn removing_a_member_the_target_lists_lands_only_when_named() {
    runner::run(scenarios::removal_is_member_change()).assert_fully_decided_green();
}

#[test]
fn dc06_a_landing_leaves_the_merged_member_set_pinned_with_the_relock_last() {
    runner::run(scenarios::dc06_member_set_from_merge()).assert_fully_decided_green();
}

#[test]
fn a_continue_judges_the_resolved_manifest_and_takes_added_consent() {
    runner::run(scenarios::continue_rejudges_the_resolved_manifest()).assert_fully_decided_green();
}

#[test]
fn a_continue_refused_on_its_resolution_offers_an_abort_that_runs() {
    runner::run(scenarios::continue_refused_on_its_resolution_aborts())
        .assert_fully_decided_green();
}

#[test]
fn a_raw_merged_entry_change_is_reported_until_published_and_named_to_land() {
    runner::run(scenarios::raw_merge_member_change()).assert_fully_decided_green();
}

#[test]
fn roles_in_a_feature_carry_pins_and_a_role_or_backward_pin_change_is_named() {
    runner::run(scenarios::roles_in_feature()).assert_fully_decided_green();
}

#[test]
fn a_landing_lists_left_behind_commits_in_its_json_envelope() {
    runner::run(scenarios::member_change_left_behind_in_json()).assert_fully_decided_green();
}

#[test]
fn a_backward_pin_landed_by_name_is_reported_in_the_json_envelope() {
    runner::run(scenarios::backward_pin_reported_in_json()).assert_fully_decided_green();
}

#[test]
fn a_continued_landing_lists_left_behind_commits_again() {
    runner::run(scenarios::member_change_left_behind_on_continue()).assert_fully_decided_green();
}

#[test]
fn update_remote_in_a_feature_repins_a_named_dependency_and_reference_on_their_declared_branch() {
    runner::run(scenarios::update_remote_repins_dependency_and_reference_in_a_feature())
        .assert_fully_decided_green();
}

#[test]
fn forward_bumps_of_a_dependency_on_both_sides_land_unnamed() {
    runner::run(scenarios::forward_bumps_on_both_sides_land_unnamed()).assert_fully_decided_green();
}

#[test]
fn a_backward_pin_offers_only_a_rebind_that_lands_it_unchanged_and_its_pin_runs_as_printed() {
    runner::run(scenarios::a_backward_pin_offers_only_a_rebind_that_lands_it_unchanged())
        .assert_fully_decided_green();
}

#[test]
fn a_carried_pin_containing_the_targets_but_off_the_receiving_branch_is_refused() {
    runner::run(scenarios::a_containing_pin_off_the_receiving_branch_is_refused())
        .assert_fully_decided_green();
}

#[test]
fn a_dependency_declared_differently_lands_by_name_and_lists_what_it_leaves() {
    runner::run(scenarios::a_dependency_declared_differently_lands_and_lists_what_it_leaves())
        .assert_fully_decided_green();
}

#[test]
fn a_removal_over_an_unreadable_merge_base_is_a_named_change() {
    runner::run(scenarios::a_removal_over_an_unreadable_merge_base_is_named())
        .assert_fully_decided_green();
}

#[test]
fn a_member_change_refusal_lists_only_the_fields_the_landing_changes() {
    runner::run(scenarios::member_changes_are_judged_per_field()).assert_fully_decided_green();
}

#[test]
fn a_consent_naming_no_change_is_reported_with_the_rest() {
    runner::run(scenarios::an_inapplicable_consent_joins_the_report()).assert_fully_decided_green();
}

#[test]
fn the_re_add_a_removal_prints_clears_it_as_printed() {
    runner::run(scenarios::the_printed_re_add_clears_a_removal()).assert_fully_decided_green();
}

#[test]
fn landing_names_every_field_and_the_one_printed_consent_lands_them() {
    runner::run(scenarios::landing_names_every_field()).assert_fully_decided_green();
}

#[test]
fn every_setter_a_member_change_prints_restores_the_target_value_as_printed() {
    runner::run(scenarios::every_printed_setter_restores_the_target_value())
        .assert_fully_decided_green();
}

#[test]
fn a_followed_receiving_branch_that_exists_nowhere_is_created_at_the_targets_pin() {
    runner::run(scenarios::a_followed_receiving_branch_that_exists_nowhere_is_created())
        .assert_fully_decided_green();
}

#[test]
fn a_dependency_moved_off_its_pin_is_refused_by_alignment_before_the_op_start_relock() {
    runner::run(scenarios::a_dependency_moved_off_its_pin_is_refused_before_the_relock())
        .assert_fully_decided_green();
}

#[test]
fn an_owned_member_the_landing_cannot_plan_is_refused_before_it_is_planned() {
    runner::run(scenarios::an_unplannable_member_is_refused_before_it_is_planned())
        .assert_fully_decided_green();
}

#[test]
fn the_copy_a_left_behind_listing_prints_ports_the_fix_and_status_stops_listing_it() {
    runner::run(scenarios::member_change_race_listing_until_ported()).assert_fully_decided_green();
}

#[test]
fn a_raw_merge_into_main_changing_an_entry_is_reported_pin_before_restore() {
    runner::run(scenarios::raw_merge_member_change_into_main()).assert_fully_decided_green();
}

#[test]
fn a_carried_pin_against_a_diverged_receiving_branch_is_refused_before_anything_moves() {
    runner::run(scenarios::a_carried_pin_against_a_diverged_receiving_branch_is_refused())
        .assert_fully_decided_green();
}

#[test]
fn an_unmoved_member_with_diverged_pins_is_refused_before_anything_moves_and_its_pin_lands_it() {
    runner::run(scenarios::an_unmoved_member_with_diverged_pins_is_refused_before_anything_moves())
        .assert_fully_decided_green();
}

#[test]
fn an_unmoved_member_pinned_backward_lands_only_when_named() {
    runner::run(scenarios::an_unmoved_member_pinned_backward_is_a_member_change())
        .assert_fully_decided_green();
}

#[test]
fn a_carried_dependency_pin_must_be_on_its_receiving_branch_and_merge_with_the_target() {
    runner::run(scenarios::carried_pin_judged_against_the_receiving_branch())
        .assert_fully_decided_green();
}

#[test]
fn a_landing_creates_a_receiving_branch_that_exists_nowhere_and_composes_an_unpinned_member() {
    runner::run(scenarios::landing_creates_a_receiving_branch_that_exists_nowhere())
        .assert_fully_decided_green();
}

#[test]
fn solo_workweave_lands_and_pushes() {
    runner::run(scenarios::solo_workweave()).assert_fully_decided_green();
}

#[test]
fn fetch_moves_no_branch_head_checkout_or_lock_on_any_repo() {
    runner::run(scenarios::fetch_moves_nothing()).assert_fully_decided_green();
}

#[test]
fn clone_completes_listing_each_member_the_lock_cannot_place_and_exits_with_the_distinct_status() {
    runner::run(scenarios::clone_exit_status()).assert_fully_decided_green();
}

#[test]
fn a_clone_while_the_remote_is_ahead_of_the_pins_places_every_member_at_its_pin_and_reads_it_as_behind(
) {
    runner::run(scenarios::fresh_take_at_pin()).assert_fully_decided_green();
}

#[test]
fn a_relock_over_another_projects_pending_commit_is_reported_to_both_projects() {
    runner::run(scenarios::proj2_relock_over_pending()).assert_fully_decided_green();
}

#[test]
fn a_pin_only_an_absent_fork_remote_holds_offers_the_remote_first() {
    runner::run(scenarios::fork_remote_missing()).assert_fully_decided_green();
}

#[test]
fn clone_of_a_project_with_a_complete_lock_exits_zero() {
    runner::run(scenarios::clone_with_a_complete_lock()).assert_fully_decided_green();
}

#[test]
fn a_step_that_exits_with_another_status_than_declared_is_unmet() {
    let report = bad_steps(
        "status exits 0",
        vec![Step::rwv(&a(), &["status"]).expect(Exit::Code(3))],
    );
    assert!(
        report
            .unmet
            .iter()
            .any(|(_, why)| why.contains("expected exit status 3")),
        "{report}"
    );
}

fn refused_by_rwv(code: i32, output: &str) -> Outcome<'_> {
    Outcome {
        by_rwv: true,
        succeeded: code == 0,
        code: Some(code),
        output,
    }
}

#[test]
fn a_refusal_is_status_one_printing_its_line_and_ending_in_its_route() {
    let pin_unpinned = Exit::refuses(Repo::App, State::PinUnpinned);
    let printed = "rwv push: local/remotes/app: pin unpinned (1 commit)\n\
                   choices:\n  record it\n    rwv lock --commit\n\n\
                   rwv explain edits\nrwv explain pin\n";
    assert_eq!(refused_by_rwv(1, printed).meets(pin_unpinned), Ok(()));
    for status in [0, 2, 101] {
        assert!(
            refused_by_rwv(status, printed).meets(pin_unpinned).is_err(),
            "status {status} read as a refusal"
        );
    }
    for (why, output) in [
        (
            "another state",
            "rwv push: local/remotes/app: pin none\n\nrwv explain pin\n",
        ),
        (
            "another repo",
            "rwv push: local/remotes/lib: pin unpinned\n\nrwv explain pin\n",
        ),
        (
            "no route",
            "rwv push: local/remotes/app: pin unpinned\n\nrwv explain name\n",
        ),
        (
            "the route not last",
            "rwv push: local/remotes/app: pin unpinned\n\nrwv explain pin\ndone\n",
        ),
    ] {
        assert!(
            refused_by_rwv(1, output).meets(pin_unpinned).is_err(),
            "{why} met the refusal:\n{output}"
        );
    }
}

#[test]
fn a_judged_line_matches_its_state_whole_and_not_a_longer_one() {
    let on_branch = Exit::refuses(Repo::Lib, State::AlignmentOnBranch);
    let longer = "rwv lock: local/remotes/lib: alignment on branch with stray commits (wip)\n\n\
                  rwv explain alignment\n";
    assert!(refused_by_rwv(1, longer).meets(on_branch).is_err());
    let named = "rwv lock: local/remotes/lib: alignment on branch (wip)\n\nrwv explain alignment\n";
    assert_eq!(refused_by_rwv(1, named).meets(on_branch), Ok(()));
    let elsewhere = "rwv lock: local/remotes/lib in /w/seat: alignment on branch\n\n\
                     rwv explain alignment\n";
    assert_eq!(refused_by_rwv(1, elsewhere).meets(on_branch), Ok(()));
}

#[test]
fn a_refusal_naming_a_kind_ends_in_that_kinds_route() {
    let kind = Exit::refuses_kind(RefusalKind::HeadNotOnBranch);
    let printed = "Error: rwv branch create: the project repo is on a detached HEAD\n\n\
                   rwv explain head-not-on-branch\n";
    assert_eq!(refused_by_rwv(1, printed).meets(kind), Ok(()));
    let other = "Error: rwv branch create: no\n\nrwv explain invalid-ref-name\n";
    assert!(refused_by_rwv(1, other).meets(kind).is_err());
}

#[test]
fn rwv_status_one_is_only_ever_a_named_refusal() {
    assert!(refused_by_rwv(1, "").meets(Exit::Code(1)).is_err());
    let git = Outcome {
        by_rwv: false,
        succeeded: false,
        code: Some(1),
        output: "",
    };
    assert_eq!(git.meets(Exit::Code(1)), Ok(()));
    assert!(git
        .meets(Exit::refuses(Repo::App, State::PinUnpinned))
        .is_err());
}

#[test]
fn a_step_with_claims_and_no_exit_is_undecided() {
    let step = Step::rwv(&a(), &["status"]).claim(Claim::OutputContains("ok"));
    assert!(!step.expect.is_decided());
    assert!(step.expect(Exit::Succeeds).expect.is_decided());
}

#[test]
fn a_refusal_of_another_state_or_by_the_argument_parser_is_unmet() {
    let report = bad_steps(
        "refusals that are not the one expected",
        vec![
            Step::commit(&a(), Repo::App, "unpinned"),
            Step::rwv(&a(), &["push"]).expect(Exit::refuses(Repo::App, State::PinNone)),
            Step::rwv(&a(), &["push", "--no-such-flag"])
                .expect(Exit::refuses(Repo::App, State::PinUnpinned)),
        ],
    );
    let why: Vec<&str> = report.unmet.iter().map(|(_, why)| why.as_str()).collect();
    assert_eq!(why.len(), 2, "{report}");
    assert!(
        why[0].contains("no line judges local/remotes/app: pin none"),
        "{report}"
    );
    assert!(why[1].contains("got Some(2)"), "{report}");
}

#[test]
fn an_output_line_claim_is_the_whole_line() {
    let report = bad_steps(
        "a list holding ww2 alone",
        vec![
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "ww2"])
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["workweave", "list", "--project", "proj"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLine("ww2"))
                .claim(Claim::OutputLine("ww")),
        ],
    );
    let why: Vec<&str> = report.unmet.iter().map(|(_, why)| why.as_str()).collect();
    assert_eq!(why.len(), 1, "{report}");
    assert!(
        why[0].starts_with("no line of the output is \"ww\""),
        "{report}"
    );
}

/// Run by [`an_undecided_step_is_announced_by_a_green_run`] as a subprocess,
/// whose stderr it reads.
#[test]
fn a_green_run_through_an_undecided_step() {
    bad_steps("one undecided step", vec![Step::rwv(&a(), &["status"])]).assert_green();
}

#[test]
fn an_undecided_step_is_announced_by_a_green_run() {
    let out = common::rerun_one_test(&common::test_name(a_green_run_through_an_undecided_step));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains(
            "SKIP: scenario one undecided step: `rwv status` ran with no decided outcome"
        ),
        "{stderr}"
    );
}

#[test]
fn a_claim_that_local_state_is_unchanged_reports_a_step_that_moves_it() {
    let report = bad_steps(
        "a commit moves app",
        vec![Step::commit(&a(), Repo::App, "moves").claim(Claim::LocalStateUnchanged(a()))],
    );
    assert!(
        report.unmet.iter().any(|(_, why)| why.contains("moved")),
        "{report}"
    );
}

#[test]
fn a_pin_left_unresolved_is_reported_unless_the_step_names_its_member() {
    let unnamed = bad_steps(
        "the lock is published ahead of app",
        vec![
            Step::commit(&a(), Repo::App, "ahead"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
        ],
    );
    assert!(
        unnamed
            .violations
            .iter()
            .any(|(_, v)| v.invariant() == Invariant::EveryPinResolves),
        "{unnamed}"
    );

    let named = bad_steps(
        "the lock is published ahead of app, and the step says so",
        vec![
            Step::commit(&a(), Repo::App, "ahead"),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main")
                .may_leave_unresolved(&[Repo::App]),
        ],
    );
    named.assert_green();
}

#[test]
fn permanent_seat_lands_twice_then_lands_its_declaration_change_by_name() {
    runner::run(scenarios::permanent_seat()).assert_fully_decided_green();
}

#[test]
fn nested_workweaves_each_on_a_branch_it_names() {
    runner::run(scenarios::nested_workweaves()).assert_green();
}

#[test]
fn nested_workweaves_sync_both_ways_and_land() {
    runner::run(scenarios::nested_workweaves_without_naming()).assert_fully_decided_green();
}

#[test]
fn raw_merge_real() {
    runner::run(scenarios::raw_merge_real()).assert_fully_decided_green();
}

#[test]
fn raw_merge_fast_forward() {
    runner::run(scenarios::raw_merge_fast_forward()).assert_fully_decided_green();
}

#[test]
fn raw_switch_in_primary_project() {
    runner::run(scenarios::raw_switch_in_primary_project()).assert_fully_decided_green();
}

#[test]
fn project_pr_merged_before_members() {
    runner::run(scenarios::project_pr_merged_before_members()).assert_fully_decided_green();
}

#[test]
fn hand_edit_declaration_to_existing_branch() {
    runner::run(scenarios::hand_edit_declaration_to_existing_branch()).assert_fully_decided_green();
}

#[test]
fn hand_edit_declaration_to_missing_branch() {
    runner::run(scenarios::hand_edit_declaration_to_missing_branch()).assert_fully_decided_green();
}

/// `workweave delete --discard-unlanded` consents to losing the
/// branch, not the commits: a backup is made first, so no commit is stranded,
/// and `backup list` shows it.
#[test]
fn giving_up_before_publishing_keeps_the_discarded_commit_as_a_backup() {
    runner::run(scenarios::workweave_without_b_given_up_before_publishing())
        .assert_fully_decided_green();
}

#[test]
fn a_clone_holding_a_backup_is_not_destroyed_by_member_remove() {
    runner::run(scenarios::dc10_store_destroy()).assert_fully_decided_green();
}

#[test]
fn a_tombstone_becomes_a_backup_and_backup_drop_releases_it() {
    runner::run(scenarios::tombstone_to_backup()).assert_green();
}

#[test]
fn a_workspace_state_that_does_not_parse_reads_as_absent_and_update_restores_it() {
    runner::run(scenarios::workspace_state_corrupt()).assert_fully_decided_green();
}

#[test]
fn a_clean_of_the_project_checkout_loses_no_record_and_stored_state_is_written_only_on_change() {
    runner::run(scenarios::dc20_state_location()).assert_fully_decided_green();
}

#[test]
fn a_marker_that_disagrees_is_restored_by_every_verb_that_does_not_destroy_the_workweave() {
    runner::run(scenarios::dc21_marker_disagreement()).assert_fully_decided_green();
}

#[test]
fn the_index_restores_a_marker_whole_with_its_parent_and_is_re_listed_from_the_markers() {
    runner::run(scenarios::index_parent()).assert_fully_decided_green();
}

#[test]
fn an_index_at_the_old_path_refuses_its_readers_and_doctor_converts_it() {
    runner::run(scenarios::index_old_path()).assert_fully_decided_green();
}

#[test]
fn the_basedir_defaults_beside_the_primary_and_set_default_create_basedir_stores_it_in_the_index() {
    runner::run(scenarios::basedir_default()).assert_fully_decided_green();
}

#[test]
fn status_lists_each_operator_branch_nothing_accounts_for_and_no_other() {
    runner::run(scenarios::status_operator_branches()).assert_fully_decided_green();
}

#[test]
fn activate_moves_no_checkout_and_update_is_the_spelling_that_does() {
    runner::run(scenarios::activate_no_checkout()).assert_fully_decided_green();
}

#[test]
fn activate_update_moves_a_member_the_outgoing_project_is_aligned_on() {
    runner::run(scenarios::activate_leaves_what_the_outgoing_project_holds())
        .assert_fully_decided_green();
}

#[test]
fn activate_over_a_shared_member_aligned_for_the_project_moves_nothing() {
    runner::run(scenarios::activate_over_aligned_shared_member()).assert_fully_decided_green();
}

#[test]
fn a_verb_for_an_inactive_project_refuses_instead_of_moving_a_shared_checkout() {
    runner::run(scenarios::inactive_project_moves_no_shared_checkout())
        .assert_fully_decided_green();
}

#[test]
fn a_reserved_name_is_refused_by_a_create_and_nothing_is_created() {
    runner::run(scenarios::records_reserved_name()).assert_fully_decided_green();
}

#[test]
fn update_remote_leaves_the_lock_uncommitted_and_a_switch_refuses_until_it_is_committed() {
    runner::run(scenarios::update_remote_writes_lock()).assert_fully_decided_green();
}

#[test]
fn a_create_from_a_detached_workspace_starts_at_its_head_and_carries_the_manifest() {
    runner::run(scenarios::detached_create_start()).assert_fully_decided_green();
}

/// The steps of each scenario `scenarios.rs` defines, read without running.
trait IntoSteps {
    fn into_steps(self) -> Vec<Step>;
}

impl IntoSteps for Scenario {
    fn into_steps(self) -> Vec<Step> {
        self.steps
    }
}

impl IntoSteps for Vec<Step> {
    fn into_steps(self) -> Vec<Step> {
        self
    }
}

macro_rules! every_scenario {
    ($($name:ident $(($($arg:expr),*))?),* $(,)?) => {
        fn every_scenario() -> Vec<(&'static str, Vec<Step>)> {
            #[allow(unused_mut)]
            let mut every = vec![
                $((stringify!($name), scenarios::$name($($($arg),*)?).into_steps())),*
            ];
            #[cfg(unix)]
            every.push((
                "three_way_pins_on_old_git",
                scenarios::three_way_pins_on_old_git().into_steps(),
            ));
            #[cfg(unix)]
            every.push((
                "git_below_2_38",
                scenarios::git_below_2_38().into_steps(),
            ));
            #[cfg(unix)]
            every.push((
                "abort_after_a_carry_stop",
                scenarios::abort_after_a_carry_stop(scenarios::BeforeTheCarry::LandingMerged)
                    .into_steps(),
            ));
            every
        }
    };
}

every_scenario!(
    solo_workweave,
    history_rebase_pin_remap,
    stack_carried,
    stack_cycle,
    stack_remote_dependent,
    stack_remote_dependent_landed(scenarios::RemoteDependentLanded::Wholly),
    stack_remote_dependent_landed(scenarios::RemoteDependentLanded::ButOneCommit),
    stack_carried_by_a_landing,
    stack_two_dependents,
    stack_transitive,
    stack_merge_setting,
    stack_checked_out(scenarios::CheckedOutChoice::Rebind),
    stack_checked_out(scenarios::CheckedOutChoice::LandThenRetire),
    stack_not_containing,
    stack_carry_conflict(scenarios::AfterHandContinue::Continue),
    stack_carry_conflict(scenarios::AfterHandContinue::Abort),
    abort_after_a_carry_stop(scenarios::BeforeTheCarry::SyncRelocked),
    a_dependent_carried_onto_a_merged_project_pins_what_it_inherits,
    abort_a_carried_stack_built_on(scenarios::BuiltOnChoice::Abandon),
    abort_a_carried_stack_built_on(scenarios::BuiltOnChoice::Continue),
    abort_a_carried_stack_blocked(scenarios::CarriedBlocked::CheckedOut),
    abort_a_carried_stack_blocked(scenarios::CarriedBlocked::Gone),
    abort_a_carried_stack_blocked(scenarios::CarriedBlocked::CheckedOutInTheTarget),
    abort_a_carried_stack_killed_after_putting_it_back,
    abort_over_a_carry_left_by_a_quit,
    a_dependent_moved_as_its_carry_moves(scenarios::MovedAsCarried::CarryAgain),
    a_dependent_moved_as_its_carry_moves(scenarios::MovedAsCarried::AbortThenSync),
    a_dependent_moved_as_its_carry_moves(scenarios::MovedAsCarried::CarryAgainLanding),
    abort_after_a_carry_stop(scenarios::BeforeTheCarry::FinishedByHand),
    stack_at_the_tip_under_update_refs,
    stack_carry_stopped_under_update_refs,
    a_landing_into_a_dependent_waits_on_its_stopped_carry(scenarios::StoppedCarryEnds::Abort),
    a_landing_into_a_dependent_waits_on_its_stopped_carry(scenarios::StoppedCarryEnds::Continue),
    a_stopped_carry_continued_meets_a_dependent_checked_out(
        scenarios::ContinuedCarryMeets::WorkweaveDelete
    ),
    a_stopped_carry_continued_meets_a_dependent_checked_out(scenarios::ContinuedCarryMeets::Rebind),
    a_stopped_carry_continued_meets_a_dependent_checked_out(scenarios::ContinuedCarryMeets::Abort),
    abort_leaving_the_target_of_a_landing_that_carried_a_dependent,
    a_carry_refuses_to_write_over_an_ignored_file,
    a_carry_replay_that_fails_without_a_conflict_leaves_the_checkout_on_its_branch,
    a_dependent_opened_elsewhere_as_its_carry_moves_it_is_not_moved,
    a_landing_killed_after_its_carry_continues,
    a_landing_resumed_after_its_target_moved_past_a_carry,
    a_carry_left_by_a_quit_is_unrecorded,
    a_project_carry_follows_its_member_recomposed_across_a_stop,
    siblings_at_one_commit_are_carried_to_one_commit,
    a_landing_resumed_after_its_carry_judges_an_entry_changed_since,
    a_carry_stopped_in_the_project_continues,
    sync_continue_judges_dependents_before_a_rejudged_rebase,
    a_sync_continued_after_its_relock_keeps_it,
    a_sync_continued_after_its_project_tip_moved_recomposes_the_project_carry,
    a_landing_whose_carry_stopped_in_the_project_continues,
    abort_refuses_a_tip_amended_during_a_stop,
    vcs_continue_mid_rebase(
        scenarios::MidRebase::Member,
        scenarios::AfterHandContinue::Continue
    ),
    vcs_continue_mid_rebase(
        scenarios::MidRebase::Project,
        scenarios::AfterHandContinue::Continue
    ),
    vcs_continue_mid_rebase(
        scenarios::MidRebase::Member,
        scenarios::AfterHandContinue::Abort
    ),
    vcs_continue_mid_rebase(
        scenarios::MidRebase::Project,
        scenarios::AfterHandContinue::Abort
    ),
    dc04_conflict_setter,
    sync_continue_merges_a_repo_the_stop_diverged,
    land_continue_composes_a_merge_the_stop_left,
    abort_after_a_landing_merge_stops,
    landing_merge_continued_writes_one_project_merge,
    abort_after_a_sync_merge_stops,
    sync_continue_merges_a_range_the_stop_gave_a_merge,
    landing_names_every_field,
    history_unset_fast_forward,
    history_unset_moved,
    history_unset_retire_behind,
    history_contradicted,
    history_contradicted_where_no_setting_is_read,
    history_equal_one_run,
    history_linear_landing,
    history_ff_only,
    history_merge,
    history_range_with_merge,
    history_ff_only_range_with_merge,
    history_sync_from_declared,
    history_member_override_lands,
    rebased_backup_lifetime,
    history_published_rebase,
    history_sync_contradicted_where_it_fast_forwards,
    sync_continue_replays_what_the_stopped_run_reported,
    history_land_merge_resolves_derived_to_the_receiving_side,
    history_land_merge_still_stops_on_an_undeclared_path,
    history_sync_merge_resolves_derived_to_the_receiving_side,
    history_sync_merge_still_stops_on_an_undeclared_path,
    history_retired_published_seat_drops_its_rebased_backups,
    history_overwrite_to_a_newer_tip_drops_the_old_rebased_backup,
    history_republish_after_a_colleague_pushed,
    rebased_backup_after_abort_offers_no_republish,
    rebased_backup_of_another_branch_offers_no_republish,
    abort_deletes_a_created_receiving_branch,
    abort_stops_at_the_first_write_that_fails,
    abort_judges_again_once_it_holds_the_target,
    leave_target_gives_up_the_target_only_after_the_source_is_restored,
    abort_refuses_while_the_target_cannot_be_read,
    abort_of_a_landing_over_a_cancel_the_vcs_refuses,
    abort_of_a_landing_stopped_in_a_replayed_project_commit(scenarios::ReplayedCommit::First),
    abort_of_a_landing_stopped_in_a_replayed_project_commit(scenarios::ReplayedCommit::Second),
    abort_after_the_target_aligned(
        scenarios::ReceivingCase::Followed,
        scenarios::AbortFrom::Source
    ),
    abort_after_the_target_aligned(
        scenarios::ReceivingCase::Followed,
        scenarios::AbortFrom::Target
    ),
    abort_after_the_target_aligned(
        scenarios::ReceivingCase::MovedAsRef,
        scenarios::AbortFrom::Source
    ),
    abort_after_the_target_aligned(
        scenarios::ReceivingCase::MovedAsRef,
        scenarios::AbortFrom::Target
    ),
    abort_after_the_target_aligned(
        scenarios::ReceivingCase::Created,
        scenarios::AbortFrom::Source
    ),
    abort_after_the_target_aligned(
        scenarios::ReceivingCase::Created,
        scenarios::AbortFrom::Target
    ),
    abort_after_the_target_aligned(
        scenarios::ReceivingCase::CreatedWithoutWork,
        scenarios::AbortFrom::Source
    ),
    abort_after_the_target_aligned(
        scenarios::ReceivingCase::CreatedWithoutWork,
        scenarios::AbortFrom::Target
    ),
    abort_after_the_target_aligned_rerun_after_a_kill(scenarios::ReceivingCase::Followed),
    abort_after_the_target_aligned_rerun_after_a_kill(scenarios::ReceivingCase::MovedAsRef),
    abort_after_the_target_aligned_rerun_after_a_kill(scenarios::ReceivingCase::Created),
    abort_after_the_target_aligned_rerun_after_a_kill(scenarios::ReceivingCase::CreatedWithoutWork),
    cherry_pick_abort_leaves_an_untouched_members_merge,
    pull_abort_refuses_a_merge_begun_on_the_operators_commit,
    pull_abort_refuses_a_merge_the_operator_began_at_the_savepoint,
    pull_abort_over_a_cancel_the_vcs_refuses,
    abort_refuses_an_interactive_rebase_the_operator_began,
    cherry_pick_abort_refuses_a_merge_begun_on_the_operators_commit,
    pull_abort_refuses_a_merge_in_a_member_it_never_merges,
    update_remote_abort_refuses_a_merge_in_a_member_it_never_merges,
    pull_abort_leaves_a_merge_in_a_member_it_never_merges,
    update_remote_abort_leaves_a_merge_in_a_member_it_never_merges,
    update_remote_abort_over_a_ref_moved_since,
    update_remote_reset_killed_after_the_move(false),
    update_remote_reset_killed_after_the_move(true),
    abort_keeps_edits_made_in_the_target_after_the_stop(
        scenarios::LandedStop::Landed,
        scenarios::AbortFrom::Source
    ),
    abort_keeps_edits_made_in_the_target_after_the_stop(
        scenarios::LandedStop::Retire,
        scenarios::AbortFrom::Source
    ),
    abort_keeps_edits_made_in_the_target_after_the_stop(
        scenarios::LandedStop::Landed,
        scenarios::AbortFrom::Target
    ),
    abort_keeps_edits_made_in_the_target_after_the_stop(
        scenarios::LandedStop::Retire,
        scenarios::AbortFrom::Target
    ),
    abort_refuses_a_merge_in_a_target_repo_the_landing_moved(scenarios::LandedStop::Landed),
    abort_refuses_a_merge_in_a_target_repo_the_landing_moved(scenarios::LandedStop::Retire),
    abort_sets_aside_an_untracked_file_in_the_way_in_the_target,
    abort_over_a_receiving_branch_deleted_after_the_landing_moved_it(scenarios::LandedStop::Landed),
    abort_over_a_receiving_branch_deleted_after_the_landing_moved_it(scenarios::LandedStop::Retire),
    abort_refuses_a_switch_back_over_colliding_edits(scenarios::LandedStop::Landed),
    abort_switch_back_blocked(scenarios::SwitchBackBlocked::Gone),
    abort_switch_back_blocked(scenarios::SwitchBackBlocked::Held),
    abort_refuses_a_switch_back_over_colliding_edits(scenarios::LandedStop::Retire),
    abort_restores_a_target_fast_forward_killed_after_the_move,
    a_continued_landing_racing_a_commit_on_a_repo_it_moved,
    vcs_continue_mid_rebase_then_an_own_commit,
    abort_abandons_colliding_edits_keeping_them(scenarios::LandedStop::Landed),
    abort_abandons_colliding_edits_keeping_them(scenarios::LandedStop::Retire),
    cherry_pick_abort_keeps_edits_made_after_the_stop,
    cherry_pick_abort_names_how_to_put_back_a_deleted_branch,
    abort_after_the_landing_released_its_target,
    abort_after_another_landing_built_on_the_target(
        scenarios::BuiltOn::OtherMember,
        scenarios::TargetAt::Primary,
        scenarios::EndedBy::LeaveTarget
    ),
    abort_of_a_landing_a_member_entered_with,
    abort_with_both_sides_foreign,
    abort_over_a_target_checkout_switched_off_its_branch(scenarios::SwitchedOffEnd::LeaveTarget),
    abort_over_a_target_checkout_switched_off_its_branch(scenarios::SwitchedOffEnd::Abandon),
    abort_after_an_entered_branch_moved(
        scenarios::EnteredThen::HandCommit,
        scenarios::EnteredEnd::LeaveTarget
    ),
    abort_at_retire_waits_for_the_target_back,
    abort_at_retire_after_the_in_place_workspace_went,
    a_landing_meets_a_landing_stopped_as_refs,
    a_landing_meets_a_stopped_landing_in_a_checkout,
    a_landing_killed_before_its_plan_releases_its_claim_on_abort,
    a_landing_ending_has_released_its_target,
    a_refused_abort_puts_back_what_it_took(scenarios::LandedStop::Landed),
    a_refused_abort_puts_back_what_it_took(scenarios::LandedStop::Retire),
    a_refused_continue_of_a_released_landing_leaves_the_target_free,
    a_continued_retire_releases_the_target_first,
    a_landing_records_its_op_before_it_claims,
    a_switch_refuses_to_align_a_member_onto_a_claimed_branch,
    a_verb_refuses_a_branch_a_stopped_landing_claims(scenarios::ClaimedMove::Switch),
    a_verb_refuses_a_branch_a_stopped_landing_claims(scenarios::ClaimedMove::SwitchDetached),
    a_verb_refuses_a_branch_a_stopped_landing_claims(scenarios::ClaimedMove::Rename),
    a_verb_refuses_a_branch_a_stopped_landing_claims(scenarios::ClaimedMove::SetMode),
    a_verb_refuses_a_branch_a_stopped_landing_claims(scenarios::ClaimedMove::SetBranch),
    a_verb_refuses_a_branch_a_stopped_landing_claims(scenarios::ClaimedMove::Delete),
    a_verb_refuses_a_branch_a_stopped_landing_claims(scenarios::ClaimedMove::Push),
    abort_typed_in_the_target_names_the_landing(false),
    abort_typed_in_the_target_names_the_landing(true),
    a_landing_killed_while_it_waits(scenarios::KilledWaiter::Continued),
    a_landing_killed_while_it_waits(scenarios::KilledWaiter::Aborted),
    a_landing_killed_while_it_waits_continues_once_free,
    landing_as_refs,
    landing_as_refs_unpinned,
    lost_race_abort,
    lost_race_on_the_first_ref,
    abort_after_the_primary_switched_to_the_target,
    dc05_compose_in_source(scenarios::Dc05::BeforeTheMoveAborted),
    dc05_compose_in_source(scenarios::Dc05::AtTheMoveAborted),
    dc05_compose_in_source(scenarios::Dc05::AtTheMoveContinued),
    dc05_compose_in_source(scenarios::Dc05::LandedAborted),
    continue_after_the_first_move(scenarios::MovedStop::CheckoutMoved),
    continue_after_the_first_move(scenarios::MovedStop::RefsMoved),
    continue_refuses_a_target_moved_while_stopped(
        scenarios::MovedWhileStopped::SwitchedOffItsBranch
    ),
    continue_refuses_a_target_moved_while_stopped(scenarios::MovedWhileStopped::RefGone),
    continue_refuses_a_target_moved_while_stopped(scenarios::MovedWhileStopped::RefMovedOn),
    a_receiving_branch_moved_as_a_ref_loses_its_race(scenarios::RefRace::Advanced),
    a_receiving_branch_moved_as_a_ref_loses_its_race(scenarios::RefRace::Rewound),
    a_receiving_branch_moved_as_a_ref_loses_its_race(scenarios::RefRace::CheckedOut),
    a_target_moved_before_the_landing_moves_it(scenarios::TargetHeld::AsRef),
    a_target_moved_before_the_landing_moves_it(scenarios::TargetHeld::InCheckout),
    abort_over_a_ref_the_landing_moved(scenarios::MovedRefThen::Committed),
    abort_over_a_ref_the_landing_moved(scenarios::MovedRefThen::CheckedOut),
    carried_pin_judged_against_the_receiving_branch,
    landing_creates_a_receiving_branch_that_exists_nowhere,
    seat_follows_after_landing,
    member_change_race_listing,
    main_diverged_after_member_change,
    stray_commits_copied_onto_the_checked_out_branch,
    main_diverged_after_member_change_reset,
    main_diverged_resolves_derived_to_the_upstream,
    main_diverged_still_stops_on_an_undeclared_path,
    main_diverged_reset_leaves_the_expected_branch,
    landing_stops_where_the_landed_lock_does_not_read,
    main_diverged_conflicting_held,
    main_diverged_conflicting_copied,
    main_diverged_conflicting_published,
    member_change_left_behind_in_json,
    member_change_left_behind_on_continue,
    backward_pin_reported_in_json,
    continue_reports_a_created_receiving_branch,
    member_change_choice_order,
    shared_checkout_detached,
    abort_restores_a_member_moved_in_the_target_and_in_place,
    abort_leaves_a_target_member_the_alignment_never_moved,
    in_place_fast_forward(scenarios::InPlaceEnd::AbortedFromTheSource),
    in_place_fast_forward(scenarios::InPlaceEnd::AbortedFromThePrimary),
    in_place_fast_forward(scenarios::InPlaceEnd::Dirty),
    in_place_fast_forward(scenarios::InPlaceEnd::Drifted),
    in_place_fast_forward(scenarios::InPlaceEnd::KilledBeforeThePlan),
    a_landing_judges_the_pin_of_a_shared_member_it_fast_forwards,
    a_branch_named_by_an_update_operand_is_refused_under_a_landing_claim(false),
    a_branch_named_by_an_update_operand_is_refused_under_a_landing_claim(true),
    in_place_fast_forward(scenarios::InPlaceEnd::Rewound),
    in_place_fast_forward(scenarios::InPlaceEnd::ContinuedAfterRelease),
    removal_is_member_change,
    dc06_member_set_from_merge,
    continue_rejudges_the_resolved_manifest,
    continue_refused_on_its_resolution_aborts,
    raw_merge_member_change,
    roles_in_feature,
    update_remote_repins_dependency_and_reference_in_a_feature,
    forward_bumps_on_both_sides_land_unnamed,
    a_backward_pin_offers_only_a_rebind_that_lands_it_unchanged,
    a_containing_pin_off_the_receiving_branch_is_refused,
    a_dependency_declared_differently_lands_and_lists_what_it_leaves,
    a_removal_over_an_unreadable_merge_base_is_named,
    member_changes_are_judged_per_field,
    an_inapplicable_consent_joins_the_report,
    the_printed_re_add_clears_a_removal,
    every_printed_setter_restores_the_target_value,
    a_followed_receiving_branch_that_exists_nowhere_is_created,
    a_dependency_moved_off_its_pin_is_refused_before_the_relock,
    an_unplannable_member_is_refused_before_it_is_planned,
    member_change_race_listing_until_ported,
    raw_merge_member_change_into_main,
    a_carried_pin_against_a_diverged_receiving_branch_is_refused,
    an_unmoved_member_with_diverged_pins_is_refused_before_anything_moves,
    an_unmoved_member_pinned_backward_is_a_member_change,
    land_into_a_target_checked_out_nowhere,
    land_reports_every_refusal_together,
    land_refuses_a_dependent,
    target_mode_changed,
    land_refuses_a_record_retargeted_elsewhere,
    push_withholds_a_feature_whose_target_is_unpublished,
    land_never_resolves_a_published_record_to_an_unrelated_branch,
    land_never_resolves_a_published_record_to_a_branch_with_a_foreign_upstream,
    land_refuses_work_built_on_another_lineage,
    land_never_rewrites_a_declared_mode_branch,
    land_into_a_target_that_exists_nowhere,
    land_target_gone_lists_only_targets_the_rebind_takes,
    land_reports_an_unmoved_member_it_pins_beyond_the_target,
    land_continue_judges_target_edits_where_it_moves,
    land_judges_target_edits_only_where_it_moves,
    land_judges_the_pin_on_its_branch_only_for_moved_members,
    land_refuses_a_moved_member_whose_branch_lacks_the_pin,
    land_reports_what_it_pins_beyond_the_target,
    land_reports_the_target_ahead_of_its_push_branch,
    land_resolves_a_target_that_publishes_where_its_record_is,
    land_refuses_a_target_diverged_from_its_upstream,
    land_refuses_a_receiving_branch_diverged_from_its_upstream,
    land_into_a_receiving_branch_checked_out_in_a_third_workspace,
    land_from_the_primary_refuses_a_target_in_a_workweave,
    land_refuses_a_target_member_off_its_receiving_branch,
    land_refuses_a_target_checkout_diverged_from_its_pin,
    land_leaves_a_shared_member_where_the_presented_project_has_it,
    land_into_a_workspace_presenting_another_project,
    land_names_a_member_composing_cannot_materialize,
    land_clones_a_member_its_target_took_in_by_fetch_alone,
    cherry_pick_runs_the_printed_merge,
    cherry_pick_runs_the_printed_pin_of_a_dependency_moved_backward,
    cherry_pick_runs_the_printed_add_of_a_member_not_listed,
    cherry_pick_runs_the_printed_add_of_a_dependency_not_listed,
    cherry_pick_except_judges_the_member_for_alignment,
    cherry_pick_runs_the_printed_relock_of_an_unpinned_member,
    cherry_pick_refuses_a_pin_this_clone_lacks,
    land_names_the_update_that_finishes_an_alignment_it_could_not_judge,
    dc01_mid_operation_land,
    deletion_offline_unpublished,
    dc01_mid_operation(scenarios::Stopped::Rebase),
    dc01_mid_operation(scenarios::Stopped::RebaseBehindPin),
    dc01_mid_operation(scenarios::Stopped::Merge),
    dc01_mid_operation(scenarios::Stopped::CherryPick),
    dc01_mid_operation(scenarios::Stopped::Revert),
    dc01_mid_operation(scenarios::Stopped::Bisect),
    dc01_mid_operation(scenarios::Stopped::ProjectBisect),
    dc02_c14_keeps_work,
    land_retire_judges_deletion_against_the_named_target,
    bump_to_cut_pin,
    c22_prints_both,
    dc29_no_bring,
    concurrent_landing_merge_upstream,
    records_local_writers,
    a_landing_waits_on_another_landings_claim,
    records_drop_races_create,
    landed_history_pin_no_ref_keeps,
    landed_history_waived_pin_on_a_second_machine,
    a_history_pin_not_repinned_still_refuses,
    merged_project_with_rebased_members_on_a_second_machine,
    a_landing_records_no_rebase_jump_the_replay_did_not_record,
    landing_writes_no_relock_of_its_own(scenarios::ProjectLands::FastForward),
    landing_writes_no_relock_of_its_own(scenarios::ProjectLands::Rebase),
    landing_writes_no_relock_of_its_own(scenarios::ProjectLands::Merge),
    a_refused_landing_leaves_a_lock_behind_as_it_stood,
    a_landing_records_the_tip_of_a_member_it_does_not_move,
    parallel_retire_fetch,
    dc11_drift_before_dirt,
    drift_staged_return_after_a_hand_refresh,
    drift_staged_return_with_an_edit_is_kept,
    cross_repo_pr,
    cross_repo_pr_fork,
    resume_on_machine_b,
    feature_in_primary_while_main_moves,
    workweave_without_b_then_pr,
    workweave_without_b_given_up_before_publishing,
    dc10_store_destroy,
    tombstone_to_backup,
    workweave_without_b_given_up_after_publishing,
    feature_off_stable,
    permanent_seat,
    review_colleague_fix,
    stable_end_to_end,
    main_change_into_stable,
    shared_member_declared_dev,
    an_inactive_projects_landing_leaves_a_dropped_member_the_presented_project_lists,
    unpresented_project_choices_carry_the_project,
    inactive_project_choices,
    inactive_landing_refusal_spelled_for_its_project,
    shared_member_pinned_behind,
    nested_workweaves,
    nested_workweaves_without_naming,
    raw_merge_real,
    raw_merge_fast_forward,
    detached_head_in_workweave,
    switch_c_wip_in_workweave,
    raw_switch_in_primary_project,
    project_pr_merged_before_members,
    hand_edit_declaration_to_existing_branch,
    hand_edit_declaration_to_missing_branch,
    worktree_add_force_on_weave_branch,
    cherry_pick_onto_stable,
    cherry_pick_port,
    cherry_pick_port_landing_merge,
    cherry_pick_conflict,
    cherry_pick_conflict_aborts,
    cherry_pick_refusals,
    cherry_pick_project_copy_killed,
    cherry_pick_op_start_killed,
    cherry_pick_continue_rechecks_members,
    cherry_pick_project_conflict,
    cherry_pick_project_conflict_aborts,
    cherry_pick_killed_after_each_move,
    pr_closed_unmerged,
    feature_whose_target_moved,
    feature_branches_in_some_members,
    fetch_moves_nothing,
    clone_exit_status,
    fresh_take_at_pin,
    clone_with_a_complete_lock,
    records_foreign_remote,
    records_hand_commit,
    records_unreadable_entry,
    workspace_state_corrupt,
    status_operator_branches,
    activate_no_checkout,
    activate_leaves_what_the_outgoing_project_holds,
    activate_over_aligned_shared_member,
    inactive_project_moves_no_shared_checkout,
    proj2_relock_over_pending,
    fork_remote_missing,
    dc03_roles_bound_push,
    dc13_push_no_lock,
    dc22_raw_project_switch,
    records_conflict,
    records_rejected,
    records_first_push,
    records_drop_per_machine,
    records_offline_write,
    records_unpushed_stay_local,
    waiver_per_merge,
    records_push_race,
    overwrite_lease_refuses_a_moved_remote,
    dc20_state_location,
    dc21_marker_disagreement,
    index_parent,
    index_old_path,
    raw_rename_published_feature,
    records_rename_adopt,
    basedir_default,
    no_superseded_on_prune,
    declared_mode_rename_retire,
    dc25_url_provenance,
    url_change_raw_merge,
    set_branch_new,
    member_pin_behind,
    records_reserved_name,
    update_remote_writes_lock,
    detached_create_start,
    records_bare_open_second_machine,
    records_raw_push_unrecorded,
    project_branch_elsewhere,
    create_diverged_at,
    push_refuses_a_project_branch_ahead_and_behind_before_publishing_a_member,
    push_dry_run_refuses_a_project_branch_ahead_and_behind,
    push_dry_run_on_an_unreachable_remote_is_unconfirmed_not_would_push,
    pull_merge_upstream_brings_a_project_branch_ahead_and_behind_to_pushable,
    pull_merge_upstream_joins_both_sides_moving_the_same_member,
    pull_merge_upstream_takes_in_a_dropped_member_beside_local_lock_commits,
    workweave_create_dirty_parent,
    dc19_idempotent_create,
    dc08_set_mode_own_workspace,
    detached_review_lib_fix,
    dc15_moved_reports,
    dc16_destroys_name_repos,
    dc18_partial_delete_status,
    three_way_pins,
    dc14_c3_under_pin_ahead,
    dc09_integrations_settle(Seed::default().integration()),
    both_sides_move_app_conflictingly,
    merge_conflict_continues,
    pull_merge_resolves_derived_to_the_upstream,
    pull_merge_still_stops_on_an_undeclared_path,
    pull_project_merge_resolves_derived_to_the_upstream,
    merge_conflict_aborts,
    resolve_app_conflict_and_continue,
    merge_upstream_as_refs,
    reset_acts_only_on_the_named_repo,
    history_set_unset,
    history_disagreement,
    history_member_override,
    lock_commit_whole_file,
    sync_takes_in_the_target,
    follow_never_backward,
    no_c3_after_reset,
    no_empty_merge,
    sync_refuses_off_lineage,
    sync_never_rebases_a_merge,
    sync_json_reports_what_it_merged_and_rebased,
    sync_refuses_to_rewrite_under_dependents,
    sync_judges_a_diverged_push_branch(true),
    sync_judges_a_diverged_push_branch(false),
    absent_pin_unmerged,
    record_retargeted_elsewhere_is_reported_and_refused,
    record_retargeted_elsewhere_accepted_by_update,
    record_retargeted_here_is_not_reported,
    record_unstored_is_stored_by_sync,
    sync_refuses_a_pre_schema_target_tip,
    merge_upstream_off_lineage,
    update_remote_merge_upstream_off_lineage,
    update_remote_advances_the_feature_branch,
    update_remote_refuses_a_foreign_feature_upstream,
    sync_from_another_branch_always_merges,
    sync_writes_no_record,
    waiver_own_lock,
    uncommitted_manifest_beside_an_unaligned_member,
    squashed_pin_off_remote_offers_the_reset,
    landed_pin_diverged_offers_the_reset,
);

/// The `pub fn` constructors of `scenarios.rs`, read from its source, less
/// those a `#[cfg(unix)]` leaves out of this build.
fn scenario_constructors() -> std::collections::BTreeSet<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/scenario/scenario_harness_test/scenarios.rs");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut unix_only = false;
    let mut found = std::collections::BTreeSet::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("pub fn ") {
            if cfg!(unix) || !unix_only {
                found.insert(rest.split('(').next().unwrap().to_owned());
            }
        }
        unix_only = line == "#[cfg(unix)]";
    }
    found
}

#[test]
fn every_scenario_constructor_is_walked() {
    let walked: std::collections::BTreeSet<String> = every_scenario()
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect();
    assert!(walked.contains("dc13_push_no_lock"), "{walked:?}");
    assert_eq!(walked, scenario_constructors());
}

/// A refusal names what refused, and a claim about a step needs an exit
/// expectation to be read against.
#[test]
fn every_refusal_step_names_its_state_and_every_claim_has_an_exit() {
    let mut bad = Vec::new();
    for (name, steps) in every_scenario() {
        for step in steps {
            if step.action.runs_rwv() && step.expect.exit == Exit::Code(1) {
                bad.push(format!("{name}: `{}` takes status 1 as a code", step.label));
            }
            if step.expect.exit == Exit::Undecided && !step.expect.claims.is_empty() {
                bad.push(format!("{name}: `{}` has claims and no exit", step.label));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn a_staged_return_of_a_whole_move_after_a_hand_refresh_is_refreshed_from_the_old_tip() {
    runner::run(scenarios::drift_staged_return_after_a_hand_refresh()).assert_fully_decided_green();
}

#[test]
fn a_staged_return_holding_content_no_commit_holds_is_never_replaced() {
    runner::run(scenarios::drift_staged_return_with_an_edit_is_kept()).assert_fully_decided_green();
}

#[test]
fn a_second_machine_opens_a_published_branch_bare_from_its_record() {
    runner::run(scenarios::records_bare_open_second_machine()).assert_fully_decided_green();
}

#[test]
fn a_raw_pushed_branch_opens_only_with_a_spelling_which_the_next_push_publishes() {
    runner::run(scenarios::records_raw_push_unrecorded()).assert_fully_decided_green();
}

#[test]
fn a_project_branch_checked_out_elsewhere_is_not_taken_by_switch_or_open_and_can_be_looked_at_detached(
) {
    runner::run(scenarios::project_branch_elsewhere()).assert_fully_decided_green();
}

#[test]
fn push_refuses_a_project_branch_ahead_and_behind_before_publishing_a_member() {
    runner::run(
        scenarios::push_refuses_a_project_branch_ahead_and_behind_before_publishing_a_member(),
    )
    .assert_green();
}

#[test]
fn push_dry_run_refuses_a_project_branch_ahead_and_behind() {
    runner::run(scenarios::push_dry_run_refuses_a_project_branch_ahead_and_behind())
        .assert_fully_decided_green();
}

#[test]
fn push_dry_run_on_an_unreachable_remote_is_unconfirmed_not_would_push() {
    runner::run(scenarios::push_dry_run_on_an_unreachable_remote_is_unconfirmed_not_would_push())
        .assert_fully_decided_green();
}

#[test]
fn pull_merge_upstream_brings_a_project_branch_ahead_and_behind_to_pushable() {
    runner::run(
        scenarios::pull_merge_upstream_brings_a_project_branch_ahead_and_behind_to_pushable(),
    )
    .assert_green();
}

#[test]
fn pull_merge_upstream_joins_both_sides_moving_the_same_member() {
    runner::run(scenarios::pull_merge_upstream_joins_both_sides_moving_the_same_member())
        .assert_green();
}

#[test]
fn pull_merge_upstream_takes_in_a_dropped_member_beside_local_lock_commits() {
    runner::run(
        scenarios::pull_merge_upstream_takes_in_a_dropped_member_beside_local_lock_commits(),
    )
    .assert_green();
}

#[test]
fn a_create_at_a_diverged_branch_refuses_naming_at_and_at_starts_at_the_local_branch() {
    runner::run(scenarios::create_diverged_at()).assert_fully_decided_green();
}

#[test]
fn a_create_leaves_the_parents_uncommitted_manifest_where_it_is_and_says_so() {
    runner::run(scenarios::workweave_create_dirty_parent()).assert_fully_decided_green();
}

#[test]
fn the_same_create_twice_returns_the_workweave_and_a_different_spelling_refuses() {
    runner::run(scenarios::dc19_idempotent_create()).assert_fully_decided_green();
}

#[test]
fn set_mode_in_the_primary_leaves_the_workweave_on_that_branch_untouched() {
    runner::run(scenarios::dc08_set_mode_own_workspace()).assert_fully_decided_green();
}

#[test]
fn a_detached_review_workspace_keeps_an_unrelated_lib_commit_on_a_branch_then_returns_to_its_pin() {
    runner::run(scenarios::detached_review_lib_fix()).assert_fully_decided_green();
}

macro_rules! pending_on {
    ($($name:ident => $verb:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                runner::run(scenarios::$name()).assert_pending_on(MissingVerb::$verb);
            }
        )*
    };
}

pending_on!(
    resume_on_machine_b => Land,
    feature_in_primary_while_main_moves => Land,
    feature_off_stable => Land,
    review_colleague_fix => Review,
    stable_end_to_end => Land,
    detached_head_in_workweave => Land,
    switch_c_wip_in_workweave => Land,
    worktree_add_force_on_weave_branch => Land,
    main_change_into_stable => Land,
    cherry_pick_onto_stable => CherryPick,
);

#[test]
fn an_inactive_projects_landing_composes_in_the_source() {
    runner::run(scenarios::shared_member_declared_dev()).assert_fully_decided_green();
    runner::run(scenarios::shared_member_pinned_behind()).assert_fully_decided_green();
}

#[test]
fn an_inactive_projects_landing_prints_choices_that_run_for_that_project() {
    runner::run(scenarios::inactive_project_choices()).assert_fully_decided_green();
}

#[test]
fn a_cross_repo_pr_publishes_members_record_and_project_and_an_amend_needs_a_named_lease() {
    runner::run(scenarios::cross_repo_pr()).assert_fully_decided_green();
}

#[test]
fn a_cross_repo_pr_through_a_fork_remote_is_judged_by_the_push_target() {
    runner::run(scenarios::cross_repo_pr_fork()).assert_fully_decided_green();
}

#[test]
fn a_pr_closed_unmerged_reports_its_gone_branches_and_publishes_none() {
    runner::run(scenarios::pr_closed_unmerged()).assert_fully_decided_green();
}

#[test]
fn a_dependency_member_is_pushed_only_when_named_and_the_gate_reads_the_url_remote() {
    runner::run(scenarios::dc03_roles_bound_push()).assert_fully_decided_green();
}

#[test]
fn push_refuses_an_unpinned_member_commit_and_never_writes_the_lock() {
    runner::run(scenarios::dc13_push_no_lock()).assert_fully_decided_green();
}

#[test]
fn a_raw_project_switch_refuses_each_member_and_never_the_project() {
    runner::run(scenarios::dc22_raw_project_switch()).assert_fully_decided_green();
}

#[test]
fn a_records_conflict_stays_on_the_machine_that_met_it_and_is_never_published() {
    runner::run(scenarios::records_conflict()).assert_fully_decided_green();
}

#[test]
fn a_remote_refusing_the_records_branch_publishes_a_feature_and_the_trunk_with_the_fact() {
    runner::run(scenarios::records_rejected()).assert_fully_decided_green();
}

#[test]
fn only_the_first_push_creates_the_records_branch_on_a_remote() {
    runner::run(scenarios::records_first_push()).assert_fully_decided_green();
}

#[test]
fn a_drop_meeting_a_branch_kept_elsewhere_reaches_a_fixed_point() {
    runner::run(scenarios::records_drop_per_machine()).assert_fully_decided_green();
}

#[test]
fn a_record_written_offline_is_published_by_the_next_push_and_nothing_drops_meanwhile() {
    runner::run(scenarios::records_offline_write()).assert_fully_decided_green();
}

#[test]
fn unpushed_branches_and_minted_seats_never_reach_a_remote() {
    runner::run(scenarios::records_unpushed_stay_local()).assert_fully_decided_green();
}

#[test]
fn a_pin_one_side_moved_backward_by_name_survives_a_merge_of_the_other() {
    let sandbox = Sandbox::new(Seed::default());
    let report = runner::run_in(&sandbox, scenarios::three_way_pins());
    report.assert_fully_decided_green();
    let pinned = sandbox.git_ok(
        &sandbox.weave_root(Machine::A).join("projects/proj"),
        &["show", "HEAD:rwv.lock"],
    );
    let origin = sandbox.remote_path(Remote::origin(Repo::App));
    let one_back = sandbox.git_ok(&origin, &["rev-parse", "main~1"]);
    assert!(pinned.contains(&one_back), "{pinned}");
}

#[cfg(unix)]
#[test]
fn the_merge_without_merge_tree_gives_the_same_pins() {
    runner::run(scenarios::three_way_pins_on_old_git()).assert_fully_decided_green();
}

#[test]
fn c3_is_printed_under_an_unpinned_commit_at_the_push_branch_and_run_keeps_a_backup() {
    runner::run(scenarios::dc14_c3_under_pin_ahead()).assert_fully_decided_green();
}

#[test]
fn a_merge_that_adds_a_member_clones_it_and_settles_its_files() {
    let sandbox = Sandbox::new(Seed::default().integration());
    let report = runner::run_in(
        &sandbox,
        scenarios::dc09_integrations_settle(Seed::default().integration()),
    );
    report.assert_fully_decided_green();
    let project = sandbox.weave_root(Machine::B).join("projects/proj");
    assert_eq!(
        sandbox
            .git_ok(&project, &["rev-list", "--parents", "-n1", "HEAD"])
            .split(' ')
            .count(),
        3,
        "the project commit is a merge"
    );
    assert!(sandbox
        .weave_root(Machine::B)
        .join(Repo::Docs.rel_path())
        .exists());
}

#[test]
fn a_member_merge_that_conflicts_stops_and_continue_finishes() {
    runner::run(scenarios::merge_conflict_continues()).assert_fully_decided_green();
}

#[test]
fn a_pull_merge_resolves_a_declared_derived_path_to_the_upstream() {
    runner::run(scenarios::pull_merge_resolves_derived_to_the_upstream())
        .assert_fully_decided_green();
}

#[test]
fn a_pull_merge_still_stops_on_an_undeclared_path_and_continue_finishes() {
    runner::run(scenarios::pull_merge_still_stops_on_an_undeclared_path())
        .assert_fully_decided_green();
}

#[test]
fn a_pull_merge_resolves_a_derived_path_of_the_project_repo_to_the_upstream() {
    runner::run(scenarios::pull_project_merge_resolves_derived_to_the_upstream())
        .assert_fully_decided_green();
}

#[test]
fn a_member_merge_that_conflicts_stops_and_abort_restores() {
    runner::run(scenarios::merge_conflict_aborts()).assert_fully_decided_green();
}

#[test]
fn a_member_merge_that_conflicts_leaves_no_project_commit_until_it_continues() {
    let sandbox = Sandbox::new(Seed::default());
    let mut steps = scenarios::both_sides_move_app_conflictingly();
    steps.extend(scenarios::resolve_app_conflict_and_continue());
    runner::run_in(
        &sandbox,
        Scenario {
            name: "member conflict",
            seed: Seed::default(),
            steps,
        },
    )
    .assert_green();
    let project = sandbox.weave_root(Machine::B).join("projects/proj");
    let parents = sandbox.git_ok(&project, &["rev-list", "--parents", "-n1", "HEAD"]);
    assert_eq!(parents.split(' ').count(), 3, "{parents}");
    assert!(!sandbox.weave_root(Machine::B).join(".rwv-op").exists());
    assert!(
        !sandbox
            .weave_root(Machine::B)
            .join(Repo::Docs.rel_path())
            .exists(),
        "the dropped member's checkout is pruned once every member has merged"
    );
}

#[test]
fn a_branch_checked_out_nowhere_merges_as_refs_and_touches_no_working_tree() {
    runner::run(scenarios::merge_upstream_as_refs()).assert_fully_decided_green();
}

#[test]
fn a_reset_to_the_upstream_keeps_a_backup_and_acts_only_on_the_repo_it_names() {
    runner::run(scenarios::reset_acts_only_on_the_named_repo()).assert_fully_decided_green();
}

#[test]
fn a_waiver_covers_the_merge_it_re_pins_after_and_is_listed() {
    runner::run(scenarios::waiver_per_merge()).assert_fully_decided_green();
}

fn a() -> Place {
    Place::Primary(Machine::A)
}

fn ww(name: &str) -> Place {
    Place::ww(Machine::A, name)
}

fn only(report: &Report) -> &Violation {
    assert_eq!(report.violations.len(), 1, "{report}");
    &report.violations[0].1
}

fn tip(sandbox: &Sandbox, dir: &std::path::Path, rev: &str) -> String {
    sandbox.git_ok(dir, &["rev-parse", rev])
}

#[test]
fn snapshot_reads_every_lock_site_and_ref_store() {
    let sandbox = Sandbox::new(Seed::default());
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "one workweave",
            seed: Seed::default(),
            steps: vec![
                Step::rwv(&a(), &["workweave", "create", "--project", "proj", "x"])
                    .expect(Exit::Succeeds),
            ],
        },
    );
    report.assert_green();
    let snapshot = invariants::snapshot(&sandbox);
    let branch = |store, name: &str| LockSite::Branch(store, Repo::Proj, name.to_owned());
    let sites = [
        LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
        LockSite::Checkout(ww("x"), Repo::Proj, "proj--x".to_owned()),
        branch(Store::Remote(RemoteKind::Origin), "main"),
        branch(Store::Clone(Machine::A), "main"),
        branch(Store::Clone(Machine::A), "proj--x"),
    ];
    for site in &sites {
        let members: Vec<Repo> = snapshot
            .pins
            .get(site)
            .unwrap_or_else(|| panic!("no pins read at {site:?}: {:?}", snapshot.pins))
            .keys()
            .copied()
            .collect();
        assert_eq!(members, Repo::MEMBERS.to_vec(), "{site:?}");
    }
    assert_eq!(snapshot.pins.len(), sites.len(), "{:?}", snapshot.pins);
    let declared_on_main: Vec<(Repo, Declaration)> = Repo::MEMBERS
        .into_iter()
        .map(|member| {
            (
                member,
                Declaration {
                    branch: "main".to_owned(),
                    role: Role::Owned,
                },
            )
        })
        .collect();
    for site in &sites[2..] {
        let declared: Vec<(Repo, Declaration)> = snapshot.declares[site]
            .iter()
            .map(|(member, declaration)| (*member, declaration.clone()))
            .collect();
        assert_eq!(declared, declared_on_main, "{site:?}");
    }
    assert_eq!(snapshot.declares.len(), sites.len() - 2);
    for repo in [Repo::Proj, Repo::App, Repo::Lib, Repo::Docs] {
        assert!(
            snapshot.reachable_from(repo) > 0,
            "nothing reachable in {repo:?}"
        );
    }
    assert_eq!(
        snapshot.remote_refs.keys().copied().collect::<Vec<_>>(),
        [Repo::Proj, Repo::App, Repo::Lib, Repo::Docs]
            .into_iter()
            .map(Remote::origin)
            .collect::<Vec<_>>()
    );
}

#[test]
fn stranded_check_catches_a_reset_hard() {
    let report = bad_steps(
        "reset --hard strands a commit",
        vec![
            Step::commit(&a(), Repo::App, "doomed"),
            Step::git(&a(), Repo::App, &["reset", "--hard", "HEAD~1"]),
        ],
    );
    let label = "git -C app reset --hard HEAD~1";
    assert_eq!(
        fired(&report),
        vec![
            (label, Invariant::NoStrandedCommit),
            (label, Invariant::DeclaredBranchesMoveForward)
        ],
        "{report}"
    );
    assert_eq!(
        report.violations[0].1,
        Violation::Stranded {
            repo: Repo::App,
            commit: report.marks["doomed"].clone(),
        },
        "{report}"
    );
}

#[test]
fn may_strand_spares_a_step_that_names_the_repo() {
    let report = bad_steps(
        "reset --hard, the strand allowed",
        vec![
            Step::commit(&a(), Repo::App, "doomed"),
            Step::git(&a(), Repo::App, &["reset", "--hard", "HEAD~1"])
                .may_strand(&[Repo::App])
                .may_rewrite(&[Repo::App]),
        ],
    );
    report.assert_green();
}

#[test]
fn stranded_check_catches_a_deleted_branch_in_a_workweave() {
    let report = bad_steps(
        "branch -D strands a commit",
        vec![
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "x"]),
            Step::git(&ww("x"), Repo::Lib, &["switch", "-c", "side"]),
            Step::commit(&ww("x"), Repo::Lib, "side-work"),
            Step::git(&ww("x"), Repo::Lib, &["switch", "proj--x"]),
            Step::git(&ww("x"), Repo::Lib, &["branch", "-D", "side"]),
        ],
    );
    assert_eq!(
        report.violations,
        vec![(
            "git -C lib branch -D side".to_owned(),
            Violation::Stranded {
                repo: Repo::Lib,
                commit: report.marks["side-work"].clone(),
            }
        )],
        "{report}"
    );
}

#[test]
fn pin_check_catches_a_project_pushed_ahead_of_its_member() {
    let b = Place::Primary(Machine::B);
    let report = bad_steps(
        "project pushed without its member",
        vec![
            Step::new_machine(Machine::B),
            Step::commit(&a(), Repo::App, "unpushed"),
            Step::rwv(&a(), &["lock", "--commit"]),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            Step::git(&b, Repo::Proj, &["pull", "--ff-only"]),
        ],
    );
    let unresolved = |site| Violation::UnresolvedPin {
        site,
        member: Repo::App,
        pin: report.marks["unpushed"].clone(),
    };
    let pull = "git -C proj pull --ff-only".to_owned();
    assert_eq!(
        report.violations,
        vec![
            (
                "git -C proj push origin main".to_owned(),
                unresolved(LockSite::remote_main(Repo::Proj))
            ),
            (
                pull.clone(),
                unresolved(LockSite::Checkout(b, Repo::Proj, "main".to_owned()))
            ),
            (
                pull,
                unresolved(LockSite::Branch(
                    Store::Clone(Machine::B),
                    Repo::Proj,
                    "main".to_owned()
                ))
            ),
        ],
        "{report}"
    );
}

/// A lock committed on a branch no workspace has checked out and no remote
/// calls `main`: before every live project branch was a lock site, nothing
/// read it.
#[test]
fn pin_check_reads_a_lock_on_a_branch_nothing_has_checked_out() {
    let sandbox = Sandbox::new(Seed::default());
    let gone = "1".repeat(40);
    let lock = common::fixture_lock_bytes(&[(
        &Repo::App.rel_path(),
        &sandbox.remote_url(Remote::origin(Repo::App)),
        &gone,
    )]);
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "a lock on stable",
            seed: Seed::default(),
            steps: vec![
                Step::git(&a(), Repo::Proj, &["branch", "stable"]),
                Step::hand_commit(&a(), Repo::Proj, "stable", &[("rwv.lock", &lock)]),
                Step::git(&a(), Repo::Proj, &["push", "origin", "stable"])
                    .pushes(Remote::origin(Repo::Proj), "stable"),
            ],
        },
    );
    let unresolved = |store| Violation::UnresolvedPin {
        site: LockSite::Branch(store, Repo::Proj, "stable".to_owned()),
        member: Repo::App,
        pin: gone.clone(),
    };
    assert_eq!(
        report.violations,
        vec![
            (
                "hand-commit rwv.lock on proj stable".to_owned(),
                unresolved(Store::Clone(Machine::A))
            ),
            (
                "git -C proj push origin stable".to_owned(),
                unresolved(Store::Remote(RemoteKind::Origin))
            ),
        ],
        "{report}"
    );
}

fn rewind_steps(at: &Place, may_rewind: &[Repo]) -> Vec<Step> {
    vec![
        Step::commit(at, Repo::App, "ahead"),
        Step::rwv(at, &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(at, Repo::App, &["branch", "keep"]),
        Step::git(at, Repo::App, &["reset", "--hard", "HEAD~1"]).may_rewrite(&[Repo::App]),
        Step::rwv(
            at,
            &["member", "pin", &Repo::App.rel_path(), "HEAD", "--commit"],
        )
        .expect(Exit::Succeeds)
        .may_rewind(may_rewind),
    ]
}

fn rewind_label() -> String {
    format!("rwv member pin {} HEAD --commit", Repo::App.rel_path())
}

#[test]
fn rewind_check_catches_a_relock_after_reset_in_the_primary() {
    let report = bad_steps("relock after reset", rewind_steps(&a(), &[]));
    let label = rewind_label();
    let relock = (label.as_str(), Invariant::NoPinRewound);
    assert_eq!(fired(&report), vec![relock, relock], "{report}");
    let rewound: Vec<(&LockSite, Repo, Vec<String>)> = report
        .violations
        .iter()
        .map(|(_, v)| {
            let Violation::PinRewound {
                site,
                member,
                dropped,
                ..
            } = v
            else {
                unreachable!()
            };
            (site, *member, dropped.clone())
        })
        .collect();
    let dropped = vec![report.marks["ahead"].clone()];
    assert_eq!(
        rewound,
        vec![
            (
                &LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                Repo::App,
                dropped.clone()
            ),
            (
                &LockSite::Branch(Store::Clone(Machine::A), Repo::Proj, "main".to_owned()),
                Repo::App,
                dropped
            ),
        ]
    );
}

#[test]
fn rewind_check_catches_a_relock_after_reset_in_a_workweave() {
    let mut steps = vec![Step::rwv(
        &a(),
        &["workweave", "create", "--project", "proj", "x"],
    )];
    steps.extend(rewind_steps(&ww("x"), &[]));
    let report = bad_steps("relock after reset in a workweave", steps);
    let label = rewind_label();
    let relock = (label.as_str(), Invariant::NoPinRewound);
    assert_eq!(fired(&report), vec![relock, relock], "{report}");
}

#[test]
fn rewind_check_spares_a_step_that_names_the_member() {
    let report = bad_steps("declared rewind", rewind_steps(&a(), &[Repo::App]));
    report.assert_green();
}

#[test]
fn pin_check_reads_a_commit_no_ref_reaches_as_unresolved() {
    let report = bad_steps(
        "remote branch deleted under a pin",
        vec![
            Step::commit(&a(), Repo::App, "feat-app"),
            Step::git(&a(), Repo::App, &["push", "origin", "main:feat-x"])
                .pushes(Remote::origin(Repo::App), "feat-x"),
            Step::rwv(&a(), &["lock", "--commit"]),
            Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
                .pushes(Remote::origin(Repo::Proj), "main"),
            Step::forge(Forge::DeleteBranch {
                repo: Repo::App,
                branch: "feat-x",
            }),
        ],
    );
    assert_eq!(
        report.violations,
        vec![(
            "forge: DeleteBranch { repo: App, branch: \"feat-x\" }".to_owned(),
            Violation::UnresolvedPin {
                site: LockSite::remote_main(Repo::Proj),
                member: Repo::App,
                pin: report.marks["feat-app"].clone(),
            }
        )],
        "{report}"
    );
}

fn feature_pushed(repo: Repo, mark: &'static str) -> Vec<Step> {
    vec![
        Step::git(&a(), repo, &["switch", "-c", "feat-x"]),
        Step::commit(&a(), repo, mark),
        Step::git(&a(), repo, &["push", "origin", "feat-x"]).pushes(Remote::origin(repo), "feat-x"),
    ]
}

#[test]
fn remote_write_check_catches_an_undeclared_push() {
    let report = bad_steps(
        "an undeclared push",
        vec![
            Step::git(&a(), Repo::App, &["switch", "-c", "feat-x"]),
            Step::commit(&a(), Repo::App, "feat-app"),
            Step::git(&a(), Repo::App, &["push", "origin", "feat-x"]),
        ],
    );
    assert_eq!(
        only(&report),
        &Violation::UnattributedRemoteWrite {
            remote: Remote::origin(Repo::App),
            refname: "refs/heads/feat-x".to_owned(),
            change: RefChange {
                kind: ChangeKind::Created,
                from: None,
                to: Some(report.marks["feat-app"].clone()),
            },
        }
    );
}

#[test]
fn remote_write_check_catches_an_undeclared_delete() {
    let mut steps = feature_pushed(Repo::App, "feat-app");
    steps.push(Step::git(
        &a(),
        Repo::App,
        &["push", "origin", ":refs/heads/feat-x"],
    ));
    let report = bad_steps("an undeclared delete", steps);
    assert_eq!(
        only(&report),
        &Violation::UnattributedRemoteWrite {
            remote: Remote::origin(Repo::App),
            refname: "refs/heads/feat-x".to_owned(),
            change: RefChange {
                kind: ChangeKind::Deleted,
                from: Some(report.marks["feat-app"].clone()),
                to: None,
            },
        }
    );
}

#[test]
fn forge_force_push_rewrites_the_branch_under_its_declaration() {
    let mut steps = feature_pushed(Repo::App, "feat-app");
    steps.extend(Step::forge_force_push(Repo::App, "feat-x", "amended"));
    let sandbox = Sandbox::new(Seed::default());
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "a colleague force-pushes feat-x",
            seed: Seed::default(),
            steps,
        },
    );
    report.assert_green();
    let origin = sandbox.remote_store(Remote::origin(Repo::App));
    let rewritten = tip(&sandbox, &origin, "refs/heads/feat-x");
    assert_eq!(rewritten, report.marks["amended"]);
    let files = sandbox.git_ok(&origin, &["show", "--name-only", "--format=", &rewritten]);
    assert_eq!(files, "amended.txt\nfeat-app.txt", "{report}");
    let parent = tip(&sandbox, &origin, &format!("{rewritten}~1"));
    assert_eq!(parent, tip(&sandbox, &origin, "refs/heads/main"));
}

#[test]
fn remote_write_check_catches_a_rewrite_declared_as_a_push() {
    let mut steps = feature_pushed(Repo::App, "feat-app");
    steps.extend(Step::forge_force_push(Repo::App, "feat-x", "amended"));
    steps.last_mut().unwrap().allow.writes = vec![RemoteWrite::Push(
        Remote::origin(Repo::App),
        "feat-x".to_owned(),
    )];
    let report = bad_steps("a force-push declared as a push", steps);
    assert_eq!(
        only(&report),
        &Violation::UnattributedRemoteWrite {
            remote: Remote::origin(Repo::App),
            refname: "refs/heads/feat-x".to_owned(),
            change: RefChange {
                kind: ChangeKind::Rewritten,
                from: Some(report.marks["feat-app"].clone()),
                to: Some(report.marks["amended"].clone()),
            },
        }
    );
}

#[test]
fn forge_rename_of_the_default_branch_moves_the_default_with_it() {
    let sandbox = Sandbox::new(Seed::default());
    let origin = sandbox.remote_store(Remote::origin(Repo::App));
    let main = tip(&sandbox, &origin, "refs/heads/main");
    let mut steps = Step::forge_rename_branch(Repo::App, "main", "trunk");
    steps.extend(Step::forge_rename_branch(Repo::Lib, "dev", "develop"));
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "the forge renames app's main and lib's dev",
            seed: Seed::default(),
            steps,
        },
    );
    report.assert_green();
    let heads = sandbox.git_ok(
        &origin,
        &["for-each-ref", "--format=%(refname)", "refs/heads/"],
    );
    assert_eq!(heads, "refs/heads/trunk");
    assert_eq!(tip(&sandbox, &origin, "refs/heads/trunk"), main);
    assert_eq!(
        sandbox.git_ok(&origin, &["symbolic-ref", "HEAD"]),
        "refs/heads/trunk"
    );
    let lib = sandbox.remote_store(Remote::origin(Repo::Lib));
    assert_eq!(
        sandbox.git_ok(&lib, &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(
        sandbox.git_ok(
            &lib,
            &["for-each-ref", "--format=%(refname)", "refs/heads/"]
        ),
        "refs/heads/develop\nrefs/heads/main"
    );
}

#[test]
fn forge_rebase_merge_replays_the_branch_onto_the_moved_target() {
    let mut steps = feature_pushed(Repo::App, "feat-app");
    steps.push(Step::forge(Forge::Commit {
        repo: Repo::App,
        branch: "main",
        mark: "main-moved",
    }));
    steps.extend(Step::forge_rebase_merge(Repo::App, "feat-x", "main"));
    let sandbox = Sandbox::new(Seed::default());
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "the forge rebase-merges feat-x",
            seed: Seed::default(),
            steps,
        },
    );
    report.assert_green();
    let origin = sandbox.remote_store(Remote::origin(Repo::App));
    let landed = tip(&sandbox, &origin, "refs/heads/main");
    assert_ne!(landed, report.marks["feat-app"], "a copy, not the commit");
    assert_eq!(
        tip(&sandbox, &origin, &format!("{landed}^1")),
        report.marks["main-moved"]
    );
    let patch = |commit: &str| {
        let diff = sandbox.git_ok_raw(&origin, &["show", commit]);
        sandbox.git_with_stdin(&origin, &["patch-id", "--stable"], &diff)[..40].to_owned()
    };
    assert_eq!(patch(&landed), patch(&report.marks["feat-app"]));
}

#[test]
fn declared_branch_check_catches_a_rewind_the_step_does_not_name() {
    let report = bad_steps(
        "main reset to its upstream, unnamed",
        vec![
            Step::commit(&a(), Repo::App, "local"),
            Step::git(&a(), Repo::App, &["reset", "--hard", "origin/main"])
                .may_strand(&[Repo::App]),
        ],
    );
    let Violation::DeclaredMovedBack {
        repo,
        store,
        branch,
        from,
        ..
    } = only(&report)
    else {
        panic!("{report}")
    };
    assert_eq!(
        (*repo, *store, branch.as_str(), from),
        (
            Repo::App,
            Store::Clone(Machine::A),
            "main",
            &report.marks["local"]
        )
    );
}

#[test]
fn declared_branch_check_spares_a_named_move_to_its_upstream() {
    let report = bad_steps(
        "main reset to its upstream, named",
        vec![
            Step::commit(&a(), Repo::App, "local"),
            Step::git(&a(), Repo::App, &["reset", "--hard", "origin/main"])
                .may_strand(&[Repo::App])
                .may_rewrite(&[Repo::App]),
        ],
    );
    report.assert_green();
}

#[test]
fn declared_branch_check_holds_a_named_move_to_a_tip_no_upstream_has() {
    let report = bad_steps(
        "main reset to a side branch, named",
        vec![
            Step::git(&a(), Repo::App, &["switch", "-c", "side"]),
            Step::commit(&a(), Repo::App, "side"),
            Step::git(&a(), Repo::App, &["switch", "main"]),
            Step::commit(&a(), Repo::App, "local"),
            Step::git(&a(), Repo::App, &["reset", "--hard", "side"])
                .may_strand(&[Repo::App])
                .may_rewrite(&[Repo::App]),
        ],
    );
    assert_eq!(
        only(&report),
        &Violation::DeclaredMovedBack {
            repo: Repo::App,
            store: Store::Clone(Machine::A),
            branch: "main".to_owned(),
            from: report.marks["local"].clone(),
            to: report.marks["side"].clone(),
        }
    );
}

/// `main` moved off `local` to `side`, a tip its old one does not contain,
/// by a step naming `app` in `may_restore`: spared only where a backup of
/// rwv's held `side` before the step.
fn main_restored_to_side(kept: bool) -> Report {
    let mut steps = vec![
        Step::git(&a(), Repo::App, &["switch", "-c", "side"]),
        Step::commit(&a(), Repo::App, "side"),
        Step::git(&a(), Repo::App, &["switch", "main"]),
    ];
    if kept {
        steps.push(Step::git(
            &a(),
            Repo::App,
            &["update-ref", "refs/rwv/backup/main/kept", "side"],
        ));
    }
    steps.extend([
        Step::commit(&a(), Repo::App, "local"),
        Step::git(&a(), Repo::App, &["reset", "--hard", "side"])
            .may_strand(&[Repo::App])
            .may_restore(&[Repo::App]),
    ]);
    bad_steps("main restored to a side tip", steps)
}

#[test]
fn declared_branch_check_spares_a_named_restore_to_a_tip_rwv_kept() {
    main_restored_to_side(true).assert_green();
}

#[test]
fn declared_branch_check_holds_a_named_restore_to_a_tip_rwv_never_kept() {
    let report = main_restored_to_side(false);
    assert_eq!(
        only(&report),
        &Violation::DeclaredMovedBack {
            repo: Repo::App,
            store: Store::Clone(Machine::A),
            branch: "main".to_owned(),
            from: report.marks["local"].clone(),
            to: report.marks["side"].clone(),
        }
    );
}

const FEATURE_ONTO_MAIN: &str = "mode = \"feature\"\ntarget = \"main\"\nas = \"declared\"\n";

/// `main` fast-forwarded to a merge of itself into a feature: its old tip is
/// now on the merge's second parent, and the feature's history is `main`'s
/// first-parent chain.
fn main_fast_forwarded_onto_a_feature_merge(recorded: bool) -> Vec<Step> {
    let mut steps = Vec::new();
    if recorded {
        steps.push(Step::hand_commit(
            &a(),
            Repo::Proj,
            sandbox::RECORDS_BRANCH,
            &[("feat-x", FEATURE_ONTO_MAIN)],
        ));
    }
    let git = |args: &[&str]| Step::git(&a(), Repo::App, args);
    steps.extend([
        git(&["switch", "-c", "feat-x"]),
        Step::commit(&a(), Repo::App, "feat-1"),
        git(&["switch", "main"]),
        Step::commit(&a(), Repo::App, "main-1"),
        git(&["switch", "feat-x"]),
        git(&["merge", "--no-ff", "--no-edit", "main"]),
        git(&["switch", "main"]),
        git(&["merge", "--ff-only", "feat-x"]),
    ]);
    steps
}

#[test]
fn first_parent_check_catches_a_target_fast_forwarded_off_its_lineage() {
    let sandbox = Sandbox::new(Seed::default());
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "main, a recorded target, fast-forwarded onto a feature merge",
            seed: Seed::default(),
            steps: main_fast_forwarded_onto_a_feature_merge(true),
        },
    );
    let merge = tip(&sandbox, &sandbox.canonical(Machine::A, Repo::App), "main");
    assert_eq!(
        report.violations,
        vec![(
            "git -C app merge --ff-only feat-x".to_owned(),
            Violation::FirstParentsBroken {
                repo: Repo::App,
                store: Store::Clone(Machine::A),
                branch: "main".to_owned(),
                from: report.marks["main-1"].clone(),
                to: merge,
            }
        )],
        "{report}"
    );
}

/// A local `main` at an unpushed commit, and a feature with no commits of
/// its own sitting at the remote's `main`: the feature merges `main` in and
/// `main` is fast-forwarded onto that merge. The merge's first parent is the
/// remote's `main`, which the old local tip already contains, so this is no
/// merge of the upstream: `main`'s unpushed commit has left its first-parent
/// chain.
#[test]
fn first_parent_check_catches_a_merge_with_an_upstream_that_brought_nothing() {
    let sandbox = Sandbox::new(Seed::default());
    let git = |args: &[&str]| Step::git(&a(), Repo::App, args);
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "main fast-forwarded onto a merge of itself into its upstream's tip",
            seed: Seed::default(),
            steps: vec![
                Step::hand_commit(
                    &a(),
                    Repo::Proj,
                    sandbox::RECORDS_BRANCH,
                    &[("feat-x", FEATURE_ONTO_MAIN)],
                ),
                git(&["switch", "-c", "feat-x"]),
                git(&["switch", "main"]),
                Step::commit(&a(), Repo::App, "main-1"),
                git(&["switch", "feat-x"]),
                git(&["merge", "--no-ff", "--no-edit", "main"]),
                git(&["switch", "main"]),
                git(&["merge", "--ff-only", "feat-x"]),
            ],
        },
    );
    let merge = tip(&sandbox, &sandbox.canonical(Machine::A, Repo::App), "main");
    assert_eq!(
        report.violations,
        vec![(
            "git -C app merge --ff-only feat-x".to_owned(),
            Violation::FirstParentsBroken {
                repo: Repo::App,
                store: Store::Clone(Machine::A),
                branch: "main".to_owned(),
                from: report.marks["main-1"].clone(),
                to: merge,
            }
        )],
        "{report}"
    );
}

/// `w`, a target of `x`, moved onto a merge of itself into `main`, the branch
/// it lands into, `main`'s tip as first parent, with no landing standing: the
/// merge a sync writes the wrong way round, taking the target's tip first.
#[test]
fn first_parent_check_catches_a_target_merged_into_the_branch_it_lands_into_with_no_landing() {
    let git = |args: &[&str]| Step::git(&a(), Repo::Proj, args);
    let sandbox = Sandbox::new(Seed::default());
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "w, a target, moved onto its merge into main",
            seed: Seed::default(),
            steps: vec![
                Step::hand_commit(
                    &a(),
                    Repo::Proj,
                    sandbox::RECORDS_BRANCH,
                    &[
                        ("w", FEATURE_ONTO_MAIN),
                        (
                            "x",
                            "mode = \"feature\"\ntarget = \"w\"\nas = \"declared\"\n",
                        ),
                    ],
                ),
                git(&["switch", "-c", "w"]),
                Step::commit(&a(), Repo::Proj, "w-1"),
                git(&["switch", "main"]),
                Step::commit(&a(), Repo::Proj, "main-1"),
                git(&["switch", "--detach", "main"]),
                git(&["merge", "--no-ff", "--no-edit", "w"]),
                git(&["branch", "-f", "w", "HEAD"]),
                git(&["switch", "main"]),
            ],
        },
    );
    let merge = tip(&sandbox, &sandbox.canonical(Machine::A, Repo::Proj), "w");
    assert_eq!(
        report.violations,
        vec![(
            "git -C proj branch -f w HEAD".to_owned(),
            Violation::FirstParentsBroken {
                repo: Repo::Proj,
                store: Store::Clone(Machine::A),
                branch: "w".to_owned(),
                from: report.marks["w-1"].clone(),
                to: merge,
            }
        )],
        "{report}"
    );
}

#[test]
fn first_parent_check_watches_only_branches_a_record_targets() {
    let report = bad_steps(
        "main, no record naming it, fast-forwarded onto a feature merge",
        main_fast_forwarded_onto_a_feature_merge(false),
    );
    report.assert_green();
}

#[test]
fn targets_follow_each_records_as_and_each_members_role() {
    let mut snapshot = Snapshot::default();
    let declares = |branch: &str| {
        [
            (Repo::App, Role::Owned),
            (Repo::Lib, Role::Fork),
            (Repo::Docs, Role::Reference),
        ]
        .into_iter()
        .map(|(member, role)| {
            (
                member,
                Declaration {
                    branch: branch.to_owned(),
                    role,
                },
            )
        })
        .collect()
    };
    let site = |branch: &str| {
        LockSite::Branch(
            Store::Remote(RemoteKind::Origin),
            Repo::Proj,
            branch.to_owned(),
        )
    };
    snapshot.declares.insert(site("main"), declares("main"));
    snapshot.declares.insert(site("w1"), declares("main"));
    snapshot.records.insert(
        (Repo::Proj, Store::Clone(Machine::A)),
        [
            (
                "w1".to_owned(),
                Record::Feature {
                    target: "main".to_owned(),
                    as_mode: Mode::Declared,
                },
            ),
            (
                "w2".to_owned(),
                Record::Feature {
                    target: "w1".to_owned(),
                    as_mode: Mode::Feature,
                },
            ),
            ("main".to_owned(), Record::Declared),
        ]
        .into_iter()
        .collect(),
    );
    let set = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect();
    assert_eq!(
        snapshot.target_branches(),
        [
            (Repo::Proj, set(&["main", "w1"])),
            (Repo::App, set(&["main", "w1"])),
            (Repo::Lib, set(&["main", "w1"])),
            (Repo::Docs, set(&["main"])),
        ]
        .into_iter()
        .collect()
    );
}

const RECORDS_FIXTURE: &[(&str, &str)] = &[
    ("feat-x", FEATURE_ONTO_MAIN),
    ("main", "mode = \"declared\"\n"),
    (
        "team/feat-y",
        "mode = \"feature\"\ntarget = \"feat-x\"\nas = \"feature\"\n",
    ),
    (
        "both",
        "conflict = [\n  { mode = \"feature\", target = \"main\", as = \"declared\" },\n  { mode = \"declared\" },\n]\n",
    ),
    ("later", "mode = \"declared\"\nwritten-by = \"9.9.9\"\n"),
    ("half", "mode = \"feature\"\ntarget = \"main\"\n"),
    ("odd-as", "mode = \"feature\"\ntarget = \"main\"\nas = \"linear\"\n"),
    (
        "three",
        "conflict = [{ mode = \"declared\" }, { mode = \"declared\" }, { mode = \"declared\" }]\n",
    ),
    ("nested", "conflict = [{ conflict = [] }, { mode = \"declared\" }]\n"),
    ("garbled", "mode = \n"),
];

fn expected_records() -> Vec<(&'static str, Option<Record>)> {
    let feature = |target: &str, as_mode| Record::Feature {
        target: target.to_owned(),
        as_mode,
    };
    vec![
        (
            "both",
            Some(Record::Conflict(vec![
                feature("main", Mode::Declared),
                Record::Declared,
            ])),
        ),
        ("feat-x", Some(feature("main", Mode::Declared))),
        ("garbled", None),
        ("half", None),
        ("later", None),
        ("main", Some(Record::Declared)),
        ("nested", None),
        ("odd-as", None),
        ("team/feat-y", Some(feature("feat-x", Mode::Feature))),
        ("three", None),
    ]
}

/// Each record as `expected_records` spells it: an unreadable one as `None`,
/// its reason being the parser's.
fn readable(records: &invariants::Records) -> Vec<(&str, Option<Record>)> {
    records
        .iter()
        .map(|(branch, record)| {
            let read = match record {
                Record::Unreadable(_) => None,
                other => Some(other.clone()),
            };
            (branch.as_str(), read)
        })
        .collect()
}

#[test]
fn records_reader_reads_each_entry_shape_on_a_machine_and_its_remote() {
    let sandbox = Sandbox::new(Seed::default());
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "a records branch committed by hand, then pushed",
            seed: Seed::default(),
            steps: vec![
                Step::hand_commit(&a(), Repo::Proj, sandbox::RECORDS_BRANCH, RECORDS_FIXTURE),
                Step::git(
                    &a(),
                    Repo::Proj,
                    &["push", "origin", sandbox::RECORDS_BRANCH],
                )
                .pushes(Remote::origin(Repo::Proj), sandbox::RECORDS_BRANCH),
            ],
        },
    );
    report.assert_green();
    let snapshot = invariants::snapshot(&sandbox);
    for store in [Store::Clone(Machine::A), Store::Remote(RemoteKind::Origin)] {
        let records = &snapshot.records[&(Repo::Proj, store)];
        assert_eq!(
            records.len(),
            RECORDS_FIXTURE.len(),
            "{store:?}: {records:?}"
        );
        assert_eq!(
            readable(records),
            expected_records(),
            "{store:?}: {records:?}"
        );
    }
    assert_eq!(snapshot.records.len(), 2, "{:?}", snapshot.records);
}

#[test]
fn records_reader_reads_nothing_where_no_records_branch_exists() {
    let sandbox = Sandbox::new(Seed::default());
    runner::run_in(
        &sandbox,
        Scenario {
            name: "no records branch",
            seed: Seed::default(),
            steps: Vec::new(),
        },
    )
    .assert_green();
    let origin = sandbox.remote_store(Remote::origin(Repo::Proj));
    assert_eq!(invariants::read_records(&sandbox, &origin), None);
}

#[test]
fn a_remote_added_only_to_fetch_contributes_no_record_until_it_is_a_push_target() {
    runner::run(scenarios::records_foreign_remote()).assert_fully_decided_green();
}

#[test]
fn a_record_changed_by_a_raw_commit_reads_as_that_entry_and_a_conflict_settles_by_set_mode() {
    runner::run(scenarios::records_hand_commit()).assert_fully_decided_green();
}

#[test]
fn an_entry_this_release_cannot_read_refuses_its_own_branch_alone() {
    runner::run(scenarios::records_unreadable_entry()).assert_fully_decided_green();
}

#[test]
fn a_workweave_makes_its_target_a_branch_the_first_parent_check_watches() {
    let sandbox = Sandbox::new(Seed::default());
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "a workweave onto main",
            seed: Seed::default(),
            steps: vec![
                Step::rwv(&a(), &["workweave", "create", "--project", "proj", "w"])
                    .expect(Exit::Succeeds),
            ],
        },
    );
    report.assert_fully_decided_green();
    let snapshot = invariants::snapshot(&sandbox);
    let watched = snapshot.target_branches();
    assert!(
        watched
            .get(&Repo::Proj)
            .is_some_and(|branches| branches.contains("main")),
        "{watched:?}"
    );
}

#[test]
fn a_clone_records_the_trunk_as_declared_and_the_reader_reads_it() {
    let sandbox = Sandbox::new(Seed::default());
    runner::run_in(
        &sandbox,
        Scenario {
            name: "a fresh clone",
            seed: Seed::default(),
            steps: Vec::new(),
        },
    )
    .assert_green();
    let canonical = sandbox.canonical(Machine::A, Repo::Proj);
    assert_eq!(
        invariants::read_records(&sandbox, &canonical),
        Some(
            [("main".to_owned(), Record::Declared)]
                .into_iter()
                .collect()
        )
    );
}

#[test]
fn forks_and_mirrors_start_as_copies_of_their_origin() {
    let sandbox = Sandbox::new(Seed::default().forks().mirrors());
    let heads = |remote: Remote| {
        sandbox.git_ok(
            &sandbox.remote_store(remote),
            &["for-each-ref", "--format=%(objectname) %(refname)"],
        )
    };
    for member in Repo::MEMBERS {
        let origin = heads(Remote::origin(member));
        for remote in [
            Remote::fork(member),
            Remote::keeping_mirror(member),
            Remote::pruning_mirror(member),
        ] {
            assert_eq!(heads(remote), origin, "{remote:?}");
            assert_ne!(
                sandbox.remote_url(remote),
                sandbox.remote_url(Remote::origin(member))
            );
        }
    }
    assert_eq!(
        sandbox.remotes().len(),
        Repo::MEMBERS.len() * RemoteKind::ALL.len() + 1
    );
}

#[test]
fn the_keeping_mirror_keeps_a_branch_its_origin_dropped_and_the_pruning_one_does_not() {
    let sandbox = Sandbox::new(Seed::default().mirrors());
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "a branch comes and goes on lib's origin",
            seed: Seed::default().mirrors(),
            steps: vec![
                Step::forge(Forge::Commit {
                    repo: Repo::Lib,
                    branch: "short-lived",
                    mark: "short-lived",
                }),
                Step::forge(Forge::Commit {
                    repo: Repo::Lib,
                    branch: "main",
                    mark: "main-moved",
                }),
                Step::forge(Forge::SyncMirrors { repo: Repo::Lib }),
                Step::forge(Forge::DeleteBranch {
                    repo: Repo::Lib,
                    branch: "short-lived",
                }),
                Step::forge(Forge::SyncMirrors { repo: Repo::Lib }),
            ],
        },
    );
    report.assert_green();
    let branches = |remote: Remote| {
        sandbox.git_ok(
            &sandbox.remote_store(remote),
            &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
        )
    };
    assert_eq!(
        branches(Remote::keeping_mirror(Repo::Lib)),
        "dev\nmain\nshort-lived"
    );
    assert_eq!(branches(Remote::pruning_mirror(Repo::Lib)), "dev\nmain");
    for remote in [
        Remote::keeping_mirror(Repo::Lib),
        Remote::pruning_mirror(Repo::Lib),
    ] {
        assert_eq!(
            tip(&sandbox, &sandbox.remote_store(remote), "refs/heads/main"),
            report.marks["main-moved"],
            "{remote:?}"
        );
    }
}

#[test]
fn a_refusing_remote_refuses_the_records_branch_and_takes_the_rest_of_the_push() {
    let seed = Seed::default().refusing_records(Remote::origin(Repo::Proj));
    let sandbox = Sandbox::new(seed.clone());
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "main and the records branch pushed to a remote refusing records",
            seed,
            steps: vec![
                Step::commit(&a(), Repo::Proj, "proj-change"),
                Step::hand_commit(
                    &a(),
                    Repo::Proj,
                    sandbox::RECORDS_BRANCH,
                    &[("main", "mode = \"declared\"\n")],
                ),
                Step::git(
                    &a(),
                    Repo::Proj,
                    &["push", "origin", "main", sandbox::RECORDS_BRANCH],
                )
                .pushes(Remote::origin(Repo::Proj), "main")
                .pushes(Remote::origin(Repo::Proj), sandbox::RECORDS_BRANCH)
                .expect(Exit::Code(1))
                .claim(Claim::RemoteContains {
                    remote: Remote::origin(Repo::Proj),
                    branch: "main",
                    mark: "proj-change",
                }),
            ],
        },
    );
    report.assert_green();
    let origin = sandbox.remote_store(Remote::origin(Repo::Proj));
    assert!(
        !sandbox
            .git(
                &origin,
                &["rev-parse", "--verify", "-q", "refs/heads/rwv-records"]
            )
            .status
            .success(),
        "{report}"
    );
    let push = report.ran.last().unwrap();
    assert!(
        push.output
            .contains("this remote refuses refs/heads/rwv-records"),
        "{}",
        push.output
    );
}

#[test]
fn an_unreachable_remote_refuses_fetches_until_it_is_reachable_again() {
    let app = Remote::origin(Repo::App);
    let report = bad_steps(
        "app's origin goes away and comes back",
        vec![
            Step::unreachable(app),
            Step::git(&a(), Repo::App, &["fetch", "origin"]).expect(Exit::Code(128)),
            Step::reachable(app),
            Step::git(&a(), Repo::App, &["fetch", "origin"]),
        ],
    );
    report.assert_green();
}

#[test]
fn an_outage_a_scenario_leaves_open_ends_with_it() {
    let sandbox = Sandbox::new(Seed::default());
    let app = Remote::origin(Repo::App);
    runner::run_in(
        &sandbox,
        Scenario {
            name: "app's origin left unreachable",
            seed: Seed::default(),
            steps: vec![Step::unreachable(app)],
        },
    )
    .assert_green();
    assert!(sandbox.is_reachable(app));
    assert!(sandbox.remote_path(app).is_dir());
}

#[test]
fn an_unreachable_guard_brings_its_remote_back_when_the_holder_panics() {
    let sandbox = Sandbox::new(Seed::default());
    let app = Remote::origin(Repo::App);
    let held = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _offline = sandbox.unreachable(app);
        assert!(!sandbox.is_reachable(app));
        assert!(!sandbox.remote_path(app).exists());
        panic!("a scenario failing mid-outage");
    }));
    assert!(held.is_err());
    assert!(sandbox.is_reachable(app));
    assert!(sandbox.remote_path(app).is_dir());
}

#[test]
fn members_take_the_roles_the_seed_gives_them() {
    let seed = Seed::default()
        .role(Repo::App, Role::Dependency)
        .role(Repo::Lib, Role::Reference);
    let sandbox = Sandbox::new(seed.clone());
    runner::run_in(
        &sandbox,
        Scenario {
            name: "roles seeded",
            seed,
            steps: Vec::new(),
        },
    )
    .assert_green();
    let roles: Vec<(Repo, Role)> = invariants::snapshot(&sandbox).declares
        [&LockSite::remote_main(Repo::Proj)]
        .iter()
        .map(|(member, declaration)| (*member, declaration.role))
        .collect();
    assert_eq!(
        roles,
        vec![
            (Repo::App, Role::Dependency),
            (Repo::Lib, Role::Reference),
            (Repo::Docs, Role::Owned),
        ]
    );
}

/// A test that holds rwv at a pause point, which only a binary built with the
/// `test-pause` feature has; in any other build it announces its skip.
macro_rules! holding_rwv {
    ($($(#[$meta:meta])* fn $name:ident() $body:block)*) => {
        $(
            $(#[$meta])*
            #[test]
            fn $name() {
                if cfg!(feature = "test-pause") {
                    $body
                } else {
                    common::report_skip(&format!(
                        "{} holds rwv at a pause point, which needs --features test-pause",
                        stringify!($name)
                    ));
                }
            }
        )*
    };
}

const ADVANCE_TARGET: &str = "advance-target";

fn landing_from(name: &'static str) -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["workweave", "create", "--project", "proj", name]).expect(Exit::Succeeds),
        Step::commit(&ww(name), Repo::App, "landing-app"),
    ]
}

fn landed() -> Claim {
    Claim::Contains {
        at: a(),
        repo: Repo::App,
        rev: "main",
        mark: "landing-app",
    }
}

holding_rwv! {
    /// The lease on an overwrite is the tip the push read: a remote that moved
    /// since refuses the replacement of its branch, and keeps what it holds.
    fn an_overwrite_leased_to_a_tip_the_remote_has_moved_past_is_refused() {
        bad_steps(
            "a stale lease",
            scenarios::overwrite_lease_refuses_a_moved_remote(),
        )
        .assert_fully_decided_green();
    }

    /// Two machines publish records at once; the loser's push to the remote
    /// loses, so it fetches, merges and retries, and no entry is lost.
    fn a_records_push_that_loses_the_race_fetches_merges_and_retries() {
        bad_steps("records push race", scenarios::records_push_race()).assert_fully_decided_green();
    }
}

holding_rwv! {
    /// A landing killed after it fast-forwarded the target's checkout: edits
    /// made there are kept as a backup by an abort run there, and refuse an
    /// abort run from the source until they are set aside.
    fn abort_keeps_edits_made_in_the_target_after_the_stop() {
        for stop in scenarios::LandedStop::ALL {
            for from in [scenarios::AbortFrom::Source, scenarios::AbortFrom::Target] {
                runner::run(scenarios::abort_keeps_edits_made_in_the_target_after_the_stop(
                    stop, from,
                ))
                .assert_fully_decided_green();
            }
        }
    }

    /// A receiving branch the landing moved as a ref, deleted before `rwv
    /// abort`: abort refuses, printing a choice that clears it when run.
    fn abort_over_a_receiving_branch_deleted_after_the_landing_moved_it() {
        for stop in scenarios::LandedStop::ALL {
            runner::run(scenarios::abort_over_a_receiving_branch_deleted_after_the_landing_moved_it(stop))
                .assert_fully_decided_green();
        }
    }

    /// Each move is journaled before it is made: killed right after a
    /// target fast-forward, the landing is restored by abort.
    fn abort_restores_a_target_fast_forward_killed_after_the_move() {
        runner::run(scenarios::abort_restores_a_target_fast_forward_killed_after_the_move())
            .assert_fully_decided_green();
    }

    /// A saved branch deleted or held elsewhere keeps abort from switching
    /// the target's checkout back; the printed command clears it.
    fn abort_prints_what_clears_a_blocked_switch_back() {
        for blocked in [
            scenarios::SwitchBackBlocked::Gone,
            scenarios::SwitchBackBlocked::Held,
        ] {
            runner::run(scenarios::abort_switch_back_blocked(blocked)).assert_fully_decided_green();
        }
    }

    /// Edits in the target that keep abort from switching a checkout back
    /// are refused with the choice that sets them aside.
    fn abort_refuses_a_switch_back_over_colliding_edits() {
        for stop in scenarios::LandedStop::ALL {
            runner::run(scenarios::abort_refuses_a_switch_back_over_colliding_edits(stop)).assert_fully_decided_green();
        }
    }

    /// The printed abandon keeps the colliding edits as a backup first.
    fn abort_abandons_colliding_edits_keeping_them() {
        for stop in scenarios::LandedStop::ALL {
            runner::run(scenarios::abort_abandons_colliding_edits_keeping_them(stop)).assert_fully_decided_green();
        }
    }

    /// The stash a refused abort prints sets aside an untracked file in the
    /// way of the restore in the other workspace.
    fn abort_sets_aside_an_untracked_file_in_the_way_in_the_target() {
        runner::run(scenarios::abort_sets_aside_an_untracked_file_in_the_way_in_the_target()).assert_fully_decided_green();
    }

    /// A merge someone started in the target's workspace on a tip the
    /// landing wrote is never canceled by `rwv abort`.
    fn abort_refuses_a_merge_in_a_target_repo_the_landing_moved() {
        for stop in scenarios::LandedStop::ALL {
            runner::run(scenarios::abort_refuses_a_merge_in_a_target_repo_the_landing_moved(stop)).assert_fully_decided_green();
        }
    }
}

holding_rwv! {
    /// After the target's workspace aligned onto a followed receiving
    /// branch, `rwv abort` from either workspace switches it back and leaves
    /// that branch's own work on it.
    fn abort_after_aligning_onto_a_followed_branch_keeps_its_work() {
        for from in [scenarios::AbortFrom::Source, scenarios::AbortFrom::Target] {
            runner::run(scenarios::abort_after_the_target_aligned(
                scenarios::ReceivingCase::Followed,
                from,
            ))
            .assert_fully_decided_green();
        }
    }
}

holding_rwv! {
    /// After the target's workspace aligned onto a receiving branch the
    /// landing moved as a ref, `rwv abort` from either workspace switches it
    /// back, then puts the branch back where it was.
    fn abort_after_aligning_onto_a_branch_moved_as_a_ref_puts_it_back() {
        for from in [scenarios::AbortFrom::Source, scenarios::AbortFrom::Target] {
            runner::run(scenarios::abort_after_the_target_aligned(
                scenarios::ReceivingCase::MovedAsRef,
                from,
            ))
            .assert_fully_decided_green();
        }
    }
}

holding_rwv! {
    /// After the target's workspace aligned onto a receiving branch the
    /// landing created, `rwv abort` from either workspace switches it back,
    /// then deletes the branch, the checkout switched at the commit it was
    /// on included.
    fn abort_after_aligning_onto_a_created_branch_deletes_it() {
        for case in [
            scenarios::ReceivingCase::Created,
            scenarios::ReceivingCase::CreatedWithoutWork,
        ] {
            for from in [scenarios::AbortFrom::Source, scenarios::AbortFrom::Target] {
                runner::run(scenarios::abort_after_the_target_aligned(case, from))
                    .assert_fully_decided_green();
            }
        }
    }
}

/// An abort that judged its plan and then took the target's claim judges
/// again before it begins: a commit made in between refuses it whole.
#[test]
fn an_abort_judges_again_once_it_holds_the_target() {
    runner::run(scenarios::abort_judges_again_once_it_holds_the_target())
        .assert_fully_decided_green();
}

#[test]
fn an_abort_stops_at_the_first_write_that_fails() {
    runner::run(scenarios::abort_stops_at_the_first_write_that_fails())
        .assert_fully_decided_green();
}

#[test]
fn abort_of_a_landing_stopped_in_its_first_replayed_project_commit() {
    runner::run(
        scenarios::abort_of_a_landing_stopped_in_a_replayed_project_commit(
            scenarios::ReplayedCommit::First,
        ),
    )
    .assert_fully_decided_green();
}

#[test]
fn abort_of_a_landing_stopped_in_its_second_replayed_project_commit() {
    runner::run(
        scenarios::abort_of_a_landing_stopped_in_a_replayed_project_commit(
            scenarios::ReplayedCommit::Second,
        ),
    )
    .assert_fully_decided_green();
}

#[test]
fn abort_of_a_landing_over_a_cancel_the_vcs_refuses() {
    runner::run(scenarios::abort_of_a_landing_over_a_cancel_the_vcs_refuses())
        .assert_fully_decided_green();
}

#[test]
fn abort_refuses_while_the_target_cannot_be_read() {
    runner::run(scenarios::abort_refuses_while_the_target_cannot_be_read())
        .assert_fully_decided_green();
}

#[test]
fn leave_target_gives_up_the_target_only_after_the_source_is_restored() {
    runner::run(scenarios::leave_target_gives_up_the_target_only_after_the_source_is_restored())
        .assert_fully_decided_green();
}

holding_rwv! {
    /// An abort killed once it has restored everything and before it ends
    /// the landing: the rerun finds each repo and branch already back, so
    /// nothing is in its way, and it ends the operation.
    fn an_abort_killed_after_restoring_a_landing_is_rerun() {
        for case in [
            scenarios::ReceivingCase::Followed,
            scenarios::ReceivingCase::MovedAsRef,
            scenarios::ReceivingCase::Created,
            scenarios::ReceivingCase::CreatedWithoutWork,
        ] {
            runner::run(scenarios::abort_after_the_target_aligned_rerun_after_a_kill(case))
            .assert_fully_decided_green();
        }
    }
}

holding_rwv! {
    /// `main` checked out nowhere: a landing moves it and its members'
    /// branches as refs, and no checkout moves.
    fn a_target_checked_out_nowhere_lands_as_refs() {
        runner::run(scenarios::landing_as_refs()).assert_fully_decided_green();
    }
}

holding_rwv! {
    /// A receiving branch the landing moves as a ref, beyond `main`'s pin by
    /// commits no lock records, refuses the landing as `pin` unpinned; its
    /// printed choices clear it.
    fn a_target_branch_beyond_its_pin_refuses_a_landing_as_refs() {
        runner::run(scenarios::landing_as_refs_unpinned()).assert_fully_decided_green();
    }
}

holding_rwv! {
    /// A landing as refs that loses one compare-and-swap after moving
    /// another ref stops; abort puts back what it moved; it lands again.
    fn a_landing_as_refs_that_loses_its_race_is_aborted_and_lands_again() {
        runner::run(scenarios::lost_race_abort()).assert_fully_decided_green();
    }

    /// A landing that loses on the first ref it moves stops there, leaving
    /// the refs after it unmoved.
    fn a_landing_that_loses_on_its_first_ref_moves_no_other() {
        runner::run(scenarios::lost_race_on_the_first_ref()).assert_fully_decided_green();
    }

    /// Abort of a landing whose primary source switched to the target puts
    /// the primary back on its feature and the target's refs back.
    fn abort_after_the_primary_switched_to_the_target_restores_both() {
        runner::run(scenarios::abort_after_the_primary_switched_to_the_target())
            .assert_fully_decided_green();
    }
}

holding_rwv! {
    /// A landing as refs killed after composing is aborted or continued,
    /// either way leaving no tip its record does not name.
    fn a_landing_as_refs_killed_after_composing_aborts_or_continues() {
        for stop in [
            scenarios::Dc05::BeforeTheMoveAborted,
            scenarios::Dc05::AtTheMoveAborted,
            scenarios::Dc05::AtTheMoveContinued,
            scenarios::Dc05::LandedAborted,
        ] {
            runner::run(scenarios::dc05_compose_in_source(stop)).assert_fully_decided_green();
        }
    }
}

holding_rwv! {
    /// A landing stopped in its retire has released its target: abort, typed
    /// in the target's workspace, takes it back and restores both sides, and
    /// the landing run again retires the workweave.
    fn abort_after_the_landing_released_its_target_restores_it() {
        runner::run(scenarios::abort_after_the_landing_released_its_target())
            .assert_fully_decided_green();
    }

    /// A target another landing built on after the first released it is
    /// restored whole or not at all: the first's abort refuses before
    /// anything moves, a landing into the target still lands, and
    /// `--leave-target` ends the first leaving the target as it stands.
    fn abort_after_another_landing_built_on_the_target_leaves_it() {
        for on in [scenarios::BuiltOn::SameMember, scenarios::BuiltOn::OtherMember] {
            for at in [
                scenarios::TargetAt::Nowhere,
                scenarios::TargetAt::InPlace,
                scenarios::TargetAt::Primary,
            ] {
                runner::run(scenarios::abort_after_another_landing_built_on_the_target(
                    on,
                    at,
                    scenarios::EndedBy::LeaveTarget,
                ))
                .assert_fully_decided_green();
            }
        }
    }

    /// Both sides foreign: one refusal, one printed command that ends it.
    fn abort_with_both_sides_foreign() {
        runner::run(scenarios::abort_with_both_sides_foreign()).assert_fully_decided_green();
    }

    /// A target checkout switched off its branch refuses the whole abort,
    /// and each printed choice ends it.
    fn abort_over_a_target_checkout_switched_off_its_branch() {
        for end in [
            scenarios::SwitchedOffEnd::LeaveTarget,
            scenarios::SwitchedOffEnd::Abandon,
        ] {
            runner::run(scenarios::abort_over_a_target_checkout_switched_off_its_branch(end))
                .assert_fully_decided_green();
        }
    }

    /// Abort of a landing a member entered with, `main` checked out.
    fn abort_of_a_landing_a_member_entered_with() {
        runner::run(scenarios::abort_of_a_landing_a_member_entered_with())
            .assert_fully_decided_green();
    }

    /// A branch a landing created as its member entered the target, moved
    /// or checked out since, refuses the whole abort before anything moves;
    /// each choice the refusal prints clears it.
    fn abort_after_an_entered_branch_moved() {
        use scenarios::{EnteredEnd, EnteredThen};
        for (then, end) in [
            (EnteredThen::BuiltOn, EnteredEnd::LeaveTarget),
            (EnteredThen::BuiltOn, EnteredEnd::ContinueTheRetire),
            (EnteredThen::HandCommit, EnteredEnd::LeaveTarget),
            (EnteredThen::HandCommit, EnteredEnd::ContinueTheRetire),
            (EnteredThen::CheckedOut, EnteredEnd::LeaveTarget),
            (EnteredThen::CheckedOut, EnteredEnd::ContinueTheRetire),
        ] {
            runner::run(scenarios::abort_after_an_entered_branch_moved(then, end))
                .assert_fully_decided_green();
        }
        runner::run(scenarios::abort_after_an_entered_branch_moved(
            EnteredThen::CheckedOut,
            EnteredEnd::Restore,
        ))
        .assert_fully_decided_green();

    }

    /// The other choice the same refusal prints: finish the first
    /// landing's retire.
    fn abort_after_another_landing_built_on_the_target_continues() {
        for on in [scenarios::BuiltOn::SameMember, scenarios::BuiltOn::OtherMember] {
            runner::run(scenarios::abort_after_another_landing_built_on_the_target(
                on,
                scenarios::TargetAt::Primary,
                scenarios::EndedBy::ContinueTheRetire,
            ))
            .assert_fully_decided_green();
        }
    }

    /// Abort at retire skips an in-place workspace that is gone, and
    /// restores the rest.
    fn abort_at_retire_after_the_in_place_workspace_went() {
        runner::run(scenarios::abort_at_retire_after_the_in_place_workspace_went())
            .assert_fully_decided_green();
    }

    /// Abort at retire takes the target back first, so it refuses while
    /// another landing holds it, and restores once that landing ended.
    fn abort_at_retire_waits_for_the_target_back() {
        runner::run(scenarios::abort_at_retire_waits_for_the_target_back())
            .assert_fully_decided_green();
    }

    /// A landing stopped on a lost race still holds its claim: a second
    /// landing into the target refuses on it, as refs or in a checkout, and
    /// lands once the printed abort ended the first.
    fn a_landing_meets_a_stopped_landing_on_its_claim() {
        runner::run(scenarios::a_landing_meets_a_landing_stopped_as_refs())
            .assert_fully_decided_green();
        runner::run(scenarios::a_landing_meets_a_stopped_landing_in_a_checkout())
            .assert_fully_decided_green();
    }

    /// A target moved while a landing sat killed refuses `--continue`,
    /// naming what moved; the printed abort ends the landing.
    fn continue_refuses_a_target_moved_while_stopped() {
        for moved in [
            scenarios::MovedWhileStopped::SwitchedOffItsBranch,
            scenarios::MovedWhileStopped::RefGone,
            scenarios::MovedWhileStopped::RefMovedOn,
        ] {
            runner::run(scenarios::continue_refuses_a_target_moved_while_stopped(moved))
                .assert_fully_decided_green();
        }
    }

    /// A landing killed after its first move, continued after a commit in
    /// the source, lands what it composed and moves no ref twice.
    fn a_landing_continued_after_its_first_move_lands_what_it_composed() {
        for stop in [scenarios::MovedStop::CheckoutMoved, scenarios::MovedStop::RefsMoved] {
            runner::run(scenarios::continue_after_the_first_move(stop)).assert_fully_decided_green();
        }
    }

    /// A landing killed before it recorded its plan leaves no claim once
    /// aborted.
    fn a_landing_killed_before_its_plan_releases_its_claim_on_abort() {
        runner::run(scenarios::a_landing_killed_before_its_plan_releases_its_claim_on_abort())
            .assert_fully_decided_green();
    }

    /// A landing without retire releases its target's lease before its claim.
    fn a_landing_ending_has_released_its_target() {
        runner::run(scenarios::a_landing_ending_has_released_its_target())
            .assert_fully_decided_green();
    }

    /// A refused abort or `--continue` puts back the claim and the lease it
    /// took, and only those: a landing that had released its target leaves
    /// it free, and one that held it still holds it.
    fn a_refused_command_puts_back_what_it_took() {
        for stop in scenarios::LandedStop::ALL {
            runner::run(scenarios::a_refused_abort_puts_back_what_it_took(stop))
                .assert_fully_decided_green();
        }
        runner::run(scenarios::a_refused_continue_of_a_released_landing_leaves_the_target_free())
            .assert_fully_decided_green();
    }

    /// A landing's record stands before its claim is taken.
    fn a_landing_records_its_op_before_it_claims() {
        runner::run(scenarios::a_landing_records_its_op_before_it_claims())
            .assert_fully_decided_green();
    }

    /// A switch that would check a claimed member branch out refuses.
    fn a_switch_refuses_to_align_a_member_onto_a_claimed_branch() {
        runner::run(scenarios::a_switch_refuses_to_align_a_member_onto_a_claimed_branch())
            .assert_fully_decided_green();
    }

    /// Each rwv verb that would move, delete, rename, re-point or publish a
    /// branch a stopped landing claims refuses, naming the abort that ends it.
    fn a_verb_refuses_a_branch_a_stopped_landing_claims() {
        for verb in scenarios::ClaimedMove::ALL {
            runner::run(scenarios::a_verb_refuses_a_branch_a_stopped_landing_claims(verb))
                .assert_fully_decided_green();
        }
    }

    /// Abort typed in the target's workspace names the one landing it takes
    /// up, and refuses two, printing where each is aborted.
    fn abort_typed_in_the_target_names_the_landing() {
        for two in [false, true] {
            runner::run(scenarios::abort_typed_in_the_target_names_the_landing(two))
                .assert_fully_decided_green();
        }
    }

    /// A landing killed while it waits on the claim leaves a record whose
    /// two printed choices both run: `--continue` waits again, `rwv abort`
    /// leaves nothing.
    fn a_landing_killed_while_it_waits_has_two_working_exits() {
        for how in [scenarios::KilledWaiter::Continued, scenarios::KilledWaiter::Aborted] {
            runner::run(scenarios::a_landing_killed_while_it_waits(how))
                .assert_fully_decided_green();
        }
        runner::run(scenarios::a_landing_killed_while_it_waits_continues_once_free())
            .assert_fully_decided_green();
    }

    /// A landing killed between recording its retire and releasing its
    /// target releases it on `--continue`, before the retire is judged.
    fn a_continued_retire_releases_the_target_first() {
        runner::run(scenarios::a_continued_retire_releases_the_target_first())
            .assert_fully_decided_green();
    }
}

/// A member the port never touches, stopped in the operator's own merge, is
/// left in it by `rwv abort`: no cancel, no reset.
#[test]
fn a_cherry_pick_abort_leaves_a_merge_in_a_member_it_never_touches() {
    runner::run(scenarios::cherry_pick_abort_leaves_an_untouched_members_merge())
        .assert_fully_decided_green();
}

#[test]
fn pull_abort_refuses_a_merge_begun_on_the_operators_commit() {
    runner::run(scenarios::pull_abort_refuses_a_merge_begun_on_the_operators_commit())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_abort_refuses_a_merge_begun_on_the_operators_commit() {
    runner::run(scenarios::cherry_pick_abort_refuses_a_merge_begun_on_the_operators_commit())
        .assert_fully_decided_green();
}

#[test]
fn pull_abort_refuses_a_merge_in_a_member_it_never_merges() {
    runner::run(scenarios::pull_abort_refuses_a_merge_in_a_member_it_never_merges())
        .assert_fully_decided_green();
}

#[test]
fn pull_abort_leaves_a_merge_in_a_member_it_never_merges() {
    runner::run(scenarios::pull_abort_leaves_a_merge_in_a_member_it_never_merges())
        .assert_fully_decided_green();
}

#[test]
fn update_remote_abort_leaves_a_merge_in_a_member_it_never_merges() {
    runner::run(scenarios::update_remote_abort_leaves_a_merge_in_a_member_it_never_merges())
        .assert_fully_decided_green();
}

#[test]
fn update_remote_abort_refuses_a_merge_in_a_member_it_never_merges() {
    runner::run(scenarios::update_remote_abort_refuses_a_merge_in_a_member_it_never_merges())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_abort_names_how_to_put_back_a_deleted_branch() {
    runner::run(scenarios::cherry_pick_abort_names_how_to_put_back_a_deleted_branch())
        .assert_fully_decided_green();
}

/// A branch `update --remote` merged as a ref, moved again before abort,
/// is refused with the `--abandon-foreign-tip` choice that, run, restores it.
#[test]
fn update_remote_abort_over_a_ref_moved_since_prints_the_choice_that_clears_it() {
    runner::run(scenarios::update_remote_abort_over_a_ref_moved_since())
        .assert_fully_decided_green();
}

/// A commit the operator adds on top of a rebase they finished by hand is
/// not read as the operation's: abort refuses, then abandons it into a backup.
#[test]
fn abort_keeps_an_own_commit_on_a_rebase_finished_by_hand() {
    runner::run(scenarios::vcs_continue_mid_rebase_then_an_own_commit())
        .assert_fully_decided_green();
}

/// A cancel the VCS refuses is refused with its message and a choice that,
/// run, keeps the edit and clears the state.
#[test]
fn pull_abort_over_a_cancel_the_vcs_refuses() {
    runner::run(scenarios::pull_abort_over_a_cancel_the_vcs_refuses()).assert_fully_decided_green();
}

/// An operator's merge begun at the savepoint, in a repo the pull merges,
/// is refused as theirs: it takes in nothing the pull journaled.
#[test]
fn pull_abort_refuses_a_merge_the_operator_began_at_the_savepoint() {
    runner::run(scenarios::pull_abort_refuses_a_merge_the_operator_began_at_the_savepoint())
        .assert_fully_decided_green();
}

/// An operator's interactive rebase, stopped at an `edit` with a commit made
/// in it, is refused as theirs, and the abandon keeps that commit.
#[test]
fn abort_refuses_an_interactive_rebase_the_operator_began() {
    runner::run(scenarios::abort_refuses_an_interactive_rebase_the_operator_began())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_abort_keeps_edits_made_after_the_stop() {
    runner::run(scenarios::cherry_pick_abort_keeps_edits_made_after_the_stop())
        .assert_fully_decided_green();
}

holding_rwv! {
    /// A receiving branch the landing moves as a ref, advanced, moved back,
    /// or checked out while the landing is held at its compare-and-swap,
    /// stops the landing; abort leaves the branch as the race left it, and
    /// the landing reruns over it.
    fn a_receiving_branch_moved_as_a_ref_that_loses_its_race_stops_the_landing() {
        for race in [
            scenarios::RefRace::Advanced,
            scenarios::RefRace::Rewound,
            scenarios::RefRace::CheckedOut,
        ] {
            runner::run(scenarios::a_receiving_branch_moved_as_a_ref_loses_its_race(race))
                .assert_fully_decided_green();
        }
    }
}

holding_rwv! {
    /// A commit onto a target repo the landing already moved, made while a
    /// continued landing is about to fast-forward it again, keeps the repo on
    /// the journal: abort refuses rather than leaving the landing's commits.
    fn a_continued_landing_racing_a_commit_keeps_the_repo_journaled() {
        runner::run(scenarios::a_continued_landing_racing_a_commit_on_a_repo_it_moved())
            .assert_fully_decided_green();
    }
}

holding_rwv! {
    /// A reset to the upstream killed after the move, in a checkout or as a
    /// ref, is put back by abort, the move having been journaled first.
    fn abort_restores_a_reset_to_the_upstream_killed_after_the_move() {
        for detached in [false, true] {
            runner::run(scenarios::update_remote_reset_killed_after_the_move(detached))
                .assert_fully_decided_green();
        }
    }
}

holding_rwv! {
    /// A commit on the target's `main` after the landing composed, as a ref
    /// or in its checkout, stops the landing at its read of the savepoint.
    fn a_target_moved_before_the_landing_moves_it_stops_at_the_tip_it_read() {
        for held in [
            scenarios::TargetHeld::AsRef,
            scenarios::TargetHeld::InCheckout,
        ] {
            runner::run(scenarios::a_target_moved_before_the_landing_moves_it(held))
                .assert_fully_decided_green();
        }
    }

    /// A ref the landing moved, then moved again or checked out before
    /// abort, is refused with a choice that, run, lets abort restore it.
    fn abort_over_a_ref_the_landing_moved_refuses_with_a_choice_that_clears_it() {
        for then in [
            scenarios::MovedRefThen::Committed,
            scenarios::MovedRefThen::CheckedOut,
        ] {
            runner::run(scenarios::abort_over_a_ref_the_landing_moved(then))
                .assert_fully_decided_green();
        }
    }
}

holding_rwv! {
    /// A landing interrupted after creating a receiving branch is undone by
    /// `rwv abort`, the created branch deleted.
    fn abort_after_an_interrupted_landing_deletes_the_branch_it_created() {
        runner::run(scenarios::abort_deletes_a_created_receiving_branch())
            .assert_fully_decided_green();
    }

    /// A landing interrupted after creating a receiving branch, continued
    /// under `--json`, reports the branch its stopped run created.
    fn a_continued_landing_reports_the_receiving_branch_its_stopped_run_created() {
        runner::run(scenarios::continue_reports_a_created_receiving_branch())
            .assert_fully_decided_green();
    }

    /// A landing interrupted after composing judges, on `--continue`, the
    /// target's edits in the repos it moves.
    fn a_continued_landing_judges_target_edits_where_it_moves() {
        runner::run(scenarios::land_continue_judges_target_edits_where_it_moves())
            .assert_fully_decided_green();
    }
}

holding_rwv! {
    /// The target's project branch moves while `land` is held at its first
    /// fast-forward, so the landing moves `app`'s `main` and then loses on
    /// the project repo. The route it prints runs as printed: `rwv abort`
    /// restores the `main` the landing moved and leaves the project branch
    /// where someone else moved it, and `rwv land` then lands onto it.
    fn a_landing_held_at_its_compare_and_swap_loses_to_a_target_that_moved() {
        let mut steps = landing_from("held");
        steps.extend([
            Step::rwv_paused(&ww("held"), &["land", "--history", "linear"], ADVANCE_TARGET, "landing"),
            Step::commit(&a(), Repo::Proj, "raced"),
            Step::release("landing")
                .expect(Exit::refuses(Repo::Proj, State::TargetMovedMidOperation))
                .claim(Claim::OutputContains("rwv abort"))
                .claim(Claim::OutputContains("rwv land"))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main",
                    mark: "raced",
                }),
            Step::rwv(&ww("held"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewrite(&[Repo::App])
                .may_rewind(&[Repo::App])
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main",
                    mark: "raced",
                })
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::App,
                }),
            Step::rwv(&ww("held"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(landed())
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main",
                    mark: "raced",
                }),
        ]);
        bad_steps("a landing loses its race", steps).assert_fully_decided_green();
    }

    /// A landing that moved `app`'s `main` and lost on the project repo; a
    /// commit is then made on that `main`. The landing moved it, so `rwv
    /// abort` judges the new tip foreign, and the choice it prints restores
    /// it, keeping the commit in a backup.
    fn an_abort_refuses_a_branch_the_landing_moved_that_moved_again() {
        let mut steps = landing_from("held");
        steps.extend([
            Step::rwv_paused(&ww("held"), &["land"], ADVANCE_TARGET, "landing"),
            Step::commit(&a(), Repo::Proj, "raced"),
            Step::release("landing")
                .expect(Exit::refuses(Repo::Proj, State::TargetMovedMidOperation)),
            Step::commit(&a(), Repo::App, "after"),
            Step::rwv(&ww("held"), &["abort"])
                .expect(Exit::refuses_kind(RefusalKind::ForeignTip))
                .may_rewind(&[Repo::App])
                .claim(Claim::OutputContains("rwv abort --abandon-foreign-tip")),
            Step::rwv(
                &ww("held"),
                &["abort", "--abandon-foreign-tip", "local/remotes/app"],
            )
            .expect(Exit::Succeeds)
            .may_rewrite(&[Repo::App])
            .may_rewind(&[Repo::App])
            .claim(Claim::BackedUp {
                repo: Repo::App,
                mark: "after",
                marked: Some("abandoned"),
            }),
        ]);
        bad_steps("a landed branch moved again", steps).assert_fully_decided_green();
    }

    /// The retire judges the deletion rule against the branch the plan
    /// recorded the landing went into, whatever the target workspace has
    /// checked out by then: switched onto a branch that lacks the work, or
    /// detached, it neither keeps the landed branch nor stops the retire.
    fn a_retire_judges_deletion_against_the_recorded_target() {
        for (name, away) in [
            ("switched", &["switch", "old"][..]),
            ("detached", &["switch", "--detach", "HEAD"][..]),
        ] {
            let mut steps = vec![Step::git(&a(), Repo::Proj, &["branch", "old"])];
            steps.extend(landing_from("held"));
            steps.extend([
                Step::rwv_paused(&ww("held"), &["land", "--retire"], "retire", "landing"),
                Step::git(&a(), Repo::Proj, away),
                Step::release("landing")
                    .expect(Exit::Succeeds)
                    .claim(landed())
                    .claim(Claim::WorkweaveGone(ww("held"))),
            ]);
            bad_steps(
                if name == "switched" {
                    "a retire after the target switched away"
                } else {
                    "a retire after the target detached"
                },
                steps,
            )
            .assert_fully_decided_green();
        }
    }

    /// A target checkout switched to another branch while `land` is held
    /// refuses: the landing moves only the branch its plan names, never the
    /// branch the checkout happens to be on. After `rwv abort`, the landing
    /// run again judges that checkout off its branch, and `rwv update` there
    /// puts it back, after which it lands.
    fn a_landing_moves_only_the_branch_its_plan_names() {
        let mut steps = landing_from("held");
        steps.extend([
            Step::rwv_paused(&ww("held"), &["land"], ADVANCE_TARGET, "landing"),
            Step::git(&a(), Repo::App, &["switch", "-c", "hotfix"]),
            Step::release("landing")
                .expect(Exit::refuses(Repo::App, State::TargetMovedMidOperation))
                .claim(Claim::OutputContains("on 'hotfix', not on 'main'"))
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::App,
                    branch: "hotfix",
                }),
            Step::rwv(&ww("held"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App]),
            Step::rwv(&ww("held"), &["land"])
                .expect(Exit::refuses(Repo::App, State::AlignmentOnBranch))
                .claim(Claim::OutputContains("rwv update")),
            Step::rwv(&a(), &["update"])
                .expect(Exit::Succeeds)
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::App,
                    branch: "main",
                }),
            Step::rwv(&ww("held"), &["land"])
                .expect(Exit::Succeeds)
                .claim(landed()),
        ]);
        bad_steps("a target switched mid-landing", steps).assert_fully_decided_green();
    }

    /// A commit made in the source after the landing composed is not what
    /// the landing fast-forwards: `T`'s project branch receives the tip the
    /// landing composed and relocked, not the source's HEAD by then.
    fn a_landing_fast_forwards_the_tip_it_composed() {
        let mut steps = landing_from("held");
        steps.extend([
            Step::rwv_paused(&ww("held"), &["land"], ADVANCE_TARGET, "landing"),
            Step::commit(&ww("held"), Repo::Proj, "late"),
            Step::release("landing")
                .expect(Exit::Succeeds)
                .claim(landed())
                .claim(Claim::Lacks {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main",
                    mark: "late",
                }),
        ]);
        bad_steps("a source commit after composing", steps).assert_fully_decided_green();
    }

    /// A branch that moves between the fast-forward's ancestry check and the
    /// move itself is the same `target` moved mid-operation: `--continue`
    /// refuses it as the plan's own stop does, and the printed route ends it.
    fn a_branch_moved_inside_the_fast_forward_is_target_moved() {
        let mut steps = landing_from("held");
        steps.extend([
            Step::rwv_paused(&ww("held"), &["land", "--history", "linear"], "advance-target-move", "landing"),
            Step::commit(&a(), Repo::App, "raced"),
            Step::release("landing")
                .expect(Exit::refuses(Repo::App, State::TargetMovedMidOperation))
                .claim(Claim::OutputContains("rwv abort")),
            Step::rwv(&ww("held"), &["land", "--continue"])
                .expect(Exit::refuses(Repo::Proj, State::TargetMovedMidOperation))
                .claim(Claim::OutputContains("local/remotes/app: now at")),
            Step::rwv(&ww("held"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App])
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "raced",
                }),
            Step::rwv(&ww("held"), &["land", "--history", "linear"])
                .expect(Exit::refuses(Repo::App, State::PinUnpinned)),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("held"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::App,
                }),
        ]);
        bad_steps("a branch moved inside the fast-forward", steps)
            .assert_fully_decided_green();
    }

    /// A member's receiving branch moves while `land` is held before
    /// fast-forwarding it: the landing refuses, and `rwv abort` ends the op,
    /// leaving that branch where someone else moved it. The landing run
    /// again judges the commit nothing pins as `pin` unpinned there, whose
    /// printed choice records it, after which the landing lands.
    fn a_landing_that_loses_on_a_member_aborts_leaving_the_member_as_moved() {
        let mut steps = landing_from("held");
        steps.extend([
            Step::rwv_paused(&ww("held"), &["land"], ADVANCE_TARGET, "landing"),
            Step::commit(&a(), Repo::App, "raced"),
            Step::release("landing")
                .expect(Exit::refuses(Repo::App, State::TargetMovedMidOperation))
                .claim(Claim::Contains {
                    at: ww("held"),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "landing-app",
                }),
            Step::rwv(&ww("held"), &["abort"])
                .expect(Exit::Succeeds)
                .may_rewind(&[Repo::App])
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "raced",
                }),
            Step::rwv(&ww("held"), &["land"])
                .expect(Exit::refuses(Repo::App, State::PinUnpinned))
                .claim(Claim::OutputContains("rwv lock --commit")),
            Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&ww("held"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains("local/remotes/app: ff-advanced main to"))
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::App,
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "raced",
                }),
        ]);
        bad_steps("a landing loses its race on a member", steps).assert_fully_decided_green();
    }

    fn a_held_landing_released_unraced_lands() {
        let mut steps = landing_from("held");
        steps.extend([
            Step::rwv_paused(&ww("held"), &["land"], ADVANCE_TARGET, "landing"),
            Step::release("landing").expect(Exit::Succeeds).claim(landed()),
        ]);
        bad_steps("a held landing, unraced", steps).assert_green();
    }

    /// Armed for a point the run never passes, rwv holds nowhere: the step
    /// meant to wait for the hold sees the landing run to completion.
    fn the_pause_holds_only_at_the_point_it_names() {
        let mut steps = landing_from("held");
        steps.push(
            Step::rwv_paused(&ww("held"), &["land"], "no-such-point", "landing")
                .expect(Exit::Undecided)
                .claim(landed()),
        );
        let report = bad_steps("armed for another point", steps);
        report.assert_green();
        let spawn = report.ran.last().unwrap();
        assert!(
            spawn.output.starts_with("exited before pausing at no-such-point"),
            "{}",
            spawn.output
        );
    }

    /// The feature compiled in but nothing armed: the landing passes its
    /// pause point and completes.
    fn an_unarmed_landing_runs_through_its_pause_point() {
        let mut steps = landing_from("unarmed");
        steps.push(
            Step::rwv(&ww("unarmed"), &["land"])
                .expect(Exit::Succeeds)
                .claim(landed()),
        );
        bad_steps("unarmed", steps).assert_green();
    }
}

/// A seat whose `app` holds a merge, with `main` moved on since: a landing
/// would have to rebase the range, which flattens the merge.
fn seat_holding_a_merge_behind_main() -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["workweave", "create", "--project", "proj", "w"]).expect(Exit::Succeeds),
        Step::git(&ww("w"), Repo::App, &["switch", "-c", "side"]),
        Step::commit(&ww("w"), Repo::App, "side-app"),
        Step::git(&ww("w"), Repo::App, &["switch", "proj--w"]),
        Step::commit(&ww("w"), Repo::App, "w-app"),
        Step::git(
            &ww("w"),
            Repo::App,
            &["merge", "--no-ff", "-m", "bump", "side"],
        ),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]
}

/// A target behind its upstream as last fetched lands: the landing takes
/// `main` and the receiving branches in from their upstreams, fast-forward
/// only, and composes onto them, rather than judging the checkouts of
/// `main`'s workspace against the upstream's lock.
#[test]
fn a_landing_into_a_target_behind_its_upstream_takes_the_upstream_in() {
    bad_steps(
        "a landing into a target behind its upstream",
        vec![
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "w"])
                .expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::new_machine(Machine::B),
            Step::commit(&Place::Primary(Machine::B), Repo::App, "b-app"),
            Step::rwv(&Place::Primary(Machine::B), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&Place::Primary(Machine::B), &["push"])
                .pushes(Remote::origin(Repo::App), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .pushes(Remote::origin(Repo::Proj), sandbox::RECORDS_BRANCH)
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "b-app",
                })
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::App,
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::App,
                    mark: "b-app",
                })
                .claim(Claim::OutputContains(
                    "(project): took in 1 commit(s) of 'main' from its upstream",
                ))
                .claim(Claim::OutputContains("local/remotes/app: pin taken in")),
        ],
    )
    .assert_fully_decided_green();
}

holding_rwv! {
    /// A landing into a target behind its upstream, killed before it
    /// fast-forwards anything and continued under `--json`, reports what it
    /// took in of the upstream: its commits and the pin they moved.
    fn a_continued_landing_reports_what_it_took_in_of_the_upstream() {
        bad_steps(
            "a continued landing reports the upstream it took in",
            vec![
                Step::rwv(&a(), &["workweave", "create", "--project", "proj", "w"])
                    .expect(Exit::Succeeds),
                Step::commit(&ww("w"), Repo::Docs, "w-docs"),
                Step::new_machine(Machine::B),
                Step::commit(&Place::Primary(Machine::B), Repo::App, "b-app"),
                Step::rwv(&Place::Primary(Machine::B), &["lock", "--commit"])
                    .expect(Exit::Succeeds),
                Step::rwv(&Place::Primary(Machine::B), &["push"])
                    .pushes(Remote::origin(Repo::App), "main")
                    .pushes(Remote::origin(Repo::Proj), "main")
                    .pushes(Remote::origin(Repo::Proj), sandbox::RECORDS_BRANCH)
                    .expect(Exit::Succeeds),
                Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
                Step::rwv_paused(&ww("w"), &["land", "--history", "linear"], ADVANCE_TARGET, "landing"),
                Step::kill("landing"),
                Step::rwv(&ww("w"), &["land", "--continue", "--json"])
                    .expect(Exit::Succeeds)
                    .claim(Claim::OutputInOrder(&[
                        "\"reports\": [",
                        "\"kind\": \"taken-in\"",
                        "\"branch\": \"main\"",
                        "\"commits\": [",
                        "\"pins\": [",
                        "\"path\": \"local/remotes/app\"",
                    ]))
                    .claim(Claim::PinContains {
                        site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                        member: Repo::App,
                        mark: "b-app",
                    }),
            ],
        )
        .assert_fully_decided_green();
    }
}

/// Machine B moves `lib` on `main` and publishes it; machine A takes in the
/// project repo's upstream alone, so its clone of `lib` lacks the pin.
fn upstream_pins_what_this_clone_lacks(seed: Seed) -> Scenario {
    Scenario {
        name: "the target's upstream pins a commit this clone lacks",
        seed,
        steps: vec![
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "w"])
                .expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::new_machine(Machine::B),
            Step::commit(&Place::Primary(Machine::B), Repo::Lib, "b-lib"),
            Step::rwv(&Place::Primary(Machine::B), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::git(
                &Place::Primary(Machine::B),
                Repo::Lib,
                &["push", "origin", "main"],
            )
            .pushes(Remote::origin(Repo::Lib), "main"),
            Step::rwv(&Place::Primary(Machine::B), &["push"])
                .pushes(Remote::origin(Repo::Proj), "main")
                .pushes(Remote::origin(Repo::Proj), sandbox::RECORDS_BRANCH)
                .expect(Exit::Succeeds),
            Step::git(&a(), Repo::Proj, &["fetch", "origin"]),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Lib, State::PinUnreachable))
                .claim(Claim::OutputContains("rwv fetch")),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::Lib,
                    mark: "b-lib",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::App,
                    mark: "w-app",
                })
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::Lib,
                })
                .claim(Claim::OutputContains("local/remotes/lib: pin taken in")),
        ],
    }
}

/// A pin of the target's upstream this clone lacks would leave the
/// landing's lock pinning a commit the clone cannot reach: the landing
/// refuses it, and the fetch it names clears it.
#[test]
fn a_landing_refuses_an_upstream_pin_this_clone_lacks() {
    runner::run(upstream_pins_what_this_clone_lacks(Seed::default())).assert_fully_decided_green();
}

/// The same for a `dependency` member, which the landing never moves and
/// whose pin it carries from the target as it is.
#[test]
fn a_landing_refuses_an_upstream_dependency_pin_this_clone_lacks() {
    runner::run(upstream_pins_what_this_clone_lacks(
        Seed::default().role(Repo::Lib, Role::Dependency),
    ))
    .assert_fully_decided_green();
}

/// The upstream moved a member the seat never touched, and its branch
/// beyond the pin the upstream's lock records: the landing keeps the
/// upstream's pin, never the branch tip beyond it.
#[test]
fn a_landing_keeps_the_upstream_pin_of_a_member_the_seat_never_touched() {
    bad_steps(
        "the upstream moved a member the seat never touched",
        vec![
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "w"])
                .expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::new_machine(Machine::B),
            Step::commit(&Place::Primary(Machine::B), Repo::Lib, "b-lib-pinned"),
            Step::rwv(&Place::Primary(Machine::B), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&Place::Primary(Machine::B), &["push"])
                .pushes(Remote::origin(Repo::Lib), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .pushes(Remote::origin(Repo::Proj), sandbox::RECORDS_BRANCH)
                .expect(Exit::Succeeds),
            Step::commit(&Place::Primary(Machine::B), Repo::Lib, "b-lib-unpinned"),
            Step::git(
                &Place::Primary(Machine::B),
                Repo::Lib,
                &["push", "origin", "main"],
            )
            .pushes(Remote::origin(Repo::Lib), "main"),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::Lib,
                    mark: "b-lib-pinned",
                })
                .claim(Claim::AtPin {
                    at: a(),
                    repo: Repo::Lib,
                })
                .claim(Claim::Lacks {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "HEAD",
                    mark: "b-lib-unpinned",
                })
                .claim(Claim::OutputContains("local/remotes/lib: pin taken in")),
        ],
    )
    .assert_fully_decided_green();
}

/// The upstream changed the branch `lib` declares, and the seat holds work
/// on `lib`: the landing takes the new declared branch in from its upstream
/// with the rest, lands the work there, says so, and leaves `main`'s
/// workspace on it.
#[test]
fn a_landing_takes_in_an_upstream_change_of_a_declared_branch() {
    bad_steps(
        "the upstream changed a declared branch",
        vec![
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "w"])
                .expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::Lib, "w-lib"),
            Step::new_machine(Machine::B),
            Step::rwv(
                &Place::Primary(Machine::B),
                &[
                    "member",
                    "set-branch",
                    "local/remotes/lib",
                    "line2",
                    "--new",
                ],
            )
            .expect(Exit::Succeeds),
            Step::rwv(&Place::Primary(Machine::B), &["push"])
                .pushes(Remote::origin(Repo::Lib), "line2")
                .pushes(Remote::origin(Repo::Proj), "main")
                .pushes(Remote::origin(Repo::Proj), sandbox::RECORDS_BRANCH)
                .expect(Exit::Succeeds),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "member-changed local/remotes/lib: branch: main -> line2 (taken in",
                ))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "line2",
                    mark: "w-lib",
                })
                .claim(Claim::Lacks {
                    at: a(),
                    repo: Repo::Lib,
                    rev: "main",
                    mark: "w-lib",
                })
                .claim(Claim::OnBranch {
                    at: a(),
                    repo: Repo::Lib,
                    branch: "line2",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::Lib,
                    mark: "w-lib",
                }),
        ],
    )
    .assert_fully_decided_green();
}

/// `main`'s workspace holds an unpushed commit of its own and its upstream
/// moved: the landing refuses the diverged target before anything moves,
/// and after the merge it names, run where `main` is checked out, it lands.
/// That merge takes the upstream as its first parent, as it says, which the
/// first-parents check holds as `main` merged with its own upstream.
#[test]
fn a_landing_into_a_target_diverged_from_its_upstream_refuses_until_merged() {
    let report = bad_steps(
        "a landing into a target diverged from its upstream",
        vec![
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "w"])
                .expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::App, "w-app"),
            Step::new_machine(Machine::B),
            Step::commit(&Place::Primary(Machine::B), Repo::Docs, "b-docs"),
            Step::rwv(&Place::Primary(Machine::B), &["lock", "--commit"]).expect(Exit::Succeeds),
            Step::rwv(&Place::Primary(Machine::B), &["push"])
                .pushes(Remote::origin(Repo::Docs), "main")
                .pushes(Remote::origin(Repo::Proj), "main")
                .pushes(Remote::origin(Repo::Proj), sandbox::RECORDS_BRANCH)
                .expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Proj, "a-notes"),
            Step::rwv(&a(), &["fetch"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land"])
                .expect(Exit::refuses(Repo::Proj, State::PushBranchDiverged))
                .claim(Claim::OutputContains("rwv pull --merge-upstream")),
            Step::rwv(&a(), &["pull", "--merge-upstream"]).expect(Exit::Succeeds),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main",
                    mark: "a-notes",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::Docs,
                    mark: "b-docs",
                })
                .claim(Claim::PinContains {
                    site: LockSite::Checkout(a(), Repo::Proj, "main".to_owned()),
                    member: Repo::App,
                    mark: "w-app",
                }),
        ],
    );
    assert!(
        report.unmet.is_empty() && report.pending.is_none(),
        "{report}"
    );
    assert!(report.violations.is_empty(), "{report}");
}

/// A landing never rebases a member range holding a merge: under `linear` it
/// integrates the range by a merge commit whose first parent is `main`, and
/// reports it `merged`. Without a setting it refuses before anything moves.
/// After `rwv sync --from main` the seat holds `main` only through a merge's
/// second parent, and the landing again makes a merge commit rather than
/// fast-forwarding `main` onto the seat's first parents.
#[test]
fn a_landing_never_rebases_a_member_range_holding_a_merge() {
    let mut steps = seat_holding_a_merge_behind_main();
    steps.extend([
        Step::rwv(&ww("w"), &["land"])
            .expect(Exit::refuses(Repo::App, State::HistoryUnset))
            .claim(Claim::LocalStateUnchanged(ww("w")))
            .claim(Claim::LocalStateUnchanged(a())),
        Step::rwv(&ww("w"), &["land", "--history", "linear"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(
                "local/remotes/app: merged: the commits a rebase would replay hold the merge",
            ))
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "w-app",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "side-app",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main^1",
                mark: "main-app",
            }),
        Step::commit(&a(), Repo::App, "main-app-2"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::commit(&ww("w"), Repo::App, "w-app-2"),
        Step::rwv(&ww("w"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["sync", "--from", "main"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w"), &["land", "--history", "linear"])
            .expect(Exit::Succeeds)
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main",
                mark: "w-app-2",
            })
            .claim(Claim::Contains {
                at: a(),
                repo: Repo::App,
                rev: "main^1",
                mark: "main-app-2",
            }),
    ]);
    bad_steps("a landing over a member merge", steps).assert_fully_decided_green();
}

/// The project branch's range is judged the same way: a seat that took
/// `main` in by a merge and then saw `main` move lands by a merge commit,
/// first parent `main`, rather than rebasing that merge away.
#[test]
fn a_landing_never_rebases_a_project_range_holding_a_merge() {
    bad_steps(
        "a landing over a project merge",
        vec![
            Step::rwv(&a(), &["workweave", "create", "--project", "proj", "w"])
                .expect(Exit::Succeeds),
            Step::commit(&ww("w"), Repo::Proj, "w-notes"),
            Step::commit(&a(), Repo::Proj, "main-notes-1"),
            Step::rwv(&ww("w"), &["sync", "--from", "main"]).expect(Exit::Succeeds),
            Step::commit(&a(), Repo::Proj, "main-notes-2"),
            Step::rwv(&ww("w"), &["land", "--history", "linear"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputContains(
                    "(project): merged: the commits a rebase would replay hold the merge",
                ))
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main",
                    mark: "w-notes",
                })
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::Proj,
                    rev: "main",
                    mark: "main-notes-2",
                }),
        ],
    )
    .assert_fully_decided_green();
}

/// Every ref the sandbox holds, local and remote, and every lock and record
/// it reads, as one comparable value.
fn refs_and_locks(sandbox: &Sandbox) -> String {
    let snapshot = invariants::snapshot(sandbox);
    format!(
        "{:?}\n{:?}\n{:?}\n{:?}",
        snapshot.branches, snapshot.remote_refs, snapshot.pins, snapshot.records
    )
}

/// The ids a kind's page lists for `state`, read off `rwv explain <kind>` as
/// the binary serves it: the `Choice` column of the choices table, `-` for a
/// choice the table does not number.
fn explained_choice_ids(sandbox: &Sandbox, kind: &str, state: &str) -> Vec<String> {
    let out = sandbox.rwv(&sandbox.root, &["explain", kind]);
    assert!(out.status.success(), "rwv explain {kind}");
    let page = String::from_utf8_lossy(&out.stdout).into_owned();
    let prefix = format!("| `{state}` | ");
    let rows: Vec<String> = page
        .lines()
        .filter(|line| line.starts_with(&prefix) && line.matches(" | ").count() == 4)
        .map(|line| {
            let choice = line[prefix.len()..].split(" | ").next().unwrap_or_default();
            match choice.split_once(':') {
                Some((id, _)) if id.starts_with('C') => id.to_owned(),
                _ => "-".to_owned(),
            }
        })
        .collect();
    assert!(
        !rows.is_empty(),
        "`rwv explain {kind}` lists no choice for `{state}`:\n{page}"
    );
    rows
}

/// dc27-envelope: one refusal judging a repo on two kinds prints a line per
/// state, then the choices, then exactly one route line per kind and nothing
/// after; it changes nothing on disk or in any ref; and `--json` carries the
/// same axes and states with the choices `rwv explain` lists for each.
///
/// `rwv member remove --delete` judges the clone's edits and every checkout derived
/// from it before refusing. It has no `--json`, so the JSON half drives the
/// same judgment where a verb that has one reaches it: a sync whose merge
/// drops the member prunes its clone through those same guards, and records
/// the refused prune in its envelope.
#[test]
fn dc27_envelope_routes_once_per_kind_last_and_changes_nothing() {
    let sandbox = Sandbox::new(Seed::default());
    runner::run_in(
        &sandbox,
        Scenario {
            name: "a workweave sharing the primary's clones",
            seed: Seed::default(),
            steps: vec![
                Step::rwv(&a(), &["workweave", "create", "--project", "proj", "other"])
                    .expect(Exit::Succeeds),
            ],
        },
    )
    .assert_green();
    let primary = sandbox.place_root(&a());
    let lib = sandbox.canonical(Machine::A, Repo::Lib);
    std::fs::write(lib.join("scratch.txt"), "untracked\n").unwrap();
    let raw = sandbox.root.join("raw-lib");
    sandbox.git_ok(
        &lib,
        &["worktree", "add", "--detach", raw.to_str().unwrap()],
    );
    let lib_path = Repo::Lib.rel_path();
    let manifest = primary.join("projects/proj/rwv.toml");
    let manifest_before = std::fs::read_to_string(&manifest).unwrap();
    let before = refs_and_locks(&sandbox);

    let out = sandbox.rwv(&primary, &["member", "remove", &lib_path, "--delete"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    for line in [
        format!("rwv member remove: {lib_path}: edits dirty\n    scratch.txt\n"),
        format!("rwv member remove: {lib_path}: checkout in another workspace (proj--other)\n"),
        format!(
            "rwv member remove: {lib_path}: checkout in a raw worktree\n    {}\n",
            raw.display()
        ),
        "choices:\n".to_owned(),
        "  free the other checkout\n    rwv workweave delete other --project proj\n".to_owned(),
        format!(
            "    git -C {} worktree remove {}\n",
            lib.display(),
            raw.display()
        ),
    ] {
        assert!(stderr.contains(&line), "expected {line:?}:\n{stderr}");
    }
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, ["edits", "checkout"], "{stderr}");
    assert!(
        stderr.ends_with("\n\nrwv explain edits\nrwv explain checkout\n"),
        "the route lines are last, after a blank one:\n{stderr}"
    );
    assert_eq!(refs_and_locks(&sandbox), before, "a refusal moves no ref");
    assert_eq!(
        std::fs::read_to_string(&manifest).unwrap(),
        manifest_before,
        "a refusal writes no rwv.toml"
    );
    assert!(lib.join("scratch.txt").is_file() && raw.is_dir());

    let seat = sandbox.place_root(&ww("drops"));
    assert!(sandbox
        .rwv(
            &primary,
            &["workweave", "create", "--project", "proj", "drops"]
        )
        .status
        .success());
    for args in [
        &["member", "remove", lib_path.as_str(), "--delete"][..],
        &["lock", "--commit"][..],
    ] {
        let out = sandbox.rwv(&seat, args);
        assert!(
            out.status.success(),
            "rwv {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = sandbox.rwv(&primary, &["sync", "--from", "proj--drops", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "one JSON document ({e}):\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let pruned: Vec<&serde_json::Value> = envelope["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["kind"] == "prune-refused" && m["path"] == lib_path.as_str())
        .collect();
    assert_eq!(pruned.len(), 1, "{envelope:#}");
    let refused = pruned[0]["refused"].as_array().unwrap();
    let judged: Vec<(&str, &str)> = refused
        .iter()
        .map(|r| (r["axis"].as_str().unwrap(), r["state"].as_str().unwrap()))
        .collect();
    assert_eq!(
        judged,
        [
            ("edits", "dirty"),
            ("checkout", "in another workspace"),
            ("checkout", "in a raw worktree"),
        ],
        "{envelope:#}"
    );
    for record in refused {
        let (kind, state) = (
            record["axis"].as_str().unwrap(),
            record["state"].as_str().unwrap(),
        );
        let ids: Vec<String> = record["choices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap_or("-").to_owned())
            .collect();
        assert_eq!(
            ids,
            explained_choice_ids(&sandbox, kind, state),
            "{kind} {state}"
        );
    }
}

fn json_of(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "one JSON document ({e}):\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// The pins `branch` of the project repo records, by member key.
fn pins_on(sandbox: &Sandbox, branch: &str) -> std::collections::BTreeMap<String, String> {
    let proj = sandbox.checkout(&a(), Repo::Proj);
    let lock: serde_json::Value =
        serde_json::from_str(&sandbox.git_ok(&proj, &["show", &format!("{branch}:rwv.lock")]))
            .unwrap();
    lock["repositories"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, entry)| (key.clone(), entry["version"].as_str().unwrap().to_owned()))
        .collect()
}

fn record<'a>(doc: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    doc["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == path)
        .unwrap_or_else(|| panic!("no record for {path}:\n{doc:#}"))
}

fn commit_ids(record: &serde_json::Value) -> Vec<String> {
    record["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["sha"].as_str().unwrap().to_owned())
        .collect()
}

fn revisions(sandbox: &Sandbox, repo: Repo, range: &str) -> Vec<String> {
    sandbox
        .git_ok(&sandbox.checkout(&a(), repo), &["rev-list", range])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// dc28-log-against-target: a feature whose target moved. `rwv log --against
/// main` lists exactly each repo's commits beyond what main's lock pins, read
/// from the pins and not from main's branch tips; `--diff` starts at the merge
/// base of each pin and HEAD; and `--json` carries path, HEAD, pin and
/// commits per repo.
#[test]
fn dc28_log_against_target_lists_the_unlanded_commits_and_anchors_the_diff() {
    let sandbox = Sandbox::new(Seed::default());
    runner::run_in(&sandbox, scenarios::feature_whose_target_moved()).assert_green();
    let primary = sandbox.place_root(&a());
    let before = refs_and_locks(&sandbox);

    let out = sandbox.rwv(&primary, &["log", "--against", "main", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json_of(&out);
    let pins = pins_on(&sandbox, "main");
    let mut listed = 0;
    for member in Repo::MEMBERS {
        let key = member.rel_path();
        let rec = record(&doc, &key);
        let pin = &pins[&key];
        let dir = sandbox.checkout(&a(), member);
        assert_eq!(rec["pin"], pin.as_str(), "{key}");
        assert_eq!(
            rec["head"],
            sandbox.git_ok(&dir, &["rev-parse", "HEAD"]).as_str(),
            "{key}"
        );
        let expected = revisions(&sandbox, member, &format!("{pin}..HEAD"));
        assert_eq!(
            commit_ids(rec),
            expected,
            "{key}: exactly the unlanded commits"
        );
        listed += expected.len();
    }
    assert_eq!(
        listed, 4,
        "app's two, lib's one and docs' unlocked commit: {doc:#}"
    );
    let docs = record(&doc, &Repo::Docs.rel_path());
    assert_ne!(
        docs["pin"],
        sandbox
            .git_ok(&sandbox.checkout(&a(), Repo::Docs), &["rev-parse", "main"])
            .as_str(),
        "docs' main holds a commit main's lock does not pin: the pin, not the tip, is read"
    );
    let project = record(&doc, "(project)");
    assert_eq!(
        commit_ids(project),
        revisions(&sandbox, Repo::Proj, "main..HEAD")
    );
    assert_eq!(
        project["pin"],
        sandbox
            .git_ok(&sandbox.checkout(&a(), Repo::Proj), &["rev-parse", "main"])
            .as_str()
    );

    let out = sandbox.rwv(&primary, &["log", "--against", "main", "--diff", "--json"]);
    assert!(out.status.success());
    let doc = json_of(&out);
    assert_eq!(doc["diff"], true);
    let app_dir = sandbox.checkout(&a(), Repo::App);
    let app = record(&doc, &Repo::App.rel_path());
    let anchor = sandbox.git_ok(
        &app_dir,
        &["merge-base", pins[&Repo::App.rel_path()].as_str(), "HEAD"],
    );
    assert_eq!(app["diff_base"], anchor.as_str());
    let diff = app["diff"].as_str().unwrap();
    assert!(
        diff.contains("feat-app-1.txt") && diff.contains("feat-app-2.txt"),
        "{diff}"
    );
    assert!(
        !diff.contains("main-app.txt"),
        "main's own commit after the fork is no reversal in the feature's diff:\n{diff}"
    );
    assert!(record(&doc, &Repo::Lib.rel_path())["diff"]
        .as_str()
        .unwrap()
        .contains("feat-lib.txt"));

    assert_eq!(refs_and_locks(&sandbox), before, "a log moves no ref");
}

/// log-against: `rwv log feat-x..` resolves the range in each repo as git
/// would, a member without the branch is reported in its own section with a
/// non-zero exit while the others are still listed, `--against` reads main's
/// pins, and `--against nosuch` refuses before listing.
#[test]
fn log_against_resolves_a_range_per_repo_and_reads_the_pins_of_a_weave_branch() {
    let sandbox = Sandbox::new(Seed::default());
    runner::run_in(&sandbox, scenarios::feature_branches_in_some_members()).assert_green();
    let primary = sandbox.place_root(&a());
    let before = refs_and_locks(&sandbox);
    let docs_key = Repo::Docs.rel_path();

    let out = sandbox.rwv(&primary, &["log", "feat-x.."]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let app_section = stdout
        .split("=== ")
        .find(|s| s.starts_with(&Repo::App.rel_path()))
        .expect("app has a section");
    let as_git = sandbox.git_ok(
        &sandbox.checkout(&a(), Repo::App),
        &["log", "--format=%s", "feat-x..HEAD"],
    );
    assert_eq!(as_git, "main-after");
    assert!(app_section.contains("main-after"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "=== {docs_key} ===\n  name no-such-branch (feat-x)\n"
        )),
        "docs has no feat-x:\n{stdout}"
    );
    assert!(stdout.contains("=== (project) ==="), "{stdout}");
    assert!(
        stderr.contains(&format!(
            "rwv log: {docs_key}: name no-such-branch (feat-x)\n"
        )),
        "{stderr}"
    );
    assert!(stderr.ends_with("\nrwv explain name\n"), "{stderr}");

    let out = sandbox.rwv(&primary, &["log", "main..feat-x", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let doc = json_of(&out);
    for repo in [Repo::App, Repo::Lib] {
        let expected = revisions(&sandbox, repo, "main..feat-x");
        assert_eq!(expected.len(), 1, "{}: one commit on feat-x", repo.name());
        assert_eq!(
            commit_ids(record(&doc, &repo.rel_path())),
            expected,
            "{}",
            repo.name()
        );
    }
    let docs = record(&doc, &docs_key);
    assert!(
        !sandbox
            .git(
                &sandbox.checkout(&a(), Repo::Docs),
                &["rev-list", "main..feat-x"]
            )
            .status
            .success(),
        "git itself finds no feat-x in docs"
    );
    assert_eq!(docs["refused"][0]["state"], "no-such-branch");
    assert_eq!(docs["refused"][0]["subject"], "feat-x");
    assert!(commit_ids(docs).is_empty());
    assert_eq!(
        commit_ids(record(&doc, "(project)")),
        revisions(&sandbox, Repo::Proj, "main..feat-x")
    );

    let out = sandbox.rwv(&primary, &["log", "--against", "main", "--json"]);
    assert!(out.status.success());
    let doc = json_of(&out);
    let pins = pins_on(&sandbox, "main");
    for member in Repo::MEMBERS {
        let key = member.rel_path();
        assert_eq!(record(&doc, &key)["pin"], pins[&key].as_str(), "{key}");
    }
    assert_eq!(
        commit_ids(record(&doc, &Repo::App.rel_path())),
        revisions(
            &sandbox,
            Repo::App,
            &format!("{}..HEAD", pins[&Repo::App.rel_path()])
        )
    );

    let out = sandbox.rwv(&primary, &["log", "--against", "nosuch"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "nothing is listed before the refusal"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("rwv log: projects/proj: name no-such-branch (nosuch)\n"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(refs_and_locks(&sandbox), before, "a log moves no ref");
}

#[test]
fn a_workweave_without_a_branch_is_named_by_the_rename_and_then_published() {
    runner::run(scenarios::workweave_without_b_then_pr()).assert_fully_decided_green();
}

#[test]
fn a_workweave_named_and_published_is_given_up_with_its_published_branches() {
    runner::run(scenarios::workweave_without_b_given_up_after_publishing())
        .assert_fully_decided_green();
}

#[test]
fn a_raw_rename_of_a_published_feature_leaves_the_old_branches_listed() {
    runner::run(scenarios::raw_rename_published_feature()).assert_fully_decided_green();
}

#[test]
fn a_declared_branch_renamed_on_one_machine_is_adopted_on_another_without_a_conflict() {
    runner::run(scenarios::records_rename_adopt()).assert_fully_decided_green();
}

#[test]
fn a_pruned_branch_records_no_superseded_tip_and_leaves_the_rename_unnamed() {
    runner::run(scenarios::no_superseded_on_prune()).assert_fully_decided_green();
}

#[test]
fn a_declared_mode_branch_is_renamed_and_retired_and_protected_only_against_rewrites() {
    runner::run(scenarios::declared_mode_rename_retire()).assert_fully_decided_green();
}

#[test]
fn dc25_url_provenance() {
    runner::run(scenarios::dc25_url_provenance()).assert_fully_decided_green();
}

#[test]
fn url_change_raw_merge() {
    runner::run(scenarios::url_change_raw_merge()).assert_fully_decided_green();
}

#[test]
fn history_set_unset() {
    runner::run(scenarios::history_set_unset()).assert_fully_decided_green();
}

#[test]
fn lock_commit_whole_file() {
    runner::run(scenarios::lock_commit_whole_file()).assert_fully_decided_green();
}

#[test]
fn set_branch_new() {
    runner::run(scenarios::set_branch_new()).assert_fully_decided_green();
}

#[test]
fn member_pin_behind() {
    runner::run(scenarios::member_pin_behind()).assert_fully_decided_green();
}

#[test]
fn update_reports_a_member_moved_off_a_branch_nothing_accounts_for() {
    runner::run(scenarios::dc15_moved_reports()).assert_fully_decided_green();
}

#[test]
fn a_delete_destroys_only_landed_push_branches_or_the_repos_it_names() {
    runner::run(scenarios::dc16_destroys_name_repos()).assert_fully_decided_green();
}

#[test]
fn a_delete_leaving_an_unlanded_branch_exits_3_naming_each_survivor() {
    runner::run(scenarios::dc18_partial_delete_status()).assert_fully_decided_green();
}

#[test]
fn sync_takes_in_the_records_target_and_relocks() {
    runner::run(scenarios::sync_takes_in_the_target()).assert_fully_decided_green();
}

#[test]
fn a_member_merged_on_the_forge_follows_forward_and_nothing_loops() {
    runner::run(scenarios::follow_never_backward()).assert_fully_decided_green();
}

#[test]
fn a_reset_to_target_keeps_a_backup_and_leaves_no_c3_behind() {
    runner::run(scenarios::no_c3_after_reset()).assert_fully_decided_green();
}

#[test]
fn sync_from_merges_only_members_lacking_the_pin() {
    runner::run(scenarios::no_empty_merge()).assert_fully_decided_green();
}

#[test]
fn sync_refuses_work_built_on_another_lineage_before_anything_moves() {
    runner::run(scenarios::sync_refuses_off_lineage()).assert_fully_decided_green();
}

#[test]
fn sync_json_reports_a_merged_range_and_a_rebased_branch() {
    runner::run(scenarios::sync_json_reports_what_it_merged_and_rebased())
        .assert_fully_decided_green();
}

#[test]
fn a_contradicting_one_run_history_is_refused_where_no_setting_is_read() {
    runner::run(scenarios::history_contradicted_where_no_setting_is_read())
        .assert_fully_decided_green();
}

#[test]
fn sync_never_rebases_a_range_holding_a_merge() {
    runner::run(scenarios::sync_never_rebases_a_merge()).assert_fully_decided_green();
}

#[test]
fn sync_refuses_to_rewrite_a_branch_with_dependents() {
    runner::run(scenarios::sync_refuses_to_rewrite_under_dependents()).assert_fully_decided_green();
}

#[test]
fn sync_refuses_a_member_whose_upstream_was_rewritten() {
    runner::run(scenarios::sync_judges_a_diverged_push_branch(true)).assert_fully_decided_green();
}

#[test]
fn sync_refuses_a_member_holding_unpushed_commits_beside_its_upstreams() {
    runner::run(scenarios::sync_judges_a_diverged_push_branch(false)).assert_fully_decided_green();
}

#[test]
fn sync_refuses_a_pre_schema_target_tip_at_that_branch() {
    runner::run(scenarios::sync_refuses_a_pre_schema_target_tip()).assert_fully_decided_green();
}

#[test]
fn merge_upstream_refuses_a_feature_member_off_lineage() {
    runner::run(scenarios::merge_upstream_off_lineage()).assert_fully_decided_green();
}

#[test]
fn update_remote_merge_upstream_refuses_a_feature_member_off_lineage() {
    runner::run(scenarios::update_remote_merge_upstream_off_lineage()).assert_fully_decided_green();
}

#[test]
fn update_remote_refuses_a_feature_branch_taking_in_from_another_branch() {
    runner::run(scenarios::update_remote_refuses_a_foreign_feature_upstream())
        .assert_fully_decided_green();
}

#[test]
fn update_remote_in_a_feature_advances_the_feature_branch_from_its_upstream() {
    runner::run(scenarios::update_remote_advances_the_feature_branch())
        .assert_fully_decided_green();
}

#[test]
fn sync_from_a_branch_other_than_the_target_writes_a_merge_commit() {
    runner::run(scenarios::sync_from_another_branch_always_merges()).assert_fully_decided_green();
}

#[test]
fn sync_reads_the_record_and_never_writes_it() {
    runner::run(scenarios::sync_writes_no_record()).assert_fully_decided_green();
}

#[test]
fn a_reset_to_target_in_syncs_own_lock_commit_waives_the_merge_it_abandons() {
    runner::run(scenarios::waiver_own_lock()).assert_fully_decided_green();
}

#[test]
fn update_aligns_under_an_uncommitted_manifest_so_lock_commit_can_follow() {
    runner::run(scenarios::uncommitted_manifest_beside_an_unaligned_member())
        .assert_fully_decided_green();
}

#[test]
fn a_squashed_pin_off_every_remote_offers_the_reset_to_target() {
    runner::run(scenarios::squashed_pin_off_remote_offers_the_reset()).assert_fully_decided_green();
}

#[test]
fn a_landed_pin_a_checkout_diverged_from_offers_the_reset_to_target() {
    runner::run(scenarios::landed_pin_diverged_offers_the_reset()).assert_fully_decided_green();
}

#[test]
fn sync_refuses_a_target_pin_this_clone_lacks() {
    runner::run(scenarios::absent_pin_unmerged()).assert_fully_decided_green();
}

#[test]
fn a_record_retargeted_on_another_machine_is_reported_and_sync_refuses_it() {
    runner::run(scenarios::record_retargeted_elsewhere_is_reported_and_refused())
        .assert_fully_decided_green();
}

#[test]
fn update_accepts_a_record_changed_elsewhere_and_sync_then_takes_in_its_target() {
    runner::run(scenarios::record_retargeted_elsewhere_accepted_by_update())
        .assert_fully_decided_green();
}

#[test]
fn a_record_written_in_this_workspace_is_never_reported_to_it() {
    runner::run(scenarios::record_retargeted_here_is_not_reported()).assert_fully_decided_green();
}

#[test]
fn sync_stores_the_record_silently_where_none_is_stored() {
    runner::run(scenarios::record_unstored_is_stored_by_sync()).assert_fully_decided_green();
}

#[test]
fn a_landing_into_a_target_checked_out_nowhere_refuses_only_for_the_source() {
    runner::run(scenarios::land_into_a_target_checked_out_nowhere()).assert_fully_decided_green();
}

#[test]
fn a_landing_reports_every_refusal_together_before_anything_moves() {
    runner::run(scenarios::land_reports_every_refusal_together()).assert_fully_decided_green();
}

#[test]
fn a_fast_forward_landing_leaves_a_dependent_alone() {
    runner::run(scenarios::land_refuses_a_dependent()).assert_fully_decided_green();
}

#[test]
fn a_target_whose_mode_changed_refuses_the_landing_until_rebound_or_restored() {
    runner::run(scenarios::target_mode_changed()).assert_fully_decided_green();
}

#[test]
fn a_landing_refuses_a_record_retargeted_on_another_machine() {
    runner::run(scenarios::land_refuses_a_record_retargeted_elsewhere())
        .assert_fully_decided_green();
}

#[test]
fn push_withholds_a_feature_whose_target_has_no_push_branch() {
    runner::run(scenarios::push_withholds_a_feature_whose_target_is_unpublished())
        .assert_fully_decided_green();
}

#[test]
fn a_published_record_never_lands_into_a_branch_with_a_foreign_upstream() {
    runner::run(
        scenarios::land_never_resolves_a_published_record_to_a_branch_with_a_foreign_upstream(),
    )
    .assert_fully_decided_green();
}

#[test]
fn a_published_record_never_lands_into_an_unrelated_local_branch() {
    runner::run(scenarios::land_never_resolves_a_published_record_to_an_unrelated_branch())
        .assert_fully_decided_green();
}

#[test]
fn a_landing_refuses_work_built_on_another_lineage() {
    runner::run(scenarios::land_refuses_work_built_on_another_lineage())
        .assert_fully_decided_green();
}

#[test]
fn a_landing_never_rewrites_a_declared_mode_branch() {
    runner::run(scenarios::land_never_rewrites_a_declared_mode_branch())
        .assert_fully_decided_green();
}

#[test]
fn dc01_a_member_mid_operation_refuses_the_landing_with_its_own_status() {
    runner::run(scenarios::dc01_mid_operation_land()).assert_fully_decided_green();
}

#[test]
fn deletion_offline_unpublished_lands_and_retires_by_local_refs() {
    runner::run(scenarios::deletion_offline_unpublished()).assert_fully_decided_green();
}

#[test]
fn dc01_a_member_stopped_mid_rebase_is_judged_on_that_alone_by_every_verb_that_moves_it() {
    runner::run(scenarios::dc01_mid_operation(scenarios::Stopped::Rebase))
        .assert_fully_decided_green();
}

#[test]
fn dc01_a_member_stopped_mid_bisect_is_judged_on_that_alone_by_every_verb_that_moves_it() {
    runner::run(scenarios::dc01_mid_operation(scenarios::Stopped::Bisect))
        .assert_fully_decided_green();
}

#[test]
fn dc01_a_member_stopped_mid_rebase_behind_its_pin_is_judged_on_that_alone() {
    runner::run(scenarios::dc01_mid_operation(
        scenarios::Stopped::RebaseBehindPin,
    ))
    .assert_fully_decided_green();
}

#[test]
fn dc01_a_member_stopped_mid_merge_is_judged_on_that_alone_by_every_verb_that_moves_it() {
    runner::run(scenarios::dc01_mid_operation(scenarios::Stopped::Merge))
        .assert_fully_decided_green();
}

#[test]
fn dc01_a_member_stopped_mid_cherry_pick_is_judged_on_that_alone_by_every_verb_that_moves_it() {
    runner::run(scenarios::dc01_mid_operation(
        scenarios::Stopped::CherryPick,
    ))
    .assert_fully_decided_green();
}

#[test]
fn dc01_a_member_stopped_mid_revert_is_judged_on_that_alone_by_every_verb_that_moves_it() {
    runner::run(scenarios::dc01_mid_operation(scenarios::Stopped::Revert))
        .assert_fully_decided_green();
}

#[test]
fn dc01_a_project_repo_stopped_mid_bisect_is_judged_on_that_alone() {
    runner::run(scenarios::dc01_mid_operation(
        scenarios::Stopped::ProjectBisect,
    ))
    .assert_fully_decided_green();
}

#[test]
fn dc02_c14_keeps_detached_work_on_a_branch_through_the_landing() {
    runner::run(scenarios::dc02_c14_keeps_work()).assert_fully_decided_green();
}

#[test]
fn land_retire_with_a_target_judges_the_deletion_against_that_target() {
    runner::run(scenarios::land_retire_judges_deletion_against_the_named_target())
        .assert_fully_decided_green();
}

#[test]
fn a_hold_bumped_to_the_cuts_pin_is_judged_off_lineage_and_lands_after_c26() {
    runner::run(scenarios::bump_to_cut_pin()).assert_fully_decided_green();
}

#[test]
fn c22s_report_prints_the_copy_with_the_new_work_and_the_merge_with_every_commit() {
    runner::run(scenarios::c22_prints_both()).assert_fully_decided_green();
}

#[test]
fn dc29_work_off_lineage_is_ported_by_cherry_pick_and_lands_with_no_porting_verb() {
    runner::run(scenarios::dc29_no_bring()).assert_fully_decided_green();
}

#[test]
fn concurrent_landings_merge_by_pull_merge_upstream_and_publish() {
    runner::run(scenarios::concurrent_landing_merge_upstream()).assert_fully_decided_green();
}

holding_rwv! {
    /// Eight creates and a retire's drop race on one machine's records
    /// branch; every lost compare-and-swap retries and no record is lost.
    fn records_written_at_once_on_one_machine_are_all_kept_and_drops_retry_against_the_winner() {
        runner::run(scenarios::records_local_writers()).assert_green();
    }
}

holding_rwv! {
    /// A landing into a target another landing holds waits on the claim on
    /// it, then lands on the tips the first left.
    fn a_landing_into_a_held_target_waits_and_then_lands() {
        runner::run(scenarios::a_landing_waits_on_another_landings_claim()).assert_green();
    }
}

holding_rwv! {
    /// A drop racing `branch create` keeps the record: the branch exists
    /// before its record is written, and a re-record of the same value still
    /// moves the records branch under the drop's compare-and-swap.
    fn a_drop_racing_a_create_keeps_the_created_branchs_record() {
        runner::run(scenarios::records_drop_races_create()).assert_fully_decided_green();
    }
}

holding_rwv! {
    /// A pruning fetch meeting the fetch claim a retiring seat holds waits
    /// for it, and every seat's retire completes its deletion.
    fn retiring_seats_fetch_one_at_a_time_under_the_store_claim_and_each_deletion_completes() {
        runner::run(scenarios::parallel_retire_fetch()).assert_green();
    }
}

#[test]
fn a_landed_history_pin_no_ref_keeps_lands_on_a_second_machine() {
    runner::run(scenarios::landed_history_pin_no_ref_keeps()).assert_fully_decided_green();
}

#[test]
fn a_waived_history_pin_lands_on_a_second_machine() {
    runner::run(scenarios::landed_history_waived_pin_on_a_second_machine())
        .assert_fully_decided_green();
}

#[test]
fn a_merged_project_with_rebased_members_lands_on_a_second_machine() {
    runner::run(scenarios::merged_project_with_rebased_members_on_a_second_machine())
        .assert_fully_decided_green();
}

#[test]
fn a_landing_records_no_rebase_jump_the_replay_did_not_record() {
    runner::run(scenarios::a_landing_records_no_rebase_jump_the_replay_did_not_record())
        .assert_fully_decided_green();
}

#[test]
fn a_landing_writes_no_relock_of_its_own() {
    for how in [
        scenarios::ProjectLands::FastForward,
        scenarios::ProjectLands::Rebase,
        scenarios::ProjectLands::Merge,
    ] {
        runner::run(scenarios::landing_writes_no_relock_of_its_own(how))
            .assert_fully_decided_green();
    }
}

#[test]
fn a_landing_records_the_tip_of_a_member_it_does_not_move() {
    runner::run(scenarios::a_landing_records_the_tip_of_a_member_it_does_not_move())
        .assert_fully_decided_green();
}

#[test]
fn a_refused_landing_leaves_a_lock_behind_as_it_stood() {
    runner::run(scenarios::a_refused_landing_leaves_a_lock_behind_as_it_stood())
        .assert_fully_decided_green();
}

#[test]
fn a_history_pin_not_repinned_still_refuses_on_a_second_machine() {
    runner::run(scenarios::a_history_pin_not_repinned_still_refuses()).assert_fully_decided_green();
}

#[test]
fn dc11_safe_drift_is_refreshed_before_edits_are_judged_and_the_moved_tip_lands() {
    runner::run(scenarios::dc11_drift_before_dirt()).assert_green();
}

#[test]
fn land_into_a_target_that_exists_nowhere_scenario() {
    runner::run(scenarios::land_into_a_target_that_exists_nowhere()).assert_fully_decided_green();
}

#[test]
fn land_reports_an_unmoved_member_it_pins_beyond_the_target_scenario() {
    runner::run(scenarios::land_reports_an_unmoved_member_it_pins_beyond_the_target())
        .assert_fully_decided_green();
}

#[test]
fn land_target_gone_lists_only_targets_the_rebind_takes_scenario() {
    runner::run(scenarios::land_target_gone_lists_only_targets_the_rebind_takes())
        .assert_fully_decided_green();
}

#[test]
fn land_judges_target_edits_only_where_it_moves_scenario() {
    runner::run(scenarios::land_judges_target_edits_only_where_it_moves())
        .assert_fully_decided_green();
}

#[test]
fn land_judges_the_pin_on_its_branch_only_for_moved_members_scenario() {
    runner::run(scenarios::land_judges_the_pin_on_its_branch_only_for_moved_members())
        .assert_fully_decided_green();
}

#[test]
fn land_refuses_a_moved_member_whose_branch_lacks_the_pin_scenario() {
    runner::run(scenarios::land_refuses_a_moved_member_whose_branch_lacks_the_pin())
        .assert_fully_decided_green();
}

#[test]
fn land_reports_what_it_pins_beyond_the_target_scenario() {
    runner::run(scenarios::land_reports_what_it_pins_beyond_the_target())
        .assert_fully_decided_green();
}

#[test]
fn land_reports_the_target_ahead_of_its_push_branch_scenario() {
    runner::run(scenarios::land_reports_the_target_ahead_of_its_push_branch())
        .assert_fully_decided_green();
}

#[test]
fn a_landing_resolves_a_target_that_publishes_where_its_record_is() {
    runner::run(scenarios::land_resolves_a_target_that_publishes_where_its_record_is())
        .assert_fully_decided_green();
}

#[test]
fn land_refuses_a_target_diverged_from_its_upstream_scenario() {
    runner::run(scenarios::land_refuses_a_target_diverged_from_its_upstream())
        .assert_fully_decided_green();
}

#[test]
fn land_refuses_a_receiving_branch_diverged_from_its_upstream_scenario() {
    runner::run(scenarios::land_refuses_a_receiving_branch_diverged_from_its_upstream())
        .assert_fully_decided_green();
}

#[test]
fn land_into_a_receiving_branch_checked_out_in_a_third_workspace_scenario() {
    runner::run(scenarios::land_into_a_receiving_branch_checked_out_in_a_third_workspace())
        .assert_fully_decided_green();
}

#[test]
fn land_from_the_primary_refuses_a_target_in_a_workweave_scenario() {
    runner::run(scenarios::land_from_the_primary_refuses_a_target_in_a_workweave())
        .assert_fully_decided_green();
}

#[test]
fn land_refuses_a_target_member_off_its_receiving_branch_scenario() {
    runner::run(scenarios::land_refuses_a_target_member_off_its_receiving_branch())
        .assert_fully_decided_green();
}

#[test]
fn land_refuses_a_target_checkout_diverged_from_its_pin_scenario() {
    runner::run(scenarios::land_refuses_a_target_checkout_diverged_from_its_pin())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_port_copies_member_ranges_carries_dependencies_and_reports_entries_not_ported() {
    runner::run(scenarios::cherry_pick_port()).assert_fully_decided_green();
}

#[test]
fn cherry_pick_continue_rechecks_each_member_and_takes_no_hand_commit_for_a_copy() {
    runner::run(scenarios::cherry_pick_continue_rechecks_members()).assert_fully_decided_green();
}

#[test]
fn cherry_pick_stopped_in_the_project_repo_continues_with_the_rename_and_this_branchs_entries() {
    runner::run(scenarios::cherry_pick_project_conflict()).assert_fully_decided_green();
}

#[test]
fn cherry_pick_stopped_in_the_project_repo_is_put_back_by_abort() {
    runner::run(scenarios::cherry_pick_project_conflict_aborts()).assert_fully_decided_green();
}

holding_rwv! {
    /// A port killed after it recorded the project copy's start, before the
    /// copy: `--continue` makes the copy rather than committing the relock
    /// alone.
    fn cherry_pick_killed_before_the_project_copy_continues_with_the_copy() {
        runner::run(scenarios::cherry_pick_project_copy_killed()).assert_fully_decided_green();
    }

    /// A port killed inside its op's start: `--continue` savepoints a member
    /// before copying onto it, so abort restores it.
    fn cherry_pick_killed_in_its_op_start_savepoints_each_member_before_copying() {
        runner::run(scenarios::cherry_pick_op_start_killed()).assert_fully_decided_green();
    }

    /// A port killed after a copy, then after the project commit, each made
    /// and not yet recorded: `--continue` takes each as made, copying
    /// nothing twice, and reports the port.
    fn cherry_pick_killed_after_each_move_continues_with_what_it_made() {
        runner::run(scenarios::cherry_pick_killed_after_each_move()).assert_fully_decided_green();
    }
}

#[test]
fn cherry_pick_stops_on_a_member_conflict_and_continue_completes_the_port() {
    runner::run(scenarios::cherry_pick_conflict()).assert_fully_decided_green();
}

#[test]
fn cherry_pick_stopped_on_a_conflict_is_put_back_by_abort() {
    runner::run(scenarios::cherry_pick_conflict_aborts()).assert_fully_decided_green();
}

#[test]
fn cherry_pick_refuses_before_anything_moves_and_the_printed_hand_port_completes() {
    runner::run(scenarios::cherry_pick_refusals()).assert_fully_decided_green();
}

#[test]
fn land_leaves_a_shared_member_where_the_presented_project_has_it_scenario() {
    runner::run(scenarios::land_leaves_a_shared_member_where_the_presented_project_has_it())
        .assert_fully_decided_green();
}

#[test]
fn land_into_a_workspace_presenting_another_project_scenario() {
    runner::run(scenarios::land_into_a_workspace_presenting_another_project())
        .assert_fully_decided_green();
}

#[test]
fn land_names_a_member_composing_cannot_materialize_scenario() {
    runner::run(scenarios::land_names_a_member_composing_cannot_materialize())
        .assert_fully_decided_green();
}

#[test]
fn history_unset_fast_forward_is_green() {
    runner::run(scenarios::history_unset_fast_forward()).assert_fully_decided_green();
}

#[test]
fn history_unset_retire_behind_is_green() {
    runner::run(scenarios::history_unset_retire_behind()).assert_fully_decided_green();
}

#[test]
fn history_unset_moved_is_green() {
    runner::run(scenarios::history_unset_moved()).assert_fully_decided_green();
}

#[test]
fn history_equal_one_run_is_green() {
    runner::run(scenarios::history_equal_one_run()).assert_fully_decided_green();
}

#[test]
fn history_contradicted_is_green() {
    runner::run(scenarios::history_contradicted()).assert_fully_decided_green();
}

#[test]
fn history_rebase_pin_remap_is_green() {
    runner::run(scenarios::history_rebase_pin_remap()).assert_fully_decided_green();
}

#[test]
fn a_landing_a_member_rebase_stopped_converges_after_the_vcs_s_own_continue() {
    runner::run(scenarios::vcs_continue_mid_rebase(
        scenarios::MidRebase::Member,
        scenarios::AfterHandContinue::Continue,
    ))
    .assert_fully_decided_green();
}

#[test]
fn a_landing_the_project_rebase_stopped_converges_after_the_vcs_s_own_continue() {
    runner::run(scenarios::vcs_continue_mid_rebase(
        scenarios::MidRebase::Project,
        scenarios::AfterHandContinue::Continue,
    ))
    .assert_fully_decided_green();
}

#[test]
fn abort_after_the_vcs_s_own_continue_of_a_member_rebase_restores_the_seat() {
    runner::run(scenarios::vcs_continue_mid_rebase(
        scenarios::MidRebase::Member,
        scenarios::AfterHandContinue::Abort,
    ))
    .assert_fully_decided_green();
}

#[test]
fn abort_after_the_vcs_s_own_continue_of_the_project_rebase_restores_the_seat() {
    runner::run(scenarios::vcs_continue_mid_rebase(
        scenarios::MidRebase::Project,
        scenarios::AfterHandContinue::Abort,
    ))
    .assert_fully_decided_green();
}

#[test]
fn stack_carried_is_green() {
    runner::run(scenarios::stack_carried()).assert_fully_decided_green();
}

#[test]
fn stack_two_dependents_is_green() {
    runner::run(scenarios::stack_two_dependents()).assert_fully_decided_green();
}

#[test]
fn stack_transitive_is_green() {
    runner::run(scenarios::stack_transitive()).assert_fully_decided_green();
}

#[test]
fn stack_merge_setting_is_green() {
    runner::run(scenarios::stack_merge_setting()).assert_fully_decided_green();
}

#[test]
fn stack_checked_out_rebind_is_green() {
    runner::run(scenarios::stack_checked_out(
        scenarios::CheckedOutChoice::Rebind,
    ))
    .assert_fully_decided_green();
}

#[test]
fn stack_checked_out_land_then_retire_is_green() {
    runner::run(scenarios::stack_checked_out(
        scenarios::CheckedOutChoice::LandThenRetire,
    ))
    .assert_fully_decided_green();
}

#[test]
fn stack_not_containing_is_green() {
    runner::run(scenarios::stack_not_containing()).assert_fully_decided_green();
}

#[test]
fn a_carry_that_conflicts_continues_once_resolved() {
    runner::run(scenarios::stack_carry_conflict(
        scenarios::AfterHandContinue::Continue,
    ))
    .assert_fully_decided_green();
}

#[test]
fn a_carry_that_conflicts_aborts_to_where_the_stack_was() {
    runner::run(scenarios::stack_carry_conflict(
        scenarios::AfterHandContinue::Abort,
    ))
    .assert_fully_decided_green();
}

#[test]
fn an_abort_after_a_sync_relocked_reads_the_relock_as_its_own() {
    runner::run(scenarios::abort_after_a_carry_stop(
        scenarios::BeforeTheCarry::SyncRelocked,
    ))
    .assert_fully_decided_green();
}

#[cfg(unix)]
#[test]
fn an_abort_after_a_landing_merged_its_project_reads_the_merge_as_its_own() {
    runner::run(scenarios::abort_after_a_carry_stop(
        scenarios::BeforeTheCarry::LandingMerged,
    ))
    .assert_fully_decided_green();
}

#[test]
fn an_abort_after_a_carry_finished_by_hand_reads_its_tip_as_its_own() {
    runner::run(scenarios::abort_after_a_carry_stop(
        scenarios::BeforeTheCarry::FinishedByHand,
    ))
    .assert_fully_decided_green();
}

#[test]
fn a_dependent_carried_onto_a_merged_project_pins_what_it_inherits() {
    runner::run(scenarios::a_dependent_carried_onto_a_merged_project_pins_what_it_inherits())
        .assert_fully_decided_green();
}

#[test]
fn an_abort_of_a_carried_stack_built_on_abandons_as_printed() {
    runner::run(scenarios::abort_a_carried_stack_built_on(
        scenarios::BuiltOnChoice::Abandon,
    ))
    .assert_fully_decided_green();
}

#[test]
fn an_abort_of_a_carried_stack_built_on_continues_as_printed() {
    runner::run(scenarios::abort_a_carried_stack_built_on(
        scenarios::BuiltOnChoice::Continue,
    ))
    .assert_fully_decided_green();
}

#[test]
fn an_abort_of_a_carried_stack_checked_out_elsewhere_refuses_whole() {
    runner::run(scenarios::abort_a_carried_stack_blocked(
        scenarios::CarriedBlocked::CheckedOut,
    ))
    .assert_fully_decided_green();
}

#[test]
fn an_abort_of_a_carried_stack_checked_out_in_the_target_refuses_whole() {
    runner::run(scenarios::abort_a_carried_stack_blocked(
        scenarios::CarriedBlocked::CheckedOutInTheTarget,
    ))
    .assert_fully_decided_green();
}

#[test]
fn an_abort_of_a_carried_stack_whose_dependent_is_gone_refuses_whole() {
    runner::run(scenarios::abort_a_carried_stack_blocked(
        scenarios::CarriedBlocked::Gone,
    ))
    .assert_fully_decided_green();
}

#[test]
fn an_abort_over_a_carry_left_by_a_quit_refuses_before_anything_moves() {
    runner::run(scenarios::abort_over_a_carry_left_by_a_quit()).assert_fully_decided_green();
}

#[test]
fn an_abort_killed_after_putting_a_carried_stack_back_is_rerun() {
    runner::run(scenarios::abort_a_carried_stack_killed_after_putting_it_back())
        .assert_fully_decided_green();
}

#[test]
fn a_dependent_moved_as_its_carry_moves_is_carried_again() {
    runner::run(scenarios::a_dependent_moved_as_its_carry_moves(
        scenarios::MovedAsCarried::CarryAgain,
    ))
    .assert_fully_decided_green();
}

#[test]
fn a_dependent_moved_as_a_landing_carries_it_is_carried_again() {
    runner::run(scenarios::a_dependent_moved_as_its_carry_moves(
        scenarios::MovedAsCarried::CarryAgainLanding,
    ))
    .assert_fully_decided_green();
}

#[test]
fn a_dependent_moved_as_its_carry_moves_aborts_then_syncs() {
    runner::run(scenarios::a_dependent_moved_as_its_carry_moves(
        scenarios::MovedAsCarried::AbortThenSync,
    ))
    .assert_fully_decided_green();
}

#[test]
fn a_landing_into_a_dependent_waits_on_its_stopped_carry() {
    runner::run(
        scenarios::a_landing_into_a_dependent_waits_on_its_stopped_carry(
            scenarios::StoppedCarryEnds::Abort,
        ),
    )
    .assert_fully_decided_green();
}

#[test]
fn a_landing_into_a_dependent_lands_once_its_stopped_carry_continues() {
    runner::run(
        scenarios::a_landing_into_a_dependent_waits_on_its_stopped_carry(
            scenarios::StoppedCarryEnds::Continue,
        ),
    )
    .assert_fully_decided_green();
}

#[test]
fn a_landing_resumed_after_its_carry_judges_an_entry_changed_since_is_green() {
    runner::run(scenarios::a_landing_resumed_after_its_carry_judges_an_entry_changed_since())
        .assert_fully_decided_green();
}

#[test]
fn siblings_at_one_commit_are_carried_to_one_commit_is_green() {
    runner::run(scenarios::siblings_at_one_commit_are_carried_to_one_commit())
        .assert_fully_decided_green();
}

#[test]
fn a_project_carry_follows_its_member_recomposed_across_a_stop_is_green() {
    runner::run(scenarios::a_project_carry_follows_its_member_recomposed_across_a_stop())
        .assert_fully_decided_green();
}

#[test]
fn a_carry_left_by_a_quit_refuses_its_continue_as_unrecorded() {
    runner::run(scenarios::a_carry_left_by_a_quit_is_unrecorded()).assert_fully_decided_green();
}

#[test]
fn a_stopped_carry_continued_meets_a_dependent_checked_out_and_each_choice_ends_it() {
    use scenarios::ContinuedCarryMeets::*;
    for ends in [WorkweaveDelete, Rebind, Abort] {
        runner::run(scenarios::a_stopped_carry_continued_meets_a_dependent_checked_out(ends))
            .assert_fully_decided_green();
    }
}

#[test]
fn abort_leaving_the_target_of_a_landing_that_carried_a_dependent() {
    runner::run(scenarios::abort_leaving_the_target_of_a_landing_that_carried_a_dependent())
        .assert_fully_decided_green();
}

#[test]
fn a_carry_refuses_to_write_over_an_ignored_file() {
    runner::run(scenarios::a_carry_refuses_to_write_over_an_ignored_file())
        .assert_fully_decided_green();
}

#[test]
fn a_carry_replay_that_fails_without_a_conflict_leaves_the_checkout_on_its_branch() {
    runner::run(
        scenarios::a_carry_replay_that_fails_without_a_conflict_leaves_the_checkout_on_its_branch(),
    )
    .assert_fully_decided_green();
}

#[test]
fn a_dependent_opened_elsewhere_as_its_carry_moves_it_is_not_moved() {
    runner::run(scenarios::a_dependent_opened_elsewhere_as_its_carry_moves_it_is_not_moved())
        .assert_fully_decided_green();
}

#[test]
fn a_landing_resumed_after_its_target_moved_past_a_carry_completes() {
    runner::run(scenarios::a_landing_resumed_after_its_target_moved_past_a_carry())
        .assert_fully_decided_green();
}

#[test]
fn a_landing_killed_after_its_carry_continues() {
    runner::run(scenarios::a_landing_killed_after_its_carry_continues())
        .assert_fully_decided_green();
}

#[test]
fn a_carry_stopped_in_the_project_continues() {
    runner::run(scenarios::a_carry_stopped_in_the_project_continues()).assert_fully_decided_green();
}

#[test]
fn a_landing_whose_carry_stopped_in_the_project_continues() {
    runner::run(scenarios::a_landing_whose_carry_stopped_in_the_project_continues())
        .assert_fully_decided_green();
}

#[test]
fn a_sync_continued_after_its_relock_keeps_it() {
    runner::run(scenarios::a_sync_continued_after_its_relock_keeps_it())
        .assert_fully_decided_green();
}

#[test]
fn a_sync_continued_after_its_project_tip_moved_recomposes_the_project_carry() {
    runner::run(
        scenarios::a_sync_continued_after_its_project_tip_moved_recomposes_the_project_carry(),
    )
    .assert_fully_decided_green();
}

#[test]
fn sync_continue_judges_dependents_before_a_rejudged_rebase() {
    runner::run(scenarios::sync_continue_judges_dependents_before_a_rejudged_rebase())
        .assert_fully_decided_green();
}

#[test]
fn a_dependent_at_the_tip_is_carried_under_update_refs() {
    runner::run(scenarios::stack_at_the_tip_under_update_refs()).assert_fully_decided_green();
}

#[test]
fn a_stopped_carry_under_update_refs_moves_no_dependent_and_aborts_clean() {
    runner::run(scenarios::stack_carry_stopped_under_update_refs()).assert_fully_decided_green();
}

#[test]
fn abort_refuses_a_tip_amended_during_a_stop_and_keeps_it() {
    runner::run(scenarios::abort_refuses_a_tip_amended_during_a_stop())
        .assert_fully_decided_green();
}

#[cfg(unix)]
#[test]
fn git_below_2_38_is_green() {
    runner::run(scenarios::git_below_2_38()).assert_fully_decided_green();
}

#[test]
fn stack_carried_by_a_landing_is_green() {
    runner::run(scenarios::stack_carried_by_a_landing()).assert_fully_decided_green();
}

#[test]
fn stack_cycle_is_green() {
    runner::run(scenarios::stack_cycle()).assert_fully_decided_green();
}

#[test]
fn stack_remote_dependent_is_green() {
    runner::run(scenarios::stack_remote_dependent()).assert_fully_decided_green();
}

#[test]
fn stack_remote_dependent_landed_wholly_carries_nothing() {
    runner::run(scenarios::stack_remote_dependent_landed(
        scenarios::RemoteDependentLanded::Wholly,
    ))
    .assert_fully_decided_green();
}

#[test]
fn stack_remote_dependent_with_an_unlanded_commit_refuses_until_it_lands() {
    runner::run(scenarios::stack_remote_dependent_landed(
        scenarios::RemoteDependentLanded::ButOneCommit,
    ))
    .assert_fully_decided_green();
}

#[test]
fn history_linear_landing_is_green() {
    runner::run(scenarios::history_linear_landing()).assert_fully_decided_green();
}

#[test]
fn history_ff_only_is_green() {
    runner::run(scenarios::history_ff_only()).assert_fully_decided_green();
}

#[test]
fn history_merge_is_green() {
    runner::run(scenarios::history_merge()).assert_fully_decided_green();
}

#[test]
fn history_range_with_merge_is_green() {
    runner::run(scenarios::history_range_with_merge()).assert_fully_decided_green();
}

#[test]
fn history_ff_only_range_with_merge_is_green() {
    runner::run(scenarios::history_ff_only_range_with_merge()).assert_fully_decided_green();
}

#[test]
fn history_sync_from_declared_is_green() {
    runner::run(scenarios::history_sync_from_declared()).assert_fully_decided_green();
}

#[test]
fn history_member_override_lands_is_green() {
    runner::run(scenarios::history_member_override_lands()).assert_fully_decided_green();
}

#[test]
fn rebased_backup_lifetime_is_green() {
    runner::run(scenarios::rebased_backup_lifetime()).assert_fully_decided_green();
}

#[test]
fn rebased_backup_after_abort_offers_no_republish_scenario() {
    runner::run(scenarios::rebased_backup_after_abort_offers_no_republish())
        .assert_fully_decided_green();
}

#[test]
fn rebased_backup_of_another_branch_offers_no_republish_scenario() {
    runner::run(scenarios::rebased_backup_of_another_branch_offers_no_republish())
        .assert_fully_decided_green();
}

#[test]
fn history_republish_after_a_colleague_pushed_is_green() {
    runner::run(scenarios::history_republish_after_a_colleague_pushed())
        .assert_fully_decided_green();
}

#[test]
fn history_retired_published_seat_drops_its_rebased_backups_is_green() {
    runner::run(scenarios::history_retired_published_seat_drops_its_rebased_backups())
        .assert_fully_decided_green();
}

#[test]
fn history_overwrite_to_a_newer_tip_drops_the_old_rebased_backup_is_green() {
    runner::run(scenarios::history_overwrite_to_a_newer_tip_drops_the_old_rebased_backup())
        .assert_fully_decided_green();
}

#[test]
fn history_land_merge_resolves_derived_to_the_receiving_side_is_green() {
    runner::run(scenarios::history_land_merge_resolves_derived_to_the_receiving_side())
        .assert_fully_decided_green();
}

#[test]
fn history_land_merge_still_stops_on_an_undeclared_path_is_green() {
    runner::run(scenarios::history_land_merge_still_stops_on_an_undeclared_path())
        .assert_fully_decided_green();
}

#[test]
fn history_sync_merge_resolves_derived_to_the_receiving_side_is_green() {
    runner::run(scenarios::history_sync_merge_resolves_derived_to_the_receiving_side())
        .assert_fully_decided_green();
}

#[test]
fn history_sync_merge_still_stops_on_an_undeclared_path_is_green() {
    runner::run(scenarios::history_sync_merge_still_stops_on_an_undeclared_path())
        .assert_fully_decided_green();
}

#[test]
fn history_sync_contradicted_where_it_fast_forwards_is_green() {
    runner::run(scenarios::history_sync_contradicted_where_it_fast_forwards())
        .assert_fully_decided_green();
}

#[test]
fn sync_continue_replays_what_the_stopped_run_reported_is_green() {
    runner::run(scenarios::sync_continue_replays_what_the_stopped_run_reported())
        .assert_fully_decided_green();
}

#[test]
fn history_published_rebase_is_green() {
    runner::run(scenarios::history_published_rebase()).assert_fully_decided_green();
}

#[test]
fn history_disagreement_is_reported_and_each_project_lands_by_its_own() {
    runner::run(scenarios::history_disagreement()).assert_fully_decided_green();
}

#[test]
fn history_member_override_is_stored_and_a_fast_forward_reads_neither() {
    runner::run(scenarios::history_member_override()).assert_fully_decided_green();
}

#[test]
fn sync_continue_merges_a_repo_the_stop_diverged_scenario() {
    runner::run(scenarios::sync_continue_merges_a_repo_the_stop_diverged())
        .assert_fully_decided_green();
}

#[test]
fn sync_continue_merges_a_range_the_stop_gave_a_merge_scenario() {
    runner::run(scenarios::sync_continue_merges_a_range_the_stop_gave_a_merge())
        .assert_fully_decided_green();
}

#[test]
fn land_continue_composes_a_merge_the_stop_left_scenario() {
    runner::run(scenarios::land_continue_composes_a_merge_the_stop_left())
        .assert_fully_decided_green();
}

#[test]
fn abort_after_a_landing_merge_stops_scenario() {
    runner::run(scenarios::abort_after_a_landing_merge_stops()).assert_fully_decided_green();
}

#[test]
fn landing_merge_continued_writes_one_project_merge_scenario() {
    runner::run(scenarios::landing_merge_continued_writes_one_project_merge())
        .assert_fully_decided_green();
}

#[test]
fn abort_after_a_sync_merge_stops_scenario() {
    runner::run(scenarios::abort_after_a_sync_merge_stops()).assert_fully_decided_green();
}

#[test]
fn dc04_a_conflicted_entry_is_settled_by_its_setters() {
    runner::run(scenarios::dc04_conflict_setter()).assert_fully_decided_green();
}

#[test]
fn land_names_the_update_that_finishes_an_alignment_it_could_not_judge_scenario() {
    runner::run(scenarios::land_names_the_update_that_finishes_an_alignment_it_could_not_judge())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_runs_the_printed_merge_scenario() {
    runner::run(scenarios::cherry_pick_runs_the_printed_merge()).assert_fully_decided_green();
}

#[test]
fn cherry_pick_runs_the_printed_pin_of_a_dependency_moved_backward_scenario() {
    runner::run(scenarios::cherry_pick_runs_the_printed_pin_of_a_dependency_moved_backward())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_runs_the_printed_add_of_a_member_not_listed_scenario() {
    runner::run(scenarios::cherry_pick_runs_the_printed_add_of_a_member_not_listed())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_runs_the_printed_add_of_a_dependency_not_listed_scenario() {
    runner::run(scenarios::cherry_pick_runs_the_printed_add_of_a_dependency_not_listed())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_except_judges_the_member_for_alignment_scenario() {
    runner::run(scenarios::cherry_pick_except_judges_the_member_for_alignment())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_runs_the_printed_relock_of_an_unpinned_member_scenario() {
    runner::run(scenarios::cherry_pick_runs_the_printed_relock_of_an_unpinned_member())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_refuses_a_pin_this_clone_lacks_scenario() {
    runner::run(scenarios::cherry_pick_refuses_a_pin_this_clone_lacks())
        .assert_fully_decided_green();
}

#[test]
fn cherry_pick_port_landing_merge_scenario() {
    runner::run(scenarios::cherry_pick_port_landing_merge()).assert_fully_decided_green();
}

#[test]
fn land_clones_a_member_its_target_took_in_by_fetch_alone_scenario() {
    runner::run(scenarios::land_clones_a_member_its_target_took_in_by_fetch_alone())
        .assert_fully_decided_green();
}
