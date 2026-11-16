//! The driving side of rwv's test pause: spawn a process armed at a named
//! point, hold it there while the test changes what it will read, then
//! release it and collect what it printed.
//!
//! The binary must be built with the `test-pause` feature for any point to
//! hold; a test that needs one is gated on that feature.
//!
//! No wait here or in the held process ends on a clock. A [`Paused`] holds a
//! lock on the handshake directory's `driver` file for as long as it lives,
//! and the operating system lets go of it when this process ends, however it
//! ends, so a process held by a test that was killed outright stops where it
//! stands rather than waiting on a release no one will write.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;

/// The variable that arms a pause point. Every rwv a test spawns has it
/// removed, so only [`Paused::spawn`] arms one.
pub const ARMING_VARIABLE: &str = "RWV_TEST_PAUSE";

/// The value of [`ARMING_VARIABLE`] that holds a process at `point`, with its
/// handshake files in `dir`.
pub fn arming(point: &str, dir: &Path) -> String {
    let dir = dir.to_str().expect("a pause directory path is valid UTF-8");
    format!("{point}@{dir}")
}

/// A child process held at its armed point.
pub struct Paused {
    child: Child,
    dir: PathBuf,
    _driver: std::fs::File,
    stdout: Option<JoinHandle<Vec<u8>>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
}

fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// Why a spawn did not end held at its point: what the process printed,
/// and how it ended.
pub fn describe(why: &str, output: &Output) -> String {
    format!(
        "{why} ({}):\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

impl Paused {
    /// Spawns `cmd` armed at `point` and returns once it is held there. `dir`
    /// must not exist yet; the handshake lives in it. A process that exits
    /// before it reaches the point is an `Err` describing what it printed.
    pub fn spawn(mut cmd: Command, point: &str, dir: &Path) -> Result<Paused, String> {
        std::fs::create_dir(dir).unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
        let driver = dir.join("driver");
        let driver_file = std::fs::File::create(&driver)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", driver.display()));
        driver_file
            .lock()
            .unwrap_or_else(|e| panic!("cannot lock {}: {e}", driver.display()));
        let mut child = cmd
            .env(ARMING_VARIABLE, arming(point, dir))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot spawn {cmd:?}: {e}"));
        let mut paused = Paused {
            stdout: Some(drain(child.stdout.take().unwrap())),
            stderr: Some(drain(child.stderr.take().unwrap())),
            child,
            dir: dir.to_path_buf(),
            _driver: driver_file,
        };
        loop {
            if paused.dir.join("paused").exists() {
                return Ok(paused);
            }
            if let Some(status) = paused.child.try_wait().expect("a child's status") {
                let output = paused.collect(status);
                return Err(describe(
                    &format!("exited before pausing at {point}"),
                    &output,
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Lets the process go on from its point and waits for it to exit.
    pub fn release(mut self) -> Output {
        std::fs::write(self.dir.join("release"), b"")
            .unwrap_or_else(|e| panic!("cannot release {}: {e}", self.dir.display()));
        let status = self.child.wait().expect("a released child's status");
        self.collect(status)
    }

    /// Kills the process where it stands and returns what it printed.
    pub fn kill(mut self) -> Output {
        let _ = self.child.kill();
        let status = self.child.wait().expect("a killed child's status");
        self.collect(status)
    }

    fn collect(&mut self, status: ExitStatus) -> Output {
        let join = |pipe: &mut Option<JoinHandle<Vec<u8>>>| {
            pipe.take()
                .map(|handle| handle.join().expect("a pipe reader"))
                .unwrap_or_default()
        };
        Output {
            status,
            stdout: join(&mut self.stdout),
            stderr: join(&mut self.stderr),
        }
    }
}

/// A process never released is killed where it stands, so it acts on nothing
/// after the test that held it has ended.
impl Drop for Paused {
    fn drop(&mut self) {
        if self.stdout.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
