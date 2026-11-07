//! The driving side of rwv's test pause: spawn a process armed at a named
//! point, hold it there while the test changes what it will read, then
//! release it and collect what it printed.
//!
//! The binary must be built with the `test-pause` feature for any point to
//! hold; a test that needs one is gated on that feature.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long either side waits for the other before giving up.
pub const PAUSE_DEADLINE: Duration = Duration::from_secs(120);

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
    /// must not exist yet; the handshake lives in it. A process that exits,
    /// or does not reach the point within [`PAUSE_DEADLINE`], is an `Err`
    /// describing what it printed.
    pub fn spawn(mut cmd: Command, point: &str, dir: &Path) -> Result<Paused, String> {
        std::fs::create_dir(dir).unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
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
        };
        let deadline = Instant::now() + PAUSE_DEADLINE;
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
            if Instant::now() > deadline {
                let _ = paused.child.kill();
                let status = paused.child.wait().expect("a killed child's status");
                let output = paused.collect(status);
                return Err(describe(
                    &format!("did not pause at {point} within {PAUSE_DEADLINE:?}"),
                    &output,
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Lets the process go on from its point and waits for it to exit,
    /// killing it after [`PAUSE_DEADLINE`].
    pub fn release(mut self) -> Output {
        std::fs::write(self.dir.join("release"), b"")
            .unwrap_or_else(|e| panic!("cannot release {}: {e}", self.dir.display()));
        let deadline = Instant::now() + PAUSE_DEADLINE;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("a child's status") {
                break status;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                break self.child.wait().expect("a killed child's status");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
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
