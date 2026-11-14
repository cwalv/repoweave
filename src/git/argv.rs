//! The argument vector of a `git` invocation, and the one `Command` that
//! carries it.
//!
//! git's argv is text, and a location is not: `&Path` converts to text in one
//! function, [`GitArg::from_path`]. [`GitCommand`] keeps its `Command` private
//! and takes arguments only as an [`Argv`], whose slots take text
//! (`AsRef<str>`, which a `&Path` is not) or a [`GitArg`]. A path handed to
//! either slot without the conversion is a type error.
//!
//! This module names nothing outside `std`, so a compile probe can include the
//! file on its own.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};

/// `GIT_*` environment variables that git itself sets for hooks (and that
/// other tooling sometimes sets) which silently misdirect any subprocess
/// `git` invocation if inherited.
const GIT_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// The locale every git runs in. Nothing the backend decides rests on git's
/// wording, which changes between releases as well as between languages;
/// the untranslated messages are what an operator reading a failure rwv
/// carries can search for.
const MESSAGE_LOCALE: &str = "C";

/// A path in the form git is told it: its UTF-8 text, unchanged.
///
/// Only [`GitArg::from_path`] makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GitArg {
    text: String,
}

/// A path that has no UTF-8 spelling for git to be told.
#[derive(Debug)]
pub(super) struct NotUtf8 {
    subject: String,
    path: PathBuf,
}

impl NotUtf8 {
    pub(super) fn subject(&self) -> &str {
        &self.subject
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl GitArg {
    /// The one conversion from a path to text for git: a path that is not
    /// valid UTF-8 is refused, naming it as `subject`, rather than handed to
    /// git in a lossy spelling that names some other path.
    pub(super) fn from_path(path: &Path, subject: &str) -> Result<GitArg, NotUtf8> {
        path.to_str()
            .map(|text| GitArg {
                text: text.to_owned(),
            })
            .ok_or_else(|| NotUtf8 {
                subject: subject.to_owned(),
                path: path.to_path_buf(),
            })
    }

    pub(super) fn as_str(&self) -> &str {
        &self.text
    }
}

/// The words after `git`, in order.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Argv {
    words: Vec<String>,
}

impl Argv {
    pub(super) fn text(mut self, word: impl AsRef<str>) -> Argv {
        self.words.push(word.as_ref().to_owned());
        self
    }

    pub(super) fn path(mut self, path: &GitArg) -> Argv {
        self.words.push(path.text.clone());
        self
    }

    pub(super) fn words(&self) -> &[String] {
        &self.words
    }

    pub(super) fn into_words(self) -> Vec<String> {
        self.words
    }
}

impl fmt::Debug for Argv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.words.fmt(f)
    }
}

impl<I> From<I> for Argv
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    fn from(words: I) -> Argv {
        Argv {
            words: words
                .into_iter()
                .map(|word| word.as_ref().to_owned())
                .collect(),
        }
    }
}

impl From<&Argv> for Argv {
    fn from(argv: &Argv) -> Argv {
        argv.clone()
    }
}

/// A `git` process to be run, with the `GIT_*` environment that would
/// redirect it removed.
///
/// rwv resolves its own paths; inheriting those variables from the
/// surrounding process (a `pre-push` hook, another git invocation) makes
/// `git` operate on the wrong repository regardless of the working directory
/// set here.
/// Whether `dir` is a repository's own directory whose `core.worktree` names
/// a directory that is gone. A submodule's store keeps naming its checkout
/// after the checkout is removed, and git refuses to run in it then; with the
/// store standing in as its own work tree, what reads only refs and objects
/// runs there.
fn names_a_gone_work_tree(dir: &Path) -> bool {
    if !dir.join("HEAD").is_file() || !dir.join("objects").is_dir() {
        return false;
    }
    let Ok(config) = std::fs::read_to_string(dir.join("config")) else {
        return false;
    };
    let mut in_core = false;
    config.lines().map(str::trim).any(|line| {
        if line.starts_with('[') {
            in_core = line.eq_ignore_ascii_case("[core]");
            return false;
        }
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        in_core
            && key.trim().eq_ignore_ascii_case("worktree")
            && !dir.join(value.trim().trim_matches('"')).exists()
    })
}

pub(super) struct GitCommand {
    command: Command,
}

impl GitCommand {
    pub(super) fn new() -> GitCommand {
        let mut command = Command::new("git");
        for var in GIT_ENV_VARS {
            command.env_remove(var);
        }
        #[cfg(test)]
        if speech::translated() {
            speech::translate(&mut command);
            return GitCommand { command };
        }
        command.env("LC_ALL", MESSAGE_LOCALE);
        GitCommand { command }
    }

    pub(super) fn args(&mut self, args: impl Into<Argv>) -> &mut GitCommand {
        self.command.args(args.into().words());
        self
    }

