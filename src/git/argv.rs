//! The argument vector of a `git` invocation, and the one `Command` that
//! carries it.
//!
//! git's argv is text, and a location is not: `&Path` converts to text in one
//! function, [`GitArg::from_path`]. [`GitCommand`] keeps its `Command` private
//! and takes arguments only as an [`Argv`], whose slots take text
//! (`AsRef<str>`, which a `&Path` is not) or a [`GitArg`]. A path handed to
//! either slot without the conversion is a type error.
//!
//! This module names nothing outside `std` and `dunce`, so a compile probe can
//! include the file on its own.

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

/// A path in the form git is told it.
///
/// Only [`GitArg::from_path`] makes one. git refuses an argument in the
/// Windows `\\?\` extended-length spelling — `git worktree add //?/C:/…` fails
/// outright — and a weave root read from a record can carry it, so the prefix
/// comes off there.
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
    /// The one conversion from a path to text for git.
    ///
    /// `dunce::simplified` drops the extended-length prefix only where Windows
    /// itself accepts the short form — not over the legacy length limit, not a
    /// UNC share, not a reserved device name — and is the identity on every
    /// other platform, so a caller states no `cfg`.
    ///
    /// `subject` names the path to the operator when it is not valid UTF-8.
    pub(super) fn from_path(path: &Path, subject: &str) -> Result<GitArg, NotUtf8> {
        dunce::simplified(path)
            .to_str()
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
pub(super) struct GitCommand {
    command: Command,
}

impl GitCommand {
    pub(super) fn new() -> GitCommand {
        let mut command = Command::new("git");
        for var in GIT_ENV_VARS {
            command.env_remove(var);
        }
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

    /// The `Command` itself, which takes any argument. The door out of this
    /// module, for a caller that must hand a finished command to code that
    /// spawns it.
    pub(super) fn into_command(self) -> Command {
        self.command
    }
}

#[cfg(test)]
mod tests {
    //! What [`GitArg::from_path`] does to the extended-length prefix.
    //!
    //! The prefix exists only on Windows, so the assertion that matters is
    //! compiled everywhere and executed on one platform — the advisory Windows
    //! workflow is the only thing that runs it. The other direction is what
    //! every platform can state, and it is the half that goes wrong if this
    //! seam ever reaches for `canonicalize` instead: a path handed to git must
    //! come back the path that was handed in, not a resolved one.

    use super::*;

    fn told(path: &str) -> String {
        GitArg::from_path(Path::new(path), "path")
            .unwrap()
            .as_str()
            .to_owned()
    }

    #[test]
    fn a_path_with_no_extended_length_prefix_is_handed_over_unchanged() {
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

    #[cfg(windows)]
    #[test]
    fn a_drive_letter_path_loses_the_prefix_git_refuses() {
        assert_eq!(told(r"\\?\C:\weave\member"), r"C:\weave\member");
    }
}
