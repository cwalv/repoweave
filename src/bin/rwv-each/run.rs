use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;

use crate::output::Stream;
use crate::report::Reporter;
use crate::select::Repo;
use crate::tree;
use crate::wire::{EachOutcomeStatus, Outcome};

/// Interrupts received so far; the Ctrl-C handler only ever increments it.
pub static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);

pub fn interrupted() -> bool {
    INTERRUPTS.load(Ordering::SeqCst) > 0
}

const POLL: Duration = Duration::from_millis(10);

/// How each repo's output reaches the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// The command gets this process's stdin, stdout and stderr, and the
    /// terminal, after a `[<path>]` header.
    Inherit,
    /// Each line, as it arrives, prefixed `[<path>] `.
    Prefixed,
    /// A `[<path>]` header, then each line unprefixed as it arrives.
    Plain,
    /// Everything a repo printed, under a `[<path>]` header, once it is done.
    Grouped,
    /// Nothing; the output is only captured.
    Silent,
}

#[derive(Debug, Clone)]
pub enum CommandSpec {
    Argv { program: Program, args: Vec<String> },
    Shell(String),
}

#[derive(Debug, Clone)]
pub enum Program {
    /// Found on `PATH` once, before any repo runs.
    Resolved(PathBuf),
    /// Spelled with a directory, so relative to each repo's checkout.
    InRepo(PathBuf),
}

impl CommandSpec {
    pub fn new(command: &[String], shell: Option<&str>) -> anyhow::Result<Self> {
        if let Some(shell) = shell {
            return Ok(Self::Shell(shell.to_owned()));
        }
        let (program, args) = command
            .split_first()
            .context("no command given to run in each repo")?;
        let path = Path::new(program);
        let program = if path.components().count() > 1 || path.is_absolute() {
            Program::InRepo(path.to_path_buf())
        } else {
            Program::Resolved(
                which::which(program)
                    .with_context(|| format!("`{program}` was not found on PATH"))?,
            )
        };
        Ok(Self::Argv {
            program,
            args: args.to_vec(),
        })
    }

    pub fn display(&self) -> String {
        match self {
            Self::Shell(line) => line.clone(),
            Self::Argv { program, args } => {
                let program = match program {
                    Program::Resolved(p) => p.file_name().map_or_else(
                        || p.to_string_lossy().into_owned(),
                        |n| n.to_string_lossy().into_owned(),
                    ),
                    Program::InRepo(p) => p.to_string_lossy().into_owned(),
                };
                std::iter::once(program.as_str())
                    .chain(args.iter().map(String::as_str))
                    .map(display_word)
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        }
    }

    fn command_in(&self, repo: &Repo) -> Command {
        let mut command = match self {
            Self::Argv { program, args } => {
                let program = match program {
                    Program::Resolved(p) => p.clone(),
                    Program::InRepo(p) => repo.dir.join(p),
                };
                let mut command = Command::new(program);
                command.args(args);
                command
            }
            Self::Shell(line) => shell_command(line),
        };
        command
            .current_dir(&repo.dir)
            .env("RWV_REPO", &repo.path)
            .env("RWV_REPO_ROLE", &repo.role)
            .env("RWV_REPO_PATH", &repo.dir);
        command
    }
}

#[cfg(unix)]
fn shell_command(line: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(line);
    command
}

#[cfg(windows)]
fn shell_command(line: &str) -> Command {
    use std::os::windows::process::CommandExt;
    // `cmd /S /C "<line>"` strips exactly the outer quotes and runs the rest
    // as typed; std's argv quoting follows the C runtime's rules, which cmd
    // does not, so the line is passed raw.
    let mut command = Command::new("cmd.exe");
    command
        .args(["/D", "/S", "/C"])
        .raw_arg(format!("\"{line}\""));
    command
}

fn display_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_alphanumeric() || "-_./:=@%+,".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

pub struct Finished {
    pub outcome: Outcome,
    pub started: Option<SystemTime>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub grouped: Vec<(Stream, Vec<u8>)>,
    /// Why the command could not be started or waited on.
    pub failure: Option<String>,
}

impl Finished {
    pub fn without_run(repo: &Repo, status: EachOutcomeStatus) -> Self {
        Self {
            outcome: Outcome {
                path: repo.path.clone(),
                role: repo.role.clone(),
                status,
                exit_code: None,
                signal: None,
                duration_ms: None,
                stdout: String::new(),
                stderr: String::new(),
            },
            started: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            grouped: Vec::new(),
            failure: None,
        }
    }
}

pub struct Runner<'a> {
    pub repos: &'a [Repo],
    pub spec: &'a CommandSpec,
    pub layout: Layout,
    pub capture: bool,
    pub timeout: Option<Duration>,
    pub fail_fast: bool,
    pub jobs: usize,
    pub reporter: &'a Reporter<'a>,
}

