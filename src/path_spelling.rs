//! The two spellings an absolute path takes on its way out of rwv.
//!
//! Both drop the Windows verbatim (`\\?\`) prefix, which a canonicalized weave
//! root carries and which nothing outside rwv wants. They differ on the
//! separator, and the difference is a difference of audience:
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
//! On Unix both are the identity — `dunce::simplified` is a no-op off
//! Windows, and no separator is rewritten, because `\` is an ordinary
//! character in a Unix filename and rewriting it would corrupt the path
//! rather than respell it. So the wire output on Unix is byte-for-byte what
//! it was before either function existed.

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
