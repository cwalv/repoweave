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

fn lib_moved_upstream() -> Vec<Step> {
    vec![
        Step::new_machine(Machine::B),
        Step::commit(&b(), Repo::Lib, "beta-lib"),
        Step::git(&b(), Repo::Lib, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::Lib), "main"),
        Step::commit(&a(), Repo::Lib, "alpha-lib"),
    ]
}

fn lib_and_app_moved_upstream() -> Vec<Step> {
    let mut steps = lib_moved_upstream();
    steps.extend([
        Step::commit(&b(), Repo::App, "beta-app"),
        Step::git(&b(), Repo::App, &["push", "origin", "main"])
            .pushes(Remote::origin(Repo::App), "main"),
        Step::commit(&a(), Repo::App, "alpha-app"),
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

fn run_of(shape: &Shape, expect: Exit, args: &'static [&'static str]) -> Step {
    Step::rwv(&(shape.at)(), args).expect(expect)
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
    steps.push(
        Step::rwv(&a(), &["doctor", "--all"])
            .expect(Exit::Succeeds)
            .claim(Claim::OutputLacks("rwv doctor --fix")),
    );
    steps
}

fn until_stopped(shape: &Shape, phase: Phase, point: &'static str) -> Vec<Step> {
    let at = (shape.at)();
    let mut steps = (shape.setup)();
    steps.push(Step::capture("before"));
    match phase {
        Phase::Start => steps.push(Step::rwv_paused(&at, shape.start, point, "held")),
        Phase::Resumed => {
            let stop = shape
                .stop
                .as_ref()
                .expect("a resumed cell has a shape that stops");
            steps.push(run_of(shape, (stop.exit)(), shape.start));
            steps.extend((stop.fix)());
            steps.push(Step::rwv_paused(&at, shape.resume, point, "held"));
        }
    }
    steps.push(Step::kill("held"));
    steps
}

fn continued(shape: &Shape, phase: Phase) -> Vec<Step> {
    let mut steps = Vec::new();
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

fn aborted(shape: &Shape, typed_at: Place) -> Vec<Step> {
    let mut steps = vec![Step::rwv(&typed_at, &["abort"])
        .expect(Exit::Succeeds)
        .may_strand(&everywhere())
        .may_rewind(&everywhere())
        .may_restore(&everywhere())
        .may_rewrite(&everywhere())
        .claim(Claim::TipsAsCaptured("before"))];
    steps.extend(ends_clean(shape));
    steps.push(Step::rwv(&a(), &["status"]).claim(Claim::TipsAsCaptured("before")));
    steps
}

fn unkilled(shape: &Shape) -> Vec<Step> {
    let mut steps = (shape.setup)();
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

fn steps_of(shape: &Shape, phase: Phase, point: &'static str, ending: Ending) -> Vec<Step> {
    let mut steps = until_stopped(shape, phase, point);
    match ending {
        Ending::Continue => steps.extend(continued(shape, phase)),
        Ending::Abort => steps.extend(aborted(shape, (shape.at)())),
        Ending::AbortInTarget => steps.extend(aborted(
            shape,
            shape.target.expect("only a landing aborts in its target")(),
        )),
    }
    steps
}

fn named(shape: &Shape, phase: Phase, point: &str, ending: Ending) -> String {
    let phase = match phase {
        Phase::Start => "",
        Phase::Resumed => " resumed",
    };
    format!("{} killed at {point}{phase}, {}", shape.id, ending.word())
}

fn scenario_of(
    shape: &Shape,
    name: &'static str,
    steps: Vec<Step>,
) -> Scenario {
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
                "the end state differs from an unkilled run's:\n--- unkilled\n{}--- continued\n{killed}",
                runner::end_state(&clean)
            );
        }
    }
    (!why.is_empty()).then(|| format!("{name}:\n{why}"))
}

/// Whether the shape's verb, killed at `point`, is held there.
fn reached(shape: &Shape, phase: Phase, point: &'static str) -> bool {
    let mut steps = (shape.setup)();
    match phase {
        Phase::Start => steps.push(Step::rwv_paused(&(shape.at)(), shape.start, point, "held")),
        Phase::Resumed => {
            let Some(stop) = &shape.stop else {
                return false;
            };
            steps.push(run_of(shape, (stop.exit)(), shape.start));
            steps.extend((stop.fix)());
            steps.push(Step::rwv_paused(&(shape.at)(), shape.resume, point, "held"));
        }
    }
    steps.push(Step::kill("held"));
    let report = runner::run(scenario_of(shape, "reach", steps));
    report.unmet.is_empty()
}

fn in_parallel<T: Send>(jobs: Vec<Box<dyn FnOnce() -> T + Send + '_>>) -> Vec<T> {
    let width = std::thread::available_parallelism().map_or(4, usize::from).min(8);
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

use Phase::{Resumed, Start};

pub(super) const POINTS: &[Row] = &[];

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

fn endings(shape: &Shape) -> Vec<Ending> {
    let mut endings = vec![Ending::Continue, Ending::Abort];
    if shape.target.is_some() {
        endings.push(Ending::AbortInTarget);
    }
    endings
}

fn run_shape(id: &str) {
    let shape = shape(id);
    let jobs: Vec<Box<dyn FnOnce() -> Option<String> + Send>> = cells_of(shape)
        .into_iter()
        .flat_map(|(point, phase)| {
            endings(shape)
                .into_iter()
                .map(move |ending| (point, phase, ending))
        })
        .map(|(point, phase, ending)| {
            Box::new(move || run_cell(shape, phase, point, ending))
                as Box<dyn FnOnce() -> Option<String> + Send>
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
    walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
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
                *phase == Start || shape.stop.is_some(),
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
        .flat_map(|shape| {
            let points = &points;
            [Start, Resumed].into_iter().flat_map(move |phase| {
                points.iter().map(move |point| {
                    let point: &'static str = leaked(point.clone());
                    Box::new(move || {
                        (!(phase == Resumed && shape.stop.is_none())
                            && reached(shape, phase, point))
                        .then(|| format!("REACH {} {point} {phase:?}", shape.id))
                    }) as Box<dyn FnOnce() -> Option<String> + Send>
                })
            })
        })
        .collect();
    for line in in_parallel(jobs).into_iter().flatten() {
        eprintln!("{line}");
    }
}
