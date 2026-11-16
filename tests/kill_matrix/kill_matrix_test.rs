//! Every pause point the source declares, for every verb that reaches it:
//! the verb killed there for real, then `--continue` in one run and
//! `rwv abort` in another.
//!
//! [`POINTS`] is the input. Each row names a point, the cells that reach it
//! and the shapes that pass it covered by those cells; a source test holds the
//! rows equal to the points the source declares, and another holds them equal
//! to the points each shape passes.
//! A shape is a verb in one setup, and its cells are generated from the rows,
//! so a point added to a row gains its cells and a point added to the source
//! without a row fails.
//!
//! A cell passes when `--continue` ends the operation in the state an unkilled
//! run of the same setup ends in, and when `rwv abort`, typed in the seat and,
//! for a landing, in its target, leaves every place as it was before the
//! verb, its backups included but for the slots the abort keeps. Both leave no
//! operation recorded and nothing `rwv doctor --all` reports.
//!
//! What a cell does not see: a verb killed twice (the two-kill scenarios of
//! `scenario_harness_test.rs` do that), a conflict resolved any other way than
//! the one the shape's fix does, and a remote that moved while the verb was
//! down.

use repoweave::diagnosis::State;
use repoweave::manifest::Role;
use repoweave::refusal::RefusalKind;

use crate::common;
use crate::common::scenario_runner::{self, Claim, Exit, Scenario, Step};
use crate::common::scenario_sandbox::{Machine, Place, Remote, Repo, Sandbox, Seed};
use common::scenario_steps::{
    a, b, create, feature_onto_stable, main_checked_out_nowhere, main_moves_app, port_me_on_main,
    publish_app_and_project, recurse_moved_back, stable_lines, stack_on_a_seat_under, ww, APP,
    DOCS, LIB,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    /// Killed in the run that starts the verb.
    Start,
    /// Killed in the `--continue` that follows the shape's stop.
    Resumed,
    /// Killed at the shape's `abort_after`, then `rwv abort` killed at the
    /// point, then abort again.
    DuringAbort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Continue,
    Abort,
    AbortInTarget,
    /// `rwv doctor --fix` in the primary and in a seat the operation does not
    /// touch, then `rwv abort`: a repair run elsewhere in the weave leaves the
    /// operation what its abort restores from.
    AbortAfterDoctorElsewhere,
    /// The same doctor runs, then the shape's `--continue`: held to an
    /// unkilled run's end state.
    ContinueAfterDoctorElsewhere,
}

impl Ending {
    fn word(self) -> &'static str {
        match self {
            Ending::Continue => "continue",
            Ending::Abort => "abort",
            Ending::AbortInTarget => "abort-in-target",
            Ending::AbortAfterDoctorElsewhere => "abort after doctor elsewhere",
            Ending::ContinueAfterDoctorElsewhere => "continue after doctor elsewhere",
        }
    }
}

/// Another landing into the same target, held at `claimed` in `w1` while the
/// shape's own landing, in `w2`, waits on its claim.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Contention {
    Live,
}

struct Stop {
    exit: fn() -> Exit,
    fix: fn() -> Vec<Step>,
}

pub(super) struct Shape {
    pub id: &'static str,
    setup: fn() -> Vec<Step>,
    seed: fn() -> Seed,
    start: &'static [&'static str],
    resume: &'static [&'static str],
    at: fn() -> Place,
    target: Option<fn() -> Place>,
    stop: Option<Stop>,
    abort_after: &'static str,
    contender: Option<Contention>,
    refreshes_drift: bool,
}

fn default_seed() -> Seed {
    Seed::default()
}

fn dependency_docs() -> Seed {
    Seed::default().role(Repo::Docs, Role::Dependency)
}

fn seat_w1() -> Place {
    ww("w1")
}

fn seat_w2() -> Place {
    ww("w2")
}

fn two_seats() -> Vec<Step> {
    vec![
        create("w1"),
        create("w2"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::commit(&ww("w2"), Repo::Lib, "w2-lib"),
    ]
}

fn seat_w() -> Place {
    ww("w")
}

fn seat_p() -> Place {
    ww("p")
}

fn primary_a() -> Place {
    a()
}

fn machine_b() -> Place {
    b()
}

fn parks() -> Exit {
    Exit::refuses_kind(RefusalKind::OpParked)
}

fn parks_on_the_replayed_manifest() -> Exit {
    Exit::refuses(Repo::Proj, State::EditsReplayDiffers)
}

fn seat_ahead_of_a_moved_main(history: &'static str) -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["branch", "set-history", history]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::commit(&ww("w1"), Repo::Proj, "w1-proj"),
        Step::commit(&a(), Repo::App, "main-app"),
        hand_moves_app(
            Step::rwv(&a(), &["member", "pin", APP, "HEAD", "--commit"]).expect(Exit::Succeeds),
        ),
    ]
}

/// `w1`'s `app` commit landed on `main`, whose `app` was then rewritten by
/// hand and pinned there: everything `w1` holds sits under a pin of `main`'s
/// lock history, which its branch no longer contains, so the sync follows.
fn following_seat() -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["branch", "set-history", "linear"]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::rwv(&ww("w1"), LAND).expect(Exit::Succeeds),
        hand_moves_app(Step::git(
            &a(),
            Repo::App,
            &["reset", "-q", "--hard", "HEAD~1"],
        )),
        Step::commit(&a(), Repo::App, "main-app"),
        hand_moves_app(
            Step::rwv(&a(), &["member", "pin", APP, "HEAD", "--commit"]).expect(Exit::Succeeds),
        ),
    ]
}

fn linear_seat() -> Vec<Step> {
    seat_ahead_of_a_moved_main("linear")
}

fn merge_seat() -> Vec<Step> {
    seat_ahead_of_a_moved_main("merge")
}

fn in_place_seat() -> Vec<Step> {
    vec![
        create("w"),
        Step::git(&a(), Repo::Proj, &["switch", "-q", "--detach"]),
        Step::commit(&ww("w"), Repo::App, "w-app"),
    ]
}

fn refs_seat() -> Vec<Step> {
    let mut steps = vec![create("w")];
    steps.extend(main_checked_out_nowhere());
    steps.push(Step::commit(&ww("w"), Repo::App, "w-app"));
    steps
}

fn carrying_seat(history: &'static str) -> Vec<Step> {
    let mut steps = stack_on_a_seat_under(history);
    steps.extend(main_moves_app("main-app"));
    steps
}

fn carrying_linear_seat() -> Vec<Step> {
    carrying_seat("linear")
}

fn carrying_merge_seat() -> Vec<Step> {
    carrying_seat("merge")
}

fn conflicting_seat(history: &'static str) -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["branch", "set-history", history]).expect(Exit::Succeeds),
        create("w1"),
        Step::hand_edit(&ww("w1"), Repo::App, "README", "app\n", "app in w1\n"),
        Step::git(&ww("w1"), Repo::App, &["commit", "-qam", "readme-w1"]),
        Step::rwv(&ww("w1"), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::hand_edit(&a(), Repo::App, "README", "app\n", "app on main\n"),
        Step::git(&a(), Repo::App, &["commit", "-qam", "readme-main"]),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]
}

fn conflicting_linear_seat() -> Vec<Step> {
    conflicting_seat("linear")
}

fn conflicting_merge_seat() -> Vec<Step> {
    conflicting_seat("merge")
}

/// The conflict's markers carry the id of the commit replayed, which differs
/// between sandboxes, so the resolution starts from one side's clean file.
fn resolve_readme(ours: &str) -> Vec<Step> {
    vec![
        Step::git(&ww("w1"), Repo::App, &["checkout", "--ours", "README"]).expect(Exit::Succeeds),
        Step::hand_edit(&ww("w1"), Repo::App, "README", ours, "resolved by hand\n"),
        Step::git(&ww("w1"), Repo::App, &["add", "README"]).expect(Exit::Succeeds),
    ]
}

fn resolve_readme_replayed() -> Vec<Step> {
    resolve_readme("app on main\n")
}

fn resolve_readme_merged() -> Vec<Step> {
    resolve_readme("app in w1\n")
}

fn manifest_stop_seat() -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["member", "remove", DOCS]).expect(Exit::Succeeds),
        create("w"),
        Step::rwv(&a(), &["member", "set-role", APP, "fork"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["member", "add", "url:origin:docs"]).expect(Exit::Succeeds),
        Step::rwv(
            &ww("w"),
            &["member", "set-recurse-submodules", APP, "false"],
        )
        .expect(Exit::Succeeds),
    ]
}

/// `w1` is ahead of a `main` that added `docs` after `w1` was made: a merge
/// sync's project merge names a member `w1` has no checkout of.
fn merge_seat_behind_an_added_member() -> Vec<Step> {
    let mut steps = vec![Step::rwv(&a(), &["member", "remove", DOCS]).expect(Exit::Succeeds)];
    steps.extend(merge_seat());
    steps.push(Step::rwv(&a(), &["member", "add", "url:origin:docs"]).expect(Exit::Succeeds));
    steps
}

/// Machine B's project is behind an upstream that added `docs` after B
/// cloned it, and holds a commit of its own: `pull --merge-upstream` merges a
/// `rwv.toml` naming a member B has no clone of.
fn published_upstream_adding_a_member() -> Vec<Step> {
    let mut steps = vec![Step::rwv(&a(), &["member", "remove", DOCS]).expect(Exit::Succeeds)];
    steps.extend(publish_app_and_project("app-one"));
    steps.extend([
        Step::new_machine(Machine::B),
        Step::rwv(&a(), &["member", "add", "url:origin:docs"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Proj), "main"),
        Step::commit(&b(), Repo::Proj, "b-notes"),
    ]);
    steps
}

fn settle_manifest() -> Vec<Step> {
    recurse_moved_back(&ww("w")).into()
}

fn published_and_moved_upstream() -> Vec<Step> {
    let mut steps = publish_app_and_project("app-one");
    steps.extend([
        Step::new_machine(Machine::B),
        Step::commit(&a(), Repo::App, "a-two"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
        Step::git(&a(), Repo::App, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::App), "main"),
        Step::git(&a(), Repo::Proj, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Proj), "main"),
        Step::commit(&b(), Repo::Proj, "b-notes"),
    ]);
    steps
}

