//! Named places where a test can hold rwv still while it changes the world
//! around it, so a race inside one verb can be driven from outside the
//! process.
//!
//! A [`point`] compiles to nothing unless the crate is built with the
//! `test-pause` feature. Built with it, a point still does nothing unless
//! `RWV_TEST_PAUSE` is `<point>@<dir>` and names that point. The first pass
//! through the named point then writes `<dir>/paused` and waits until
//! `<dir>/release` exists; every pass after the release proceeds. The driver
//! holds a lock on `<dir>/driver` for as long as it holds the process, and a
//! process that finds that lock free, or the file gone, without a release
//! stops where it stands: its driver has ended, however it ended, and nothing
//! is left to release it. No clock ends a wait. A value of any other shape
//! stops the process rather than running unpaused.
//!
//! `RWV_TEST_PAUSE_TRACE=<file>` makes every pass through any point append
//! the point's name to `<file>`, one per line, so a test can learn which
//! points a run reaches without holding it at each.

pub(crate) fn point(name: &str) {
    #[cfg(feature = "test-pause")]
    {
        armed::trace(name);
        armed::wait_at(name);
    }
    #[cfg(not(feature = "test-pause"))]
    let _ = name;
}

/// Whether a test withholds the filesystem capability `capability` from the
/// publish of `path`, as if the filesystem it writes to lacked it:
/// `RWV_TEST_WITHHOLD` lists the withheld ones, comma-separated, and
/// `RWV_TEST_WITHHOLD_UNDER`, where set, narrows them to the paths that begin
/// with it, so a test can withhold a capability from one directory and leave
/// it to every other. Always false unless the crate is built with the
/// `test-pause` feature.
pub(crate) fn withheld_at(capability: &str, path: &std::path::Path) -> bool {
    #[cfg(feature = "test-pause")]
    return std::env::var("RWV_TEST_WITHHOLD")
        .is_ok_and(|listed| listed.split(',').any(|named| named == capability))
        && std::env::var_os("RWV_TEST_WITHHOLD_UNDER").is_none_or(|under| {
            path.to_string_lossy()
                .starts_with(under.to_string_lossy().as_ref())
        });
    #[cfg(not(feature = "test-pause"))]
    {
        let _ = (capability, path);
        false
    }
}

#[cfg(feature = "test-pause")]
mod armed {
    use std::path::Path;

    pub(super) fn trace(name: &str) {
        if let Some(file) = std::env::var_os("RWV_TEST_PAUSE_TRACE") {
            trace_to(Path::new(&file), name);
        }
    }

    pub(super) fn trace_to(file: &Path, name: &str) {
        use std::io::Write;
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .unwrap_or_else(|e| panic!("cannot open {}: {e:#}", file.display()));
        out.write_all(format!("{name}\n").as_bytes())
            .expect("a trace line is written");
    }

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
        if let Ended::DriverGone = held_in(dir) {
            panic!(
                "paused at {name} and its driver is gone: {} is held by no one and {} never appeared",
                dir.join(DRIVER).display(),
                dir.join(RELEASE).display()
            );
        }
    }

    pub(super) const DRIVER: &str = "driver";
    const RELEASE: &str = "release";

    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Ended {
        Released,
        DriverGone,
    }

    pub(super) fn held_in(dir: &Path) -> Ended {
        let release = dir.join(RELEASE);
        let driver = dir.join(DRIVER);
        loop {
            if release.exists() {
                return Ended::Released;
            }
            if driver_gone(&driver) {
                return match release.exists() {
                    true => Ended::Released,
                    false => Ended::DriverGone,
                };
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn driver_gone(driver: &Path) -> bool {
        let file = match std::fs::File::open(driver) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
            Err(e) => panic!("cannot open {}: {e:#}", driver.display()),
        };
        match file.try_lock_shared() {
            Ok(()) => true,
            Err(std::fs::TryLockError::WouldBlock) => false,
            Err(std::fs::TryLockError::Error(e)) => {
                panic!("cannot read the lock on {}: {e:#}", driver.display())
            }
        }
    }
}

#[cfg(all(test, feature = "test-pause"))]
mod tests {
    use super::armed::{held_in, Ended, DRIVER};
    #[test]
    fn a_traced_pass_through_a_point_appends_its_name() {
        let dir = crate::tempdir().unwrap();
        let file = dir.path().join("trace");
        for name in ["first-point", "second-point", "first-point"] {
            super::armed::trace_to(&file, name);
        }
        let traced = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            traced.lines().collect::<Vec<_>>(),
            ["first-point", "second-point", "first-point"]
        );
    }

    fn driven(dir: &std::path::Path) -> std::fs::File {
        let driver = std::fs::File::create(dir.join(DRIVER)).unwrap();
        driver.lock().unwrap();
        driver
    }

    #[test]
    fn a_held_point_goes_on_once_its_driver_releases_it() {
        let dir = crate::tempdir().unwrap();
        let driver = driven(dir.path());
        let held = std::thread::spawn({
            let dir = dir.path().to_path_buf();
            move || held_in(&dir)
        });
        std::fs::write(dir.path().join("release"), b"").unwrap();
        assert_eq!(held.join().unwrap(), Ended::Released);
        drop(driver);
    }

    #[test]
    fn a_held_point_whose_driver_lets_go_without_a_release_finds_its_driver_gone() {
        let dir = crate::tempdir().unwrap();
        let driver = driven(dir.path());
        let held = std::thread::spawn({
            let dir = dir.path().to_path_buf();
            move || held_in(&dir)
        });
        drop(driver);
        assert_eq!(held.join().unwrap(), Ended::DriverGone);
    }

    #[test]
    fn a_held_point_with_no_driver_file_finds_its_driver_gone() {
        let dir = crate::tempdir().unwrap();
        assert_eq!(held_in(dir.path()), Ended::DriverGone);
    }
}
