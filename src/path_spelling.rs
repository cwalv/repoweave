//! Where a resolved path comes from, and the spellings a path takes on its
//! way out of rwv.
//!
//! [`resolve`] is the one producer of a resolved path. `std::fs::canonicalize`
//! answers on Windows in the verbatim (`\\?\`) spelling, which no consumer of
//! a path wants, and under which a join folds `..` away; `resolve` answers in
//! the plain spelling wherever one names the same path ([`plain_spelling`]).
//! `clippy.toml` bans every other way to canonicalize, so no path rwv holds
//! carries the prefix unless no plain spelling exists for it.
//!
//! [`CanonicalPath`] is a resolved path held as an identity: the type every
//! compared or recorded location takes, comparable only with another.
//!
//! A held path carries the prefix only where no plain spelling names it, so
//! `Display` of one is the spelling a person receives: the native one, which
//! pastes into their own shell and file manager.
//!
//! [`WirePath`] answers a **program** instead, and differs on the separator.
//! `--json` documents shell composition as its consumer (`jq -r … | xargs -I
//! {} git -C {}` is the shipped recipe), and a backslash-separated value
//! cannot survive bare `xargs` — the measured failure. `C:/Users/…` carries no
//! `xargs`-active bytes and is accepted by git, cmd and PowerShell alike.
//!
//! [`weave_relative`] answers for a path *inside* a weave, where the audience
//! question does not arise: a location rwv keeps a name for is spelled the way
//! rwv spells it.
//!
//! On Unix neither rewrites a separator: `\` is an ordinary character in a
//! Unix filename, and rewriting it would corrupt the path rather than respell
//! it. So the wire output on Unix is byte-for-byte the path.
//!
//! [`mint_file_url`] spells a path as a `file://` URL.
//!
//! [`native_join`] spells a path into a file another tool keeps, the way that
//! tool's own path library would have written it.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

/// `path` with every symlink and `.`/`..` resolved, spelled as
/// [`plain_spelling`] spells it.
///
/// On Windows the answer is the spelling on disk, case included: NTFS names
/// each component as stored, whatever case it was asked in. Two spellings of
/// one directory therefore resolve to one string there, which is what lets a
/// resolved spelling serve as an identity ([`CanonicalPath`]). Wine, like a
/// case-folding Linux filesystem, answers in the case it was asked in.
pub fn resolve(path: &Path) -> io::Result<PathBuf> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one canonicalize, whose verbatim answer is respelled before it leaves"
    )]
    let resolved = std::fs::canonicalize(path)?;
    Ok(plain_path(&resolved).into_owned())
}

/// `path` spelled as [`plain_spelling`] spells it.
pub(crate) fn plain_path(path: &Path) -> Cow<'_, Path> {
    match path.to_str().map(plain_spelling) {
        Some(Cow::Owned(plain)) => Cow::Owned(PathBuf::from(plain)),
        Some(Cow::Borrowed(plain)) => Cow::Borrowed(Path::new(plain)),
        None => Cow::Borrowed(path),
    }
}

/// The identity of a location: the one spelling every equivalent spelling of
/// it resolves to.
///
/// Equality, order and hash are on that spelling, so a set or a record keyed
/// by it is lawful. Wherever [`resolve`] answers in the spelling on disk that
/// is also the filesystem's verdict; where it echoes the spelling it was asked
/// with — a case-folding Linux filesystem — [`Self::same_directory`] asks the
/// filesystem instead.
///
/// A path that is not on disk resolves through its nearest existing ancestor,
/// with the missing tail joined back on, so a directory nobody has created yet
/// compares by the same rule as one that exists. A path none of whose
/// ancestors exist — a record naming another machine's tree — is kept as
/// given, and compares textually, which is all a comparison can offer for a
/// path the filesystem cannot speak about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CanonicalPath(PathBuf);

