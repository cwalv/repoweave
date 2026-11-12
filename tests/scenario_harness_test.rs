//! Branching scenarios run step by step in hermetic sandbox weaves, with
//! invariants checked after every step.
//!
//! A scenario is a sequence of plain git commands, rwv verbs, and what a forge
//! or colleague does to a remote. Each step carries its expected outcome as
//! data; a step whose outcome a branching design decides stays undecided, and
//! only its invariants are held. A step that needs a verb rwv does not have is
//! recorded as pending and ends the scenario there, announced on stderr.
//!
//! The invariants and what they cannot see are stated in
//! `tests/scenario_harness_test/invariants.rs`; the sandbox's fixed shape in
//! `tests/scenario_harness_test/sandbox.rs`. A test that holds rwv at a pause
//! point runs only in a build with the `test-pause` feature, and announces
//! its skip in any other.

mod common;

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
use repoweave::manifest::Role;
use runner::{Claim, Exit, Forge, MissingVerb, Report, Scenario, Step};
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
fn permanent_seat_lands_twice_then_needs_a_declaration_verb() {
    runner::run(scenarios::permanent_seat()).assert_pending_on(MissingVerb::Declare);
}

#[test]
fn nested_workweaves_sync_both_ways_and_land() {
    runner::run(scenarios::nested_workweaves_without_naming()).assert_green();
}

#[test]
fn raw_merge_real() {
    runner::run(scenarios::raw_merge_real()).assert_green();
}

#[test]
fn raw_merge_fast_forward() {
    runner::run(scenarios::raw_merge_fast_forward()).assert_green();
}

#[test]
fn detached_head_in_workweave() {
    runner::run(scenarios::detached_head_in_workweave()).assert_green();
}

#[test]
fn switch_c_wip_in_workweave() {
    runner::run(scenarios::switch_c_wip_in_workweave()).assert_green();
}

#[test]
fn raw_switch_in_primary_project() {
    runner::run(scenarios::raw_switch_in_primary_project()).assert_green();
}

#[test]
fn project_pr_merged_before_members() {
    runner::run(scenarios::project_pr_merged_before_members()).assert_green();
}

#[test]
fn hand_edit_declaration_to_existing_branch() {
    runner::run(scenarios::hand_edit_declaration_to_existing_branch()).assert_green();
}

#[test]
fn hand_edit_declaration_to_missing_branch() {
    runner::run(scenarios::hand_edit_declaration_to_missing_branch()).assert_green();
}

#[test]
fn worktree_add_force_on_weave_branch() {
    runner::run(scenarios::worktree_add_force_on_weave_branch()).assert_green();
}

#[test]
fn shared_member_declared_dev() {
    runner::run(scenarios::shared_member_declared_dev()).assert_green();
}

#[test]
fn shared_member_pinned_behind() {
    runner::run(scenarios::shared_member_pinned_behind()).assert_green();
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
    feature_off_stable => Declare,
    review_colleague_fix => Review,
    stable_end_to_end => Declare,
    main_change_into_stable => Land,
    nested_workweaves => WeaveBranch,
    cherry_pick_onto_stable => CherryPick,
);

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
fn a_remote_refusing_the_records_branch_withholds_a_feature_and_publishes_the_trunk() {
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
    runner::run(scenarios::three_way_pins()).assert_fully_decided_green();
}

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
            steps: vec![Step::rwv(
                &a(),
                &["workweave", "create", "--project", "proj", "x"],
            )],
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
        Step::rwv(at, &["lock", "--commit"]),
        Step::git(at, Repo::App, &["branch", "keep"]),
        Step::git(at, Repo::App, &["reset", "--hard", "HEAD~1"]).may_rewrite(&[Repo::App]),
        Step::rwv(
            at,
            &["member", "pin", &Repo::App.rel_path(), "HEAD", "--commit"],
        )
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
                .expect(Exit::Fails)
                .claim(Claim::RemoteContains {
                    repo: Repo::Proj,
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
            Step::git(&a(), Repo::App, &["fetch", "origin"]).expect(Exit::Fails),
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
    /// The target's `main` moves while `sync-to` is held before the
    /// compare-and-swap that advances it, so the compare reads the moved
    /// tip and the landing refuses rather than dropping the move.
    fn a_landing_held_at_its_compare_and_swap_loses_to_a_target_that_moved() {
        let mut steps = landing_from("held");
        steps.extend([
            Step::rwv_paused(&ww("held"), &["sync-to"], ADVANCE_TARGET, "landing"),
            Step::commit(&a(), Repo::App, "raced"),
            Step::release("landing")
                .expect(Exit::Fails)
                .claim(Claim::Contains {
                    at: a(),
                    repo: Repo::App,
                    rev: "main",
                    mark: "raced",
                })
                .claim(Claim::Contains {
                    at: ww("held"),
                    repo: Repo::App,
                    rev: "HEAD",
                    mark: "landing-app",
                }),
        ]);
        let report = bad_steps("a landing loses its race", steps);
        report.assert_green();
        let release = report
            .ran
            .iter()
            .find(|record| record.label == "release landing")
            .unwrap();
        assert!(
            release.output.contains("cannot be fast-forwarded"),
            "{report}\n{}",
            release.output
        );
    }

    fn a_held_landing_released_unraced_lands() {
        let mut steps = landing_from("held");
        steps.extend([
            Step::rwv_paused(&ww("held"), &["sync-to"], ADVANCE_TARGET, "landing"),
            Step::release("landing").expect(Exit::Succeeds).claim(landed()),
        ]);
        bad_steps("a held landing, unraced", steps).assert_green();
    }

    /// Armed for a point the run never passes, rwv holds nowhere: the step
    /// meant to wait for the hold sees the landing run to completion.
    fn the_pause_holds_only_at_the_point_it_names() {
        let mut steps = landing_from("held");
        steps.push(
            Step::rwv_paused(&ww("held"), &["sync-to"], "no-such-point", "landing")
                .expect(Exit::Fails)
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
            Step::rwv(&ww("unarmed"), &["sync-to"])
                .expect(Exit::Succeeds)
                .claim(landed()),
        );
        bad_steps("unarmed", steps).assert_green();
    }
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
/// same judgment where a verb that has one reaches it: a sync whose replay
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
    let out = sandbox.rwv(&primary, &["sync", "drops", "--json"]);
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
    runner::run(scenarios::dc25_url_provenance()).assert_green();
}

#[test]
fn url_change_raw_merge() {
    runner::run(scenarios::url_change_raw_merge()).assert_green();
}

#[test]
fn set_branch_new() {
    runner::run(scenarios::set_branch_new()).assert_green();
}

#[test]
fn member_pin_behind() {
    runner::run(scenarios::member_pin_behind()).assert_green();
}