fn drifted_in_place_seat() -> Vec<Step> {
    let mut steps = in_place_seat();
    steps.push(Step::hand_commit(
        &ww("w"),
        Repo::App,
        "main",
        &[("moved.txt", "moved\n")],
    ));
    steps
}

fn lib_moved_upstream() -> Vec<Step> {
    vec![
        Step::new_machine(Machine::B),
        Step::commit(&b(), Repo::Lib, "beta-lib"),
        Step::git(&b(), Repo::Lib, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Lib), "main"),
        Step::commit(&a(), Repo::Lib, "alpha-lib"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]
}

fn lib_and_app_moved_upstream() -> Vec<Step> {
    let mut steps = lib_moved_upstream();
    steps.extend([
        Step::commit(&b(), Repo::App, "beta-app"),
        Step::git(&b(), Repo::App, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::App), "main"),
        Step::commit(&a(), Repo::App, "alpha-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
    ]);
    steps
}

fn port_seat() -> Vec<Step> {
    let mut steps = stable_lines();
    steps.extend(port_me_on_main());
    steps.extend(feature_onto_stable());
    steps
}

const SYNC: &[&str] = &["sync"];
const SYNC_CONTINUE: &[&str] = &["sync", "--continue"];
const LAND: &[&str] = &["land"];
const LAND_RETIRE: &[&str] = &["land", "--retire"];
const LAND_CONTINUE: &[&str] = &["land", "--continue"];
const LAND_LINEAR_MANIFEST: &[&str] =
    &["land", "--history", "linear", "--allow-member-change", APP];
const SYNC_LINEAR_MANIFEST: &[&str] = &["sync", "--history", "linear"];
const PULL_MERGE: &[&str] = &["pull", "--merge-upstream"];
const PULL_RESET: &[&str] = &["pull", "--reset-to-upstream"];
const PULL_CONTINUE: &[&str] = &["pull", "--continue"];
const UPDATE_MERGE: &[&str] = &["update", "--remote", "--merge-upstream"];
const UPDATE_RESET: &[&str] = &["update", "--remote", "--reset-to-upstream", LIB];
const UPDATE_BOTH: &[&str] = &[
    "update",
    "--remote",
    "--merge-upstream",
    "--reset-to-upstream",
    LIB,
];
const UPDATE_CONTINUE: &[&str] = &["update", "--remote", "--continue"];
const PICK: &[&str] = &["cherry-pick", "main"];
const PICK_CONTINUE: &[&str] = &["cherry-pick", "--continue"];

pub(super) const SHAPES: &[Shape] = &[
    Shape {
        id: "sync_linear",
        setup: linear_seat,
        seed: default_seed,
        start: SYNC,
        resume: SYNC_CONTINUE,
        at: seat_w1,
        target: None,
        stop: None,
        abort_after: "sync-relock-committed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "sync_linear_following",
        setup: following_seat,
        seed: default_seed,
        start: SYNC,
        resume: SYNC_CONTINUE,
        at: seat_w1,
        target: None,
        stop: None,
        abort_after: "sync-member-followed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "sync_merge",
        setup: merge_seat,
        seed: default_seed,
        start: SYNC,
        resume: SYNC_CONTINUE,
        at: seat_w1,
        target: None,
        stop: None,
        abort_after: "carry-moved",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "sync_linear_carrying",
        setup: carrying_linear_seat,
        seed: default_seed,
        start: SYNC,
        resume: SYNC_CONTINUE,
        at: seat_w1,
        target: None,
        stop: None,
        abort_after: "carry-moved",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "sync_linear_own_stop",
        setup: conflicting_linear_seat,
        seed: default_seed,
        start: SYNC,
        resume: SYNC_CONTINUE,
        at: seat_w1,
        target: None,
        stop: Some(Stop {
            exit: parks,
            fix: resolve_readme_replayed,
        }),
        abort_after: "op-savepoint-taken",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "sync_linear_manifest_stop",
        setup: manifest_stop_seat,
        seed: default_seed,
        start: SYNC_LINEAR_MANIFEST,
        resume: SYNC_CONTINUE,
        at: seat_w,
        target: None,
        stop: Some(Stop {
            exit: parks_on_the_replayed_manifest,
            fix: settle_manifest,
        }),
        abort_after: "op-savepoint-taken",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_linear",
        setup: linear_seat,
        seed: default_seed,
        start: LAND,
        resume: LAND_CONTINUE,
        at: seat_w1,
        target: Some(primary_a),
        stop: None,
        abort_after: "landed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_merge",
        setup: merge_seat,
        seed: default_seed,
        start: LAND,
        resume: LAND_CONTINUE,
        at: seat_w1,
        target: Some(primary_a),
        stop: None,
        abort_after: "landed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_linear_retiring",
        setup: linear_seat,
        seed: default_seed,
        start: LAND_RETIRE,
        resume: LAND_CONTINUE,
        at: seat_w1,
        target: None,
        stop: None,
        abort_after: "landed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_linear_in_place",
        setup: in_place_seat,
        seed: default_seed,
        start: &["land", "--history", "linear"],
        resume: LAND_CONTINUE,
        at: seat_w,
        target: Some(primary_a),
        stop: None,
        abort_after: "landed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_linear_as_refs",
        setup: refs_seat,
        seed: default_seed,
        start: &["land", "--history", "linear"],
        resume: LAND_CONTINUE,
        at: seat_w,
        target: Some(primary_a),
        stop: None,
        abort_after: "landed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_linear_carrying",
        setup: carrying_linear_seat,
        seed: default_seed,
        start: LAND,
        resume: LAND_CONTINUE,
        at: seat_w1,
        target: Some(primary_a),
        stop: None,
        abort_after: "landed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_linear_own_stop",
        setup: conflicting_linear_seat,
        seed: default_seed,
        start: LAND,
        resume: LAND_CONTINUE,
        at: seat_w1,
        target: None,
        stop: Some(Stop {
            exit: parks,
            fix: resolve_readme_replayed,
        }),
        abort_after: "claimed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_linear_manifest_stop",
        setup: manifest_stop_seat,
        seed: default_seed,
        start: LAND_LINEAR_MANIFEST,
        resume: LAND_CONTINUE,
        at: seat_w,
        target: None,
        stop: Some(Stop {
            exit: parks_on_the_replayed_manifest,
            fix: settle_manifest,
        }),
        abort_after: "claimed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "sync_merge_carrying",
        setup: carrying_merge_seat,
        seed: default_seed,
        start: SYNC,
        resume: SYNC_CONTINUE,
        at: seat_w1,
        target: None,
        stop: None,
        abort_after: "carry-moved",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "sync_merge_own_stop",
        setup: conflicting_merge_seat,
        seed: default_seed,
        start: SYNC,
        resume: SYNC_CONTINUE,
        at: seat_w1,
        target: None,
        stop: Some(Stop {
            exit: parks,
            fix: resolve_readme_merged,
        }),
        abort_after: "op-savepoint-taken",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_merge_carrying",
        setup: carrying_merge_seat,
        seed: default_seed,
        start: LAND,
        resume: LAND_CONTINUE,
        at: seat_w1,
        target: Some(primary_a),
        stop: None,
        abort_after: "landed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_merge_own_stop",
        setup: conflicting_merge_seat,
        seed: default_seed,
        start: LAND,
        resume: LAND_CONTINUE,
        at: seat_w1,
        target: None,
        stop: Some(Stop {
            exit: parks,
            fix: resolve_readme_merged,
        }),
        abort_after: "claimed",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "land_linear_in_place_drifted",
        setup: drifted_in_place_seat,
        seed: default_seed,
        start: &["land", "--history", "linear"],
        resume: LAND_CONTINUE,
        at: seat_w,
        target: None,
        stop: None,
        abort_after: "landed",
        contender: None,
        refreshes_drift: true,
    },
    Shape {
        id: "land_linear_waiting",
        setup: two_seats,
        seed: default_seed,
        start: &["land", "--history", "linear"],
        resume: LAND_CONTINUE,
        at: seat_w2,
        target: None,
        stop: None,
        abort_after: "claimed",
        contender: Some(Contention::Live),
        refreshes_drift: false,
    },
    Shape {
        id: "pull_merge_upstream",
        setup: published_and_moved_upstream,
        seed: default_seed,
        start: PULL_MERGE,
        resume: PULL_CONTINUE,
        at: machine_b,
        target: None,
        stop: None,
        abort_after: "op-savepoint-taken",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "sync_merge_adding",
        setup: merge_seat_behind_an_added_member,
        seed: default_seed,
        start: SYNC,
        resume: SYNC_CONTINUE,
        at: seat_w1,
        target: None,
        stop: None,
        abort_after: "carry-moved",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "pull_merge_upstream_adding",
        setup: published_upstream_adding_a_member,
        seed: default_seed,
        start: PULL_MERGE,
        resume: PULL_CONTINUE,
        at: machine_b,
        target: None,
        stop: None,
        abort_after: "op-savepoint-taken",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "pull_reset_to_upstream",
        setup: published_and_moved_upstream,
        seed: default_seed,
        start: PULL_RESET,
        resume: PULL_CONTINUE,
        at: machine_b,
        target: None,
        stop: None,
        abort_after: "reset-to-upstream",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "update_remote_merge_upstream",
        setup: lib_and_app_moved_upstream,
        seed: default_seed,
        start: UPDATE_MERGE,
        resume: UPDATE_CONTINUE,
        at: primary_a,
        target: None,
        stop: None,
        abort_after: "op-savepoint-taken",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "update_remote_reset_to_upstream",
        setup: lib_moved_upstream,
        seed: default_seed,
        start: UPDATE_RESET,
        resume: UPDATE_CONTINUE,
        at: primary_a,
        target: None,
        stop: None,
        abort_after: "reset-to-upstream",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "update_remote_merge_and_reset_to_upstream",
        setup: lib_and_app_moved_upstream,
        seed: default_seed,
        start: UPDATE_BOTH,
        resume: UPDATE_CONTINUE,
        at: primary_a,
        target: None,
        stop: None,
        abort_after: "reset-to-upstream",
        contender: None,
        refreshes_drift: false,
    },
    Shape {
        id: "cherry_pick",
        setup: port_seat,
        seed: dependency_docs,
        start: PICK,
        resume: PICK_CONTINUE,
        at: seat_p,
        target: None,
        stop: None,
        abort_after: "cherry-pick-member-copied",
        contender: None,
        refreshes_drift: false,
    },
];