impl Runner<'_> {
    /// Every repo's outcome, in manifest order; a repo never started is
    /// `not-run`.
    pub fn run_all(&self) -> Vec<Finished> {
        let next = AtomicUsize::new(0);
        let halted = AtomicBool::new(false);
        let done: Mutex<Vec<Option<Finished>>> =
            Mutex::new((0..self.repos.len()).map(|_| None).collect());
        thread::scope(|s| {
            for _ in 0..self.jobs.min(self.repos.len()) {
                s.spawn(|| loop {
                    if halted.load(Ordering::SeqCst) || interrupted() {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(repo) = self.repos.get(i) else {
                        break;
                    };
                    let finished = self.run_one(i, repo);
                    if self.fail_fast && finished.outcome.status != EachOutcomeStatus::Ok {
                        halted.store(true, Ordering::SeqCst);
                    }
                    self.reporter.finished(i, &finished);
                    done.lock().unwrap()[i] = Some(finished);
                });
            }
        });
        done.into_inner()
            .unwrap()
            .into_iter()
            .zip(self.repos)
            .map(|(f, repo)| {
                f.unwrap_or_else(|| Finished::without_run(repo, EachOutcomeStatus::NotRun))
            })
            .collect()
    }

    fn run_one(&self, idx: usize, repo: &Repo) -> Finished {
        if !repo.present {
            return Finished::without_run(repo, EachOutcomeStatus::NotPresent);
        }
        let inherit = self.layout == Layout::Inherit;
        let mut command = self.spec.command_in(repo);
        if inherit {
            command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        } else {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        }

        self.reporter.started(idx);
        let started = SystemTime::now();
        let clock = Instant::now();
        let mut child = match tree::spawn_tree(command) {
            Ok(child) => child,
            Err(e) => {
                let mut finished = Finished::without_run(repo, EachOutcomeStatus::Failed);
                finished.started = Some(started);
                finished.outcome.duration_ms = Some(millis(clock.elapsed()));
                finished.failure = Some(format!("failed to start: {e:#}"));
                return finished;
            }
        };
        let foreground = if inherit {
            tree::Foreground::hand_to(child.id())
        } else {
            None
        };

        let pipes = [
            (Stream::Out, child.stdout().take().map(boxed)),
            (Stream::Err, child.stderr().take().map(boxed)),
        ];
        let captured = [Mutex::new(Vec::new()), Mutex::new(Vec::new())];
        let grouped = Mutex::new(Vec::new());

        let mut status: Option<ExitStatus> = None;
        let mut wait_error: Option<std::io::Error> = None;
        let mut timed_out = false;
        thread::scope(|s| {
            let readers: Vec<_> = pipes
                .into_iter()
                .filter_map(|(stream, pipe)| pipe.map(|pipe| (stream, pipe)))
                .map(|(stream, pipe)| {
                    let sink = Sink {
                        runner: self,
                        idx,
                        stream,
                        captured: &captured[stream as usize],
                        grouped: &grouped,
                    };
                    s.spawn(move || sink.pump(pipe))
                })
                .collect();

            let mut delivered = 0;
            let mut swept = false;
            loop {
                if status.is_none() && wait_error.is_none() {
                    match child.try_wait() {
                        Ok(s) => status = s,
                        Err(e) => {
                            let _ = tree::kill_tree(child.as_mut());
                            wait_error = Some(e);
                        }
                    }
                }
                let exited = status.is_some() || wait_error.is_some();
                if exited && readers.iter().all(|r| r.is_finished()) {
                    break;
                }
                // A non-interactive shell starts background commands with
                // SIGINT ignored, so they outlive an interrupt that ended the
                // command itself, holding its output open.
                if exited && delivered > 0 && !swept {
                    let _ = tree::kill_tree(child.as_mut());
                    swept = true;
                }
                let interrupts = INTERRUPTS.load(Ordering::SeqCst);
                if interrupts > delivered {
                    let _ = tree::interrupt_tree(child.as_mut(), interrupts);
                    delivered = interrupts;
                }
                if let Some(limit) = self.timeout {
                    if !timed_out && clock.elapsed() >= limit {
                        let _ = tree::kill_tree(child.as_mut());
                        timed_out = true;
                    }
                }
                thread::sleep(POLL);
            }
        });
        drop(foreground);

        let duration = clock.elapsed();
        if let Some(status) = &status {
            if inherit && tree::died_of_interrupt(status) {
                INTERRUPTS.fetch_max(1, Ordering::SeqCst);
            }
        }
        let [stdout, stderr] = captured.map(|c| c.into_inner().unwrap());
        let outcome_status = if timed_out {
            EachOutcomeStatus::TimedOut
        } else if status.as_ref().is_some_and(ExitStatus::success) {
            EachOutcomeStatus::Ok
        } else {
            EachOutcomeStatus::Failed
        };
        let failure = wait_error.map(|e| format!("failed while waiting for it: {e:#}"));
        let mut stderr_text = String::from_utf8_lossy(&stderr).into_owned();
        if let Some(failure) = &failure {
            stderr_text.push_str(failure);
        }
        Finished {
            outcome: Outcome {
                path: repo.path.clone(),
                role: repo.role.clone(),
                status: outcome_status,
                exit_code: status.as_ref().and_then(ExitStatus::code),
                signal: status.as_ref().and_then(tree::exit_signal),
                duration_ms: Some(millis(duration)),
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: stderr_text,
            },
            started: Some(started),
            stdout,
            stderr,
            grouped: grouped.into_inner().unwrap(),
            failure,
        }
    }
}

