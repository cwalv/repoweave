use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

#[derive(Debug, Default, Clone)]
pub struct Progress {
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub running: Vec<String>,
}

const PROGRESS_WIDTH: usize = 100;

impl Progress {
    pub fn render(&self) -> String {
        let remaining = self.total - self.done - self.running.len();
        let mut line = format!(
            "rwv each: {}/{} done, {} running, {} failed, {} remaining",
            self.done,
            self.total,
            self.running.len(),
            self.failed,
            remaining
        );
        if !self.running.is_empty() {
            line.push_str(": ");
            line.push_str(&self.running.join(", "));
        }
        if line.chars().count() > PROGRESS_WIDTH {
            line = line.chars().take(PROGRESS_WIDTH - 1).collect();
            line.push('…');
        }
        line
    }
}

const CLEAR_LINE: &[u8] = b"\r\x1b[2K";

/// The one writer every repo's output goes through. Each call writes whole
/// lines while holding `&mut self`, so lines from concurrent repos never
/// interleave mid-line, and the progress line is erased before and redrawn
/// after them.
pub struct Printer<O: Write, E: Write> {
    out: O,
    err: E,
    progress: Option<Progress>,
}

impl<O: Write, E: Write> Printer<O, E> {
    /// `progress` is drawn only when it is `Some`; callers pass `None` unless
    /// `--progress` was given and stderr is a terminal.
    pub fn new(out: O, err: E, progress: Option<Progress>) -> Self {
        let mut printer = Self { out, err, progress };
        printer.draw_progress();
        printer
    }

    pub fn lines<'a>(
        &mut self,
        prefix: Option<&str>,
        lines: impl IntoIterator<Item = (Stream, &'a [u8])>,
    ) {
        self.erase_progress();
        for (stream, line) in lines {
            let mut buf = Vec::with_capacity(line.len() + 1);
            if let Some(prefix) = prefix {
                buf.extend_from_slice(b"[");
                buf.extend_from_slice(prefix.as_bytes());
                buf.extend_from_slice(b"] ");
            }
            buf.extend_from_slice(line);
            if !buf.ends_with(b"\n") {
                buf.push(b'\n');
            }
            let target: &mut dyn Write = match stream {
                Stream::Out => &mut self.out,
                Stream::Err => &mut self.err,
            };
            let _ = target.write_all(&buf);
            let _ = target.flush();
        }
        self.draw_progress();
    }

    pub fn line(&mut self, prefix: Option<&str>, stream: Stream, line: &[u8]) {
        self.lines(prefix, [(stream, line)]);
    }

    pub fn header(&mut self, path: &str) {
        self.line(None, Stream::Out, format!("[{path}]").as_bytes());
    }

    pub fn update_progress(&mut self, update: impl FnOnce(&mut Progress)) {
        if let Some(progress) = &mut self.progress {
            update(progress);
            self.draw_progress();
        }
    }

    pub fn finish_progress(&mut self) {
        self.erase_progress();
        self.progress = None;
    }

    fn erase_progress(&mut self) {
        if self.progress.is_some() {
            let _ = self.err.write_all(CLEAR_LINE);
        }
    }

    fn draw_progress(&mut self) {
        if let Some(progress) = &self.progress {
            let _ = self.err.write_all(CLEAR_LINE);
            let _ = self.err.write_all(progress.render().as_bytes());
            let _ = self.err.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefixed_line_is_whole_and_newline_terminated() {
        let mut printer = Printer::new(Vec::new(), Vec::new(), None);
        printer.line(Some("github/a/b"), Stream::Out, b"no newline");
        printer.line(Some("github/a/b"), Stream::Err, b"oops\n");
        assert_eq!(printer.out, b"[github/a/b] no newline\n");
        assert_eq!(printer.err, b"[github/a/b] oops\n");
    }

    #[test]
    fn the_progress_line_is_erased_before_output_and_redrawn_after() {
        let progress = Progress {
            total: 2,
            ..Progress::default()
        };
        let mut printer = Printer::new(Vec::new(), Vec::new(), Some(progress));
        printer.err.clear();
        printer.line(Some("r"), Stream::Err, b"line\n");
        let err = String::from_utf8(printer.err.clone()).unwrap();
        let expected = format!(
            "\r\x1b[2K[r] line\n\r\x1b[2K{}",
            printer.progress.as_ref().unwrap().render()
        );
        assert_eq!(err, expected);
    }

    #[test]
    fn without_progress_nothing_but_the_lines_is_written() {
        let mut printer = Printer::new(Vec::new(), Vec::new(), None);
        printer.update_progress(|p| p.done += 1);
        printer.line(None, Stream::Err, b"x\n");
        printer.finish_progress();
        assert_eq!(printer.err, b"x\n");
    }

    #[test]
    fn progress_counts_and_names_running_repos() {
        let progress = Progress {
            total: 5,
            done: 2,
            failed: 1,
            running: vec!["a".into(), "b".into()],
        };
        assert_eq!(
            progress.render(),
            "rwv each: 2/5 done, 2 running, 1 failed, 1 remaining: a, b"
        );
    }
}
