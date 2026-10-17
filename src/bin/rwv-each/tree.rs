//! Process trees: each command runs as the root of a tree that can be ended
//! as a whole — a new process group on Unix, a job object on Windows — so a
//! timeout or an interrupt reaches the grandchildren a shell or a build tool
//! starts, not just the process this plugin spawned.

use std::process::Command;

use process_wrap::std::{ChildWrapper, CommandWrap};

pub fn spawn_tree(command: Command) -> std::io::Result<Box<dyn ChildWrapper>> {
    let mut wrap = CommandWrap::from(command);
    #[cfg(unix)]
    wrap.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    wrap.wrap(process_wrap::std::JobObject);
    wrap.spawn()
}

/// Forward an interrupt to a tree. The first one asks (SIGINT on Unix); any
/// later one, and every one on Windows, where a job cannot be sent a console
/// control event, ends the tree outright.
pub fn interrupt_tree(child: &mut dyn ChildWrapper, nth_interrupt: usize) -> std::io::Result<()> {
    #[cfg(unix)]
    if nth_interrupt == 1 {
        return child.signal(nix::sys::signal::Signal::SIGINT as i32);
    }
    let _ = nth_interrupt;
    child.start_kill()
}

pub fn kill_tree(child: &mut dyn ChildWrapper) -> std::io::Result<()> {
    child.start_kill()
}

#[cfg(unix)]
pub fn died_of_interrupt(status: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    status.signal() == Some(nix::sys::signal::Signal::SIGINT as i32)
}

#[cfg(not(unix))]
pub fn died_of_interrupt(_status: &std::process::ExitStatus) -> bool {
    false
}

#[cfg(unix)]
pub fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
pub fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

pub use foreground::Foreground;

#[cfg(unix)]
mod foreground {
    use std::io::IsTerminal;

    use nix::sys::signal::{killpg, pthread_sigmask, SigSet, SigmaskHow, Signal};
    use nix::unistd::{getpgrp, tcgetpgrp, tcsetpgrp, Pid};

    /// The terminal handed to a command's process group for as long as this
    /// is held, as a job-control shell does for a foreground job. Without it
    /// a command in its own group that reads the terminal, or sets its modes
    /// (a pager does), is stopped by SIGTTIN/SIGTTOU and never resumes.
    pub struct Foreground {
        ours: Pid,
    }

    impl Foreground {
        /// `None` when stdin is not a terminal, or this process does not hold
        /// it — then there is nothing to hand over.
        pub fn hand_to(pgid: u32) -> Option<Self> {
            let stdin = std::io::stdin();
            if !stdin.is_terminal() {
                return None;
            }
            let ours = getpgrp();
            if tcgetpgrp(&stdin).ok()? != ours {
                return None;
            }
            let theirs = Pid::from_raw(i32::try_from(pgid).ok()?);
            tcsetpgrp(&stdin, theirs).ok()?;
            // The child may have touched the terminal before it was handed
            // over and been stopped for it.
            let _ = killpg(theirs, Signal::SIGCONT);
            Some(Self { ours })
        }
    }

    impl Drop for Foreground {
        fn drop(&mut self) {
            // Taking the terminal back from a background group raises SIGTTOU
            // unless the caller blocks it; blocking on this thread only keeps
            // the disposition children inherit untouched.
            let mut ttou = SigSet::empty();
            ttou.add(Signal::SIGTTOU);
            let mut previous = SigSet::empty();
            let blocked =
                pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&ttou), Some(&mut previous)).is_ok();
            let _ = tcsetpgrp(std::io::stdin(), self.ours);
            if blocked {
                let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None);
            }
        }
    }
}

#[cfg(not(unix))]
mod foreground {
    pub struct Foreground;

    impl Foreground {
        pub fn hand_to(_pgid: u32) -> Option<Self> {
            None
        }
    }
}

pub use self_job::KillOnExitJob;

/// This process's membership in a kill-on-close job object: if it is killed,
/// the system closes the job and every child goes with it. Dropping it on a
/// normal exit clears the kill-on-close limit first, so a child a command
/// deliberately left running survives a successful run.
#[cfg(windows)]
mod self_job {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    pub struct KillOnExitJob {
        job: HANDLE,
    }

    fn set_limits(job: HANDLE, flags: JOB_OBJECT_LIMIT) -> windows::core::Result<()> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = flags;
        // SAFETY: `info` outlives the call and the length passed is its size.
        unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of_val(&info) as u32,
            )
        }
    }

    use windows::Win32::System::JobObjects::JOB_OBJECT_LIMIT;

    impl KillOnExitJob {
        pub fn join() -> Option<Self> {
            // SAFETY: plain Win32 calls on handles this function owns; the
            // job handle is closed on every path that does not return it.
            unsafe {
                let job = CreateJobObjectW(None, None).ok()?;
                let joined = set_limits(job, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE).is_ok()
                    && AssignProcessToJobObject(job, GetCurrentProcess()).is_ok();
                if !joined {
                    let _ = CloseHandle(job);
                    return None;
                }
                Some(Self { job })
            }
        }
    }

    impl Drop for KillOnExitJob {
        fn drop(&mut self) {
            let _ = set_limits(self.job, JOB_OBJECT_LIMIT(0));
            // SAFETY: the handle is owned by this value and closed once.
            unsafe {
                let _ = CloseHandle(self.job);
            }
        }
    }
}

#[cfg(not(windows))]
mod self_job {
    pub struct KillOnExitJob;

    impl KillOnExitJob {
        pub fn join() -> Option<Self> {
            None
        }
    }
}
