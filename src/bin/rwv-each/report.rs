use std::fs::File;
use std::io::{Stderr, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use anyhow::Context;

use crate::output::{Printer, Progress, Stream};
use crate::run::{Finished, Layout};
use crate::schema_url::{EACH_RECORD_SCHEMA_URL, EACH_SCHEMA_URL};
use crate::select::Repo;
use crate::wire::{EachJsonOutput, EachNdjsonRecord, EachOutcomeStatus, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Json {
    Envelope,
    Ndjson,
}

pub struct Reporter<'a> {
    repos: &'a [Repo],
    layout: Layout,
    json: Option<Json>,
    command: String,
    printer: Mutex<Printer<Stdout, Stderr>>,
    joblog: Option<Mutex<File>>,
    results: Option<PathBuf>,
    errors: Mutex<Vec<String>>,
}

pub const JOBLOG_HEADER: &str = "Seq\tRepo\tStarttime\tJobRuntime\tExitval\tSignal\tCommand";

impl<'a> Reporter<'a> {
    pub fn new(
        repos: &'a [Repo],
        layout: Layout,
        json: Option<Json>,
        command: String,
        progress: bool,
        joblog: Option<&Path>,
        results: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let joblog = match joblog {
            Some(path) => {
                let mut file = File::create(path)
                    .with_context(|| format!("cannot write the joblog {}", path.display()))?;
                writeln!(file, "{JOBLOG_HEADER}")?;
                Some(Mutex::new(file))
            }
            None => None,
        };
        let progress = progress.then(|| Progress {
            total: repos.len(),
            ..Progress::default()
        });
        Ok(Self {
            repos,
            layout,
            json,
            command,
            printer: Mutex::new(Printer::new(std::io::stdout(), std::io::stderr(), progress)),
            joblog,
            results,
            errors: Mutex::new(Vec::new()),
        })
    }

    pub fn started(&self, idx: usize) {
        let path = &self.repos[idx].path;
        let mut printer = self.printer.lock().unwrap();
        printer.update_progress(|p| p.running.push(path.clone()));
        if matches!(self.layout, Layout::Inherit | Layout::Plain) {
            printer.header(path);
        }
    }

    pub fn line(&self, prefix_of: Option<usize>, stream: Stream, line: &[u8]) {
        let prefix = prefix_of.map(|idx| self.repos[idx].path.as_str());
        self.printer.lock().unwrap().line(prefix, stream, line);
    }

    pub fn finished(&self, idx: usize, finished: &Finished) {
        let repo = &self.repos[idx];
        let outcome = &finished.outcome;
        {
            let mut printer = self.printer.lock().unwrap();
            printer.update_progress(|p| {
                p.running.retain(|r| r != &repo.path);
                p.done += 1;
                if outcome.status != EachOutcomeStatus::Ok {
                    p.failed += 1;
                }
            });
            if self.layout == Layout::Grouped && finished.started.is_some() {
                printer.header(&repo.path);
                printer.lines(
                    None,
                    finished.grouped.iter().map(|(s, l)| (*s, l.as_slice())),
                );
            }
            if self.json.is_none() {
                if let Some(note) = text_note(outcome, finished.failure.as_deref()) {
                    let prefix = (self.layout == Layout::Prefixed).then_some(repo.path.as_str());
                    let note = match prefix {
                        Some(_) => note,
                        None => format!("[{}] {note}", repo.path),
                    };
                    printer.line(prefix, Stream::Err, note.as_bytes());
                }
            }
            if self.json == Some(Json::Ndjson) {
                let record = EachNdjsonRecord {
                    schema: EACH_RECORD_SCHEMA_URL,
                    outcome,
                };
                let line = serde_json::to_string(&record).expect("an outcome serializes");
                printer.line(None, Stream::Out, line.as_bytes());
            }
        }
        if finished.started.is_some() {
            self.write_joblog(idx, finished);
            self.write_results(repo, finished);
        }
    }

    /// Prints what is owed once every repo is done and returns the exit code.
    pub fn finish(&self, all: &[Finished], interrupted: bool) -> u8 {
        let mut printer = self.printer.lock().unwrap();
        printer.finish_progress();
        match self.json {
            Some(Json::Envelope) => {
                let envelope = EachJsonOutput {
                    schema: EACH_SCHEMA_URL.to_owned(),
                    outcomes: all.iter().map(|f| f.outcome.clone()).collect(),
                };
                let text = serde_json::to_string_pretty(&envelope).expect("an envelope serializes");
                printer.line(None, Stream::Out, text.as_bytes());
            }
            Some(Json::Ndjson) => {
                for finished in all
                    .iter()
                    .filter(|f| f.outcome.status == EachOutcomeStatus::NotRun)
                {
                    let record = EachNdjsonRecord {
                        schema: EACH_RECORD_SCHEMA_URL,
                        outcome: &finished.outcome,
                    };
                    let line = serde_json::to_string(&record).expect("an outcome serializes");
                    printer.line(None, Stream::Out, line.as_bytes());
                }
            }
            None => {
                let outcomes: Vec<&Outcome> = all.iter().map(|f| &f.outcome).collect();
                if let Some(summary) = summary(&outcomes, interrupted) {
                    printer.line(None, Stream::Err, summary.as_bytes());
                }
            }
        }
        let errors = self.errors.lock().unwrap();
        for error in errors.iter() {
            printer.line(None, Stream::Err, format!("error: {error:#}").as_bytes());
        }
        if interrupted {
            130
        } else if !errors.is_empty()
            || all
                .iter()
                .any(|f| f.outcome.status != EachOutcomeStatus::Ok)
        {
            1
        } else {
            0
        }
    }

    fn write_joblog(&self, idx: usize, finished: &Finished) {
        let Some(joblog) = &self.joblog else {
            return;
        };
        let line = joblog_line(idx + 1, &finished.outcome, finished, &self.command);
        let mut file = joblog.lock().unwrap();
        if let Err(e) = writeln!(file, "{line}").and_then(|()| file.flush()) {
            self.errors
                .lock()
                .unwrap()
                .push(format!("cannot write the joblog: {e:#}"));
        }
    }

    fn write_results(&self, repo: &Repo, finished: &Finished) {
        let Some(root) = &self.results else {
            return;
        };
        let dir = root.join(&repo.path);
        let written = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(dir.join("stdout"), &finished.stdout))
            .and_then(|()| std::fs::write(dir.join("stderr"), &finished.stderr))
            .and_then(|()| {
                std::fs::write(
                    dir.join("exit"),
                    format!("{}\n", exit_word(&finished.outcome)),
                )
            });
        if let Err(e) = written {
            self.errors.lock().unwrap().push(format!(
                "cannot write results for {} under {}: {e:#}",
                repo.path,
                root.display()
            ));
        }
    }
}