fn shape(id: &str) -> &'static Shape {
    SHAPES
        .iter()
        .find(|shape| shape.id == id)
        .unwrap_or_else(|| panic!("no shape is named {id}"))
}

/// What the abort of a shape keeps beyond the backups it started with: the
/// commits and edits it discards go to `aborted` and `aborted-working-copy`,
/// and a verb that resets or follows a branch keeps the tip it superseded,
/// named by its abbreviated id.
fn abort_keeps(shape: &Shape) -> &'static [&'static str] {
    match shape.id {
        "cherry_pick"
        | "land_linear"
        | "land_merge"
        | "land_linear_manifest_stop"
        | "sync_linear"
        | "sync_linear_manifest_stop" => &["aborted", "aborted-working-copy"],
        "pull_reset_to_upstream"
        | "update_remote_reset_to_upstream"
        | "update_remote_merge_and_reset_to_upstream"
        | "sync_linear_following" => &["<tip>"],
        _ => &["aborted"],
    }
}

/// What the abort of a shape held at its stop keeps beyond the backups it
/// started with: the commits it discards go to `aborted`, and the conflicted
/// files of the stopped member to `aborted-working-copy`.
fn stopped_abort_keeps(shape: &Shape) -> &'static [&'static str] {
    match shape.id {
        "sync_linear_manifest_stop" | "land_linear_manifest_stop" => &["aborted"],
        "sync_linear_own_stop" | "sync_merge_own_stop" | "land_merge_own_stop" => {
            &["aborted-working-copy"]
        }
        "land_linear_own_stop" => &["aborted", "aborted-working-copy"],
        other => panic!("{other} does not stop"),
    }
}

/// What a shape's verb, its continue and its abort move backwards, as
/// `(pins rewound, declared branches restored, declared branches rewritten)`:
/// an abort puts back what the verb moved, and a reset moves a declared
/// branch off the commits it held.
fn moved_back(shape: &Shape) -> (&'static [Repo], &'static [Repo], &'static [Repo]) {
    const APP: &[Repo] = &[Repo::App];
    const NONE: &[Repo] = &[];
    match shape.id {
        "cherry_pick" => (&[Repo::App, Repo::Lib, Repo::Docs], NONE, NONE),
        "land_linear"
        | "land_linear_as_refs"
        | "land_linear_carrying"
        | "land_linear_in_place"
        | "land_linear_in_place_drifted"
        | "land_linear_own_stop"
        | "land_linear_retiring"
        | "land_merge"
        | "land_merge_carrying"
        | "land_merge_own_stop" => (APP, &[Repo::Proj, Repo::App], NONE),
        "land_linear_manifest_stop" => (NONE, &[Repo::Proj], NONE),
        "pull_reset_to_upstream" => (APP, &[Repo::Proj], &[Repo::Proj]),
        "sync_linear"
        | "sync_linear_carrying"
        | "sync_linear_following"
        | "sync_linear_own_stop"
        | "sync_merge"
        | "sync_merge_adding"
        | "sync_merge_carrying"
        | "sync_merge_own_stop" => (APP, NONE, NONE),
        "update_remote_reset_to_upstream" | "update_remote_merge_and_reset_to_upstream" => {
            (&[Repo::Lib], &[Repo::Lib], &[Repo::Lib])
        }
        _ => (NONE, NONE, NONE),
    }
}

/// The repo a hand resolution replaces a commit of: the file the shape's
/// conflict is in, or the manifest.
fn strands_on_resolving(shape: &Shape) -> &'static [Repo] {
    match shape.id {
        "land_linear_manifest_stop" | "sync_linear_manifest_stop" => &[Repo::Proj],
        _ => &[Repo::App],
    }
}

fn settled(shape: &Shape, step: Step) -> Step {
    let (rewound, restored, rewritten) = moved_back(shape);
    step.may_rewind(rewound)
        .may_restore(restored)
        .may_rewrite(rewritten)
}

fn hand_moves_app(step: Step) -> Step {
    step.may_strand(&[Repo::App])
        .may_rewind(&[Repo::App])
        .may_restore(&[Repo::App])
        .may_rewrite(&[Repo::App])
}

fn run_of(shape: &Shape, expect: Exit, args: &'static [&'static str]) -> Step {
    settled(shape, Step::rwv(&(shape.at)(), args).expect(expect))
}

fn retires_its_seat(shape: &Shape) -> bool {
    shape.start.contains(&"--retire")
}

