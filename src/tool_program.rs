//! The program the library spawns: looked up on `PATH` once, and run from the
//! file that lookup returned.
//!
//! The answer is absolutized, so a relative `PATH` entry names the same file
//! to the child as it did to the lookup, whatever directory the child starts
//! in. The extension on Windows is the one `PATHEXT` matched, never a guess.
//!
//! `rwv each` has its own program type rather than sharing this one: that
//! binary is a plugin that never names this library.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::integration::Tool;

/// A program found on `PATH`: shown as the name it was looked up by, run from
/// the file the lookup returned.
#[derive(Debug, Clone)]
pub(crate) struct ToolProgram {
    name: String,
    found: PathBuf,
}

impl ToolProgram {
    /// The tool an integration declares, or `None` when it is not on `PATH`.
    pub(crate) fn of(tool: Tool) -> Option<Self> {
        Self::find(tool.name())
    }

    /// `name` as the platform resolves it on `PATH` (honoring `PATHEXT` on
    /// Windows), or `None` when nothing there would run.
    pub(crate) fn find(name: &str) -> Option<Self> {
        Self::located(name, which::which(name).ok()?)
    }

    /// A relative answer is read against this process's directory, the one the
    /// lookup that returned it read the entry against.
    fn located(name: &str, found: PathBuf) -> Option<Self> {
        Some(Self {
            name: name.to_string(),
            found: std::path::absolute(found).ok()?,
        })
    }

    /// [`Self::find`] over an explicit search path, so a test is independent of
    /// the host's `PATH`.
    #[cfg(test)]
    pub(crate) fn find_in(name: &str, search: &std::ffi::OsStr, cwd: &Path) -> Option<Self> {
        Self::located(name, which::which_in(name, Some(search), cwd).ok()?)
    }

    /// The file that will run, for a message that has to say which copy.
    pub(crate) fn path(&self) -> &Path {
        &self.found
    }

    /// A command that spawns the file the lookup found.
    pub(crate) fn command(&self) -> Command {
        Command::new(&self.found)
    }
}

impl fmt::Display for ToolProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn search(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    fn write_script(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        std::fs::write(
            dir.join(format!("{name}.cmd")),
            format!("@echo off\r\n{body}\r\n"),
        )
        .unwrap();
    }

    /// The test binary itself, placed in `dir` as the program `name`: a real
    /// executable of the platform's own kind (`.exe` on Windows) that answers
    /// `--list` with the test names.
    fn write_executable(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let file = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        // A link, not a copy, on Unix: a copy's write handle can leak into a
        // child forked by a parallel test, and executing it then fails with
        // "text file busy".
        #[cfg(unix)]
        std::os::unix::fs::symlink(std::env::current_exe().unwrap(), dir.join(file)).unwrap();
        #[cfg(windows)]
        std::fs::copy(std::env::current_exe().unwrap(), dir.join(file)).unwrap();
    }

    #[test]
    fn a_relative_lookup_answer_is_read_from_this_process_not_from_a_child() {
        let program =
            ToolProgram::located("mytool", PathBuf::from("tools").join("mytool")).unwrap();

        assert_eq!(
            program.path(),
            std::env::current_dir()
                .unwrap()
                .join("tools")
                .join("mytool")
        );
    }

    #[test]
    fn the_spawn_is_the_file_the_lookup_found() {
        let tmp = tempfile::tempdir().unwrap();
        write_script(tmp.path(), "mytool", "echo from-mytool");

        let program = ToolProgram::find_in("mytool", &search(&[tmp.path()]), tmp.path()).unwrap();
        let command = program.command();

        assert_eq!(command.get_program(), program.path().as_os_str());
        assert_eq!(program.to_string(), "mytool", "Display is the name");
    }

    #[test]
    fn a_name_on_no_search_entry_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(ToolProgram::find_in("mytool", &search(&[tmp.path()]), tmp.path()).is_none());
    }

    #[test]
    fn a_script_tool_spawns() {
        let tmp = tempfile::tempdir().unwrap();
        write_script(tmp.path(), "scripttool", "echo ran-script");

        let program =
            ToolProgram::find_in("scripttool", &search(&[tmp.path()]), tmp.path()).unwrap();
        let out = program.command().output().unwrap();

        assert!(out.status.success(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("ran-script"),
            "{out:?}"
        );
    }

    /// A tool installed as an executable spawns under the bare name it was
    /// looked up by, whatever extension the platform gives it.
    #[test]
    fn an_executable_tool_spawns() {
        let tmp = tempfile::tempdir().unwrap();
        write_executable(tmp.path(), "exetool");

        let program = ToolProgram::find_in("exetool", &search(&[tmp.path()]), tmp.path()).unwrap();
        let out = program.command().arg("--list").output().unwrap();

        assert!(out.status.success(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("an_executable_tool_spawns"),
            "{out:?}"
        );
    }
}
