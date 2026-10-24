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
//!
//! A job is waited on through [`wait_job`], never `Child::wait`: a job that
//! does not end within its bound has every process killed and fails its test
//! with a message, where an unbounded wait would hold the whole suite.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output};
use std::time::{Duration, Instant};

pub const ROLE: &str = "RWV_TEST_JOB_ROLE";
const READY: &str = "RWV_TEST_JOB_READY";
#[cfg(windows)]
const CONSOLE_OF: &str = "RWV_TEST_JOB_CONSOLE_OF";

/// Long enough for any job a test starts to end after its interrupt, short
/// enough that a job that never ends fails its test within the CI run.
pub const JOB_BOUND: Duration = Duration::from_secs(60);
const HELPER_BOUND: Duration = Duration::from_secs(20);
const REAP_BOUND: Duration = Duration::from_secs(10);

/// The file a tree root writes its leaf's pid into, in its working directory.
pub const LEAF_PID_FILE: &str = "leaf.pid";

/// A process that starts a leaf, records the leaf's pid in [`LEAF_PID_FILE`],
/// and waits on it.
pub const TREE_ROOT: &str = "tree-root";
/// A process that would outlive its parent by a minute and takes no action on
/// an interrupt.
const TREE_LEAF: &str = "tree-leaf";
/// A [`TREE_ROOT`] whose leaf holds none of its stdio, so nothing but the
/// tree ties the leaf to the command. Its leaf records its own pid once it
/// takes no action on an interrupt, and on a terminal the root starts it only
/// once the terminal has been handed to the root's group.
pub const ORPHANING_ROOT: &str = "orphaning-root";
const QUIET_LEAF: &str = "quiet-leaf";
/// A process that writes its pid into the file [`hold`] names once it can
/// take an interrupt, waits for one, then winds down and exits
/// [`HOLD_EXIT`].
const HOLD: &str = "hold";
pub const HOLD_EXIT: i32 = 7;
/// A process that writes its pid into the file [`linger`] names, then sleeps
/// past any job's bound with every interrupt at its default disposition.
const LINGER: &str = "linger";

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
        ORPHANING_ROOT => orphaning_root(),
        QUIET_LEAF => quiet_leaf(),
        HOLD => hold_until_interrupted(),
        LINGER => linger_with_default_interrupts(),
        #[cfg(windows)]
        CONSOLE_INTERRUPTER => interrupt_console(),
        #[cfg(windows)]
        CONSOLE_TERMINATOR => terminate_console(),
        other => panic!("{ROLE}={other} names no helper"),
    }
}

/// The environment that makes a [`helper_args`] run of this binary a
/// [`HOLD`] process, marking `ready` once it can take an interrupt.
pub fn hold(cmd: &mut Command, ready: &Path) {
    cmd.env(ROLE, HOLD).env(READY, ready);
}

/// The environment that makes a [`helper_args`] run of this binary a
/// [`LINGER`] process, marking `ready` once it is running.
pub fn linger(cmd: &mut Command, ready: &Path) {
    cmd.env(ROLE, LINGER).env(READY, ready);
}

fn linger_with_default_interrupts() {
    std::fs::write(
        std::env::var_os(READY).expect("a linger names its ready file"),
        format!("{}\n", std::process::id()),
    )
    .expect("the ready file is written");
    std::thread::sleep(JOB_BOUND * 2);
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

fn orphaning_root() {
    wait_for_the_terminal();
    let mut leaf = Command::new(helper_exe())
        .args(helper_args())
        .env(ROLE, QUIET_LEAF)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the leaf starts");
    let _ = leaf.wait();
}

fn quiet_leaf() {
    ctrlc::set_handler(|| {}).expect("the leaf takes interrupts itself");
    std::fs::write(LEAF_PID_FILE, format!("{}\n", std::process::id()))
        .expect("the leaf pid is recorded");
    std::thread::sleep(Duration::from_secs(60));
}

#[cfg(unix)]
fn wait_for_the_terminal() {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return;
    }
    let start = Instant::now();
    while nix::unistd::tcgetpgrp(&stdin) != Ok(nix::unistd::getpgrp()) {
        assert!(
            start.elapsed() < HELPER_BOUND,
            "the terminal was never handed to this group"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(unix))]
fn wait_for_the_terminal() {}

fn hold_until_interrupted() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INTERRUPTED: AtomicBool = AtomicBool::new(false);
    ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst))
        .expect("the hold takes interrupts itself");
    std::fs::write(
        std::env::var_os(READY).expect("a hold names its ready file"),
        format!("{}\n", std::process::id()),
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

/// A process inherits whether it ignores `CTRL_C_EVENT`, and while it does
/// none of its handlers run. A shell with no terminal sets that on what it
/// starts — Git for Windows' bash does on a CI runner — and a terminal's job
/// does not have it, so it is cleared here for the job to inherit.
#[cfg(windows)]
pub fn as_foreground_job(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows::Win32::System::Threading::CREATE_NEW_CONSOLE;
    // SAFETY: no handler routine is passed; this only clears the attribute.
    unsafe { SetConsoleCtrlHandler(None, false) }
        .expect("this process stops ignoring Ctrl-C, so the job does not inherit that");
    cmd.creation_flags(CREATE_NEW_CONSOLE.0)
}

#[cfg(unix)]
pub fn interrupt_foreground_job(job: &Child) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    let leader = Pid::from_raw(i32::try_from(job.id()).expect("a pid fits an i32"));
    killpg(leader, Signal::SIGINT).expect("the job's group is signaled");
}

