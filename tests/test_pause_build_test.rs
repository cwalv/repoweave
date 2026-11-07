//! The `rwv` an integration test runs carries the test pause exactly when the
//! cargo invocation that built it enabled `test-pause`.
//!
//! The suite's own stages pass the feature, so there this pins that the
//! binary every harness spawns is the paused build, and that the scan below
//! can see the arming variable at all. scripts/ci-local.sh's no-test-pause
//! stage runs this file again without the feature, which is the build every
//! release makes, and there it pins that no pause survives.
//!
//! The arming variable's name is the marker: the only code in the binary that
//! spells it is the code that reads it, so a build holding none of its bytes
//! holds no read of it.

mod common;

use common::pause::ARMING_VARIABLE;

#[test]
fn the_rwv_an_integration_test_runs_carries_the_pause_exactly_when_built_with_the_feature() {
    let spawned = assert_cmd::cargo::cargo_bin("rwv");
    assert_eq!(
        spawned,
        std::path::Path::new(env!("CARGO_BIN_EXE_rwv")),
        "the binary tests spawn is not the one cargo built for this test target"
    );
    let bytes = std::fs::read(&spawned)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", spawned.display()));
    let marker = ARMING_VARIABLE.as_bytes();
    let carries = bytes.windows(marker.len()).any(|window| window == marker);
    assert_eq!(
        carries,
        cfg!(feature = "test-pause"),
        "{} {} the arming variable, built {} the test-pause feature",
        spawned.display(),
        if carries { "carries" } else { "lacks" },
        if cfg!(feature = "test-pause") {
            "with"
        } else {
            "without"
        },
    );
}