impl CanonicalPath {
    pub fn of(path: &Path) -> Self {
        let mut missing = Vec::new();
        let mut existing = path;
        loop {
            if let Ok(resolved) = resolve(existing) {
                return CanonicalPath(missing.iter().rev().fold(resolved, |p, leaf| p.join(leaf)));
            }
            match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(leaf)) => {
                    missing.push(leaf);
                    existing = parent;
                }
                _ => return CanonicalPath(path.to_path_buf()),
            }
        }
    }

    /// Whether `other` is this location or lies beneath it.
    #[expect(
        clippy::disallowed_methods,
        reason = "a prefix test between two resolved spellings is containment"
    )]
    pub fn contains(&self, other: &CanonicalPath) -> bool {
        other.0.starts_with(&self.0)
    }

    /// Whether `other` is the directory object this names, by the
    /// filesystem's own verdict rather than by spelling.
    ///
    /// On Unix that is `(dev, ino)`, wherever both exist. Stable Rust offers
    /// no file identity on Windows, where `resolve` answers in the spelling on
    /// disk, so the resolved spelling is the whole answer there; it is also
    /// the answer for a path that is gone and so has no identity to read.
    pub fn same_directory(&self, other: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let (Ok(a), Ok(b)) = (std::fs::metadata(&self.0), std::fs::metadata(other)) {
                return (a.dev(), a.ino()) == (b.dev(), b.ino());
            }
        }
        *self == CanonicalPath::of(other)
    }

    /// The identity of a location that must exist, or the error resolving
    /// it gave.
    pub fn existing(path: &Path) -> std::io::Result<Self> {
        resolve(path).map(CanonicalPath)
    }

    /// The directory containing this one. A resolved path's parent is itself
    /// resolved, so no resolution runs.
    pub fn parent(&self) -> Option<CanonicalPath> {
        self.0.parent().map(|p| CanonicalPath(p.to_path_buf()))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl<'de> Deserialize<'de> for CanonicalPath {
    /// A recorded path is resolved as it is read, so a record written in any
    /// spelling — by an earlier rwv, or by hand — compares by the rule a
    /// fresh one does.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(CanonicalPath::of(&PathBuf::deserialize(d)?))
    }
}

/// `path` without the Windows verbatim prefix, where Win32 reads the plain
/// spelling as the same path: `\\?\C:\…` becomes `C:\…` and `\\?\UNC\h\s\…`
/// becomes `\\h\s\…`.
///
/// Win32 opens a verbatim path as written, and rewrites a plain one first: it
/// trims a trailing dot or space from a name, folds `.` and `..`, reads `/`
/// as a separator, maps a reserved device name such as `CON` or `nul.txt` to
/// the device, and refuses one longer than `MAX_PATH` (260) units. The prefix
/// therefore stays on a path any of those rewrites would change, on one
/// longer than 260 units, and on every input that is not a verbatim drive or
/// UNC path.
///
/// Read from the string's shape, not from `cfg(windows)`, so a Windows-shaped
/// path drives the same code on every host.
pub fn plain_spelling(path: &str) -> Cow<'_, str> {
    const LEGACY_MAX_PATH: usize = 260;
    let Some(verbatim) = path.strip_prefix(r"\\?\") else {
        return Cow::Borrowed(path);
    };
    if path.encode_utf16().count() > LEGACY_MAX_PATH {
        return Cow::Borrowed(path);
    }
    if let Some(unc) = verbatim.strip_prefix(r"UNC\") {
        let mut names = without_trailing_separator(unc).split('\\');
        let host_and_share =
            names.next().is_some_and(is_plain_name) && names.next().is_some_and(is_plain_name);
        if host_and_share && names.all(is_plain_name) {
            return Cow::Owned(format!(r"\\{unc}"));
        }
    } else if let Some(below_root) = drive_root(verbatim) {
        if below_root.is_empty()
            || without_trailing_separator(below_root)
                .split('\\')
                .all(is_plain_name)
        {
            return Cow::Borrowed(verbatim);
        }
    }
    Cow::Borrowed(path)
}

fn without_trailing_separator(names: &str) -> &str {
    names.strip_suffix('\\').unwrap_or(names)
}

fn drive_root(path: &str) -> Option<&str> {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == br":\")
        .then(|| &path[3..])
}

fn is_plain_name(name: &str) -> bool {
    const MAX_NAME_UNITS: usize = 255;
    !name.is_empty()
        && name.encode_utf16().count() <= MAX_NAME_UNITS
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c < ' ' || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        && !is_reserved_device_name(name)
}

fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|port| {
        upper.strip_prefix(port).is_some_and(|n| {
            matches!(
                n,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

/// An absolute path as a program receives it: `/`-separated on every
/// platform.
///
/// Every `--json` field whose value is a filesystem absolute path holds one.
/// [`WirePath::of`] is its one mint; reading it back out of `--json` refuses
/// what the mint never writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WirePath(String);

impl WirePath {
    pub fn of(path: &Path) -> WirePath {
        #[cfg(windows)]
        {
            WirePath(path.to_string_lossy().replace('\\', "/"))
        }
        #[cfg(not(windows))]
        {
            WirePath(path.to_string_lossy().into_owned())
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl serde::Serialize for WirePath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for WirePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if !is_absolute_spelling(&text) || text.starts_with(r"\\?\") {
            return Err(serde::de::Error::custom(format!(
                "`{text}` is not a path as `--json` publishes one"
            )));
        }
        Ok(WirePath(text))
    }
}

impl schemars::JsonSchema for WirePath {
    fn schema_name() -> String {
        "WirePath".to_owned()
    }

    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some("Absolute path, `/`-separated on every platform.".to_owned()),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

/// [`WirePath::of`]'s text.
pub fn wire_path(path: &Path) -> String {
    WirePath::of(path).0
}

/// `fragment`, a `/`-separated relative name rwv keeps, as a native relative
/// path: one component per segment, so a join onto a native root is spelled
/// natively throughout rather than `C:\w\github/org/repo`.
///
/// Only `/` separates. A `\` is filename content on Unix, and the names rwv
/// keeps refuse one wherever it would be a separator.
pub fn native_relative(fragment: &str) -> PathBuf {
    fragment.split('/').collect()
}

/// The spelling a location inside a weave takes: `/`-separated everywhere.
///
/// Such a path is a name rwv already keeps, and rwv keeps it with slashes —
/// manifest keys, surfacing declarations, `rwv.lock` entries. Rendered
/// natively it comes back to the operator as a second spelling of a name they
/// hold, and one they cannot paste into the file it came from. An absolute
/// path renders through `Display` instead: that names a location on their
/// host, which rwv has no vocabulary for.
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

/// Mint a `file://` URL from an absolute path spelled however the operator's
/// platform spells one — this is pure string shape, not `cfg(windows)`, so a
/// Windows-shaped root drives the same code on every host.
///
/// Detects a drive letter (`C:\` or `C:/`) textually rather than through
/// `Path::is_absolute`, which answers for the *compiling* platform, not the
/// shape of the string in hand. A drive-letter path takes `\` as its
/// separator and needs the authority slot `C:` would otherwise fill left
/// empty behind an extra `/`; anything else is passed through unsplit, so a
/// `/`-rooted path (its `\` is filename content, not a separator) keeps its
/// bytes. A verbatim path is read in its [`plain_spelling`]: git config and
/// manifests can hold one that rwv did not resolve.
pub fn mint_file_url(path: &str) -> String {
    let path = plain_spelling(path);
    let path = path.as_ref();
    if let Some(unc) = path
        .strip_prefix(r"\\")
        .filter(|rest| !rest.starts_with('?'))
    {
        return format!("file://{}", encode_url_path(&unc.replace('\\', "/")));
    }
    let is_drive_letter_path = starts_with_drive_letter(path);
    let forward_separated = if is_drive_letter_path {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let encoded = encode_url_path(&forward_separated);
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

/// The `/`-separated weave-relative `relative` under `root`, spelled as the
/// host's own path library spells an absolute path: every separator `\` under
/// a drive letter or a UNC share, `/` otherwise.
///
/// For a tool that stores a path and compares it as a string. gita stores
/// `os.path.abspath` of the path it is handed, and on Windows Python rewrites
/// every `/` there to `\`, so its `repos.csv` holds `C:\…\a\b`. Read from the
/// root's shape, like [`mint_file_url`], so a Windows-shaped root drives the
/// same code on every host.
pub fn native_join(root: &Path, relative: &str) -> String {
    let root = root.to_string_lossy();
    if starts_with_drive_letter(&root) || root.starts_with(r"\\") {
        format!(r"{}\{relative}", root.trim_end_matches(['\\', '/'])).replace('/', r"\")
    } else {
        format!("{}/{relative}", root.trim_end_matches('/'))
    }
}

fn starts_with_drive_letter(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(1) == Some(&b':')
        && matches!(bytes.get(2), None | Some(b'/') | Some(b'\\'))
}

/// The `file://` URL an operator would write for `path`: exact for an
/// absolute path, a placeholder for a relative one, whose base differs between
/// a shell and the weave root.
pub fn file_url_suggestion(path: &str) -> String {
    if is_absolute_spelling(path) {
        mint_file_url(path)
    } else {
        "file://<absolute path>".to_owned()
    }
}

/// Whether `path` is absolute in any platform's spelling — `/`-rooted, a
/// drive letter, or a UNC `\\` prefix — judged from the string alone, like
/// [`mint_file_url`].
pub fn is_absolute_spelling(path: &str) -> bool {
    path.starts_with('/') || path.starts_with(r"\\") || starts_with_drive_letter(path)
}

fn encode_url_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wire_path_reads_back_only_what_the_mint_writes() {
        let minted = WirePath::of(Path::new("/weave/github/acme/server"));
        let back: WirePath =
            serde_json::from_value(serde_json::to_value(&minted).unwrap()).unwrap();
        assert_eq!(back, minted);
        for not_minted in ["relative/x", "", r"\\?\C:\x"] {
            assert!(
                serde_json::from_value::<WirePath>(serde_json::json!(not_minted)).is_err(),
                "{not_minted:?}"
            );
        }
    }

    /// On Unix the wire spelling is the identity, which is the whole reason
    /// the decision costs nothing where the users are. A backslash in a Unix
    /// path is a character in a filename, not a separator, and must survive.
    #[cfg(not(windows))]
    #[test]
    fn unix_wire_output_is_byte_identical_to_the_path() {
        let p = PathBuf::from("/weave/projects/web-app");
        assert_eq!(wire_path(&p), "/weave/projects/web-app");

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
    /// suite is where it executes.
    #[cfg(windows)]
    #[test]
    fn windows_wire_output_is_forward_separated() {
        assert_eq!(
            wire_path(Path::new(r"C:\Users\dev\weave")),
            "C:/Users/dev/weave",
            "no backslash may reach a value documented as xargs-composable"
        );
        assert_eq!(
            wire_path(Path::new(r"\\server\share\weave")),
            "//server/share/weave"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_resolved_path_carries_no_verbatim_prefix() {
        let tmp = crate::tempdir().unwrap();
        let resolved = resolve(tmp.path()).unwrap();
        assert!(
            !resolved.as_os_str().to_string_lossy().starts_with(r"\\?\"),
            "{}",
            resolved.display()
        );
        assert!(resolved.is_absolute(), "{}", resolved.display());
        assert_eq!(resolve(&resolved).unwrap(), resolved);
    }

    #[cfg(windows)]
    #[test]
    fn a_link_to_a_resolved_path_reads_back_unchanged() {
        let tmp = crate::tempdir().unwrap();
        let target = resolve(tmp.path()).unwrap().join("shared.db");
        std::fs::write(&target, "").unwrap();
        let link = tmp.path().join("link");
        crate::symlink::create(&target, &link, crate::symlink::LinkTarget::File).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
    }

    // -----------------------------------------------------------------------
    // plain_spelling

    #[test]
    fn a_verbatim_drive_path_is_spelled_plain() {
        assert_eq!(
            plain_spelling(r"\\?\C:\Users\dev\weave"),
            r"C:\Users\dev\weave"
        );
        assert_eq!(plain_spelling(r"\\?\C:\"), r"C:\");
        assert_eq!(plain_spelling(r"\\?\d:\w\"), r"d:\w\");
        assert_eq!(
            plain_spelling(r"\\?\C:\Users\José\weave"),
            r"C:\Users\José\weave"
        );
    }

    #[test]
    fn a_verbatim_unc_path_is_spelled_as_a_share() {
        assert_eq!(
            plain_spelling(r"\\?\UNC\server\share\weave"),
            r"\\server\share\weave"
        );
        assert_eq!(plain_spelling(r"\\?\UNC\server\share"), r"\\server\share");
        assert_eq!(plain_spelling(r"\\?\UNC\server\share\"), r"\\server\share\");
    }

    /// The limit is measured on the verbatim spelling, in UTF-16 units: a
    /// four-byte character is two of them.
    #[test]
    fn a_verbatim_path_over_the_legacy_limit_keeps_its_prefix() {
        let within = format!(r"\\?\C:\{}", "a".repeat(253));
        assert_eq!(within.encode_utf16().count(), 260);
        assert_eq!(plain_spelling(&within), &within[4..]);

        let over = format!("{within}b");
        assert_eq!(plain_spelling(&over), over);

        let within_in_units = format!(r"\\?\C:\{}", "é".repeat(253));
        assert!(within_in_units.len() > 260);
        assert_eq!(plain_spelling(&within_in_units), &within_in_units[4..]);

        let over_in_units = format!(r"\\?\C:\{}🦀", "a".repeat(252));
        assert_eq!(over_in_units.encode_utf16().count(), 261);
        assert_eq!(plain_spelling(&over_in_units), over_in_units);

        let unc_over = format!(r"\\?\UNC\server\share\{}", "a".repeat(240));
        assert_eq!(plain_spelling(&unc_over), unc_over);
    }

    #[test]
    fn a_reserved_device_name_keeps_the_prefix() {
        for name in [
            "CON",
            "nul",
            "Aux.txt",
            "com1",
            "LPT9.tar.gz",
            "COM¹",
            "prn .log",
            "CONIN$",
        ] {
            let verbatim = format!(r"\\?\C:\w\{name}\x");
            assert_eq!(plain_spelling(&verbatim), verbatim, "{name}");
        }
        for name in ["CONSOLE", "COM10", ".con", " nul", "auxiliary.txt"] {
            let verbatim = format!(r"\\?\C:\w\{name}");
            assert_eq!(plain_spelling(&verbatim), &verbatim[4..], "{name}");
        }
        let share = r"\\?\UNC\server\share\NUL";
        assert_eq!(plain_spelling(share), share);
    }

    #[test]
    fn a_trailing_dot_or_space_keeps_the_prefix() {
        for verbatim in [r"\\?\C:\w\a.", r"\\?\C:\w\a \x", r"\\?\UNC\server\share.\x"] {
            assert_eq!(plain_spelling(verbatim), verbatim);
        }
        assert_eq!(plain_spelling(r"\\?\C:\w\a.b"), r"C:\w\a.b");
    }

    #[test]
    fn a_literal_dot_component_keeps_the_prefix() {
        for verbatim in [
            r"\\?\C:\w\..\x",
            r"\\?\C:\w\.\x",
            r"\\?\C:\w\..",
            r"\\?\UNC\server\share\..\other",
        ] {
            assert_eq!(plain_spelling(verbatim), verbatim);
        }
    }

    /// Each of these names one file verbatim and a different path, or none, to
    /// Win32: `/` is a separator there, `:` opens a stream, and an empty name
    /// collapses.
    #[test]
    fn a_name_win32_would_split_or_reject_keeps_the_prefix() {
        for verbatim in [
            r"\\?\C:\w\a/b",
            r"\\?\C:\w\a:b",
            r"\\?\C:\w\a*",
            r"\\?\C:\w\\x",
            r"\\?\C:\\",
            r"\\?\UNC\server\\share",
        ] {
            assert_eq!(plain_spelling(verbatim), verbatim);
        }
    }

    #[test]
    fn a_verbatim_path_with_no_plain_spelling_is_unchanged() {
        for verbatim in [
            r"\\?\C:",
            r"\\?\C:w",
            r"\\?\UNC\server",
            r"\\?\UNC\",
            r"\\?\Volume{b75e2c83-0000-0000-0000-602f00000000}\w",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\w",
        ] {
            assert_eq!(plain_spelling(verbatim), verbatim);
        }
    }

    #[test]
    fn a_path_without_the_verbatim_prefix_is_unchanged() {
        for path in [
            "/srv/weave",
            r"/srv/\\?\C:\x",
            r"C:\Users\dev",
            r"\\server\share\weave",
            r"\\.\pipe\x",
            r"relative\dir",
            "",
        ] {
            assert_eq!(plain_spelling(path), path);
        }
    }

    // -----------------------------------------------------------------------
    // mint_file_url

    #[test]
    fn a_verbatim_drive_path_mints_the_plain_url() {
        assert_eq!(mint_file_url(r"\\?\C:\Users\x"), "file:///C:/Users/x");
    }

    #[test]
    fn a_verbatim_unc_path_mints_the_share_url() {
        assert_eq!(mint_file_url(r"\\?\UNC\srv\share\x"), "file://srv/share/x");
    }

    #[test]
    fn mint_file_url_names_a_unc_share_by_host() {
        assert_eq!(
            mint_file_url(r"\\server\share\My Repos\acme"),
            "file://server/share/My%20Repos/acme"
        );
    }
    // -----------------------------------------------------------------------

    /// `mint_file_url` reads the drive letter out of the string itself, not
    /// out of `cfg(windows)`, so this fixture drives the Windows shape on
    /// whatever platform runs the suite.
    #[test]
    fn a_drive_letter_root_mints_a_well_formed_file_url() {
        assert_eq!(
            mint_file_url(r"C:\Users\runneradmin\AppData\Local\Temp\.tmp8MlRUw\lab/acme/fresh"),
            "file:///C:/Users/runneradmin/AppData/Local/Temp/.tmp8MlRUw/lab/acme/fresh"
        );
    }

    #[test]
    fn a_posix_root_mints_the_same_as_before() {
        assert_eq!(
            mint_file_url("/srv/repos/acme/fresh"),
            "file:///srv/repos/acme/fresh"
        );
    }

    /// A `/`-rooted path never loses a `\`: off a drive letter, it is
    /// filename content, not a separator.
    #[test]
    fn a_backslash_off_a_drive_letter_is_left_alone() {
        assert_eq!(
            mint_file_url("/srv/repos/a\\b/fresh"),
            "file:///srv/repos/a%5Cb/fresh"
        );
    }

    #[test]
    fn a_space_in_the_root_is_percent_encoded() {
        assert_eq!(
            mint_file_url(r"C:\Users\dev\My Repos\acme\fresh"),
            "file:///C:/Users/dev/My%20Repos/acme/fresh"
        );
    }

    /// `%` is the percent-encoding escape character itself, so a literal one
    /// in the path must be escaped too or the result is ambiguous.
    #[test]
    fn a_percent_in_the_root_is_percent_encoded() {
        assert_eq!(
            mint_file_url("/srv/repos/100%/fresh"),
            "file:///srv/repos/100%25/fresh"
        );
    }

    // -----------------------------------------------------------------------
    // native_join

    /// The expected strings are what CPython's `ntpath.abspath` returns for
    /// the same root and tail.
    #[test]
    fn native_join_under_a_drive_letter_is_backslashed_throughout() {
        assert_eq!(
            native_join(
                Path::new(r"C:\Users\runneradmin\AppData\Local\Temp\.tmpOC0a9S"),
                "github/chatly/protocol"
            ),
            r"C:\Users\runneradmin\AppData\Local\Temp\.tmpOC0a9S\github\chatly\protocol"
        );
        assert_eq!(
            native_join(Path::new("C:/Users/dev/weave/"), "github/chatly/protocol"),
            r"C:\Users\dev\weave\github\chatly\protocol"
        );
    }

    #[test]
    fn native_join_under_a_unc_share_is_backslashed_throughout() {
        assert_eq!(
            native_join(Path::new(r"\\server\share\weave"), "github/chatly/protocol"),
            r"\\server\share\weave\github\chatly\protocol"
        );
    }

    #[test]
    fn native_join_under_a_posix_root_keeps_a_backslash_as_content() {
        assert_eq!(
            native_join(Path::new(r"/srv/a\b/weave"), "github/chatly/protocol"),
            r"/srv/a\b/weave/github/chatly/protocol"
        );
    }
}