fn ends_clean(shape: &Shape, seat_gone: bool) -> Vec<Step> {
    let mut steps = vec![Step::rwv(&a(), &["status"])
        .expect(Exit::Succeeds)
        .claim(Claim::OutputLacks("in progress"))];
    let at = (shape.at)();
    if seat_gone {
        steps.push(
            Step::rwv(&a(), &["doctor", "--all"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("rwv doctor --fix")),
        );
        return steps;
    }
    if at != a() {
        steps.push(
            Step::rwv(&at, &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
        );
    }
    steps.extend([
        Step::rwv(&at, &["abort"]).expect(Exit::refuses_kind(RefusalKind::NoOpRecorded)),
        Step::rwv(&at, shape.resume).expect(Exit::refuses_kind(RefusalKind::NoOpRecorded)),
    ]);
    steps.push(
        Step::rwv(&a(), &["doctor", "--all"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("rwv doctor --fix")),
    );
    steps
}

fn savepoints_outlive(shape: &Shape) -> Vec<Step> {
    let mut steps = vec![
        Step::rwv(&a(), &["doctor", "--all"])
            .claim(Claim::FindingsOnly("orphaned-savepoint"))
            .claim(Claim::OutputContains("`rwv doctor --fix`")),
        Step::rwv(&a(), &["doctor", "--fix"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("backup projects/proj  HEAD/"))
            .claim(Claim::OutputContains("backup local/remotes/app  HEAD/")),
        Step::rwv(&a(), &["backup", "list"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains(" w1-proj\n"))
            .claim(Claim::OutputContains(" w1-app\n")),
    ];
    steps.extend(ends_clean(shape, true));
    steps
}

const HOLDER: &[&str] = &["land", "--history", "linear"];

fn holding(shape: &Shape) -> Vec<Step> {
    match shape.contender {
        None => Vec::new(),
        Some(Contention::Live) => vec![Step::rwv_paused(&ww("w1"), HOLDER, "claimed", "first")],
    }
}

/// What ends the contender once the shape's own run is over or about to be.
fn contender_ends(shape: &Shape) -> Vec<Step> {
    match shape.contender {
        None => Vec::new(),
        Some(Contention::Live) => vec![Step::release("first").expect(Exit::Succeeds)],
    }
}

fn until_stopped(shape: &Shape, phase: Phase, point: &'static str) -> Vec<Step> {
    let at = (shape.at)();
    let mut steps = (shape.setup)();
    steps.extend(holding(shape));
    steps.push(Step::capture("before"));
    match phase {
        Phase::Start => {
            let held = settled(shape, Step::rwv_paused(&at, shape.start, point, "held"));
            steps.push(
                match AFTER_THE_RECORD.contains(&point) && aligns_by_update(shape) {
                    true => held.may_leave_unresolved(&[Repo::Docs]),
                    false => held,
                },
            );
        }
        Phase::DuringAbort => {
            steps.push(settled(
                shape,
                Step::rwv_paused(&at, shape.start, shape.abort_after, "held"),
            ));
            steps.push(Step::kill("held"));
            steps.push(settled(
                shape,
                Step::rwv_paused(&at, &["abort"], point, "aborting"),
            ));
            steps.push(Step::kill("aborting"));
            return steps;
        }
        Phase::Resumed => {
            let stop = shape
                .stop
                .as_ref()
                .expect("a resumed cell has a shape that stops");
            steps.push(run_of(shape, (stop.exit)(), shape.start));
            steps.extend((stop.fix)());
            let resumed = settled(shape, Step::rwv_paused(&at, shape.resume, point, "held"));
            steps.push(resumed.resolved_by_hand(strands_on_resolving(shape)));
        }
    }
    steps.push(Step::kill("held"));
    steps
}

/// Points a verb passes before it has written its operation record: a kill
/// there leaves nothing to continue or abort, and the verb run again ends as
/// an unkilled run does.
const BEFORE_THE_RECORD: &[&str] = &[
    "durable-file-created",
    "durable-file-staged",
    "fetch-under-claim",
];

fn recorded(point: &str) -> bool {
    !BEFORE_THE_RECORD.contains(&point) && !AFTER_THE_RECORD.contains(&point)
}

/// Points a verb passes once its operation record is gone: a kill there
/// leaves nothing to continue or abort, `rwv status` shows a member the verb
/// left without a checkout as missing, `rwv doctor --all` names `rwv doctor
/// --fix` as what makes it, and a verb run again ends as an unkilled run does.
const AFTER_THE_RECORD: &[&str] = &["aligning-source"];

/// Shapes whose verb, at [`AFTER_THE_RECORD`], has yet to make the checkout
/// of a member the composed manifest added.
const ALIGNS_A_MEMBER_IT_ADDS: &[&str] = &[
    "land_linear_manifest_stop",
    "sync_linear_manifest_stop",
    "sync_merge_adding",
];

/// Shapes whose verb is `rwv pull`, which aligns the members of the
/// workspace it runs in as `rwv update` does once its record is gone: a kill
/// at [`AFTER_THE_RECORD`] leaves them, a pin it took in at a commit no clone
/// here holds among them, to `rwv update`.
fn aligns_by_update(shape: &Shape) -> bool {
    shape.start.first() == Some(&"pull")
}

fn converged(shape: &Shape) -> Vec<Step> {
    let at = (shape.at)();
    let pending = |step: Step| match aligns_by_update(shape) {
        true => step.may_leave_unresolved(&[Repo::Docs]),
        false => step,
    };
    let mut steps = vec![
        pending(Step::rwv(&at, &["abort"]).expect(Exit::refuses_kind(RefusalKind::NoOpRecorded))),
        pending(Step::rwv(&at, shape.resume).expect(Exit::refuses_kind(RefusalKind::NoOpRecorded))),
    ];
    let mut status = Step::rwv(&at, &["status"]).claim(Claim::OutputLacks("in progress"));
    if ALIGNS_A_MEMBER_IT_ADDS.contains(&shape.id) {
        status = status.claim(Claim::OutputContains("[missing]"));
    }
    steps.push(pending(status.expect(Exit::Succeeds)));
    if aligns_by_update(shape) {
        steps.push(settled(
            shape,
            Step::rwv(&at, &["update"]).expect(Exit::Succeeds),
        ));
    }
    if ALIGNS_A_MEMBER_IT_ADDS.contains(&shape.id) {
        steps.push(
            Step::rwv(&a(), &["doctor", "--all"])
                .claim(Claim::OutputContains(
                    "[error: workweave-drift/missing] core: workweave drift in proj--",
                ))
                .claim(Claim::OutputContains("missing worktree local/remotes/docs"))
                .claim(Claim::OutputContains("`rwv doctor --fix`")),
        );
        steps.push(Step::rwv(&at, &["doctor", "--fix"]).expect(Exit::Succeeds));
    }
    steps.extend(ends_clean(shape, false));
    steps
}

/// A reset keeps the commits it leaves in a backup, so a run that ends has
/// one holding them, killed or not.
fn kept_by_a_reset(shape: &Shape) -> Vec<Step> {
    let subject = match shape.id {
        "pull_reset_to_upstream" => "b-notes",
        "update_remote_reset_to_upstream" | "update_remote_merge_and_reset_to_upstream" => {
            "alpha-lib"
        }
        _ => return Vec::new(),
    };
    vec![Step::rwv(&(shape.at)(), &["backup", "list"])
        .expect(Exit::Succeeds)
        .claim(Claim::OutputContains(subject))]
}

fn rerun(shape: &Shape) -> Vec<Step> {
    let mut steps = Vec::new();
    match &shape.stop {
        None => steps.push(run_of(shape, Exit::Succeeds, shape.start)),
        Some(stop) => {
            steps.push(run_of(shape, (stop.exit)(), shape.start));
            steps.extend((stop.fix)());
            steps.push(finishing(shape));
        }
    }
    steps
}

fn continued(shape: &Shape, phase: Phase, point: &str) -> Vec<Step> {
    if AFTER_THE_RECORD.contains(&point) {
        return converged(shape);
    }
    let mut steps = Vec::new();
    if !recorded(point) {
        steps.extend(rerun(shape));
        steps.extend(kept_by_a_reset(shape));
        steps.extend(ends_clean(shape, retires_its_seat(shape)));
        return steps;
    }
    if shape.contender == Some(Contention::Live) {
        steps.extend(contender_ends(shape));
    }
    match (&shape.stop, phase) {
        (Some(stop), Phase::Start) => {
            steps.push(run_of(shape, (stop.exit)(), shape.resume));
            steps.extend((stop.fix)());
            steps.push(finishing(shape));
        }
        _ => steps.push(finishing(shape)),
    }
    steps.extend(kept_by_a_reset(shape));
    steps.extend(ends_clean(shape, retires_its_seat(shape)));
    steps
}

fn finishing(shape: &Shape) -> Step {
    let step = run_of(shape, Exit::Succeeds, shape.resume);
    if shape.stop.is_some() {
        step.resolved_by_hand(strands_on_resolving(shape))
    } else {
        step
    }
}

/// How `rwv abort` typed in the target of a landing meets a landing killed at
/// a point.
enum InTheTarget {
    Ends,
    /// The target holds no part of the record yet: abort there refuses, and
    /// the seat's abort ends it.
    NotYet,
    /// The landing released the target: abort there refuses and prints the
    /// abort to run from the seat.
    Released,
}

fn in_the_target(shape: &Shape, point: &str) -> InTheTarget {
    if shape.id == "land_linear_as_refs" || point.starts_with("drift-") {
        return InTheTarget::NotYet;
    }
    match point {
        "recorded" | "claimed" | "landing-claim-held" => InTheTarget::NotYet,
        "ending" | "retire" => InTheTarget::Released,
        _ => InTheTarget::Ends,
    }
}

fn aborted(shape: &Shape, typed_at: Place, point: &str, in_target: bool) -> Vec<Step> {
    aborted_keeping(shape, typed_at, point, in_target, abort_keeps(shape))
}

/// [`aborted`], its abort keeping as backups the slots `kept` names.
fn aborted_keeping(
    shape: &Shape,
    typed_at: Place,
    point: &str,
    in_target: bool,
    kept: &'static [&'static str],
) -> Vec<Step> {
    let ends = if recorded(point) {
        Exit::Succeeds
    } else {
        Exit::refuses_kind(RefusalKind::NoOpRecorded)
    };
    let mut steps = Vec::new();
    let mut ender = Step::rwv(&typed_at, &["abort"]);
    if in_target && recorded(point) {
        match in_the_target(shape, point) {
            InTheTarget::Ends => {}
            InTheTarget::NotYet => {
                steps.push(
                    Step::rwv(&typed_at, &["abort"])
                        .expect(Exit::refuses_kind(RefusalKind::NoOpRecorded)),
                );
                ender = Step::rwv(&(shape.at)(), &["abort"]);
            }
            InTheTarget::Released => {
                steps.push(
                    Step::rwv(&typed_at, &["abort"])
                        .expect(Exit::refuses_kind(RefusalKind::AbortInItsWorkspace)),
                );
                ender = Step::rwv_printed(&typed_at, "rwv -C ");
            }
        }
    }
    let ender = settled(shape, ender.expect(ends));
    let ender = ender.claim(Claim::BackupsAsCaptured {
        name: "before",
        kept,
    });
    steps.push(if shape.refreshes_drift {
        ender.claim(Claim::BranchesAsCaptured("before"))
    } else {
        ender.claim(Claim::TipsAsCaptured("before"))
    });
    steps.extend(contender_ends(shape));
    if shape.refreshes_drift {
        steps.push(
            Step::rwv(&a(), &["status"])
                .expect(Exit::Succeeds)
                .claim(Claim::OutputLacks("in progress")),
        );
        return steps;
    }
    steps.extend(ends_clean(shape, false));
    if shape.contender.is_none() {
        steps.push(Step::rwv(&a(), &["status"]).claim(Claim::TipsAsCaptured("before")));
    }
    if shape.stop.is_none() {
        steps.push(run_of(shape, Exit::Succeeds, shape.start));
    }
    steps
}

/// An unkilled run of the shape, with the bystander created where its cells
/// create it when `with_bystander`.
fn unkilled_with(shape: &Shape, with_bystander: bool) -> Vec<Step> {
    let mut steps = (shape.setup)();
    if with_bystander {
        steps.push(bystander(shape));
    }
    if shape.contender == Some(Contention::Live) {
        steps.push(Step::rwv(&ww("w1"), HOLDER).expect(Exit::Succeeds));
    }
    steps.extend(rerun(shape));
    steps.extend(kept_by_a_reset(shape));
    steps
}

/// A landing that retires its seat has, at these points, removed the seat and
/// its record: there is nothing to continue or abort. Its savepoints outlive
/// it, which `rwv doctor` reports and `rwv doctor --fix` clears, keeping the
/// seat's pre-rebase tips as backups and leaving the unkilled run's state
/// otherwise: by decision, the single-repo parity of a kill in a verb's last
/// instant.
fn already_ended(shape: &Shape, point: &str) -> bool {
    retires_its_seat(shape) && matches!(point, "ending" | "records-write")
}

/// How the end state of a cell killed at `point` differs from an unkilled
/// run's, as [`line_diff`] prints it. A retiring landing killed as it writes
/// the records has deleted the seat's branch and not yet dropped its record;
/// the next run of the deletion rule anywhere drops it
/// (`a_stale_record_is_dropped_by_the_next_deletion_rule`). Killed there or
/// at its end, the next `rwv doctor --fix` keeps the seat's two pre-rebase
/// tips as backups.
fn left_by_the_kill(shape: &Shape, point: &str) -> &'static str {
    if retires_its_seat(shape) && point == "records-write" {
        "  only unkilled: refs/heads/rwv-records [records: drop proj--w1 #3]\n  \
         only continued: refs/heads/rwv-records [records: set proj--w1 feature #2]\n  \
         only continued: refs/rwv/backup/HEAD/<tip> [w1-proj #3]\n  \
         only continued: refs/rwv/backup/HEAD/<tip> [w1-app #2]\n"
    } else if retires_its_seat(shape) && point == "ending" {
        "  only continued: refs/rwv/backup/HEAD/<tip> [w1-proj #3]\n  \
         only continued: refs/rwv/backup/HEAD/<tip> [w1-app #2]\n"
    } else {
        ""
    }
}

/// `rwv doctor --fix` in the primary of the shape's weave and in the
/// bystander, twice over, each leaving every record, lease and landing claim
/// as the held operation left them.
fn doctors_elsewhere(shape: &Shape) -> Vec<Step> {
    let machine = weave_of(shape)
        .machine()
        .expect("a shape runs on a machine");
    let mut steps = Vec::new();
    for _ in 0..2 {
        for at in [weave_of(shape), Place::ww(machine, BYSTANDER)] {
            steps.push(settled(
                shape,
                Step::rwv(&at, &["doctor", "--fix"])
                    .expect(Exit::Succeeds)
                    .claim(Claim::HoldsAsCaptured("held")),
            ));
        }
    }
    steps
}

/// The bystander's creation, in the shape's weave.
fn bystander(shape: &Shape) -> Step {
    Step::rwv(
        &weave_of(shape),
        &[
            "workweave",
            "create",
            "--project",
            "proj",
            BYSTANDER,
            "--detach",
            "main",
        ],
    )
    .expect(Exit::Succeeds)
}

/// The primary of the weave the shape's operation runs in.
fn weave_of(shape: &Shape) -> Place {
    Place::Primary((shape.at)().machine().expect("a shape runs on a machine"))
}

/// The seat [`Ending::AbortAfterDoctorElsewhere`] runs `rwv doctor --fix` in,
/// which no shape's operation touches: detached, so it lands into nothing.
const BYSTANDER: &str = "bystander";

fn steps_of(shape: &Shape, phase: Phase, point: &'static str, ending: Ending) -> Vec<Step> {
    let mut steps = Vec::new();
    match ending {
        Ending::AbortAfterDoctorElsewhere | Ending::ContinueAfterDoctorElsewhere => {
            steps.extend((shape.setup)());
            steps.extend(holding(shape));
            steps.push(bystander(shape));
            steps.push(Step::capture("before"));
            match &shape.stop {
                Some(stop) => steps.push(run_of(shape, (stop.exit)(), shape.start)),
                None => {
                    let at = (shape.at)();
                    steps.push(settled(
                        shape,
                        Step::rwv_paused(&at, shape.start, point, "held"),
                    ));
                    steps.push(Step::kill("held"));
                }
            }
            steps.push(Step::capture("held"));
            steps.extend(doctors_elsewhere(shape));
        }
        _ => steps.extend(until_stopped(shape, phase, point)),
    }
    if already_ended(shape, point) {
        steps.extend(savepoints_outlive(shape));
        return steps;
    }
    match ending {
        Ending::Continue => steps.extend(continued(shape, phase, point)),
        Ending::Abort => steps.extend(aborted(shape, (shape.at)(), point, false)),
        Ending::AbortInTarget => steps.extend(aborted(
            shape,
            shape.target.expect("only a landing aborts in its target")(),
            point,
            true,
        )),
        Ending::ContinueAfterDoctorElsewhere => match &shape.stop {
            Some(stop) => {
                steps.push(run_of(shape, (stop.exit)(), shape.resume));
                steps.extend((stop.fix)());
                steps.push(finishing(shape));
                steps.extend(kept_by_a_reset(shape));
                steps.extend(ends_clean(shape, retires_its_seat(shape)));
            }
            None => steps.extend(continued(shape, phase, point)),
        },
        Ending::AbortAfterDoctorElsewhere => {
            let kept = match shape.stop {
                Some(_) => stopped_abort_keeps(shape),
                None => abort_keeps(shape),
            };
            steps.extend(aborted_keeping(shape, (shape.at)(), point, false, kept));
        }
    }
    steps
}

fn named(shape: &Shape, phase: Phase, point: &str, ending: Ending) -> String {
    let phase = match phase {
        Phase::Start => "",
        Phase::Resumed => " resumed",
        Phase::DuringAbort => " during abort",
    };
    if point == STOPPED {
        return format!("{} stopped, {}", shape.id, ending.word());
    }
    format!("{} killed at {point}{phase}, {}", shape.id, ending.word())
}

fn scenario_of(shape: &Shape, name: &'static str, steps: Vec<Step>) -> Scenario {
    Scenario {
        name,
        seed: (shape.seed)(),
        steps,
    }
}

fn leaked(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn failures(report: &scenario_runner::Report) -> String {
    let mut out = String::new();
    for (label, why) in &report.unmet {
        out.push_str(&format!("  unmet at `{label}`: {why}\n"));
    }
    for (label, violation) in &report.violations {
        out.push_str(&format!("  invariant at `{label}`: {violation:?}\n"));
    }
    out
}

/// Runs one cell and returns what went wrong, if anything.
fn line_diff(unkilled: &str, continued: &str) -> String {
    let mut out = String::new();
    let mut remaining: Vec<&str> = continued.lines().collect();
    for line in unkilled.lines() {
        match remaining.iter().position(|l| *l == line) {
            Some(at) => {
                remaining.remove(at);
            }
            None => out.push_str(&format!("  only unkilled: {line}\n")),
        }
    }
    for line in remaining {
        out.push_str(&format!("  only continued: {line}\n"));
    }
    out
}

/// What an unkilled run of the shape ends in.
fn reference_end(shape: &Shape) -> String {
    reference_end_with(shape, false)
}

/// [`reference_end`] of [`unkilled_with`].
fn reference_end_with(shape: &Shape, with_bystander: bool) -> String {
    let clean = Sandbox::new((shape.seed)());
    let report = scenario_runner::run_in(
        &clean,
        scenario_of(shape, "unkilled", unkilled_with(shape, with_bystander)),
    );
    let failed = failures(&report);
    assert!(
        failed.is_empty(),
        "the unkilled run of {} failed:\n{failed}",
        shape.id
    );
    scenario_runner::end_state(&clean)
}

fn run_cell(
    shape: &Shape,
    phase: Phase,
    point: &'static str,
    ending: Ending,
    reference: &str,
) -> Option<String> {
    let name = named(shape, phase, point, ending);
    let steps = steps_of(shape, phase, point, ending);
    let sandbox = Sandbox::new((shape.seed)());
    let report = scenario_runner::run_in(&sandbox, scenario_of(shape, leaked(name.clone()), steps));
    let mut why = failures(&report);
    if matches!(
        ending,
        Ending::Continue | Ending::ContinueAfterDoctorElsewhere
    ) && why.is_empty()
    {
        let continued = scenario_runner::end_state(&sandbox);
        let with_bystander;
        let reference = if ending == Ending::ContinueAfterDoctorElsewhere {
            with_bystander = reference_end_with(shape, true);
            with_bystander.as_str()
        } else {
            reference
        };
        let differs = line_diff(reference, &continued);
        let left = left_by_the_kill(shape, point);
        if differs != left {
            why = format!(
                "the end state differs from an unkilled run's:\n{differs}expected only:\n{left}"
            );
        }
    }
    (!why.is_empty()).then(|| format!("{name}:\n{why}"))
}

/// The declared points the shape's verb passes in `phase`, read from the
/// trace of one run of it.
fn traced(shape: &Shape, phase: Phase) -> std::collections::BTreeSet<String> {
    let at = (shape.at)();
    let mut steps = (shape.setup)();
    steps.extend(holding(shape));
    let run = |args: &'static [&'static str]| Step::rwv(&at, args).expect(Exit::Undecided);
    match phase {
        Phase::Start => {
            steps.push(Step::trace_points(true));
            steps.push(run(shape.start));
        }
        Phase::DuringAbort => {
            steps.push(Step::rwv_paused(
                &at,
                shape.start,
                shape.abort_after,
                "held",
            ));
            steps.push(Step::kill("held"));
            steps.push(Step::trace_points(true));
            steps.push(run(&["abort"]));
        }
        Phase::Resumed => {
            let Some(stop) = &shape.stop else {
                return Default::default();
            };
            steps.push(run(shape.start));
            steps.extend((stop.fix)());
            steps.push(Step::trace_points(true));
            steps.push(run(shape.resume));
        }
    }
    steps.push(Step::trace_points(false));
    steps.extend(contender_ends(shape));
    let sandbox = Sandbox::new((shape.seed)());
    scenario_runner::run_in(&sandbox, scenario_of(shape, "trace", steps));
    let passed = std::fs::read_to_string(sandbox.point_trace()).unwrap_or_default();
    let declared = declared_points();
    passed
        .lines()
        .filter(|point| declared.contains(*point))
        .filter(|point| point.starts_with("abort-") == (phase == DuringAbort))
        .map(str::to_owned)
        .collect()
}

fn in_parallel<T: Send>(jobs: Vec<Box<dyn FnOnce() -> T + Send + '_>>) -> Vec<T> {
    let width = std::thread::available_parallelism()
        .map_or(4, usize::from)
        .min(8);
    let queue = std::sync::Mutex::new(jobs.into_iter().enumerate().collect::<Vec<_>>());
    let done = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..width {
            scope.spawn(|| loop {
                let next = queue.lock().unwrap().pop();
                let Some((at, job)) = next else { break };
                let out = job();
                done.lock().unwrap().push((at, out));
            });
        }
    });
    let mut done = done.into_inner().unwrap();
    done.sort_by_key(|(at, _)| *at);
    done.into_iter().map(|(_, out)| out).collect()
}

/// A point and the cells that reach it.
pub(super) struct Row {
    pub point: &'static str,
    pub cells: &'static [(&'static str, Phase)],
    /// Why every shape not in `cells` does not pass this point.
    pub elsewhere: &'static str,
    /// Shapes outside `cells` that also pass the point, where what sets the
    /// shape apart is not yet live: the row's cells cover them. A point passed
    /// before the shape's own state exists (a record being written, a scratch
    /// taken, a claim not yet waited on) is the case; a point after the
    /// shape's drift, stop or abort plan has taken effect gets a cell.
    pub same_as: &'static [&'static str],
    /// The point is passed only when two processes, or two threads of one,
    /// meet at the same instant, or a sweep lands in the middle of a write:
    /// a run may or may not pass it, so passing it is no failure.
    pub timing_dependent: bool,
}

