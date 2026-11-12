//! The `rwv` an integration test runs carries the test pause exactly when the
//! cargo invocation that built it enabled `test-pause`, and the build every
//! release makes carries none.
//!
//! The suite's own stages pass the feature, so there the first test pins that
//! the binary every harness spawns is the paused build, and that the scan can
//! see the arming variable at all. scripts/ci-local.sh's no-test-pause stage
//! builds this file with no feature flags, as releases build, and runs both:
//! the second asserts no pause survived whatever the feature defaults are,
//! which the first, comparing against this build's own features, cannot.
//!
//! The arming variable's name is the marker: the only code in the binary that
//! spells it is the code that reads it, so a build holding none of its bytes
//! holds no read of it.

use crate::common;

use common::pause::ARMING_VARIABLE;

fn spawned_rwv_carries_the_arming_variable() -> bool {
    let spawned = assert_cmd::cargo::cargo_bin("rwv");
    assert_eq!(
        spawned,
        std::path::Path::new(env!("CARGO_BIN_EXE_rwv")),
        "the binary tests spawn is not the one cargo built for this test target"
    );
    let bytes = std::fs::read(&spawned)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", spawned.display()));
    let marker = ARMING_VARIABLE.as_bytes();
    bytes.windows(marker.len()).any(|window| window == marker)
}

#[test]
fn the_rwv_an_integration_test_runs_carries_the_pause_exactly_when_built_with_the_feature() {
    let featured = cfg!(feature = "test-pause");
    assert_eq!(
        spawned_rwv_carries_the_arming_variable(),
        featured,
        "built {} the test-pause feature, the spawned rwv {} the arming variable",
        if featured { "with" } else { "without" },
        if featured { "lacks" } else { "carries" },
    );
}

#[test]
#[ignore = "scripts/ci-local.sh's no-test-pause stage runs it, in the build releases make"]
fn the_build_releases_make_carries_no_pause() {
    assert!(
        !spawned_rwv_carries_the_arming_variable(),
        "the rwv built with no feature flags carries the arming variable: a release would ship the pause"
    );
}
