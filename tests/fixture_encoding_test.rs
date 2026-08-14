//! Pins the fixture-side encoders in `tests/common/mod.rs` on the input
//! shape that broke them: a path whose components contain backslashes.
//!
//! On Windows every fixture path carries backslashes, and the first
//! `--no-fail-fast` advisory run showed what raw interpolation does to them:
//! pasted into a TOML or JSON string a backslash reads as an escape, so rwv
//! refused ~170 fixture-written manifests, locks and owner records as
//! unparseable. A Unix host never produces such a path from its temp root,
//! which is exactly why these assertions construct one by hand — the cause
//! is portable even where the platform is absent, and these pins hold on
//! every host while the advisory Windows run measures the real thing.

mod common;

use std::path::Path;

/// The Windows shape: drive letter, backslash separators. On Unix this is a
/// legal (if odd) relative path whose single component contains backslashes,
/// which is all the encoders need to see.
const WINDOWS_SHAPED: &str = "C:\\Users\\runner\\fixture";

#[test]
fn file_url_forward_slashes_and_roots_a_drive_letter_path() {
    assert_eq!(
        common::file_url(Path::new(WINDOWS_SHAPED)),
        "file:///C:/Users/runner/fixture"
    );
}

#[test]
fn file_url_is_unchanged_for_a_rooted_unix_path() {
    assert_eq!(
        common::file_url(Path::new("/tmp/fixture")),
        "file:///tmp/fixture"
    );
}

#[test]
fn json_escaped_survives_a_json_round_trip_with_spelling_intact() {
    let body = common::json_escaped(Path::new(WINDOWS_SHAPED));
    let parsed: String = serde_json::from_str(&format!("\"{body}\"")).unwrap();
    assert_eq!(parsed, WINDOWS_SHAPED);
}

#[test]
fn json_escaped_is_the_identity_for_a_unix_path() {
    assert_eq!(
        common::json_escaped(Path::new("/tmp/fixture")),
        "/tmp/fixture"
    );
}

#[test]
fn workweave_marker_parses_as_json_and_preserves_the_recorded_spelling() {
    let marker = common::workweave_marker(
        Path::new(WINDOWS_SHAPED),
        "web-app",
        Path::new(WINDOWS_SHAPED),
    );
    let parsed: serde_json::Value = serde_json::from_str(&marker).unwrap();
    assert_eq!(parsed["primary"], WINDOWS_SHAPED);
    assert_eq!(parsed["project"], "web-app");
    assert_eq!(parsed["parent"], WINDOWS_SHAPED);
}
