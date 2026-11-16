//! The git an `rwv` command starts, as a `test-pause` build records them in
//! `RWV_TEST_GIT_TRACE`: one line of arguments per start, appended, so every
//! rwv sharing the file adds to it. The count comes from the binary, never
//! from a clock.
//!
//! The first test holds that the trace records what rwv starts. The ignored
//! one is an instrument, not a gate: it prints how many git each of a fixed
//! set of commands starts in a project of three members and two workweaves,
//! with a tally by arguments.

use std::collections::BTreeMap;
use std::path::Path;

use crate::common::scenario_runner::{self as runner, Scenario, Step};
use crate::common::scenario_sandbox::{Place, Repo, Sandbox, Seed};
use crate::common::scenario_steps::{a, create, ww};

fn traced(sb: &Sandbox, at: &Place, args: &[&str], trace: &Path) -> Vec<String> {
    let out = sb
        .rwv_command(&sb.place_root(at), args)
        .env("RWV_TEST_GIT_TRACE", trace)
        .output()
        .unwrap_or_else(|e| panic!("rwv {args:?} failed to start: {e}"));
    assert!(
        out.status.success(),
        "rwv {args:?} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(trace)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn two_workweaves() -> Sandbox {
    let sb = Sandbox::new(Seed::default());
    runner::run_in(
        &sb,
        Scenario {
            name: "two workweaves, one with work to land",
            seed: Seed::default(),
            steps: vec![
                create("w1"),
                create("w2"),
                Step::commit(&ww("w1"), Repo::App, "w1-app"),
                Step::commit(&ww("w1"), Repo::Lib, "w1-lib"),
            ],
        },
    )
    .assert_green();
    sb
}

#[test]
fn the_trace_holds_each_git_a_command_starts_and_every_rwv_appends_to_it() {
    if !cfg!(feature = "test-pause") {
        eprintln!("rwv traces the git it starts only in a test-pause build: skipped");
        return;
    }
    let sb = two_workweaves();
    let status = traced(&sb, &ww("w1"), &["status"], &sb.root.join("status"));
    assert!(
        status
            .iter()
            .any(|line| line == "rev-parse --show-toplevel"),
        "rwv status asks where each checkout's top level is; traced:\n{}",
        status.join("\n")
    );
    let listed = traced(&sb, &a(), &["workweave", "list"], &sb.root.join("list"));
    assert!(!listed.is_empty(), "rwv workweave list traced no git");

    let shared = sb.root.join("shared");
    traced(&sb, &ww("w1"), &["status"], &shared);
    let both = traced(&sb, &a(), &["workweave", "list"], &shared);
    assert_eq!(
        both,
        [status, listed].concat(),
        "a second rwv appends its git after the first's, and runs alike trace alike"
    );
}

fn tally(started: &[String]) -> String {
    let mut by_args: BTreeMap<&str, usize> = BTreeMap::new();
    for args in started {
        *by_args.entry(args.as_str()).or_default() += 1;
    }
    let mut rows: Vec<(usize, &str)> = by_args.into_iter().map(|(a, n)| (n, a)).collect();
    rows.sort_by(|x, y| y.cmp(x));
    rows.iter()
        .map(|(n, args)| format!("{n:5}  git {args}\n"))
        .collect()
}

/// `cargo test --release --features test-pause --test scenario --
/// git_spawns_test::git_started_per_command --ignored --nocapture`
#[test]
#[ignore = "an instrument: prints how many git each command starts"]
fn git_started_per_command() {
    if !cfg!(feature = "test-pause") {
        eprintln!("rwv traces the git it starts only in a test-pause build: skipped");
        return;
    }
    let sb = two_workweaves();
    let commands: [(Place, &[&str]); 7] = [
        (ww("w1"), &["status"]),
        (a(), &["workweave", "list"]),
        (ww("w1"), &["prime"]),
        (ww("w1"), &["lock", "--commit"]),
        (ww("w1"), &["doctor"]),
        (ww("w1"), &["land"]),
        (ww("w2"), &["sync"]),
    ];
    let mut summary = String::new();
    for (i, (at, args)) in commands.iter().enumerate() {
        let started = traced(&sb, at, args, &sb.root.join(format!("trace-{i}")));
        summary.push_str(&format!("{:6}  rwv {}\n", started.len(), args.join(" ")));
        eprintln!(
            "rwv {}: {} git\n{}",
            args.join(" "),
            started.len(),
            tally(&started)
        );
    }
    eprintln!("{summary}");
}
