//! The spellings a path takes on its way out of rwv.
//!
//! Two of them answer for an absolute path. Both drop the Windows verbatim
//! (`\\?\`) prefix, which a canonicalized weave root carries and which nothing
//! outside rwv wants. They differ on the separator, and the difference is a
//! difference of audience:
//!
//! - [`wire_path`] answers a **program**. `--json` documents shell
//!   composition as its consumer (`jq -r … | xargs -I {} git -C {}` is the
//!   shipped recipe), and a backslash-separated value cannot survive bare
//!   `xargs` — the measured failure. `C:/Users/…` carries no `xargs`-active
//!   bytes and is accepted by git, cmd and PowerShell alike.
//! - [`operator_path`] answers a **person**: the native spelling, which is
//!   what pastes into their own shell and file manager. `\\?\C:\…` in a
//!   refusal is an internal spelling leaking out.
//!
//! The third, [`weave_relative`], answers for a path *inside* a weave, where
//! the audience question does not arise: a location rwv keeps a name for is
//! spelled the way rwv spells it.
//!
//! On Unix all three are the identity — `dunce::simplified` is a no-op off
//! Windows, and no separator is rewritten, because `\` is an ordinary
//! character in a Unix filename and rewriting it would corrupt the path
//! rather than respell it. So the wire output on Unix is byte-for-byte what
//! it was before any of them existed.

use std::path::Path;

/// The spelling a program receives: verbatim prefix dropped, `/`-separated on
/// every platform.
///
/// Every `--json` field whose value is a filesystem absolute path is built
/// from this. Two surfaces that must agree consume one function's result.
pub fn wire_path(path: &Path) -> String {
    let simplified = dunce::simplified(path);
    #[cfg(windows)]
    {
        simplified.to_string_lossy().replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        simplified.to_string_lossy().into_owned()
    }
}

/// The spelling a person receives: verbatim prefix dropped, native separators
/// kept.
///
/// Every absolute path printed into operator text — a refusal, a status
/// header, a materialize list — is built from this. Identities in that same
/// text keep rendering through their own `Display`; this is for locations, not
/// for names.
pub fn operator_path(path: &Path) -> String {
    dunce::simplified(path).to_string_lossy().into_owned()
}

/// The spelling a location inside a weave takes: `/`-separated everywhere.
///
/// Such a path is a name rwv already keeps, and rwv keeps it with slashes —
/// manifest keys, surfacing declarations, `rwv.lock` entries. Rendered
/// natively it comes back to the operator as a second spelling of a name they
/// hold, and one they cannot paste into the file it came from. An absolute
/// path takes [`operator_path`] instead: that names a location on their host,
/// which rwv has no vocabulary for.
///
/// Unix keeps the bytes. A `\` there is a character in a filename, so
/// rewriting one renames the file rather than respelling it.
pub fn weave_relative(path: &Path) -> String {
    #[cfg(windows)]
    {
        path.to_string_lossy().replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
}

/// The mint applied at the serde boundary, for a wire field that stays a
/// `PathBuf` because something inside the binary still reads it as a path.
///
/// serde's own `PathBuf` impl publishes whatever the host spelled, so a field
/// reaching `--json` as a `PathBuf` needs this attached to it; a field whose
/// only consumer is the wire holds a `String` minted at construction instead.
pub fn serialize_wire_path<S: serde::Serializer>(
    path: &Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&wire_path(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// On Unix the wire spelling is the identity, which is the whole reason
    /// the decision costs nothing where the users are. A backslash in a Unix
    /// path is a character in a filename, not a separator, and must survive.
    #[cfg(not(windows))]
    #[test]
    fn unix_wire_output_is_byte_identical_to_the_path() {
        let p = PathBuf::from("/weave/projects/web-app");
        assert_eq!(wire_path(&p), "/weave/projects/web-app");
        assert_eq!(operator_path(&p), "/weave/projects/web-app");

        let odd = PathBuf::from("/weave/a\\b/c");
        assert_eq!(
            wire_path(&odd),
            "/weave/a\\b/c",
            "a backslash in a Unix filename is content, not a separator"
        );
    }

    /// A weave-relative path already spelled the way rwv spells it comes back
    /// unchanged, and a backslash inside a Unix filename stays content: the
    /// unconditional `replace` this function is one `cfg` away from would
    /// report the file as living one directory further down.
    #[cfg(not(windows))]
    #[test]
    fn a_unix_weave_relative_path_keeps_its_bytes() {
        assert_eq!(
            weave_relative(Path::new("projects/alpha/DROPPED.md")),
            "projects/alpha/DROPPED.md"
        );
        assert_eq!(
            weave_relative(Path::new("projects/alpha/a\\b.md")),
            "projects/alpha/a\\b.md",
            "a backslash in a Unix filename is content, not a separator"
        );
    }

    /// The Windows arm of the same function: the separator the filesystem
    /// answers in is not the separator rwv's own vocabulary uses.
    #[cfg(windows)]
    #[test]
    fn a_windows_weave_relative_path_is_forward_separated() {
        assert_eq!(
            weave_relative(Path::new(r"projects\alpha\DROPPED.md")),
            "projects/alpha/DROPPED.md"
        );
    }

    /// The Windows arm, which no run on this host exercises — the CI advisory
    /// suite is where it executes. Kept as a unit test over the two functions
    /// rather than as an end-to-end fixture because the behaviour under test
    /// is entirely in the spelling, and a Windows runner is the only thing
    /// that can produce the inputs.
    #[cfg(windows)]
    #[test]
    fn windows_wire_output_is_simplified_and_forward_separated() {
        let verbatim = PathBuf::from(r"\\?\C:\Users\dev\weave");
        assert_eq!(
            wire_path(&verbatim),
            "C:/Users/dev/weave",
            "the wire spelling drops the verbatim prefix and forward-separates"
        );
        assert_eq!(
            operator_path(&verbatim),
            r"C:\Users\dev\weave",
            "the operator spelling drops the prefix and keeps native separators"
        );
        assert!(
            !wire_path(&verbatim).contains('\\'),
            "no backslash may reach a value documented as xargs-composable"
        );
    }

    /// A UNC share keeps its prefix — `dunce::simplified` drops it only where
    /// Windows itself accepts the short form, and that judgement is the whole
    /// reason this seam calls a crate instead of stripping by hand.
    #[cfg(windows)]
    #[test]
    fn a_unc_share_keeps_the_prefix_it_needs() {
        let unc = PathBuf::from(r"\\?\UNC\server\share\weave");
        let wire = wire_path(&unc);
        assert!(
            wire.starts_with("//"),
            "a UNC path must keep the prefix Windows requires: {wire}"
        );
    }
}