fn boxed(pipe: impl Read + Send + 'static) -> Box<dyn Read + Send> {
    Box::new(pipe)
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

struct Sink<'a> {
    runner: &'a Runner<'a>,
    idx: usize,
    stream: Stream,
    captured: &'a Mutex<Vec<u8>>,
    grouped: &'a Mutex<Vec<(Stream, Vec<u8>)>>,
}

impl Sink<'_> {
    fn pump(self, pipe: Box<dyn Read + Send>) {
        let mut reader = BufReader::new(pipe);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => self.deliver(&line),
            }
        }
    }

    fn deliver(&self, line: &[u8]) {
        if self.runner.capture {
            self.captured.lock().unwrap().extend_from_slice(line);
        }
        match self.runner.layout {
            Layout::Prefixed => self.runner.reporter.line(Some(self.idx), self.stream, line),
            Layout::Plain => self.runner.reporter.line(None, self.stream, line),
            Layout::Grouped => self
                .grouped
                .lock()
                .unwrap()
                .push((self.stream, line.to_vec())),
            Layout::Inherit | Layout::Silent => {}
        }
    }
}

pub fn rwv_program() -> OsString {
    let sibling = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(format!("rwv{}", std::env::consts::EXE_SUFFIX)));
    match sibling {
        Some(path) if path.is_file() => path.into_os_string(),
        _ => "rwv".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_quotes_only_words_a_shell_would_split_or_expand() {
        let spec = CommandSpec::Argv {
            program: Program::InRepo(PathBuf::from("./build.sh")),
            args: vec![
                "--out=dir".into(),
                "two words".into(),
                "it's".into(),
                "$HOME".into(),
            ],
        };
        assert_eq!(
            spec.display(),
            r#"./build.sh --out=dir 'two words' 'it'\''s' '$HOME'"#
        );
    }

    #[test]
    fn a_program_spelled_with_a_directory_runs_relative_to_each_repo() {
        let spec = CommandSpec::new(&["./scripts/test.sh".into()], None).unwrap();
        assert!(matches!(
            spec,
            CommandSpec::Argv {
                program: Program::InRepo(_),
                ..
            }
        ));
    }

    #[test]
    fn a_bare_program_name_missing_from_path_is_refused_before_anything_runs() {
        let err = CommandSpec::new(&["rwv-each-no-such-program".into()], None).unwrap_err();
        assert!(err.to_string().contains("not found on PATH"), "{err}");
    }
}
