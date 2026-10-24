use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgGroup, Parser};

/// Run a command in each repo of the checkout rwv resolved
#[derive(Parser, Debug)]
#[command(
    name = "rwv each",
    bin_name = "rwv each",
    version,
    group(ArgGroup::new("what").required(true).args(["command", "shell"]))
)]
pub struct Cli {
    /// Run in repos with this role. Repeat to union multiple roles. Combined as a union with --repo. With neither flag, every repo except role reference runs.
    #[arg(long = "role", value_name = "ROLE")]
    pub roles: Vec<String>,

    /// Run in repos matching this selector. Bare strings match a manifest path exactly; `re:<pat>` matches as regex; `glob:<pat>` matches as glob. Repeat for union. Combined as a union with --role.
    #[arg(long = "repo", value_name = "SEL")]
    pub repos: Vec<String>,

    /// Number of repos to run at once. Default: min(nproc, 8). `-j 1` runs one repo at a time with the terminal handed to the command.
    #[arg(short = 'j', long = "jobs", value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub jobs: Option<u32>,

    /// Buffer each repo's output and print it whole, under a `[<path>]` header, when that repo finishes.
    #[arg(long)]
    pub group: bool,

    /// Start no new repo after the first failure; repos already running finish.
    #[arg(long)]
    pub fail_fast: bool,

    /// Print the repos that would run and the command, and run nothing.
    #[arg(long)]
    pub dry_run: bool,

    /// Emit one outcome per repo as JSON, with each command's output captured into it, in one envelope printed once every repo is done, whatever `-j` is. `--joblog` and `--results` report each repo as it finishes.
    #[arg(long)]
    pub json: bool,

    /// Kill a repo's whole process tree once its command has run this long, and count the repo as failed. Seconds, or a number suffixed s, m, h or d (`90`, `1m30s`, `2h`).
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub timeout: Option<Duration>,

    /// Write one tab-separated line per repo to FILE as each finishes, after a header row.
    #[arg(long, value_name = "FILE", value_parser = parse_output_path)]
    pub joblog: Option<PathBuf>,

    /// Also write each repo's stdout, stderr and outcome to `DIR/<path>/stdout`, `stderr` and `exit`.
    #[arg(long, value_name = "DIR", value_parser = parse_output_path)]
    pub results: Option<PathBuf>,

    /// Keep a one-line progress summary at the bottom of stderr while repos run. Draws nothing when stderr is not a terminal.
    #[arg(long)]
    pub progress: bool,

    /// Run this command string through the platform shell (`sh -c` on Unix, `cmd /C` on Windows) instead of executing a program directly.
    #[arg(long, value_name = "COMMAND")]
    pub shell: Option<String>,

    /// The program to run in each repo, then its arguments, passed as values with no shell in between.
    #[arg(
        value_name = "COMMAND",
        trailing_var_arg = true,
        num_args = 1..,
        conflicts_with = "shell"
    )]
    pub command: Vec<String>,
}

pub fn parse_output_path(raw: &str) -> Result<PathBuf, String> {
    if raw == "-" {
        return Err(
            "`-` does not mean stdout here: stdout carries the commands' output. \
             Name a file; spell it `./-` for one named `-`"
                .to_owned(),
        );
    }
    Ok(PathBuf::from(raw))
}

pub fn parse_duration(raw: &str) -> Result<Duration, String> {
    let invalid =
        || format!("invalid duration `{raw}`: expected seconds, or numbers suffixed s, m, h or d");
    if raw.is_empty() {
        return Err(invalid());
    }
    if let Ok(seconds) = raw.parse::<f64>() {
        return seconds_to_duration(seconds).ok_or_else(invalid);
    }
    let mut total = 0.0;
    let mut number = String::new();
    for c in raw.chars() {
        let unit = match c {
            's' => 1.0,
            'm' => 60.0,
            'h' => 3600.0,
            'd' => 86400.0,
            _ => {
                number.push(c);
                continue;
            }
        };
        let value: f64 = number.parse().map_err(|_| invalid())?;
        total += value * unit;
        number.clear();
    }
    if !number.is_empty() {
        return Err(invalid());
    }
    seconds_to_duration(total).ok_or_else(invalid)
}

fn seconds_to_duration(seconds: f64) -> Option<Duration> {
    (seconds.is_finite() && seconds > 0.0).then(|| Duration::from_secs_f64(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_dash_is_refused_and_any_other_spelling_is_a_path() {
        let err = parse_output_path("-").unwrap_err();
        assert!(err.contains("stdout carries the commands' output"), "{err}");
        assert!(err.contains("`./-`"), "{err}");
        assert_eq!(parse_output_path("./-"), Ok(PathBuf::from("./-")));
        assert_eq!(parse_output_path("run.tsv"), Ok(PathBuf::from("run.tsv")));
    }

    #[test]
    fn a_bare_number_is_seconds() {
        assert_eq!(parse_duration("90"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("0.5"), Ok(Duration::from_millis(500)));
    }

    #[test]
    fn suffixes_scale_and_combine() {
        assert_eq!(parse_duration("2s"), Ok(Duration::from_secs(2)));
        assert_eq!(parse_duration("3m"), Ok(Duration::from_secs(180)));
        assert_eq!(parse_duration("2h"), Ok(Duration::from_secs(7200)));
        assert_eq!(parse_duration("1d"), Ok(Duration::from_secs(86400)));
        assert_eq!(parse_duration("1m30s"), Ok(Duration::from_secs(90)));
    }

    #[test]
    fn malformed_and_non_positive_durations_are_refused() {
        for raw in ["", "s", "1x", "1m30", "-5", "0", "m5"] {
            assert!(parse_duration(raw).is_err(), "{raw:?} should be refused");
        }
    }
}
