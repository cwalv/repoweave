//! `rwv each`: the bundled plugin that runs a command in each repo of the
//! checkout rwv resolved. It is a plugin like any other — it learns the
//! checkout from the `RWV_*` envelope and the repos from `rwv status --json` —
//! so it never names the library this package also builds.

mod cli;
mod output;
mod program;
mod report;
mod run;
mod schema_url;
mod select;
mod tree;
mod wire;

use std::process::ExitCode;
use std::sync::atomic::Ordering;

use clap::Parser;

use crate::cli::Cli;
use crate::report::{Json, Reporter};
use crate::run::{CommandSpec, Layout, Runner, INTERRUPTS};
use crate::select::Envelope;

const DEFAULT_JOBS_CAP: usize = 8;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = {
        let _job = tree::KillOnExitJob::join();
        match execute(&cli) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        }
    };
    ExitCode::from(code)
}

fn execute(cli: &Cli) -> anyhow::Result<u8> {
    let rwv = run::rwv_program();
    let repos = select::select(&rwv, &Envelope::from_process(), &cli.roles, &cli.repos)?;
    let spec = CommandSpec::new(&cli.command, cli.shell.as_deref())?;
    let command = spec.display();

    if cli.dry_run {
        println!("rwv each (dry-run):");
        for repo in &repos {
            if repo.present {
                println!(
                    "  {}: would run `{command}` in {}",
                    repo.path,
                    repo.dir.display()
                );
            } else {
                println!("  {}: not present on disk; run `rwv fetch`", repo.path);
            }
        }
        return Ok(0);
    }

    ctrlc::set_handler(|| {
        INTERRUPTS.fetch_add(1, Ordering::SeqCst);
    })?;

    let jobs = cli.jobs.map_or_else(default_jobs, |j| j as usize);
    let json = cli.json.then_some(if jobs > 1 {
        Json::Ndjson
    } else {
        Json::Envelope
    });
    let layout = if json.is_some() {
        Layout::Silent
    } else if cli.group {
        Layout::Grouped
    } else if jobs > 1 {
        Layout::Prefixed
    } else if cli.results.is_some() || cli.progress {
        Layout::Plain
    } else {
        Layout::Inherit
    };
    let progress = cli.progress && std::io::IsTerminal::is_terminal(&std::io::stderr());

    let reporter = Reporter::new(
        &repos,
        layout,
        json,
        command,
        progress,
        cli.joblog.as_deref(),
        cli.results.clone(),
    )?;
    let runner = Runner {
        repos: &repos,
        spec: &spec,
        layout,
        capture: json.is_some() || cli.results.is_some(),
        timeout: cli.timeout,
        fail_fast: cli.fail_fast,
        jobs,
        reporter: &reporter,
    };
    let all = runner.run_all();
    Ok(reporter.finish(&all, run::interrupted()))
}

fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(DEFAULT_JOBS_CAP))
}
