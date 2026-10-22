//! Path arguments as argv states them, and the directory they are read from.
//!
//! A relative path on the command line names a location from the directory
//! rwv runs in (or the `-C`/`-w` directory), never from any other base. An
//! [`Operand`] is that argv text: nothing outside this module tree can turn it
//! into a path, because the one conversion, `Operand::locate`, takes the
//! [`InvocationDir`] it is read from, and only `crate::cli` can mint one.
//! Dispatch locates every path argument before it calls a handler, so a
//! handler holds absolute paths and has no base left to get wrong.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::manifest::WorkweaveName;
use crate::naming::WorkweaveNameError;
use crate::path_spelling::{plain_path, resolve};
use crate::sync::SyncSource;

/// The directory a relative path argument is read from: the process cwd, the
/// `-C` directory, or the `-w` workweave's recorded directory, always resolved.
#[derive(Debug)]
pub struct InvocationDir(PathBuf);

impl InvocationDir {
    pub(in crate::cli) fn resolved(dir: &Path) -> std::io::Result<Self> {
        resolve(dir).map(Self)
    }

    /// For the verbs whose subject is the directory itself: the resolution
    /// walk, `init`'s bootstrap target, a plugin's cwd.
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// A path argument as argv spelled it.
#[derive(Clone, Debug)]
pub struct Operand(OsString);

impl From<OsString> for Operand {
    fn from(text: OsString) -> Self {
        Self(text)
    }
}

impl Operand {
    /// The location this argument names, read from `base`. An absolute
    /// argument, by this host's rule, names itself, in the plain spelling.
    pub(in crate::cli) fn locate(self, base: &InvocationDir) -> PathBuf {
        let path = PathBuf::from(self.0);
        if path.is_absolute() {
            plain_path(&path).into_owned()
        } else {
            base.0.join(path)
        }
    }

    pub(in crate::cli) fn text(&self) -> &std::ffi::OsStr {
        &self.0
    }
}

/// A workspace named on the command line, as argv spelled it: `primary`, a
/// bare workweave name, or a path. Path-like means a separator, a leading `.`,
/// or an absolute spelling; any other bare word names a workweave.
#[derive(Clone, Debug)]
pub enum SourceArg {
    Primary,
    Workweave(WorkweaveName),
    Path(Operand),
}

impl FromStr for SourceArg {
    type Err = WorkweaveNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "primary" {
            return Ok(Self::Primary);
        }
        if s.contains('/')
            || s.contains(std::path::MAIN_SEPARATOR)
            || s.starts_with('.')
            || Path::new(s).is_absolute()
        {
            return Ok(Self::Path(Operand(OsString::from(s))));
        }
        Ok(Self::Workweave(WorkweaveName::new(s)?))
    }
}

impl SourceArg {
    pub(in crate::cli) fn locate(self, base: &InvocationDir) -> SyncSource {
        match self {
            Self::Primary => SyncSource::Primary,
            Self::Workweave(name) => SyncSource::Workweave(name),
            Self::Path(operand) => SyncSource::Path(operand.locate(base)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> InvocationDir {
        InvocationDir(PathBuf::from(if cfg!(windows) {
            r"C:\base"
        } else {
            "/base"
        }))
    }

    #[test]
    fn a_relative_operand_is_read_from_the_invocation_dir() {
        let located = Operand(OsString::from("sub/dir")).locate(&base());
        assert_eq!(located, base().0.join("sub/dir"));
    }

    #[test]
    fn an_absolute_operand_names_itself() {
        let absolute = if cfg!(windows) {
            r"D:\elsewhere"
        } else {
            "/elsewhere"
        };
        let located = Operand(OsString::from(absolute)).locate(&base());
        assert_eq!(located, PathBuf::from(absolute));
    }

    #[test]
    fn a_source_parses_primary_a_workweave_name_or_a_path() {
        assert!(matches!(
            "primary".parse::<SourceArg>(),
            Ok(SourceArg::Primary)
        ));
        assert!(matches!(
            "hotfix".parse::<SourceArg>(),
            Ok(SourceArg::Workweave(name)) if name.as_str() == "hotfix"
        ));
        for path_like in [".workweaves/foo--bar", "./foo", "."] {
            assert!(
                matches!(path_like.parse::<SourceArg>(), Ok(SourceArg::Path(_))),
                "{path_like} is path-like"
            );
        }
        let absolute = std::env::temp_dir().join("some/path");
        assert!(matches!(
            absolute.to_str().unwrap().parse::<SourceArg>(),
            Ok(SourceArg::Path(_))
        ));
    }

    #[test]
    fn a_bare_word_that_is_no_workweave_name_refuses() {
        assert!(matches!(
            "proj--feat--v2".parse::<SourceArg>(),
            Err(WorkweaveNameError::AmbiguousDelimiter(_))
        ));
    }

    #[test]
    fn a_path_source_is_located_from_the_invocation_dir() {
        let located = "./foo".parse::<SourceArg>().unwrap().locate(&base());
        assert_eq!(located, SyncSource::Path(base().0.join("./foo")));
    }

    #[cfg(windows)]
    #[test]
    fn an_absolute_verbatim_operand_is_respelled_plain() {
        let located = Operand(OsString::from(r"\\?\C:\x")).locate(&base());
        assert_eq!(located, PathBuf::from(r"C:\x"));
    }
}
