//! `common::fixture_lock` is a fixture builder every lock-bearing test can
//! reach, so it is pinned here rather than trusted.
//!
//! What it must give a caller that hand-formatted JSON does not: bytes that
//! parse, bytes that carry what it was handed, and bytes the shipped
//! serializer produced rather than a string literal that drifted.

mod common;

use repoweave::manifest::LockFile;

const URL: &str = "https://github.com/example/server.git";
const SHA: &str = "1111111111111111111111111111111111111111";

/// The written file parses. A fixture whose lock has drifted to an
/// unparseable shape is the failure this helper exists to make impossible:
/// nothing downstream reads it, every assertion about it holds vacuously, and
/// the suite stays green measuring nothing.
#[test]
fn fixture_lock_writes_bytes_that_parse() {
    let tmp = common::tempdir().unwrap();
    common::fixture_lock(tmp.path(), &[("github/example/server", URL, SHA)]);

    let text = std::fs::read_to_string(tmp.path().join("rwv.lock")).unwrap();
    LockFile::from_json_str(&text).expect("the file the helper wrote must parse");
}

/// It carries what it was handed. A helper that writes a well-formed lock
/// with the wrong contents parses just as cleanly as one with the right
/// contents, so parsing alone is not the property.
#[test]
fn fixture_lock_carries_every_entry_it_was_given() {
    let tmp = common::tempdir().unwrap();
    let other = "2222222222222222222222222222222222222222";
    common::fixture_lock(
        tmp.path(),
        &[
            ("github/example/server", URL, SHA),
            (
                "github/example/web",
                "https://github.com/example/web.git",
                other,
            ),
        ],
    );

    let text = std::fs::read_to_string(tmp.path().join("rwv.lock")).unwrap();
    for needle in [
        "github/example/server",
        "github/example/web",
        URL,
        "https://github.com/example/web.git",
        SHA,
        other,
    ] {
        assert!(
            text.contains(needle),
            "the written lock must carry {needle}; got:\n{text}"
        );
    }
}

/// The bytes come from the shipped serializer, not from a format string.
/// `lock::write_lock` terminates with a newline; a hand-formatted fixture is
/// free not to, and the difference shows up as a phantom diff to anything
/// that stages or commits the lock.
#[test]
fn fixture_lock_writes_what_the_shipped_serializer_writes() {
    let tmp = common::tempdir().unwrap();
    common::fixture_lock(tmp.path(), &[("github/example/server", URL, SHA)]);

    let text = std::fs::read_to_string(tmp.path().join("rwv.lock")).unwrap();
    assert!(
        text.ends_with('\n'),
        "write_lock terminates the file with a newline; got:\n{text:?}"
    );
    assert!(
        text.contains("  "),
        "write_lock emits pretty-printed JSON; got:\n{text}"
    );
}