fn text_note(outcome: &Outcome, failure: Option<&str>) -> Option<String> {
    match outcome.status {
        EachOutcomeStatus::NotPresent => {
            Some("not present on disk; run `rwv fetch` to restore it".to_owned())
        }
        EachOutcomeStatus::TimedOut => Some("timed out; its process tree was killed".to_owned()),
        _ => failure.map(str::to_owned),
    }
}

pub fn exit_word(outcome: &Outcome) -> String {
    match (outcome.status, outcome.exit_code, outcome.signal) {
        (EachOutcomeStatus::Ok, _, _) => "ok".to_owned(),
        (EachOutcomeStatus::TimedOut, _, _) => "timed-out".to_owned(),
        (_, Some(code), _) => code.to_string(),
        (_, None, Some(signal)) => format!("signal {signal}"),
        (status, None, None) => serde_json::to_value(status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .expect("a status serializes to a string"),
    }
}

fn joblog_line(seq: usize, outcome: &Outcome, finished: &Finished, command: &str) -> String {
    let start = finished
        .started
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_secs_f64());
    let runtime = outcome.duration_ms.unwrap_or(0) as f64 / 1000.0;
    format!(
        "{seq}\t{}\t{start:.3}\t{runtime:.3}\t{}\t{}\t{command}",
        outcome.path,
        outcome.exit_code.unwrap_or(-1),
        outcome.signal.unwrap_or(0),
    )
}

pub fn summary(outcomes: &[&Outcome], interrupted: bool) -> Option<String> {
    let bad: Vec<&&Outcome> = outcomes
        .iter()
        .filter(|o| o.status != EachOutcomeStatus::Ok)
        .collect();
    if bad.is_empty() && !interrupted {
        return None;
    }
    let mut text = if interrupted {
        format!(
            "rwv each: interrupted; {} of {} repos did not succeed",
            bad.len(),
            outcomes.len()
        )
    } else {
        format!(
            "rwv each: {} of {} repos did not succeed",
            bad.len(),
            outcomes.len()
        )
    };
    for outcome in bad {
        let why = match (outcome.status, outcome.exit_code, outcome.signal) {
            (EachOutcomeStatus::TimedOut, _, _) => "timed out".to_owned(),
            (EachOutcomeStatus::NotRun, _, _) => "not run".to_owned(),
            (EachOutcomeStatus::NotPresent, _, _) => {
                "not present on disk; run `rwv fetch`".to_owned()
            }
            (_, Some(code), _) => format!("exit {code}"),
            (_, None, Some(signal)) => format!("killed by signal {signal}"),
            (_, None, None) => "failed to run".to_owned(),
        };
        text.push_str(&format!("\n  {}: {why}", outcome.path));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Role;

    fn outcome(path: &str, status: EachOutcomeStatus, exit_code: Option<i32>) -> Outcome {
        Outcome {
            path: path.into(),
            role: Role::Owned,
            status,
            exit_code,
            signal: None,
            duration_ms: Some(1500),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[test]
    fn a_clean_run_has_no_summary() {
        let ok = outcome("a", EachOutcomeStatus::Ok, Some(0));
        assert_eq!(summary(&[&ok], false), None);
    }

    #[test]
    fn the_summary_lists_every_unsuccessful_repo_in_the_order_given() {
        let all = [
            outcome("a", EachOutcomeStatus::Failed, Some(3)),
            outcome("b", EachOutcomeStatus::Ok, Some(0)),
            outcome("c", EachOutcomeStatus::TimedOut, None),
            outcome("d", EachOutcomeStatus::NotPresent, None),
            outcome("e", EachOutcomeStatus::NotRun, None),
        ];
        let refs: Vec<&Outcome> = all.iter().collect();
        assert_eq!(
            summary(&refs, false).unwrap(),
            "rwv each: 4 of 5 repos did not succeed\n  a: exit 3\n  c: timed out\n  d: not present on disk; run `rwv fetch`\n  e: not run"
        );
    }

    #[test]
    fn exit_words_name_the_outcome() {
        assert_eq!(
            exit_word(&outcome("a", EachOutcomeStatus::Ok, Some(0))),
            "ok"
        );
        assert_eq!(
            exit_word(&outcome("a", EachOutcomeStatus::Failed, Some(2))),
            "2"
        );
        assert_eq!(
            exit_word(&outcome("a", EachOutcomeStatus::TimedOut, None)),
            "timed-out"
        );
        let mut signaled = outcome("a", EachOutcomeStatus::Failed, None);
        signaled.signal = Some(9);
        assert_eq!(exit_word(&signaled), "signal 9");
    }
}
