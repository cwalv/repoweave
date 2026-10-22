//! A reader that closes rwv's stdout has stopped reading, which is its choice:
//! `rwv status | head -1` ends quietly, and a verb that changes the workspace
//! finishes its work rather than stopping because nobody reads what it says.
//!
//! Each case hands the verb a pipe whose read end is already closed, so every
//! write fails with a broken pipe whatever size the output is. That is stricter
//! than closing after one line, which is only certain when the output outgrows
//! the pipe buffer; `explain_test.rs` has that one for the largest page.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod common;

fn closed_stdout(mut cmd: Command) -> Output {
    let (reader, writer) = std::io::pipe().expect("a pipe");
    drop(reader);
    cmd.stdout(writer)
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    cmd.output().expect("rwv runs")
}

fn assert_quiet(verb: &str, out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "`rwv {verb}` reported a closed stdout:\n{stderr}"
    );
    assert!(
        out.status.success(),
        "`rwv {verb}`: {:?}\n{stderr}",
        out.status
    );
}

struct Weave {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
}

impl Weave {
    /// One project with one member, whose checkout is present unless `missing`.
    fn new(missing: bool) -> Self {
        let tmp = common::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        common::git_in(&origin, &["init", "-q", "--initial-branch=main"]);
        common::git_in(&origin, &["commit", "-q", "--allow-empty", "-m", "init"]);

        let member = ws.join("github/org/app");
        if !missing {
            std::fs::create_dir_all(&member).unwrap();
            common::git_in(&member, &["init", "-q", "--initial-branch=main"]);
            common::git_in(&member, &["commit", "-q", "--allow-empty", "-m", "init"]);
        }
        let project = ws.join("projects/demo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("rwv.toml"),
            format!(
                "[repositories.\"github/org/app\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
                common::file_url(if missing { &origin } else { &member })
            ),
        )
        .unwrap();
        std::fs::write(ws.join(".rwv-active"), "demo\n").unwrap();
        Self { _tmp: tmp, ws }
    }

    fn command(&self, args: &[&str]) -> Command {
        let rwv = PathBuf::from(env!("CARGO_BIN_EXE_rwv"));
        let rest = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(rwv.parent().unwrap().to_path_buf())
                .chain(std::env::split_paths(&rest)),
        )
        .unwrap();
        let mut cmd = common::rwv_std();
        cmd.args(args).current_dir(&self.ws).env("PATH", path);
        cmd
    }
}

macro_rules! verbs_that_print {
    ($($name:ident: [$($arg:expr),+]),+ $(,)?) => {
        $(
            #[test]
            fn $name() {
                let weave = Weave::new(false);
                let args = [$($arg),+];
                assert_quiet(&args.join(" "), &closed_stdout(weave.command(&args)));
            }
        )+
    };
}

verbs_that_print! {
    status_text: ["status"],
    status_json: ["status", "--json"],
    doctor_text: ["doctor"],
    doctor_json: ["doctor", "--json"],
    prime_page: ["prime"],
    resolve_line: ["resolve"],
    completions_script: ["completions", "bash"],
    explain_index: ["explain"],
    explain_page: ["explain", "doctor"],
    each_plan: ["each", "--dry-run", "--", "true"],
}

#[test]
fn fetch_still_clones_the_missing_member() {
    let weave = Weave::new(true);
    let out = closed_stdout(weave.command(&["fetch"]));
    assert_quiet("fetch", &out);
    assert!(
        Path::new(&weave.ws).join("github/org/app/.git").exists(),
        "fetch stopped before cloning"
    );
}

#[test]
fn setup_claude_still_registers_its_hooks() {
    let tmp = common::tempdir().unwrap();
    let settings = tmp.path().join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, "{}").unwrap();

    let mut cmd = common::rwv_std();
    cmd.args(["setup", "claude"])
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path());
    assert_quiet("setup claude", &closed_stdout(cmd));

    let written = std::fs::read_to_string(&settings).unwrap();
    assert!(
        written.contains("rwv prime"),
        "no hook registered:\n{written}"
    );
}
