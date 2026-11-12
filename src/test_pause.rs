//! Named places where a test can hold rwv still while it changes the world
//! around it, so a race inside one verb can be driven from outside the
//! process.
//!
//! A [`point`] compiles to nothing unless the crate is built with the
//! `test-pause` feature. Built with it, a point still does nothing unless
//! `RWV_TEST_PAUSE` is `<point>@<dir>` and names that point. The first pass
//! through the named point then writes `<dir>/paused` and waits until
//! `<dir>/release` exists; every pass after the release proceeds. A value of
//! any other shape stops the process rather than running unpaused.

pub(crate) fn point(name: &str) {
    #[cfg(feature = "test-pause")]
    armed::wait_at(name);
    #[cfg(not(feature = "test-pause"))]
    let _ = name;
}

/// Whether a test withholds the filesystem capability `capability` from this
/// process, as if the filesystem it writes to lacked it: `RWV_TEST_WITHHOLD`
/// lists the withheld ones, comma-separated. Always false unless the crate is
/// built with the `test-pause` feature.
pub(crate) fn withheld(capability: &str) -> bool {
    #[cfg(feature = "test-pause")]
    return std::env::var("RWV_TEST_WITHHOLD")
        .is_ok_and(|listed| listed.split(',').any(|named| named == capability));
    #[cfg(not(feature = "test-pause"))]
    {
        let _ = capability;
        false
    }
}

#[cfg(feature = "test-pause")]
mod armed {
    use std::path::Path;
    use std::time::{Duration, Instant};

    const RELEASE_DEADLINE: Duration = Duration::from_secs(120);

    pub(super) fn wait_at(name: &str) {
        let Some(arming) = std::env::var_os("RWV_TEST_PAUSE") else {
            return;
        };
        let Some((point, dir)) = arming.to_str().and_then(|value| value.split_once('@')) else {
            panic!("RWV_TEST_PAUSE={arming:?} is not <point>@<dir>");
        };
        if point != name {
            return;
        }
        let dir = Path::new(dir);
        let paused = dir.join("paused");
        std::fs::write(&paused, std::process::id().to_string())
            .unwrap_or_else(|e| panic!("cannot write {}: {e:#}", paused.display()));
        let release = dir.join("release");
        let deadline = Instant::now() + RELEASE_DEADLINE;
        while !release.exists() {
            assert!(
                Instant::now() < deadline,
                "paused at {name} and not released within {RELEASE_DEADLINE:?}: {} never appeared",
                release.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