use Phase::{DuringAbort, Resumed, Start};

pub(super) const POINTS: &[Row] = &[
    Row {
        point: "abort-begun",
        cells: &[("cherry_pick", DuringAbort), ("land_linear", DuringAbort), ("land_linear_as_refs", DuringAbort), ("land_linear_carrying", DuringAbort), ("land_linear_in_place", DuringAbort), ("land_linear_in_place_drifted", DuringAbort), ("land_linear_manifest_stop", DuringAbort), ("land_linear_own_stop", DuringAbort), ("land_linear_retiring", DuringAbort), ("land_merge", DuringAbort), ("land_merge_carrying", DuringAbort), ("land_merge_own_stop", DuringAbort), ("pull_merge_upstream", DuringAbort), ("pull_merge_upstream_adding", DuringAbort), ("pull_reset_to_upstream", DuringAbort), ("sync_linear", DuringAbort), ("sync_linear_carrying", DuringAbort), ("sync_linear_following", DuringAbort), ("sync_linear_manifest_stop", DuringAbort), ("sync_linear_own_stop", DuringAbort), ("sync_merge", DuringAbort), ("sync_merge_adding", DuringAbort), ("sync_merge_carrying", DuringAbort), ("sync_merge_own_stop", DuringAbort), ("update_remote_merge_and_reset_to_upstream", DuringAbort), ("update_remote_merge_upstream", DuringAbort), ("update_remote_reset_to_upstream", DuringAbort)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "every verb that records an operation reaches it through `rwv abort`, and every shape that records one has a cell.",
    },
    Row {
        point: "abort-carried",
        cells: &[("cherry_pick", DuringAbort), ("land_linear", DuringAbort), ("land_linear_as_refs", DuringAbort), ("land_linear_carrying", DuringAbort), ("land_linear_in_place", DuringAbort), ("land_linear_in_place_drifted", DuringAbort), ("land_linear_manifest_stop", DuringAbort), ("land_linear_own_stop", DuringAbort), ("land_linear_retiring", DuringAbort), ("land_merge", DuringAbort), ("land_merge_carrying", DuringAbort), ("land_merge_own_stop", DuringAbort), ("pull_merge_upstream", DuringAbort), ("pull_merge_upstream_adding", DuringAbort), ("pull_reset_to_upstream", DuringAbort), ("sync_linear", DuringAbort), ("sync_linear_carrying", DuringAbort), ("sync_linear_following", DuringAbort), ("sync_linear_manifest_stop", DuringAbort), ("sync_linear_own_stop", DuringAbort), ("sync_merge", DuringAbort), ("sync_merge_adding", DuringAbort), ("sync_merge_carrying", DuringAbort), ("sync_merge_own_stop", DuringAbort), ("update_remote_merge_and_reset_to_upstream", DuringAbort), ("update_remote_merge_upstream", DuringAbort), ("update_remote_reset_to_upstream", DuringAbort)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "same as abort-begun; the carried step runs in every abort plan.",
    },
    Row {
        point: "abort-executed",
        cells: &[("cherry_pick", DuringAbort), ("land_linear", DuringAbort), ("land_linear_as_refs", DuringAbort), ("land_linear_carrying", DuringAbort), ("land_linear_in_place", DuringAbort), ("land_linear_in_place_drifted", DuringAbort), ("land_linear_manifest_stop", DuringAbort), ("land_linear_own_stop", DuringAbort), ("land_linear_retiring", DuringAbort), ("land_merge", DuringAbort), ("land_merge_carrying", DuringAbort), ("land_merge_own_stop", DuringAbort), ("pull_merge_upstream", DuringAbort), ("pull_merge_upstream_adding", DuringAbort), ("pull_reset_to_upstream", DuringAbort), ("sync_linear", DuringAbort), ("sync_linear_carrying", DuringAbort), ("sync_linear_following", DuringAbort), ("sync_linear_manifest_stop", DuringAbort), ("sync_linear_own_stop", DuringAbort), ("sync_merge", DuringAbort), ("sync_merge_adding", DuringAbort), ("sync_merge_carrying", DuringAbort), ("sync_merge_own_stop", DuringAbort), ("update_remote_merge_and_reset_to_upstream", DuringAbort), ("update_remote_merge_upstream", DuringAbort), ("update_remote_reset_to_upstream", DuringAbort)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "same as abort-begun.",
    },
    Row {
        point: "abort-retaken",
        cells: &[("cherry_pick", DuringAbort), ("land_linear", DuringAbort), ("land_linear_as_refs", DuringAbort), ("land_linear_carrying", DuringAbort), ("land_linear_in_place", DuringAbort), ("land_linear_in_place_drifted", DuringAbort), ("land_linear_manifest_stop", DuringAbort), ("land_linear_own_stop", DuringAbort), ("land_linear_retiring", DuringAbort), ("land_merge", DuringAbort), ("land_merge_carrying", DuringAbort), ("land_merge_own_stop", DuringAbort), ("pull_merge_upstream", DuringAbort), ("pull_merge_upstream_adding", DuringAbort), ("pull_reset_to_upstream", DuringAbort), ("sync_linear", DuringAbort), ("sync_linear_carrying", DuringAbort), ("sync_linear_following", DuringAbort), ("sync_linear_manifest_stop", DuringAbort), ("sync_linear_own_stop", DuringAbort), ("sync_merge", DuringAbort), ("sync_merge_adding", DuringAbort), ("sync_merge_carrying", DuringAbort), ("sync_merge_own_stop", DuringAbort), ("update_remote_merge_and_reset_to_upstream", DuringAbort), ("update_remote_merge_upstream", DuringAbort), ("update_remote_reset_to_upstream", DuringAbort)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "same as abort-begun; a landing's target is retaken there and other verbs pass it as a no-op.",
    },
    Row {
        point: "advance-target",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing advances a target; sync, pull, update --remote and cherry-pick have none.",
    },
    Row {
        point: "advance-target-move",
        cells: &[("land_linear", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing moves its target's checkout; a landing as refs moves refs instead and does not pass it.",
    },
    Row {
        point: "aligning-target",
        cells: &[("land_linear", Start), ("land_linear_carrying", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing into a checked-out target aligns it; a landing as refs leaves checkouts alone.",
    },
    Row {
        point: "carry-composed",
        cells: &[("land_linear_carrying", Start), ("sync_linear_carrying", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a verb carrying a dependent branch composes a carry, and only on the linear history setting; the merge-history carry composes elsewhere.",
    },
    Row {
        point: "carry-journaled",
        cells: &[("land_linear_carrying", Start), ("sync_linear_carrying", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "as carry-composed.",
    },
    Row {
        point: "aligning-source",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed), ("pull_merge_upstream", Start), ("pull_merge_upstream_adding", Start), ("pull_reset_to_upstream", Start), ("sync_linear", Start), ("sync_linear_carrying", Start), ("sync_linear_following", Start), ("sync_linear_manifest_stop", Resumed), ("sync_linear_own_stop", Resumed), ("sync_merge", Start), ("sync_merge_adding", Start), ("sync_merge_carrying", Start), ("sync_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "a landing that retires its seat does not align it, and a landing waiting on its claim is killed while the contender holds it; update and cherry-pick align nothing once their record is gone.",
    },
    Row {
        point: "carry-move",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed), ("sync_linear", Start), ("sync_linear_carrying", Start), ("sync_linear_following", Start), ("sync_linear_manifest_stop", Resumed), ("sync_linear_own_stop", Resumed), ("sync_merge", Start), ("sync_merge_adding", Start), ("sync_merge_carrying", Start), ("sync_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "every sync and landing moves a branch through a carry journal entry; pull, update --remote and cherry-pick journal their moves another way.",
    },
    Row {
        point: "carry-moved",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed), ("sync_linear", Start), ("sync_linear_carrying", Start), ("sync_linear_following", Start), ("sync_linear_manifest_stop", Resumed), ("sync_linear_own_stop", Resumed), ("sync_merge", Start), ("sync_merge_adding", Start), ("sync_merge_carrying", Start), ("sync_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "as carry-move.",
    },
    Row {
        point: "carry-recorded",
        cells: &[("land_linear_carrying", Start), ("sync_linear_carrying", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a verb carrying a dependent branch records a carry, and only on the linear history setting.",
    },
    Row {
        point: "cherry-pick-member-copied",
        cells: &[("cherry_pick", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only cherry-pick copies member ranges.",
    },
    Row {
        point: "cherry-pick-project-committed",
        cells: &[("cherry_pick", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only cherry-pick commits a ported project commit.",
    },
    Row {
        point: "cherry-pick-project-copy",
        cells: &[("cherry_pick", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only cherry-pick copies the project's files.",
    },
    Row {
        point: "claim-held",
        cells: &[],
        same_as: &[],
        timing_dependent: true,
        elsewhere: "reached only by a process waiting on a state-file, workweave-index, owned-digest or fetch claim another holds, before any operation is recorded; nothing is left to continue or abort. A pull's parallel fetches of members sharing one store meet at that store's fetch claim in one process, so a run may pass it.",
    },
    Row {
        point: "claimed",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start)],
        same_as: &["land_linear_manifest_stop", "land_linear_own_stop", "land_merge_own_stop"],
        timing_dependent: false,
        elsewhere: "only a landing claims its target.",
    },
    Row {
        point: "composed",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing composes its source before advancing the target.",
    },
    Row {
        point: "doctor-savepoints-listed",
        cells: &[],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "reached only by `rwv doctor`, which records no operation: there is nothing to continue or abort. tests/doctor/stopped_op_savepoints_test.rs holds it there while an operation starts.",
    },
    Row {
        point: "drift-merge-carry",
        cells: &[],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "as drift-merge-read.",
    },
    Row {
        point: "drift-merge-read",
        cells: &[],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "reached only by `rwv doctor --fix --merge-edited-drift` and `--set-aside-edited-drift`, which record no operation: there is nothing to continue or abort.",
    },
    Row {
        point: "drift-merge-set-back",
        cells: &[],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "as drift-merge-read.",
    },
    Row {
        point: "drift-read",
        cells: &[("land_linear_in_place_drifted", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing in place that finds the primary's checkout drifted refreshes it there.",
    },
    Row {
        point: "drift-write",
        cells: &[("land_linear_in_place_drifted", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "as drift-read.",
    },
    Row {
        point: "drift-write-records",
        cells: &[("land_linear_in_place_drifted", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "as drift-read.",
    },
    Row {
        point: "drift-write-spawn",
        cells: &[("land_linear_in_place_drifted", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "as drift-read.",
    },
    Row {
        point: "durable-file-created",
        cells: &[("cherry_pick", Start), ("land_linear", Start), ("land_merge", Start), ("pull_merge_upstream", Start), ("pull_merge_upstream_adding", Start), ("pull_reset_to_upstream", Start), ("sync_linear", Start), ("sync_merge", Start), ("sync_merge_adding", Start), ("update_remote_merge_and_reset_to_upstream", Start), ("update_remote_merge_upstream", Start), ("update_remote_reset_to_upstream", Start)],
        same_as: &["land_linear_as_refs", "land_linear_carrying", "land_linear_in_place", "land_linear_in_place_drifted", "land_linear_manifest_stop", "land_linear_own_stop", "land_linear_retiring", "land_linear_waiting", "land_merge_carrying", "land_merge_own_stop", "sync_linear_carrying", "sync_linear_following", "sync_linear_manifest_stop", "sync_linear_own_stop", "sync_merge_carrying", "sync_merge_own_stop"],
        timing_dependent: false,
        elsewhere: "every verb that records an operation writes it through the durable file; one shape of each verb and history stands for the rest, which differ only in what they go on to do.",
    },
    Row {
        point: "durable-file-restage",
        cells: &[],
        same_as: &[],
        timing_dependent: true,
        elsewhere: "reached only when a sweep removes a durable file's staged temp while the publish is still writing it, which no verb run does, and no operation is involved.",
    },
    Row {
        point: "durable-file-staged",
        cells: &[("cherry_pick", Start), ("land_linear", Start), ("land_merge", Start), ("pull_merge_upstream", Start), ("pull_merge_upstream_adding", Start), ("pull_reset_to_upstream", Start), ("sync_linear", Start), ("sync_merge", Start), ("sync_merge_adding", Start), ("update_remote_merge_and_reset_to_upstream", Start), ("update_remote_merge_upstream", Start), ("update_remote_reset_to_upstream", Start)],
        same_as: &["land_linear_as_refs", "land_linear_carrying", "land_linear_in_place", "land_linear_in_place_drifted", "land_linear_manifest_stop", "land_linear_own_stop", "land_linear_retiring", "land_linear_waiting", "land_merge_carrying", "land_merge_own_stop", "sync_linear_carrying", "sync_linear_following", "sync_linear_manifest_stop", "sync_linear_own_stop", "sync_merge_carrying", "sync_merge_own_stop"],
        timing_dependent: false,
        elsewhere: "as durable-file-created.",
    },
    Row {
        point: "ending",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing has an ending that releases its target.",
    },
    Row {
        point: "fetch-under-claim",
        cells: &[("pull_merge_upstream", Start), ("pull_merge_upstream_adding", Start), ("pull_reset_to_upstream", Start), ("sync_linear", Start), ("sync_merge", Start), ("sync_merge_adding", Start), ("update_remote_merge_and_reset_to_upstream", Start), ("update_remote_merge_upstream", Start), ("update_remote_reset_to_upstream", Start)],
        same_as: &["sync_linear_carrying", "sync_linear_following", "sync_linear_manifest_stop", "sync_linear_own_stop", "sync_merge_carrying", "sync_merge_own_stop"],
        timing_dependent: false,
        elsewhere: "every verb that fetches passes it; one shape of each verb and history stands for the rest.",
    },
    Row {
        point: "landed",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing records that its target moved.",
    },
    Row {
        point: "landing-claim-dead-seen",
        cells: &[],
        same_as: &[],
        timing_dependent: true,
        elsewhere: "reached only by a landing that finds the claim's holder dead and removes it, which needs a claim whose recorded operation is gone; tests/process/landing_claim_contention_test.rs kills there.",
    },
    Row {
        point: "landing-claim-held",
        cells: &[("land_linear_waiting", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing waiting on another landing's claim passes it.",
    },
    Row {
        point: "landing-claim-takeover",
        cells: &[],
        same_as: &[],
        timing_dependent: true,
        elsewhere: "as landing-claim-dead-seen.",
    },
    Row {
        point: "leased-in-place",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start)],
        same_as: &["land_linear_manifest_stop", "land_linear_own_stop", "land_merge_own_stop"],
        timing_dependent: false,
        elsewhere: "only a landing takes the lease on a primary whose checkouts it fast-forwards in place.",
    },
    Row {
        point: "op-savepoint-taken",
        cells: &[("cherry_pick", Start), ("pull_merge_upstream", Start), ("pull_merge_upstream_adding", Start), ("pull_reset_to_upstream", Start), ("sync_linear", Start), ("sync_linear_carrying", Start), ("sync_linear_following", Start), ("sync_linear_manifest_stop", Start), ("sync_linear_own_stop", Start), ("sync_merge", Start), ("sync_merge_adding", Start), ("sync_merge_carrying", Start), ("sync_merge_own_stop", Start), ("update_remote_merge_and_reset_to_upstream", Start), ("update_remote_merge_upstream", Start), ("update_remote_reset_to_upstream", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "every verb that records an operation passes it once; a landing records its plan at recorded instead.",
    },
    Row {
        point: "push-members",
        cells: &[],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "reached only by `rwv push`, which records no operation: there is nothing to continue or abort.",
    },
    Row {
        point: "recorded",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Start), ("land_linear_own_stop", Start), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Start)],
        same_as: &["land_linear_waiting"],
        timing_dependent: false,
        elsewhere: "only a landing records its plan before claiming.",
    },
    Row {
        point: "records-publish",
        cells: &[],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "reached only by `rwv push` publishing the records branch, which records no operation: there is nothing to continue or abort.",
    },
    Row {
        point: "records-publish-settled",
        cells: &[],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "as records-publish.",
    },
    Row {
        point: "records-write",
        cells: &[("land_linear_retiring", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing that retires its workweave deletes the workweave's record.",
    },
    Row {
        point: "relock-committed",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_retiring", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing, on the linear history setting, commits its relock under its own name; a sync's is sync-relock-committed and a merge landing's is relock-merge-committed.",
    },
    Row {
        point: "relock-merge-committed",
        cells: &[("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing on the merge history setting commits a merge relock; pull and update --remote do not pass it in any shape tried.",
    },
    Row {
        point: "relocking",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing relocks at that step; sync's relock is sync-relock-committed.",
    },
    Row {
        point: "remap-moved",
        cells: &[("land_linear_carrying", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("sync_linear_carrying", Start), ("sync_linear_manifest_stop", Resumed), ("sync_linear_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a replay that stops and is continued remaps the branches it moved; a replay that finishes in one run remaps nothing.",
    },
    Row {
        point: "remap-recorded",
        cells: &[("land_linear_carrying", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("sync_linear_carrying", Start), ("sync_linear_manifest_stop", Resumed), ("sync_linear_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "as remap-moved.",
    },
    Row {
        point: "reparent",
        cells: &[],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "reached only by `rwv workweave remove` re-parenting its children and by doctor's repair of a workweave marker, neither of which records an operation.",
    },
    Row {
        point: "reset-to-upstream",
        cells: &[("pull_reset_to_upstream", Start), ("update_remote_merge_and_reset_to_upstream", Start), ("update_remote_reset_to_upstream", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only the --reset-to-upstream forms of pull and update --remote move a branch to its upstream.",
    },
    Row {
        point: "retire",
        cells: &[("land_linear_retiring", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing run with --retire retires its workweave.",
    },
    Row {
        point: "retiring",
        cells: &[("land_linear_retiring", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "as retire.",
    },
    Row {
        point: "scratch-taken",
        cells: &[("cherry_pick", Start), ("land_linear", Start), ("sync_linear", Start)],
        same_as: &["land_linear_as_refs", "land_linear_carrying", "land_linear_in_place", "land_linear_in_place_drifted", "land_linear_manifest_stop", "land_linear_own_stop", "land_linear_retiring", "sync_linear_carrying", "sync_linear_following", "sync_linear_manifest_stop", "sync_linear_own_stop"],
        timing_dependent: false,
        elsewhere: "every verb that rebases or merges takes a scratch checkout; one shape of each verb and history stands for the rest.",
    },
    Row {
        point: "sync-member-followed",
        cells: &[("sync_linear_following", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "a sync passes it only for a member whose commits all sit under pins of the target's lock history while the target's branch no longer contains them, which only a rewrite of the target outside rwv builds: rwv refuses one under a live feature.",
    },
    Row {
        point: "sync-relock-committed",
        cells: &[("sync_linear", Start)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a sync that relocks over a commit the lock did not pin commits it; a landing's is relock-committed.",
    },
    Row {
        point: "target-fast-forwarded",
        cells: &[("land_linear", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_in_place_drifted", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        same_as: &[],
        timing_dependent: false,
        elsewhere: "only a landing that fast-forwards a checked-out target; a landing as refs moves refs and does not pass it.",
    },
];

fn cells_of(shape: &Shape) -> Vec<(&'static str, Phase)> {
    POINTS
        .iter()
        .flat_map(|row| {
            row.cells
                .iter()
                .filter(|(id, _)| *id == shape.id)
                .map(|(_, phase)| (row.point, *phase))
        })
        .collect()
}

fn endings(shape: &Shape, phase: Phase, point: &str) -> Vec<Ending> {
    if already_ended(shape, point) || AFTER_THE_RECORD.contains(&point) {
        return vec![Ending::Continue];
    }
    let mut endings = if phase == DuringAbort {
        vec![Ending::Abort]
    } else {
        vec![Ending::Continue, Ending::Abort]
    };
    if shape.target.is_some() && phase != DuringAbort {
        endings.push(Ending::AbortInTarget);
    }
    endings
}

/// Every cell of the shape: each point it reaches with each ending there,
/// and once [`Ending::AbortAfterDoctorElsewhere`], holding its operation:
/// stopped where the shape stops, else killed at `abort_after`, or, for a
/// landing that waits on another's claim, where it waits.
fn cell_runs(shape: &Shape) -> Vec<(&'static str, Phase, Ending)> {
    let mut runs: Vec<_> = cells_of(shape)
        .into_iter()
        .flat_map(|(point, phase)| {
            endings(shape, phase, point)
                .into_iter()
                .map(move |ending| (point, phase, ending))
        })
        .collect();
    for ending in [
        Ending::AbortAfterDoctorElsewhere,
        Ending::ContinueAfterDoctorElsewhere,
    ] {
        runs.push((doctor_elsewhere_point(shape), Start, ending));
    }
    runs
}

/// Shapes whose [`Ending::AbortAfterDoctorElsewhere`] cell cannot tell a
/// doctor that drops the operation's savepoints from one that keeps them, and
/// why: measured by dropping the live set and listing `refs/rwv/` before the
/// doctor runs and after.
const DOCTOR_ELSEWHERE_CANNOT_FAIL: &[(&str, &str)] = &[
    (
        "land_linear_waiting",
        "waiting on the other landing's claim it has filed no savepoint: refs/rwv/ is empty \
         in every repo before the doctor runs",
    ),
    (
        "pull_merge_upstream",
        "its one recorded point is op-savepoint-taken, where only the project's \
         refs/rwv/pre-op/<id>/checkout/main stands and nothing has moved: the doctor drops it \
         and the abort, which has nothing to put back, ends the same",
    ),
    ("pull_merge_upstream_adding", "as pull_merge_upstream"),
    ("update_remote_merge_upstream", "as pull_merge_upstream"),
];

/// What [`Ending::AbortAfterDoctorElsewhere`] names the place the shape holds
/// its operation at: [`STOPPED`] for a shape that stops, else the point it is
/// killed at.
fn doctor_elsewhere_point(shape: &Shape) -> &'static str {
    match (&shape.stop, shape.contender) {
        (Some(_), _) => STOPPED,
        (None, Some(Contention::Live)) => "landing-claim-held",
        (None, None) => shape.abort_after,
    }
}

/// Where a shape that stops holds its operation, named as a point no source
/// declares.
const STOPPED: &str = "its stop";

const STAGE: &str = "scripts/ci-local.sh --stages=kill-matrix";

fn run_shape(id: &str) {
    if std::env::var("RWV_KILL_MATRIX").as_deref() != Ok("run") {
        println!("kill matrix cells skipped: they run in `{STAGE}`");
        return;
    }
    let shape = shape(id);
    let reference = reference_end(shape);
    let reference = reference.as_str();
    let jobs: Vec<Box<dyn FnOnce() -> Option<String> + Send>> = cell_runs(shape)
        .into_iter()
        .filter(|(point, phase, ending)| {
            std::env::var("KILL_MATRIX_ONLY").map_or(true, |only| {
                named(shape, *phase, point, *ending).contains(&only)
            })
        })
        .map(|(point, phase, ending)| {
            Box::new(move || {
                let failed =
                    std::panic::catch_unwind(|| run_cell(shape, phase, point, ending, reference))
                        .unwrap_or_else(|_| {
                            Some(format!(
                                "{}: a step could not run (see the panic above)",
                                named(shape, phase, point, ending)
                            ))
                        });
                failed
            }) as Box<dyn FnOnce() -> Option<String> + Send>
        })
        .collect();
    let failed: Vec<String> = in_parallel(jobs).into_iter().flatten().collect();
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Every pause point the source declares, in the order its files hold them.
fn declared_points() -> std::collections::BTreeSet<String> {
    fn walk(dir: &std::path::Path, found: &mut std::collections::BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap().map(Result::unwrap) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                for piece in text.split("test_pause::point(\"").skip(1) {
                    found.insert(piece.split('"').next().unwrap().to_owned());
                }
            }
        }
    }
    let mut found = std::collections::BTreeSet::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    found
}

#[test]
fn the_enumeration_holds_every_pause_point_the_source_declares_and_no_other() {
    let declared = declared_points();
    assert!(declared.len() > 20, "{declared:?}");
    let listed: std::collections::BTreeSet<String> =
        POINTS.iter().map(|row| row.point.to_owned()).collect();
    let missing: Vec<_> = declared.difference(&listed).collect();
    let gone: Vec<_> = listed.difference(&declared).collect();
    assert!(
        missing.is_empty() && gone.is_empty(),
        "declared in src and not enumerated: {missing:?}; enumerated and not in src: {gone:?}"
    );
}

#[test]
fn every_row_names_real_shapes_and_says_why_the_others_do_not_reach_it() {
    for row in POINTS {
        assert!(
            !row.elsewhere.trim().is_empty(),
            "{} gives no reason for the shapes it leaves out",
            row.point
        );
        assert!(
            !row.timing_dependent || row.cells.is_empty(),
            "{} is timing-dependent and has cells, which a run may not reach",
            row.point
        );
        for (id, phase) in row.cells {
            let shape = shape(id);
            assert!(
                *phase != Resumed || shape.stop.is_some(),
                "{} is resumed in {id}, which never stops",
                row.point
            );
        }
    }
}

#[test]
fn every_shape_has_a_test_and_a_cell() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/kill_matrix/kill_matrix_test.rs"),
    )
    .unwrap();
    for shape in SHAPES {
        assert!(
            source.contains(&format!("    {},\n", shape.id))
                || source.contains(&format!("{}(", shape.id)),
            "{} has no test",
            shape.id
        );
        assert!(!cells_of(shape).is_empty(), "{} has no cell", shape.id);
    }
}

/// Every shape holds its operation once, and is aborted, and continued,
/// after `rwv doctor --fix` has run twice elsewhere in its weave; the shapes
/// whose abort cell cannot fail are named, each one real.
#[test]
fn every_shape_aborts_and_continues_once_after_a_doctor_fix_elsewhere() {
    for shape in SHAPES {
        let point = doctor_elsewhere_point(shape);
        assert!(recorded(point), "{}", shape.id);
        let after_doctor: Vec<_> = cell_runs(shape)
            .into_iter()
            .filter(|(_, _, ending)| {
                matches!(
                    ending,
                    Ending::AbortAfterDoctorElsewhere | Ending::ContinueAfterDoctorElsewhere
                )
            })
            .collect();
        assert_eq!(
            after_doctor,
            [
                (point, Start, Ending::AbortAfterDoctorElsewhere),
                (point, Start, Ending::ContinueAfterDoctorElsewhere)
            ],
            "{}",
            shape.id
        );
    }
    for (id, why) in DOCTOR_ELSEWHERE_CANNOT_FAIL {
        assert!(!why.is_empty());
        shape(id);
    }
}

#[test]
fn a_stale_record_is_dropped_by_the_next_deletion_rule() {
    let mut steps = linear_seat();
    steps.push(create("w2"));
    steps.push(Step::rwv_paused(
        &ww("w1"),
        LAND_RETIRE,
        "records-write",
        "held",
    ));
    steps.push(Step::kill("held"));
    steps.extend([
        Step::git(&a(), Repo::Proj, &["log", "--format=%s", "rwv-records"])
            .claim(Claim::OutputLine("records: set proj--w1 feature"))
            .claim(Claim::OutputLacks("drop proj--w1")),
        Step::rwv(&a(), &["status"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputContains("landed into by proj--w2"))
            .claim(Claim::OutputLacks("proj--w1")),
        Step::rwv(
            &a(),
            &["update", "--remote", "--reset-to-upstream", APP, "--commit"],
        )
        .expect(Exit::refuses(Repo::Proj, State::TargetDependents))
        .claim(Claim::OutputContains("proj--w2 lands into this branch"))
        .claim(Claim::OutputLacks("proj--w1")),
        Step::rwv(&a(), &["doctor", "--fix"]).expect(Exit::Succeeds),
        Step::rwv(&a(), &["workweave", "delete", "w2"])
            .expect(Exit::Succeeds)
            .claim(Claim::RecordIs {
                machine: Machine::A,
                branch: "proj--w1",
                record: None,
            }),
    ]);
    let report = scenario_runner::run(Scenario {
        name: "a stale record is dropped by the next deletion rule",
        seed: default_seed(),
        steps,
    });
    let failed = failures(&report);
    assert!(failed.is_empty(), "{failed}");
}

macro_rules! shape_tests {
    ($($id:ident),* $(,)?) => {
        $(
            #[test]
            fn $id() {
                run_shape(stringify!($id));
            }
        )*
    };
}

shape_tests!(
    sync_linear,
    sync_merge,
    sync_linear_following,
    sync_linear_carrying,
    land_linear_waiting,
    land_linear_in_place_drifted,
    sync_merge_carrying,
    sync_merge_own_stop,
    sync_linear_own_stop,
    sync_linear_manifest_stop,
    land_linear,
    land_merge,
    land_linear_retiring,
    land_linear_in_place,
    land_linear_as_refs,
    land_linear_carrying,
    land_merge_carrying,
    land_merge_own_stop,
    land_linear_own_stop,
    land_linear_manifest_stop,
    sync_merge_adding,
    pull_merge_upstream,
    pull_merge_upstream_adding,
    pull_reset_to_upstream,
    update_remote_merge_upstream,
    update_remote_reset_to_upstream,
    update_remote_merge_and_reset_to_upstream,
    cherry_pick,
);

/// Every shape, phase and point the shape's verb passes.
type Reached = (String, String, String);

fn discovered() -> std::collections::BTreeSet<Reached> {
    let jobs: Vec<Box<dyn FnOnce() -> Vec<Reached> + Send>> = SHAPES
        .iter()
        .filter(|shape| {
            std::env::var("DISCOVER_SHAPE").map_or(true, |only| shape.id.starts_with(&only))
        })
        .flat_map(|shape| {
            [Start, Resumed, DuringAbort].into_iter().map(move |phase| {
                Box::new(move || {
                    let expected: std::collections::BTreeSet<String> = POINTS
                        .iter()
                        .filter(|row| {
                            row.cells
                                .iter()
                                .any(|(id, p)| *id == shape.id && *p == phase)
                        })
                        .map(|row| row.point.to_owned())
                        .collect();
                    let mut passed = traced(shape, phase);
                    for _ in 0..2 {
                        if passed.is_superset(&expected) {
                            break;
                        }
                        passed.extend(traced(shape, phase));
                    }
                    passed
                        .into_iter()
                        .map(|point| (shape.id.to_owned(), point, format!("{phase:?}")))
                        .collect()
                }) as Box<dyn FnOnce() -> Vec<Reached> + Send>
            })
        })
        .collect();
    in_parallel(jobs).into_iter().flatten().collect()
}

/// A shape that starts reaching a point gains no cell on its own: this fails
/// until the row lists it, and until a row stops listing a shape that no
/// longer reaches the point.
#[test]
fn the_cells_are_exactly_where_the_shapes_reach_the_points() {
    let found = discovered();
    let listed: std::collections::BTreeSet<Reached> = POINTS
        .iter()
        .flat_map(|row| {
            row.cells.iter().map(move |(id, phase)| {
                ((*id).to_owned(), row.point.to_owned(), format!("{phase:?}"))
            })
        })
        .filter(|(id, point, _)| {
            std::env::var("DISCOVER_SHAPE").map_or(true, |only| id.starts_with(&only))
                && std::env::var("DISCOVER").map_or(true, |only| point.starts_with(&only))
        })
        .collect();
    let same_as: std::collections::BTreeSet<(&str, &str)> = POINTS
        .iter()
        .flat_map(|row| row.same_as.iter().map(move |id| (*id, row.point)))
        .collect();
    let timing: std::collections::BTreeSet<&str> = POINTS
        .iter()
        .filter(|row| row.timing_dependent)
        .map(|row| row.point)
        .collect();
    let without_a_cell: Vec<_> = found
        .difference(&listed)
        .filter(|(id, point, _)| !same_as.contains(&(id.as_str(), point.as_str())))
        .filter(|(_, point, _)| !timing.contains(point.as_str()))
        .collect();
    let never_reached: Vec<_> = listed.difference(&found).collect();
    assert!(
        without_a_cell.is_empty() && never_reached.is_empty(),
        "reached and not in POINTS: {without_a_cell:#?}\nin POINTS and not reached: {never_reached:#?}"
    );
}

#[test]
fn without_the_stage_a_shape_test_returns_at_once_naming_it() {
    let began = std::time::Instant::now();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "kill_matrix_test::cherry_pick", "--nocapture"])
        .env_remove("RWV_KILL_MATRIX")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains(STAGE), "{stdout}");
    assert!(
        stdout.contains("test kill_matrix_test::cherry_pick ... ok"),
        "{stdout}"
    );
    assert!(
        began.elapsed() < std::time::Duration::from_secs(30),
        "{:?}",
        began.elapsed()
    );
}
