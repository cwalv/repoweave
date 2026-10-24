//! The one way rwv writes to its standard output.
//!
//! A reader that closes the pipe, as `rwv status | head -1` does, has stopped
//! reading, which is its choice and not a failure. Writes after that are
//! dropped and the verb carries on: a verb that changes the workspace is not
//! abandoned halfway because nobody is looking at what it prints.
//!
//! `println!` panics on a closed pipe, and resetting SIGPIPE for the process
//! would end rwv wherever it writes to a subprocess's stdin that has already
//! exited, so neither does this job. Any other write error is still an error.

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static READER_GONE: AtomicBool = AtomicBool::new(false);

fn settle(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
            READER_GONE.store(true, Ordering::Relaxed);
            Ok(())
        }
        other => other,
    }
}

pub(crate) fn write(args: fmt::Arguments<'_>, newline: bool) -> io::Result<()> {
    if READER_GONE.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut out = io::stdout().lock();
    settle(out.write_fmt(args).and_then(|()| {
        if newline {
            out.write_all(b"\n")
        } else {
            Ok(())
        }
    }))
}

pub(crate) fn write_bytes(bytes: &[u8]) -> io::Result<()> {
    if READER_GONE.load(Ordering::Relaxed) {
        return Ok(());
    }
    settle(io::stdout().lock().write_all(bytes))
}

pub(crate) fn flush() -> io::Result<()> {
    if READER_GONE.load(Ordering::Relaxed) {
        return Ok(());
    }
    settle(io::stdout().flush())
}

/// What `out!` and `outln!` expand to: `print!` and `println!` that survive a
/// closed pipe and panic, as they do, on any other failure.
pub(crate) fn print(args: fmt::Arguments<'_>, newline: bool) {
    if let Err(e) = write(args, newline) {
        panic!("failed printing to stdout: {e:#}");
    }
}

macro_rules! out {
    ($($arg:tt)*) => {
        $crate::stdout::print(format_args!($($arg)*), false)
    };
}

macro_rules! outln {
    () => {
        $crate::stdout::print(format_args!(""), true)
    };
    ($($arg:tt)*) => {
        $crate::stdout::print(format_args!($($arg)*), true)
    };
}
