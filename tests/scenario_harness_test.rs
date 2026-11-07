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
            steps: vec![Step::rwv(&a(), &["workweave", "proj", "create", "x"])],
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
            Step::rwv(&a(), &["workweave", "proj", "create", "x"]),
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
        Step::rwv(at, &["lock", "--commit"]).may_rewind(may_rewind),
    ]
}

#[test]
fn rewind_check_catches_a_relock_after_reset_in_the_primary() {
    let report = bad_steps("relock after reset", rewind_steps(&a(), &[]));
    let relock = ("rwv lock --commit", Invariant::NoPinRewound);
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
    let mut steps = vec![Step::rwv(&a(), &["workweave", "proj", "create", "x"])];
    steps.extend(rewind_steps(&ww("x"), &[]));
    let report = bad_steps("relock after reset in a workweave", steps);
    let relock = ("rwv lock --commit", Invariant::NoPinRewound);
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
            Step::git(&a(), Repo::App, &["switch", "-c", "feat-x"]),
            Step::commit(&a(), Repo::App, "feat-app"),
            Step::git(&a(), Repo::App, &["push", "origin", "feat-x"])
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
    let canonical = sandbox.canonical(Machine::A, Repo::Proj);
    assert_eq!(invariants::read_records(&sandbox, &canonical), None);
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
        Step::rwv(&a(), &["workweave", "proj", "create", name]).expect(Exit::Succeeds),
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
/// `rwv remove --delete` judges the clone's edits and every checkout derived
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
                Step::rwv(&a(), &["workweave", "proj", "create", "other"]).expect(Exit::Succeeds)
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

    let out = sandbox.rwv(&primary, &["remove", &lib_path, "--delete"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    for line in [
        format!("rwv remove: {lib_path}: edits dirty\n    scratch.txt\n"),
        format!("rwv remove: {lib_path}: checkout in another workspace (proj--other)\n"),
        format!(
            "rwv remove: {lib_path}: checkout in a raw worktree\n    {}\n",
            raw.display()
        ),
        "choices:\n".to_owned(),
        "  free the other checkout\n    rwv workweave proj delete other\n".to_owned(),
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
        .rwv(&primary, &["workweave", "proj", "create", "drops"])
        .status
        .success());
    for args in [
        &["remove", lib_path.as_str(), "--delete"][..],
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
