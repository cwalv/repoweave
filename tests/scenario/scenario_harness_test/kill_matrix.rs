//! Every pause point the source declares, for every verb that reaches it:
//! the verb killed there for real, then `--continue` in one run and
//! `rwv abort` in another.
//!
//! [`POINTS`] is the input. Each row names a point and the cells that reach
//! it; a source test holds the rows equal to the points the source declares.
//! A shape is a verb in one setup, and its cells are generated from the rows,
//! so a point added to a row gains its cells and a point added to the source
//! without a row fails.
//!
//! A cell passes when `--continue` ends the operation in the state an unkilled
//! run of the same setup ends in, and when `rwv abort`, typed in the seat and,
//! for a landing, in its target, leaves every place as it was before the
//! verb. Both leave no operation recorded and nothing `rwv doctor --all`
//! reports.
//!
//! What a cell does not see: a verb killed twice (the two-kill scenarios of
//! `scenario_harness_test.rs` do that), a conflict resolved any other way than
//! the one the shape's fix does, and a remote that moved while the verb was
//! down.

use super::*;
use crate::scenario_harness_test::runner;
use crate::scenario_harness_test::sandbox::Sandbox;

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
}

impl Ending {
    fn word(self) -> &'static str {
        match self {
            Ending::Continue => "continue",
            Ending::Abort => "abort",
            Ending::AbortInTarget => "abort-in-target",
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

fn parks_on_the_manifest() -> Exit {
    Exit::refuses(Repo::Proj, State::UnparseableProjectMidOperation)
}

fn seat_ahead_of_a_moved_main(history: &'static str) -> Vec<Step> {
    vec![
        Step::rwv(&a(), &["branch", "set-history", history]).expect(Exit::Succeeds),
        create("w1"),
        Step::commit(&ww("w1"), Repo::App, "w1-app"),
        Step::commit(&ww("w1"), Repo::Proj, "w1-proj"),
        Step::commit(&a(), Repo::App, "main-app"),
        Step::rwv(&a(), &["lock", "--commit"]).expect(Exit::Succeeds),
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

fn resolve_readme() -> Vec<Step> {
    vec![
        Step::hand_edit(
            &ww("w1"),
            Repo::App,
            "README",
            "app in w1\n",
            "resolved by hand\n",
        ),
        Step::git(&ww("w1"), Repo::App, &["add", "README"]).expect(Exit::Succeeds),
    ]
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
const PULL_RESET: &[&str] = &["pull", "--reset-to-upstream", "projects/proj"];
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
            fix: resolve_readme,
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
            exit: parks_on_the_manifest,
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
        target: Some(primary_a),
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
        target: Some(primary_a),
        stop: Some(Stop {
            exit: parks,
            fix: resolve_readme,
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
        target: Some(primary_a),
        stop: Some(Stop {
            exit: parks_on_the_manifest,
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
            fix: resolve_readme,
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
        target: Some(primary_a),
        stop: Some(Stop {
            exit: parks,
            fix: resolve_readme,
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
        target: Some(primary_a),
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
        target: Some(primary_a),
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

fn everywhere() -> [Repo; 4] {
    [Repo::Proj, Repo::App, Repo::Lib, Repo::Docs]
}

fn loose(step: Step) -> Step {
    step.may_strand(&everywhere())
        .may_rewind(&everywhere())
        .may_restore(&everywhere())
        .may_rewrite(&everywhere())
}

fn paused(at: &Place, args: &[&str], point: &'static str, handle: &'static str) -> Step {
    loose(Step::rwv_paused(at, args, point, handle))
}

fn run_of(shape: &Shape, expect: Exit, args: &'static [&'static str]) -> Step {
    loose(Step::rwv(&(shape.at)(), args).expect(expect))
}

fn ends_clean(shape: &Shape) -> Vec<Step> {
    let mut steps = vec![Step::rwv(&a(), &["status"])
        .expect(Exit::Succeeds)
        .claim(Claim::OutputLacks("in progress"))];
    let at = (shape.at)();
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
        Phase::Start => steps.push(paused(&at, shape.start, point, "held")),
        Phase::DuringAbort => {
            steps.push(paused(&at, shape.start, shape.abort_after, "held"));
            steps.push(Step::kill("held"));
            steps.push(paused(&at, &["abort"], point, "aborting"));
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
            steps.push(paused(&at, shape.resume, point, "held"));
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
    !BEFORE_THE_RECORD.contains(&point)
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
    let mut steps = Vec::new();
    if !recorded(point) {
        steps.extend(rerun(shape));
        steps.extend(ends_clean(shape));
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
    steps.extend(ends_clean(shape));
    steps
}

fn finishing(shape: &Shape) -> Step {
    let step = run_of(shape, Exit::Succeeds, shape.resume);
    if shape.stop.is_some() {
        step.resolved_by_hand(&everywhere())
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
        "ending" => InTheTarget::Released,
        _ => InTheTarget::Ends,
    }
}

fn aborted(shape: &Shape, typed_at: Place, point: &str, in_target: bool) -> Vec<Step> {
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
    let ender = loose(ender.expect(ends));
    steps.push(if shape.refreshes_drift {
        ender
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
    steps.extend(ends_clean(shape));
    if shape.contender.is_none() {
        steps.push(Step::rwv(&a(), &["status"]).claim(Claim::TipsAsCaptured("before")));
    }
    if shape.stop.is_none() {
        steps.push(
            run_of(shape, Exit::Succeeds, shape.start)
                .may_strand(&everywhere())
                .may_rewind(&everywhere())
                .may_rewrite(&everywhere()),
        );
    }
    steps
}

fn unkilled(shape: &Shape) -> Vec<Step> {
    let mut steps = (shape.setup)();
    if shape.contender == Some(Contention::Live) {
        steps.push(Step::rwv(&ww("w1"), HOLDER).expect(Exit::Succeeds));
    }
    steps.extend(rerun(shape));
    steps
}

fn steps_of(shape: &Shape, phase: Phase, point: &'static str, ending: Ending) -> Vec<Step> {
    let mut steps = until_stopped(shape, phase, point);
    match ending {
        Ending::Continue => steps.extend(continued(shape, phase, point)),
        Ending::Abort => steps.extend(aborted(shape, (shape.at)(), point, false)),
        Ending::AbortInTarget => steps.extend(aborted(
            shape,
            shape.target.expect("only a landing aborts in its target")(),
            point,
            true,
        )),
    }
    steps
}

fn named(shape: &Shape, phase: Phase, point: &str, ending: Ending) -> String {
    let phase = match phase {
        Phase::Start => "",
        Phase::Resumed => " resumed",
        Phase::DuringAbort => " during abort",
    };
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

fn failures(report: &runner::Report) -> String {
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

fn run_cell(shape: &Shape, phase: Phase, point: &'static str, ending: Ending) -> Option<String> {
    let name = named(shape, phase, point, ending);
    let steps = steps_of(shape, phase, point, ending);
    let sandbox = Sandbox::new((shape.seed)());
    let report = runner::run_in(&sandbox, scenario_of(shape, leaked(name.clone()), steps));
    let mut why = failures(&report);
    if ending == Ending::Continue && why.is_empty() {
        let killed = runner::end_state(&sandbox);
        let clean = Sandbox::new((shape.seed)());
        let report = runner::run_in(&clean, scenario_of(shape, "unkilled", unkilled(shape)));
        let reference = failures(&report);
        if !reference.is_empty() {
            why = format!("the unkilled run of the same setup failed:\n{reference}");
        } else if killed != runner::end_state(&clean) {
            why = format!(
                "the end state differs from an unkilled run's:\n{}",
                line_diff(&runner::end_state(&clean), &killed)
            );
        }
    }
    (!why.is_empty()).then(|| format!("{name}:\n{why}"))
}

/// Whether the shape's verb, killed at `point`, is held there.
fn reached(shape: &Shape, phase: Phase, point: &'static str) -> bool {
    let mut steps = (shape.setup)();
    steps.extend(holding(shape));
    match phase {
        Phase::Start => steps.push(paused(&(shape.at)(), shape.start, point, "held")),
        Phase::DuringAbort => {
            steps.push(Step::rwv_paused(
                &(shape.at)(),
                shape.start,
                shape.abort_after,
                "held",
            ));
            steps.push(Step::kill("held"));
            steps.push(Step::rwv_paused(
                &(shape.at)(),
                &["abort"],
                point,
                "aborting",
            ));
            steps.push(Step::kill("aborting"));
            let report = runner::run(scenario_of(shape, "reach", steps));
            return report.unmet.is_empty();
        }
        Phase::Resumed => {
            let Some(stop) = &shape.stop else {
                return false;
            };
            steps.push(run_of(shape, (stop.exit)(), shape.start));
            steps.extend((stop.fix)());
            steps.push(paused(&(shape.at)(), shape.resume, point, "held"));
        }
    }
    steps.push(Step::kill("held"));
    let report = runner::run(scenario_of(shape, "reach", steps));
    report.unmet.is_empty()
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
}

use Phase::{DuringAbort, Resumed, Start};

pub(super) const POINTS: &[Row] = &[
    Row {
        point: "abort-begun",
        cells: &[("cherry_pick", DuringAbort), ("land_linear", DuringAbort), ("land_linear_as_refs", DuringAbort), ("land_linear_carrying", DuringAbort), ("land_linear_in_place", DuringAbort), ("land_merge", DuringAbort), ("pull_merge_upstream", DuringAbort), ("pull_reset_to_upstream", DuringAbort), ("sync_linear", DuringAbort), ("sync_linear_carrying", DuringAbort), ("update_remote_merge_and_reset_to_upstream", DuringAbort), ("update_remote_merge_upstream", DuringAbort), ("update_remote_reset_to_upstream", DuringAbort)],
        elsewhere: "every verb that records an operation reaches it through `rwv abort`; the shapes listed are the ones whose abort plans differ (checked-out target, in place, as refs, carried dependents, take-ins, a port).",
    },
    Row {
        point: "abort-carried",
        cells: &[("cherry_pick", DuringAbort), ("land_linear", DuringAbort), ("land_linear_as_refs", DuringAbort), ("land_linear_carrying", DuringAbort), ("land_linear_in_place", DuringAbort), ("land_merge", DuringAbort), ("pull_merge_upstream", DuringAbort), ("pull_reset_to_upstream", DuringAbort), ("sync_linear", DuringAbort), ("sync_linear_carrying", DuringAbort), ("update_remote_merge_and_reset_to_upstream", DuringAbort), ("update_remote_merge_upstream", DuringAbort), ("update_remote_reset_to_upstream", DuringAbort)],
        elsewhere: "same as abort-begun; the carried step runs in every abort plan.",
    },
    Row {
        point: "abort-executed",
        cells: &[("cherry_pick", DuringAbort), ("land_linear", DuringAbort), ("land_linear_as_refs", DuringAbort), ("land_linear_carrying", DuringAbort), ("land_linear_in_place", DuringAbort), ("land_merge", DuringAbort), ("pull_merge_upstream", DuringAbort), ("pull_reset_to_upstream", DuringAbort), ("sync_linear", DuringAbort), ("sync_linear_carrying", DuringAbort), ("update_remote_merge_and_reset_to_upstream", DuringAbort), ("update_remote_merge_upstream", DuringAbort), ("update_remote_reset_to_upstream", DuringAbort)],
        elsewhere: "same as abort-begun.",
    },
    Row {
        point: "abort-retaken",
        cells: &[("cherry_pick", DuringAbort), ("land_linear", DuringAbort), ("land_linear_as_refs", DuringAbort), ("land_linear_carrying", DuringAbort), ("land_linear_in_place", DuringAbort), ("land_merge", DuringAbort), ("pull_merge_upstream", DuringAbort), ("pull_reset_to_upstream", DuringAbort), ("sync_linear", DuringAbort), ("sync_linear_carrying", DuringAbort), ("update_remote_merge_and_reset_to_upstream", DuringAbort), ("update_remote_merge_upstream", DuringAbort), ("update_remote_reset_to_upstream", DuringAbort)],
        elsewhere: "same as abort-begun; a landing's target is retaken there and other verbs pass it as a no-op.",
    },
    Row {
        point: "advance-target",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        elsewhere: "only a landing advances a target; sync, pull, update --remote and cherry-pick have none.",
    },
    Row {
        point: "advance-target-move",
        cells: &[("land_linear", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        elsewhere: "only a landing moves its target's checkout; a landing as refs moves refs instead and does not pass it.",
    },
    Row {
        point: "aligning-target",
        cells: &[("land_linear", Start), ("land_linear_carrying", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        elsewhere: "only a landing into a checked-out target aligns it; a landing as refs leaves checkouts alone.",
    },
    Row {
        point: "carry-composed",
        cells: &[("land_linear_carrying", Start), ("sync_linear_carrying", Start)],
        elsewhere: "only a verb carrying a dependent branch composes a carry, and only on the linear history setting; the merge-history carry composes elsewhere.",
    },
    Row {
        point: "carry-journaled",
        cells: &[("land_linear_carrying", Start), ("sync_linear_carrying", Start)],
        elsewhere: "as carry-composed.",
    },
    Row {
        point: "carry-move",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed), ("sync_linear", Start), ("sync_linear_carrying", Start), ("sync_linear_manifest_stop", Resumed), ("sync_linear_own_stop", Resumed), ("sync_merge", Start), ("sync_merge_carrying", Start), ("sync_merge_own_stop", Resumed)],
        elsewhere: "every sync and landing moves a branch through a carry journal entry; pull, update --remote and cherry-pick journal their moves another way.",
    },
    Row {
        point: "carry-moved",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed), ("sync_linear", Start), ("sync_linear_carrying", Start), ("sync_linear_manifest_stop", Resumed), ("sync_linear_own_stop", Resumed), ("sync_merge", Start), ("sync_merge_carrying", Start), ("sync_merge_own_stop", Resumed)],
        elsewhere: "as carry-move.",
    },
    Row {
        point: "carry-recorded",
        cells: &[("land_linear_carrying", Start), ("sync_linear_carrying", Start)],
        elsewhere: "only a verb carrying a dependent branch records a carry, and only on the linear history setting.",
    },
    Row {
        point: "cherry-pick-member-copied",
        cells: &[("cherry_pick", Start)],
        elsewhere: "only cherry-pick copies member ranges.",
    },
    Row {
        point: "cherry-pick-project-committed",
        cells: &[("cherry_pick", Start)],
        elsewhere: "only cherry-pick commits a ported project commit.",
    },
    Row {
        point: "cherry-pick-project-copy",
        cells: &[("cherry_pick", Start)],
        elsewhere: "only cherry-pick copies the project's files.",
    },
    Row {
        point: "claim-held",
        cells: &[],
        elsewhere: "reached only by a process waiting on a state-file, workweave-index, owned-digest or fetch claim another process holds, before any operation is recorded; nothing is left to continue or abort.",
    },
    Row {
        point: "claimed",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start)],
        elsewhere: "only a landing claims its target.",
    },
    Row {
        point: "composed",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        elsewhere: "only a landing composes its source before advancing the target.",
    },
    Row {
        point: "drift-merge-carry",
        cells: &[],
        elsewhere: "as drift-merge-read.",
    },
    Row {
        point: "drift-merge-read",
        cells: &[],
        elsewhere: "reached only by `rwv doctor --fix --merge-edited-drift` and `--set-aside-edited-drift`, which record no operation: there is nothing to continue or abort.",
    },
    Row {
        point: "drift-merge-set-back",
        cells: &[],
        elsewhere: "as drift-merge-read.",
    },
    Row {
        point: "drift-read",
        cells: &[("land_linear_in_place_drifted", Start)],
        elsewhere: "only a landing in place that finds the primary's checkout drifted refreshes it there.",
    },
    Row {
        point: "drift-write",
        cells: &[("land_linear_in_place_drifted", Start)],
        elsewhere: "as drift-read.",
    },
    Row {
        point: "drift-write-records",
        cells: &[("land_linear_in_place_drifted", Start)],
        elsewhere: "as drift-read.",
    },
    Row {
        point: "drift-write-spawn",
        cells: &[("land_linear_in_place_drifted", Start)],
        elsewhere: "as drift-read.",
    },
    Row {
        point: "durable-file-created",
        cells: &[("cherry_pick", Start), ("land_linear", Start), ("land_merge", Start), ("pull_merge_upstream", Start), ("pull_reset_to_upstream", Start), ("sync_linear", Start), ("sync_merge", Start), ("update_remote_merge_and_reset_to_upstream", Start), ("update_remote_merge_upstream", Start), ("update_remote_reset_to_upstream", Start)],
        elsewhere: "every verb that records an operation writes it through the durable file; one shape of each verb and history stands for the rest, which differ only in what they go on to do.",
    },
    Row {
        point: "durable-file-restage",
        cells: &[],
        elsewhere: "reached only when a sweep removes a durable file's staged temp while the publish is still writing it, which no verb run does, and no operation is involved.",
    },
    Row {
        point: "durable-file-staged",
        cells: &[("cherry_pick", Start), ("land_linear", Start), ("land_merge", Start), ("pull_merge_upstream", Start), ("pull_reset_to_upstream", Start), ("sync_linear", Start), ("sync_merge", Start), ("update_remote_merge_and_reset_to_upstream", Start), ("update_remote_merge_upstream", Start), ("update_remote_reset_to_upstream", Start)],
        elsewhere: "as durable-file-created.",
    },
    Row {
        point: "ending",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        elsewhere: "only a landing has an ending that releases its target.",
    },
    Row {
        point: "fetch-under-claim",
        cells: &[("pull_merge_upstream", Start), ("pull_reset_to_upstream", Start), ("sync_linear", Start), ("sync_merge", Start), ("update_remote_merge_and_reset_to_upstream", Start), ("update_remote_merge_upstream", Start), ("update_remote_reset_to_upstream", Start)],
        elsewhere: "every verb that fetches passes it; one shape of each verb and history stands for the rest.",
    },
    Row {
        point: "landed",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        elsewhere: "only a landing records that its target moved.",
    },
    Row {
        point: "landing-claim-dead-seen",
        cells: &[],
        elsewhere: "reached only by a landing that finds the claim's holder dead and removes it, which needs a claim whose recorded operation is gone; tests/process/landing_claim_contention_test.rs kills there.",
    },
    Row {
        point: "landing-claim-held",
        cells: &[("land_linear_waiting", Start)],
        elsewhere: "only a landing waiting on another landing's claim passes it.",
    },
    Row {
        point: "landing-claim-takeover",
        cells: &[],
        elsewhere: "as landing-claim-dead-seen.",
    },
    Row {
        point: "leased-in-place",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start)],
        elsewhere: "only a landing takes the lease on a primary whose checkouts it fast-forwards in place.",
    },
    Row {
        point: "op-savepoint-taken",
        cells: &[("cherry_pick", Start), ("pull_merge_upstream", Start), ("pull_reset_to_upstream", Start), ("sync_linear", Start), ("sync_linear_carrying", Start), ("sync_linear_manifest_stop", Start), ("sync_linear_own_stop", Start), ("sync_merge", Start), ("sync_merge_carrying", Start), ("sync_merge_own_stop", Start), ("update_remote_merge_and_reset_to_upstream", Start), ("update_remote_merge_upstream", Start), ("update_remote_reset_to_upstream", Start)],
        elsewhere: "every verb that records an operation passes it once; a landing records its plan at recorded instead.",
    },
    Row {
        point: "push-members",
        cells: &[],
        elsewhere: "reached only by `rwv push`, which records no operation: there is nothing to continue or abort.",
    },
    Row {
        point: "recorded",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_manifest_stop", Start), ("land_linear_own_stop", Start), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Start)],
        elsewhere: "only a landing records its plan before claiming.",
    },
    Row {
        point: "records-publish",
        cells: &[],
        elsewhere: "reached only by `rwv push` publishing the records branch, which records no operation: there is nothing to continue or abort.",
    },
    Row {
        point: "records-publish-settled",
        cells: &[],
        elsewhere: "as records-publish.",
    },
    Row {
        point: "records-write",
        cells: &[("land_linear_retiring", Start)],
        elsewhere: "only a landing that retires its workweave deletes the workweave's record.",
    },
    Row {
        point: "relock-committed",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_in_place", Start), ("land_linear_retiring", Start)],
        elsewhere: "only a landing, on the linear history setting, commits its relock under its own name; a sync's is sync-relock-committed and a merge landing's is relock-merge-committed.",
    },
    Row {
        point: "relock-merge-committed",
        cells: &[("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        elsewhere: "only a landing on the merge history setting commits a merge relock; pull and update --remote do not pass it in any shape tried.",
    },
    Row {
        point: "relocking",
        cells: &[("land_linear", Start), ("land_linear_as_refs", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
        elsewhere: "only a landing relocks at that step; sync's relock is sync-relock-committed.",
    },
    Row {
        point: "remap-moved",
        cells: &[("land_linear_carrying", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("sync_linear_carrying", Start), ("sync_linear_manifest_stop", Resumed), ("sync_linear_own_stop", Resumed)],
        elsewhere: "only a replay that stops and is continued remaps the branches it moved; a replay that finishes in one run remaps nothing.",
    },
    Row {
        point: "remap-recorded",
        cells: &[("land_linear_carrying", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("sync_linear_carrying", Start), ("sync_linear_manifest_stop", Resumed), ("sync_linear_own_stop", Resumed)],
        elsewhere: "as remap-moved.",
    },
    Row {
        point: "reparent",
        cells: &[],
        elsewhere: "reached only by `rwv workweave remove` re-parenting its children and by doctor's repair of a workweave marker, neither of which records an operation.",
    },
    Row {
        point: "reset-to-upstream",
        cells: &[("pull_reset_to_upstream", Start), ("update_remote_merge_and_reset_to_upstream", Start), ("update_remote_reset_to_upstream", Start)],
        elsewhere: "only the --reset-to-upstream forms of pull and update --remote move a branch to its upstream.",
    },
    Row {
        point: "retire",
        cells: &[("land_linear_retiring", Start)],
        elsewhere: "only a landing run with --retire retires its workweave.",
    },
    Row {
        point: "retiring",
        cells: &[("land_linear_retiring", Start)],
        elsewhere: "as retire.",
    },
    Row {
        point: "scratch-taken",
        cells: &[("cherry_pick", Start), ("land_linear", Start), ("sync_linear", Start)],
        elsewhere: "every verb that rebases or merges takes a scratch checkout; one shape of each verb and history stands for the rest.",
    },
    Row {
        point: "sync-member-followed",
        cells: &[],
        elsewhere: "no shape reaches it: a sync passes it only for a member whose commits all sit under pins of the target's lock history while the target's branch no longer contains them, and no setup tried builds that; this is a coverage gap, not an unreachable point.",
    },
    Row {
        point: "sync-relock-committed",
        cells: &[("sync_linear", Start)],
        elsewhere: "only a sync that relocks over a commit the lock did not pin commits it; a landing's is relock-committed.",
    },
    Row {
        point: "target-fast-forwarded",
        cells: &[("land_linear", Start), ("land_linear_carrying", Start), ("land_linear_in_place", Start), ("land_linear_manifest_stop", Resumed), ("land_linear_own_stop", Resumed), ("land_linear_retiring", Start), ("land_merge", Start), ("land_merge_carrying", Start), ("land_merge_own_stop", Resumed)],
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

fn endings(shape: &Shape, phase: Phase) -> Vec<Ending> {
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

/// Cells that fail on this tree, each with the defect it shows. A listed cell
/// must still fail: one that passes is a stale entry.
fn known_failing(shape: &Shape, phase: Phase, ending: Ending) -> Option<&'static str> {
    (shape.id == "land_linear_manifest_stop"
        && phase == Resumed
        && matches!(ending, Ending::Abort | Ending::AbortInTarget))
    .then_some(
        "abort of a landing stopped on a replayed rwv.toml leaves the checkout of the member \
         that manifest added in the source workweave",
    )
}

fn run_shape(id: &str) {
    let shape = shape(id);
    let jobs: Vec<Box<dyn FnOnce() -> Option<String> + Send>> = cells_of(shape)
        .into_iter()
        .flat_map(|(point, phase)| {
            endings(shape, phase)
                .into_iter()
                .map(move |ending| (point, phase, ending))
        })
        .map(|(point, phase, ending)| {
            Box::new(move || {
                let failed = run_cell(shape, phase, point, ending);
                match (known_failing(shape, phase, ending), failed) {
                    (None, failed) => failed,
                    (Some(_), Some(_)) => None,
                    (Some(defect), None) => Some(format!(
                        "{} now passes: remove its known-failing entry ({defect})",
                        named(shape, phase, point, ending)
                    )),
                }
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
            .join("tests/scenario/scenario_harness_test/kill_matrix.rs"),
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
    pull_merge_upstream,
    pull_reset_to_upstream,
    update_remote_merge_upstream,
    update_remote_reset_to_upstream,
    update_remote_merge_and_reset_to_upstream,
    cherry_pick,
);

#[test]
#[ignore = "prints which shapes reach which points"]
fn discover() {
    let points: Vec<String> = declared_points().into_iter().collect();
    let jobs: Vec<Box<dyn FnOnce() -> Option<String> + Send>> = SHAPES
        .iter()
        .filter(|shape| {
            std::env::var("DISCOVER_SHAPE").map_or(true, |only| shape.id.starts_with(&only))
        })
        .flat_map(|shape| {
            let points = &points;
            [Start, Resumed, DuringAbort]
                .into_iter()
                .flat_map(move |phase| {
                    points
                        .iter()
                        .filter(move |point| {
                            point.starts_with("abort-") == (phase == DuringAbort)
                                && std::env::var("DISCOVER")
                                    .map_or(true, |only| point.starts_with(&only))
                        })
                        .map(move |point| {
                            let point: &'static str = leaked(point.clone());
                            Box::new(move || {
                                (!(phase == Resumed && shape.stop.is_none())
                                    && reached(shape, phase, point))
                                .then(|| format!("REACH {} {point} {phase:?}", shape.id))
                            })
                                as Box<dyn FnOnce() -> Option<String> + Send>
                        })
                })
        })
        .collect();
    for line in in_parallel(jobs).into_iter().flatten() {
        eprintln!("{line}");
    }
}
