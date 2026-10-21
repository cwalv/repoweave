//! `rwv --version` leads with the Cargo version on every build. A binary built
//! inside a git checkout adds the checkout's `git describe` after it; one built
//! from an archive has none and prints the Cargo version alone.

mod common;

/// The expectation is read from `CARGO_PKG_VERSION` and the build script's
/// `RWV_GIT_DESCRIBE` rather than from the library, so a library that returned
/// the describe string in place of the Cargo version could not agree with it.
#[test]
fn version_flag_prints_the_cargo_version_then_the_build_describe() {
    let output = common::rwv().arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    let cargo_version = env!("CARGO_PKG_VERSION");
    let expected = match option_env!("RWV_GIT_DESCRIBE") {
        Some(describe) => format!("rwv {cargo_version} ({describe})\n"),
        None => format!("rwv {cargo_version}\n"),
    };
    assert_eq!(stdout, expected);
}
