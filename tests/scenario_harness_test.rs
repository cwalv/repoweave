//! Branching scenarios run step by step in hermetic sandbox weaves, with
//! design-independent invariants checked after every step.
//!
//! A scenario is a sequence of plain git commands, rwv verbs, and what a forge
//! or colleague does to a remote. Each step carries its expected outcome as
//! data; a step whose outcome a branching design decides stays undecided, and
//! only its invariants are held. A step that needs a verb rwv does not have is
//! recorded as pending and ends the scenario there, announced on stderr.
//!
//! The invariants and what they cannot see are stated in
//! `tests/scenario_harness_test/invariants.rs`; the sandbox's fixed shape in
//! `tests/scenario_harness_test/sandbox.rs`.

mod common;

#[path = "scenario_harness_test/invariants.rs"]
mod invariants;
#[path = "scenario_harness_test/runner.rs"]
mod runner;
#[path = "scenario_harness_test/sandbox.rs"]
mod sandbox;
#[path = "scenario_harness_test/scenarios.rs"]
mod scenarios;

use invariants::{Invariant, LockSite, Violation};
use runner::{MissingVerb, Report, Scenario, Step};
use sandbox::{Machine, Place, Repo, Sandbox};

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
        second_project: None,
        steps,
    })
}

#[test]
fn solo_workweave_lands_and_pushes() {
    runner::run(scenarios::solo_workweave()).assert_fully_decided_green();
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

/// `workweave delete --discard-unmerged-commits` leaves the discarded commit
/// reachable from no ref. The consent is explicit, but the commit is stranded
/// all the same, and the adopted design decides whether that stays.
#[test]
fn giving_up_before_publishing_strands_the_discarded_commit() {
    let report = runner::run(scenarios::workweave_without_b_given_up_before_publishing());
    assert!(
        report.unmet.is_empty() && report.pending.is_none(),
        "{report}"
    );
    assert_eq!(
        report.violations,
        vec![(
            "rwv workweave proj delete anon --discard-unmerged-commits".to_owned(),
            Violation::Stranded {
                repo: Repo::App,
                commit: report.marks["anon-app"].clone(),
            }
        )],
        "{report}"
    );
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
    cross_repo_pr => Publish,
    cross_repo_pr_fork => Publish,
    resume_on_machine_b => Publish,
    feature_in_primary_while_main_moves => WeaveBranch,
    workweave_without_b_then_pr => Rename,
    workweave_without_b_given_up_after_publishing => Publish,
    feature_off_stable => WeaveBranch,
    review_colleague_fix => Review,
    stable_end_to_end => WeaveBranch,
    main_change_into_stable => WeaveBranch,
    nested_workweaves => WeaveBranch,
    origin_and_base_edge_cases => BaseRecord,
    hand_edit_origin_record => BaseRecord,
    cherry_pick_onto_stable => WeaveBranch,
    pr_closed_unmerged => Publish,
);

#[test]
fn snapshot_reads_every_lock_site_and_ref_store() {
    let sandbox = Sandbox::new(None);
    let report = runner::run_in(
        &sandbox,
        Scenario {
            name: "one workweave",
            second_project: None,
            steps: vec![Step::rwv(
                &Place::Primary(Machine::A),
                &["workweave", "proj", "create", "x"],
            )],
        },
    );
    report.assert_green();
    let snapshot = invariants::snapshot(&sandbox);
    let sites = [
        LockSite::Checkout(Place::Primary(Machine::A), Repo::Proj, "main".to_owned()),
        LockSite::Checkout(Place::ww(Machine::A, "x"), Repo::Proj, "proj--x".to_owned()),
        LockSite::RemoteMain(Repo::Proj),
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
    for repo in [Repo::Proj, Repo::App, Repo::Lib, Repo::Docs] {
        assert!(
            snapshot.reachable_from(repo) > 0,
            "nothing reachable in {repo:?}"
        );
    }
}

#[test]
fn stranded_check_catches_a_reset_hard() {
    let a = Place::Primary(Machine::A);
    let report = bad_steps(
        "reset --hard strands a commit",
        vec![
            Step::commit(&a, Repo::App, "doomed"),
            Step::git(&a, Repo::App, &["reset", "--hard", "HEAD~1"]),
        ],
    );
    assert_eq!(
        report.violations,
        vec![(
            "git -C app reset --hard HEAD~1".to_owned(),
            Violation::Stranded {
                repo: Repo::App,
                commit: report.marks["doomed"].clone(),
            }
        )],
        "{report}"
    );
}

#[test]
fn stranded_check_catches_a_deleted_branch_in_a_workweave() {
    let ww = Place::ww(Machine::A, "x");
    let report = bad_steps(
        "branch -D strands a commit",
        vec![
            Step::rwv(
                &Place::Primary(Machine::A),
                &["workweave", "proj", "create", "x"],
            ),
            Step::git(&ww, Repo::Lib, &["switch", "-c", "side"]),
            Step::commit(&ww, Repo::Lib, "side-work"),
            Step::git(&ww, Repo::Lib, &["switch", "proj--x"]),
            Step::git(&ww, Repo::Lib, &["branch", "-D", "side"]),
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
    let a = Place::Primary(Machine::A);
    let b = Place::Primary(Machine::B);
    let report = bad_steps(
        "project pushed without its member",
        vec![
            Step::new_machine(Machine::B),
            Step::commit(&a, Repo::App, "unpushed"),
            Step::rwv(&a, &["lock", "--commit"]),
            Step::git(&a, Repo::Proj, &["push", "origin", "main"]),
            Step::git(&b, Repo::Proj, &["pull", "--ff-only"]),
        ],
    );
    let unpushed = report.marks["unpushed"].clone();
    assert_eq!(
        report.violations,
        vec![
            (
                "git -C proj push origin main".to_owned(),
                Violation::UnresolvedPin {
                    site: LockSite::RemoteMain(Repo::Proj),
                    member: Repo::App,
                    pin: unpushed.clone(),
                }
            ),
            (
                "git -C proj pull --ff-only".to_owned(),
                Violation::UnresolvedPin {
                    site: LockSite::Checkout(b, Repo::Proj, "main".to_owned()),
                    member: Repo::App,
                    pin: unpushed,
                }
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
        Step::git(at, Repo::App, &["reset", "--hard", "HEAD~1"]),
        Step::rwv(at, &["lock", "--commit"]).may_rewind(may_rewind),
    ]
}

#[test]
fn rewind_check_catches_a_relock_after_reset_in_the_primary() {
    let a = Place::Primary(Machine::A);
    let report = bad_steps("relock after reset", rewind_steps(&a, &[]));
    assert_eq!(
        fired(&report),
        vec![("rwv lock --commit", Invariant::NoPinRewound)],
        "{report}"
    );
    let Violation::PinRewound {
        site,
        member,
        dropped,
        ..
    } = &report.violations[0].1
    else {
        unreachable!()
    };
    assert_eq!(
        (site, *member, dropped.clone()),
        (
            &LockSite::Checkout(a, Repo::Proj, "main".to_owned()),
            Repo::App,
            vec![report.marks["ahead"].clone()]
        )
    );
}

#[test]
fn rewind_check_catches_a_relock_after_reset_in_a_workweave() {
    let ww = Place::ww(Machine::A, "x");
    let mut steps = vec![Step::rwv(
        &Place::Primary(Machine::A),
        &["workweave", "proj", "create", "x"],
    )];
    steps.extend(rewind_steps(&ww, &[]));
    let report = bad_steps("relock after reset in a workweave", steps);
    assert_eq!(
        fired(&report),
        vec![("rwv lock --commit", Invariant::NoPinRewound)],
        "{report}"
    );
}

#[test]
fn rewind_check_spares_a_step_that_names_the_member() {
    let a = Place::Primary(Machine::A);
    let report = bad_steps("declared rewind", rewind_steps(&a, &[Repo::App]));
    report.assert_green();
}

#[test]
fn pin_check_reads_a_commit_no_ref_reaches_as_unresolved() {
    let a = Place::Primary(Machine::A);
    let report = bad_steps(
        "remote branch deleted under a pin",
        vec![
            Step::git(&a, Repo::App, &["switch", "-c", "feat-x"]),
            Step::commit(&a, Repo::App, "feat-app"),
            Step::git(&a, Repo::App, &["push", "origin", "feat-x"]),
            Step::rwv(&a, &["lock", "--commit"]),
            Step::git(&a, Repo::Proj, &["push", "origin", "main"]),
            Step::forge(runner::Forge::DeleteBranch {
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
                site: LockSite::RemoteMain(Repo::Proj),
                member: Repo::App,
                pin: report.marks["feat-app"].clone(),
            }
        )],
        "{report}"
    );
}
