//! A terminal's foreground job, reproduced so a test can interrupt it the way
//! Ctrl-C does, and the processes a test needs inside that job.
//!
//! The processes are this test binary run again: a binary that calls
//! [`serve`] from a `#[test] fn job_helper` becomes the role [`ROLE`] names
//! when started with [`helper_args`]. Without [`ROLE`] set that test returns
//! at once, so an ordinary suite run sees an empty pass.
//!
//! Ctrl-C reaches every process of the foreground job, and each platform
//! defines that job differently. On Unix it is a process group, and the
//! terminal signals the group: [`as_foreground_job`] makes the started process
//! a group leader and [`interrupt_foreground_job`] signals its group. On
//! Windows it is every process attached to one console, and the console
//! raises `CTRL_C_EVENT` in each: [`as_foreground_job`] gives the started
//! process a console of its own, and [`interrupt_foreground_job`] has a helper
//! attach to that console and raise the event there, as the console does for
//! a keypress. The test's own process is attached to neither, so it is not
//! interrupted with the job.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub const ROLE: &str = "RWV_TEST_JOB_ROLE";
const READY: &str = "RWV_TEST_JOB_READY";
#[cfg(windows)]
const CONSOLE_OF: &str = "RWV_TEST_JOB_CONSOLE_OF";

/// The file a tree root writes its leaf's pid into, in its working directory.
pub const LEAF_PID_FILE: &str = "leaf.pid";

/// A process that starts a leaf, records the leaf's pid in [`LEAF_PID_FILE`],
/// and waits on it.
pub const TREE_ROOT: &str = "tree-root";
/// A process that would outlive its parent by a minute and takes no action on
/// an interrupt.
const TREE_LEAF: &str = "tree-leaf";
/// A process that creates the file [`hold`] names once it can take an
/// interrupt, waits for one, then winds down and exits [`HOLD_EXIT`].
const HOLD: &str = "hold";
pub const HOLD_EXIT: i32 = 7;

pub fn helper_exe() -> PathBuf {
    std::env::current_exe().expect("this test binary's own path")
}

pub fn helper_args() -> [&'static str; 3] {
    ["--exact", "job_helper", "--test-threads=1"]
}

pub fn serve() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    match role.as_str() {
        TREE_ROOT => tree_root(),
        TREE_LEAF => tree_leaf(),
        HOLD => hold_until_interrupted(),
        #[cfg(windows)]
        CONSOLE_INTERRUPTER => interrupt_console(),
        other => panic!("{ROLE}={other} names no helper"),
    }
}

/// The environment that makes a [`helper_args`] run of this binary a
/// [`HOLD`] process, marking `ready` once it can take an interrupt.
pub fn hold(cmd: &mut Command, ready: &Path) {
    cmd.env(ROLE, HOLD).env(READY, ready);
}

fn tree_root() {
    let mut leaf = Command::new(helper_exe())
        .args(helper_args())
        .env(ROLE, TREE_LEAF)
        .spawn()
        .expect("the leaf starts");
    std::fs::write(LEAF_PID_FILE, format!("{}\n", leaf.id())).expect("the leaf pid is recorded");
    let _ = leaf.wait();
}

fn tree_leaf() {
    ctrlc::set_handler(|| {}).expect("the leaf takes interrupts itself");
    std::thread::sleep(Duration::from_secs(60));
}

fn hold_until_interrupted() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INTERRUPTED: AtomicBool = AtomicBool::new(false);
    ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst))
        .expect("the hold takes interrupts itself");
    std::fs::write(
        std::env::var_os(READY).expect("a hold names its ready file"),
        "",
    )
    .expect("the ready file is written");
    while !INTERRUPTED.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    std::process::exit(HOLD_EXIT);
}

#[cfg(unix)]
pub fn as_foreground_job(cmd: &mut Command) -> &mut Command {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0)
}

#[cfg(windows)]
pub fn as_foreground_job(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    use windows::Win32::System::Threading::CREATE_NEW_CONSOLE;
    cmd.creation_flags(CREATE_NEW_CONSOLE.0)
}

#[cfg(unix)]
pub fn interrupt_foreground_job(job: &Child) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    let leader = Pid::from_raw(i32::try_from(job.id()).expect("a pid fits an i32"));
    killpg(leader, Signal::SIGINT).expect("the job's group is signaled");
}

#[cfg(windows)]
const CONSOLE_INTERRUPTER: &str = "console-interrupter";

#[cfg(windows)]
pub fn interrupt_foreground_job(job: &Child) {
    let out = Command::new(helper_exe())
        .args(helper_args())
        .env(ROLE, CONSOLE_INTERRUPTER)
        .env(CONSOLE_OF, job.id().to_string())
        .output()
        .expect("the console interrupter runs");
    assert!(
        out.status.success(),
        "raising Ctrl-C in the job's console failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A process can be attached to one console at a time, and an event raised
/// for process group 0 reaches every process attached to the raiser's.
#[cfg(windows)]
fn interrupt_console() {
    use windows::Win32::System::Console::{
        AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler, CTRL_C_EVENT,
    };
    let owner: u32 = std::env::var(CONSOLE_OF)
        .expect("the interrupter names the console's owner")
        .parse()
        .expect("a pid");
    // SAFETY: plain Win32 calls with no pointers; this process exists only to
    // make them.
    unsafe {
        let _ = FreeConsole();
        AttachConsole(owner).expect("the job's console can be attached");
        SetConsoleCtrlHandler(None, true).expect("the interrupter ignores its own event");
        GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0).expect("Ctrl-C is raised in the console");
    }
}

#[cfg(unix)]
pub fn alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(
        Pid::from_raw(i32::try_from(pid).expect("a pid fits an i32")),
        None,
    )
    .is_ok()
}

#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    // SAFETY: the handle is opened, waited on without blocking, and closed
    // here.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return false;
        };
        let running = WaitForSingleObject(process, 0) == WAIT_TIMEOUT;
        let _ = CloseHandle(process);
        running
    }
}

pub fn wait_dead(pid: u32) {
    let start = Instant::now();
    while alive(pid) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{pid} outlived its tree"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The pid a [`TREE_ROOT`] running in `dir` recorded, once it has.
pub fn leaf_pid(dir: &Path, limit: Duration) -> u32 {
    let path = dir.join(LEAF_PID_FILE);
    let start = Instant::now();
    loop {
        if let Ok(s) = std::fs::read_to_string(&path) {
            if let Some(pid) = s.strip_suffix('\n') {
                return pid.parse().expect("a recorded pid");
            }
        }
        assert!(start.elapsed() < limit, "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}