#[cfg(unix)]
pub fn kill_job(leader: u32) -> Result<(), String> {
    use nix::errno::Errno;
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    let leader = Pid::from_raw(i32::try_from(leader).expect("a pid fits an i32"));
    match killpg(leader, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(e) => Err(format!("killing the job's group failed: {e}")),
    }
}

#[cfg(windows)]
const CONSOLE_INTERRUPTER: &str = "console-interrupter";
#[cfg(windows)]
const CONSOLE_TERMINATOR: &str = "console-terminator";

#[cfg(windows)]
pub fn interrupt_foreground_job(job: &Child) {
    if let Err(e) = run_console_helper(CONSOLE_INTERRUPTER, job.id()) {
        panic!("raising Ctrl-C in the job's console failed: {e}");
    }
}

#[cfg(windows)]
pub fn kill_job(owner: u32) -> Result<(), String> {
    run_console_helper(CONSOLE_TERMINATOR, owner)
}

#[cfg(windows)]
fn run_console_helper(role: &str, owner: u32) -> Result<(), String> {
    let mut helper = Command::new(helper_exe())
        .args(helper_args())
        .env(ROLE, role)
        .env(CONSOLE_OF, owner.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{role} did not start: {e}"))?;
    let Some(status) = wait_for(&mut helper, HELPER_BOUND) else {
        let _ = helper.kill();
        let _ = helper.wait();
        return Err(format!("{role} was still running after {HELPER_BOUND:?}"));
    };
    if status.success() {
        return Ok(());
    }
    let mut said = String::new();
    if let Some(mut out) = helper.stdout.take() {
        let _ = out.read_to_string(&mut said);
    }
    if let Some(mut err) = helper.stderr.take() {
        let _ = err.read_to_string(&mut said);
    }
    Err(format!("{role} exited {status}: {said}"))
}

#[cfg(windows)]
fn console_owner() -> u32 {
    std::env::var(CONSOLE_OF)
        .expect("a console helper names the console's owner")
        .parse()
        .expect("a pid")
}

/// A process can be attached to one console at a time, and an event raised
/// for process group 0 reaches every process attached to the raiser's.
#[cfg(windows)]
fn interrupt_console() {
    use windows::Win32::System::Console::{
        AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler, CTRL_C_EVENT,
    };
    let owner = console_owner();
    // SAFETY: plain Win32 calls with no pointers; this process exists only to
    // make them.
    unsafe {
        let _ = FreeConsole();
        AttachConsole(owner).expect("the job's console can be attached");
        SetConsoleCtrlHandler(None, true).expect("the interrupter ignores its own event");
        GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0).expect("Ctrl-C is raised in the console");
    }
}

/// Every process attached to the job's console is the job, whatever its
/// parentage.
#[cfg(windows)]
fn terminate_console() {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Console::{AttachConsole, FreeConsole, GetConsoleProcessList};
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    let owner = console_owner();
    let mut attached = [0u32; 256];
    // SAFETY: plain Win32 calls; each opened handle is closed here.
    unsafe {
        let _ = FreeConsole();
        AttachConsole(owner).expect("the job's console can be attached");
        let count = GetConsoleProcessList(&mut attached) as usize;
        assert!(
            (1..=attached.len()).contains(&count),
            "the console lists {count} processes"
        );
        for &pid in attached[..count]
            .iter()
            .filter(|&&pid| pid != std::process::id())
        {
            let Ok(process) = OpenProcess(PROCESS_TERMINATE, false, pid) else {
                continue;
            };
            let _ = TerminateProcess(process, 1);
            let _ = CloseHandle(process);
        }
    }
}

fn wait_for(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("the child can be polled") {
            return Some(status);
        }
        if start.elapsed() >= limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The job's exit status if it ends within `limit`. Past it, every process
/// of the job is killed and the job reaped, and the error says the job
/// outran `limit`.
pub fn wait_job(job: &mut Child, limit: Duration) -> Result<ExitStatus, String> {
    if let Some(status) = wait_for(job, limit) {
        return Ok(status);
    }
    let killed = kill_job(job.id());
    let _ = job.kill();
    let reaped = wait_for(job, REAP_BOUND);
    match (killed, reaped) {
        (Ok(()), Some(_)) => Err(format!(
            "the job was still running after {limit:?}; every process of it was killed"
        )),
        (killed, reaped) => Err(format!(
            "the job was still running after {limit:?}, and killing it did not finish it: \
             {killed:?}, reaped {reaped:?}"
        )),
    }
}

/// [`wait_job`] for a job whose stdout and stderr are piped, collecting both.
pub fn wait_job_with_output(mut job: Child, limit: Duration) -> Result<Output, String> {
    fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    }
    let stdout = drain(job.stdout.take());
    let stderr = drain(job.stderr.take());
    let status = wait_job(&mut job, limit)?;
    Ok(Output {
        status,
        stdout: stdout.join().expect("stdout is drained"),
        stderr: stderr.join().expect("stderr is drained"),
    })
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
    recorded_pid(&dir.join(LEAF_PID_FILE), limit)
}

/// The pid written into `path` by a [`TREE_ROOT`] or a [`hold`], once it is.
pub fn recorded_pid(path: &Path, limit: Duration) -> u32 {
    let start = Instant::now();
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Some(pid) = s.strip_suffix('\n') {
                return pid.parse().expect("a recorded pid");
            }
        }
        assert!(start.elapsed() < limit, "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}
