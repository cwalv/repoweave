//! How many git processes one `rwv` command starts, in a weave of a project,
//! three members and two workweaves, each command run once in a fixed order.
//!
//! A process start costs several times more on macOS and Windows than on
//! Linux, and every command pays it once per git it asks, so a fact asked
//! again by each layer, or once per member where once per repo would answer
//! it, is what this bounds. The count comes from the binary itself
//! (`RWV_TEST_GIT_TRACE`, honored only in a `test-pause` build), never from a
//! clock, and a red prints the tally of what was started.
//!
//! What it reads: the commands below, in this fixture. A fan-out a command
//! reaches only through a shape the fixture lacks (more members, a second
//! project, a conflict, a remote that refuses) is invisible to it.

use std::collections::BTreeMap;
use std::path::Path;

use crate::common::scenario_runner::{self as runner, Scenario, Step};
use crate::common::scenario_sandbox::{Place, Repo, Sandbox, Seed};
use crate::common::scenario_steps::{a, create, ww};

struct Measured {
    at: Place,
    args: &'static [&'static str],
    ceiling: usize,
}

fn counted(sb: &Sandbox, at: &Place, args: &[&str], trace: &Path) -> Vec<String> {
    let _ = std::fs::remove_file(trace);
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

/// Each ceiling is the count measured when it was set, plus a tenth. A change
/// that starts more is a new fan-out, and raises the ceiling in plain sight.
#[test]
fn each_command_starts_no_more_git_than_its_ceiling() {
    if !cfg!(feature = "test-pause") {
        eprintln!("rwv traces the git it starts only in a test-pause build: skipped");
        return;
    }
    let sb = Sandbox::new(Seed::default());
    if std::fs::metadata(&sb.root)
        .and_then(|meta| meta.created())
        .is_err()
    {
        eprintln!(
            "{} keeps no creation time, so rwv recalls nothing there: skipped",
            sb.root.display()
        );
        return;
    }
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
    let measured = [
        Measured {
            at: ww("w1"),
            args: &["status"],
            ceiling: 263,
        },
        Measured {
            at: a(),
            args: &["workweave", "list"],
            ceiling: 10,
        },
        Measured {
            at: ww("w1"),
            args: &["prime"],
            ceiling: 7,
        },
        Measured {
            at: ww("w1"),
            args: &["lock", "--commit"],
            ceiling: 174,
        },
        Measured {
            at: ww("w1"),
            args: &["doctor"],
            ceiling: 268,
        },
        Measured {
            at: ww("w1"),
            args: &["land"],
            ceiling: 1107,
        },
        Measured {
            at: ww("w2"),
            args: &["sync"],
            ceiling: 595,
        },
    ];
    let trace = sb.root.join("git-trace");
    let mut over = Vec::new();
    for m in &measured {
        let started = counted(&sb, &m.at, m.args, &trace);
        eprintln!("rwv {}: {} git", m.args.join(" "), started.len());
        assert!(
            !started.is_empty(),
            "rwv {} traced no git at all: the trace is not being written",
            m.args.join(" ")
        );
        if started.len() > m.ceiling {
            over.push(format!(
                "rwv {} started {} git, over its ceiling of {}:\n{}",
                m.args.join(" "),
                started.len(),
                m.ceiling,
                tally(&started)
            ));
        }
    }
    assert!(over.is_empty(), "{}", over.join("\n"));
}