    pub(super) fn env(&mut self, key: &str, value: &str) -> &mut GitCommand {
        self.command.env(key, value);
        self
    }

    pub(super) fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut GitCommand {
        let dir = dir.as_ref();
        if names_a_gone_work_tree(dir) {
            self.command.env("GIT_DIR", dir).env("GIT_WORK_TREE", dir);
        }
        self.command.current_dir(dir);
        self
    }

    pub(super) fn stdin(&mut self, stdin: Stdio) -> &mut GitCommand {
        self.command.stdin(stdin);
        self
    }

    pub(super) fn stdout(&mut self, stdout: Stdio) -> &mut GitCommand {
        self.command.stdout(stdout);
        self
    }

    pub(super) fn stderr(&mut self, stderr: Stdio) -> &mut GitCommand {
        self.command.stderr(stderr);
        self
    }

    pub(super) fn output(&mut self) -> io::Result<Output> {
        self.command.output()
    }

    pub(super) fn status(&mut self) -> io::Result<ExitStatus> {
        self.command.status()
    }

    pub(super) fn spawn(&mut self) -> io::Result<Child> {
        self.command.spawn()
    }

    /// The `Command` itself, which takes any argument. Only a test fixture
    /// that builds the state the code under test reads has a use for it.
    #[cfg(test)]
    pub(super) fn into_command(self) -> Command {
        self.command
    }
}

/// The language a test has git speak. Every git a thread spawns while
/// [`speech::in_translated_git`] runs is spawned without the locale pin and
/// asked for German, so a decision that rests on git's wording fails there.
#[cfg(test)]
pub(crate) mod speech {
    use std::cell::Cell;
    use std::process::Command;

    const LANGUAGE: &str = "de";

    /// glibc ignores `LANGUAGE` while the messages locale is `C`.
    const MESSAGES_LOCALE: &str = "en_US.UTF-8";

    thread_local! {
        static TRANSLATED: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn translated() -> bool {
        TRANSLATED.with(Cell::get)
    }

    pub(super) fn translate(command: &mut Command) {
        command
            .env_remove("LC_ALL")
            .env("LANGUAGE", LANGUAGE)
            .env("LC_MESSAGES", MESSAGES_LOCALE);
    }

    /// Run `body` with every git this thread spawns unpinned and asked for
    /// German. A host whose git has no German says so on stderr, and `body`
    /// still runs without the pin.
    pub(crate) fn in_translated_git<T>(body: impl FnOnce() -> T) -> T {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                TRANSLATED.with(|t| t.set(false));
            }
        }
        TRANSLATED.with(|t| t.set(true));
        let _restore = Restore;
        if !speaks_german() {
            eprintln!("this host's git has no German: the run is unpinned but untranslated");
        }
        body()
    }

    fn speaks_german() -> bool {
        let mut probe = Command::new("git");
        translate(&mut probe);
        probe
            .args(["rev-parse", "--verify", "no-such-revision-here"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .is_ok_and(|out| !out.stderr.starts_with(b"fatal:"))
    }
}

#[cfg(test)]
mod tests {
    //! A path handed to git is the path that was handed in, not a resolved
    //! one, and a path that is not valid UTF-8 is refused rather than
    //! respelled.

    use super::*;

    fn told(path: &str) -> String {
        GitArg::from_path(Path::new(path), "path")
            .unwrap()
            .as_str()
            .to_owned()
    }

    #[test]
    fn a_path_is_handed_over_unchanged() {
        let absolute = if cfg!(windows) {
            r"C:\weave\member"
        } else {
            "/weave/member"
        };
        assert_eq!(told(absolute), absolute);
        assert_eq!(told("nested/entry"), "nested/entry");
    }

    #[test]
    fn every_variable_that_redirects_git_is_removed_from_its_environment() {
        let command = GitCommand::new().into_command();
        let removed: Vec<&std::ffi::OsStr> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name)
            .collect();
        assert!(!GIT_ENV_VARS.is_empty());
        for var in GIT_ENV_VARS {
            assert!(
                removed.contains(&std::ffi::OsStr::new(var)),
                "{var} is inherited by the git this command spawns"
            );
        }
    }

    #[test]
    fn git_speaks_untranslated_whatever_language_the_operator_asks_for() {
        let out = GitCommand::new()
            .args(["rev-parse", "--verify", "no-such-revision-here"])
            .env("LANGUAGE", "de")
            .env("LC_MESSAGES", "en_US.UTF-8")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success());
        assert!(stderr.starts_with("fatal: "), "{stderr}");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_refused_by_name() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/weave/\xff"));
        let refused = GitArg::from_path(path, "worktree path").unwrap_err();
        assert_eq!(refused.subject(), "worktree path");
        assert_eq!(refused.path(), path);
    }
}
