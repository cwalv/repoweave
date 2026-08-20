//! `common::fixture_lock` is a fixture builder every lock-bearing test can
//! reach, so it is pinned here rather than trusted.
//!
//! What it must give a caller that hand-formatted JSON does not: bytes
//! byte-identical to what the shipped serializer emits for the content, and
//! exactly the entries it was handed — no swaps, no extras. Each pin below
//! holds through the exact form (a whole-file literal; parsed triples), so a
//! hand-written format string that merely resembles the output cannot
//! satisfy either.

mod common;

use repoweave::manifest::{LockFile, RepoPath};

const URL: &str = "https://github.com/example/server.git";
const SHA: &str = "1111111111111111111111111111111111111111";

/// The bytes are the shipped serializer's, pinned against a literal of the
/// whole file. A trailing-newline-plus-indentation check is satisfiable by a
/// hand-written format string; a byte-equal literal is not, and it also
/// pins what `rwv lock` itself would emit for equal content, so a drift in
/// either the helper or the serializer reddens here.
#[test]
fn fixture_lock_writes_what_the_shipped_serializer_writes() {
    let tmp = common::tempdir().unwrap();
    common::fixture_lock(tmp.path(), &[("github/example/server", URL, SHA)]);

    let text = std::fs::read_to_string(tmp.path().join("rwv.lock")).unwrap();
    let expected = format!(
        "{{\n  \"repositories\": {{\n    \"github/example/server\": {{\n      \"type\": \"git\",\n      \"url\": {URL:?},\n      \"version\": {SHA:?}\n    }}\n  }}\n}}\n"
    );
    assert_eq!(
        text, expected,
        "the helper's bytes must equal the shipped serializer's for this content"
    );
}

/// It carries exactly what it was handed, checked through the parsed form:
/// each path maps to its own url and version, and the entry count is exact.
/// A raw-text substring sweep passes with the urls swapped between entries
/// or a spurious third entry appended; the parsed triples do not.
#[test]
fn fixture_lock_carries_every_entry_it_was_given() {
    let tmp = common::tempdir().unwrap();
    let other_url = "https://github.com/example/web.git";
    let other_sha = "2222222222222222222222222222222222222222";
    common::fixture_lock(
        tmp.path(),
        &[
            ("github/example/server", URL, SHA),
            ("github/example/web", other_url, other_sha),
        ],
    );

    let text = std::fs::read_to_string(tmp.path().join("rwv.lock")).unwrap();
    let lock = LockFile::from_json_str(&text).expect("the file the helper wrote must parse");
    assert_eq!(
        lock.iter_entries().count(),
        2,
        "exactly the given entries, nothing spurious"
    );
    for (path, url, version) in [
        ("github/example/server", URL, SHA),
        ("github/example/web", other_url, other_sha),
    ] {
        let key = RepoPath::new(path).expect("fixture path is a valid repo path");
        let entry = lock
            .get_entry(&key)
            .unwrap_or_else(|| panic!("the written lock must carry {path}"));
        assert_eq!(entry.url.to_string(), url, "{path} must keep its own url");
        assert_eq!(
            entry.version.as_str(),
            version,
            "{path} must keep its own version"
        );
    }
}
