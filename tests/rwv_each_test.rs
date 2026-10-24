//! `rwv each`, the bundled plugin, driven the way an operator reaches it:
//! `rwv each …` dispatched off `PATH`, re-addressing rwv through the envelope.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

mod common;

const APP: &str = "github/org/app";
const LIB: &str = "github/org/lib";
const EACH_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/each.json";
const DOCS: &str = "github/ref/docs";

struct Weave {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
}

impl Weave {
    fn new(repos: &[(&str, &str)]) -> Self {
        Self::with_missing(repos, &[])
    }

    /// `missing` are manifest members whose checkout is never created.
    fn with_missing(repos: &[(&str, &str)], missing: &[(&str, &str)]) -> Self {
        let tmp = common::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let project = ws.join("projects/demo");
        std::fs::create_dir_all(&project).unwrap();
        let mut manifest = String::new();
        for (path, role) in repos.iter().chain(missing) {
            let dir = ws.join(path);
            if !missing.iter().any(|(m, _)| m == path) {
                std::fs::create_dir_all(&dir).unwrap();
                common::git_in(&dir, &["init", "-q", "--initial-branch=main"]);
                common::git_in(&dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
            }
            manifest.push_str(&format!(
                "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"{role}\"\n\n",
                common::file_url(&dir)
            ));
        }
        std::fs::write(project.join("rwv.toml"), manifest).unwrap();
        std::fs::write(ws.join(".rwv-active"), "demo\n").unwrap();
        Self { _tmp: tmp, ws }
    }

    fn standard() -> Self {
        Self::new(&[(APP, "owned"), (LIB, "dependency"), (DOCS, "reference")])
    }

    fn each(&self, args: &[&str]) -> Output {
        self.each_cmd(args).output().expect("rwv each runs")
    }

    fn each_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = common::rwv_std();
        cmd.arg("each")
            .args(args)
            .current_dir(&self.ws)
            .env("PATH", path_with_bins())
            .stdin(Stdio::null());
        cmd
    }

    fn ran(&self, repo: &str) -> bool {
        common::git()
            .args(["config", "--get", "each.ran"])
            .current_dir(self.ws.join(repo))
            .output()
            .unwrap()
            .status
            .success()
    }
}

/// `PATH` with the directory holding this package's binaries first, so `rwv`
/// finds `rwv-each` the way an installed copy would.
fn path_with_bins() -> OsString {
    let rwv = PathBuf::from(env!("CARGO_BIN_EXE_rwv"));
    let bins = rwv.parent().unwrap().to_path_buf();
    let rest = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(bins).chain(std::env::split_paths(&rest))).unwrap()
}

const MARK: &[&str] = &["git", "config", "each.ran", "yes"];

const JOBLOG_HEADER: &str = "Seq\tRepo\tStarttime\tJobRuntime\tExitval\tSignal\tCommand";

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn envelope(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(o)))
}

fn outcome_of<'a>(envelope: &'a Value, path: &str) -> &'a Value {
    envelope["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["path"] == path)
        .unwrap_or_else(|| panic!("no outcome for {path} in {envelope}"))
}

fn paths_in(envelope: &Value) -> Vec<String> {
    envelope["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["path"].as_str().unwrap().to_owned())
        .collect()
}

fn args_then(head: &[&'static str], tail: &[&'static str]) -> Vec<&'static str> {
    head.iter().chain(tail).copied().collect()
}

#[test]
fn reference_repos_are_left_out_by_default() {
    let w = Weave::standard();
    let out = w.each(&args_then(&["--"], MARK));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(w.ran(APP) && w.ran(LIB));
    assert!(!w.ran(DOCS));
}

#[test]
fn a_selector_replaces_the_default_scope_rather_than_narrowing_it() {
    let w = Weave::standard();
    let out = w.each(&[
        "--json",
        "-j",
        "1",
        "--role",
        "reference",
        "--",
        "git",
        "status",
    ]);
    assert_eq!(paths_in(&envelope(&out)), [DOCS]);

    let out = w.each(&["--json", "-j", "1", "--repo", APP, "--", "git", "status"]);
    assert_eq!(paths_in(&envelope(&out)), [APP]);
}

#[test]
fn selectors_use_rwv_status_grammar_unioned() {
    let w = Weave::standard();
    let run = |sel: &[&str]| {
        let args = [&["--json", "-j", "1"], sel, &["--", "git", "status"]].concat();
        let mut paths = paths_in(&envelope(&w.each(&args)));
        paths.sort();
        paths
    };
    assert_eq!(run(&["--repo", LIB]), [LIB]);
    assert_eq!(run(&["--repo", "re:^github/org/"]), [APP, LIB]);
    assert_eq!(run(&["--repo", "glob:github/ref/*"]), [DOCS]);
    assert_eq!(run(&["--role", "owned", "--repo", DOCS]), [APP, DOCS]);
}

#[test]
fn an_empty_selection_fails_naming_the_selectors_and_runs_nothing() {
    let w = Weave::standard();
    let out = w.each(&args_then(
        &["--role", "fork", "--repo", "glob:none/*", "--"],
        MARK,
    ));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("no repo matched --role fork --repo glob:none/*"),
        "{}",
        stderr(&out)
    );
    assert!(!w.ran(APP) && !w.ran(LIB) && !w.ran(DOCS));
}

#[test]
fn a_default_scope_holding_only_reference_repos_fails_saying_how_to_include_them() {
    let w = Weave::new(&[(DOCS, "reference")]);
    let out = w.each(&["--", "git", "status"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("--role reference"),
        "{}",
        stderr(&out)
    );

    let out = w.each(&[
        "--json",
        "-j",
        "1",
        "--role",
        "reference",
        "--",
        "git",
        "status",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(paths_in(&envelope(&out)), [DOCS]);
}

#[test]
fn an_unregistered_workweave_runs_once_doctor_fix_registers_it() {
    let pw = common::build_workspace("demo", &[(APP, "owned")]);
    let created = common::rwv()
        .args(["workweave", "demo", "create", "seat"])
        .current_dir(&pw.workspace)
        .output()
        .unwrap();
    assert!(created.status.success(), "{}", stderr(&created));
    let status: Value = serde_json::from_slice(
        &common::rwv()
            .args(["-w", "demo--seat", "status", "--json"])
            .current_dir(&pw.workspace)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let checkout = PathBuf::from(status["repos"][0]["absolute_path"].as_str().unwrap());
    let seat = checkout
        .ancestors()
        .nth(APP.split('/').count())
        .unwrap()
        .to_path_buf();

    let index = pw.workspace.join("projects/demo/.rwv-workweave-index");
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&index).unwrap()).unwrap();
    doc["workweaves"] = serde_json::json!({});
    std::fs::write(&index, serde_json::to_string(&doc).unwrap()).unwrap();

    let each = |dir: &Path| {
        common::rwv_std()
            .args([
                "each",
                "--json",
                "-j",
                "1",
                "--",
                "git",
                "rev-parse",
                "--show-toplevel",
            ])
            .current_dir(dir)
            .env("PATH", path_with_bins())
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let refused = each(&seat);
    assert_eq!(refused.status.code(), Some(1));
    let advised = common::advised_command(&stderr(&refused), "rwv doctor");
    assert!(advised.contains("--fix"), "{advised}");

    let fixed = common::run_advised(&advised, &seat);
    assert!(
        fixed.status.success(),
        "`{advised}`:\n{}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    let out = each(&seat);
    assert!(out.status.success(), "{}", stderr(&out));
    let ran_in = PathBuf::from(
        outcome_of(&envelope(&out), APP)["stdout"]
            .as_str()
            .unwrap()
            .trim(),
    );
    let canonical = |p: &Path| repoweave::path_spelling::resolve(p).unwrap();
    assert_eq!(canonical(&ran_in), canonical(&checkout));
}

#[test]
fn every_repo_runs_after_a_failure_and_the_exit_code_is_1() {
    let w = Weave::standard();
    let out = w.each(&[
        "--json",
        "-j",
        "1",
        "--role",
        "owned",
        "--role",
        "dependency",
        "--shell",
        "git config each.ran yes && exit 3",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(w.ran(APP) && w.ran(LIB), "the second repo still ran");
    let env = envelope(&out);
    for path in [APP, LIB] {
        assert_eq!(outcome_of(&env, path)["status"], "failed");
        assert_eq!(outcome_of(&env, path)["exit_code"], 3);
    }
}

#[test]
fn the_exit_code_is_0_when_every_repo_succeeds() {
    let w = Weave::standard();
    let out = w.each(&["-j", "2", "--", "git", "status"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn the_text_summary_lists_each_failed_repo_with_its_exit_status_in_path_order() {
    let w = Weave::new(&[(LIB, "dependency"), (APP, "owned")]);
    let out = w.each(&["-j", "2", "--shell", "exit 4"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    let summary = err
        .find("rwv each: 2 of 2 repos did not succeed")
        .unwrap_or_else(|| panic!("no summary in:\n{err}"));
    let tail = &err[summary..];
    let app = tail.find(&format!("  {APP}: exit 4")).expect("app listed");
    let lib = tail.find(&format!("  {LIB}: exit 4")).expect("lib listed");
    assert!(app < lib, "path order, not rwv.toml's:\n{tail}");
}

#[test]
fn a_child_gets_its_repo_in_the_environment_and_runs_in_its_checkout() {
    let w = Weave::standard();
    #[cfg(unix)]
    let line = r#"echo "$RWV_REPO|$RWV_REPO_ROLE|$RWV_REPO_PATH|$(pwd)""#;
    #[cfg(windows)]
    let line = "echo %RWV_REPO%^|%RWV_REPO_ROLE%^|%RWV_REPO_PATH%^|%CD%";
    let out = w.each(&["--json", "-j", "1", "--repo", LIB, "--shell", line]);
    let env = envelope(&out);
    let fields: Vec<String> = outcome_of(&env, LIB)["stdout"]
        .as_str()
        .unwrap()
        .trim()
        .split('|')
        .map(str::to_owned)
        .collect();
    assert_eq!(fields[0], LIB);
    assert_eq!(fields[1], "dependency");
    let canonical = |p: &str| repoweave::path_spelling::resolve(Path::new(p)).unwrap();
    assert_eq!(
        canonical(&fields[2]),
        canonical(w.ws.join(LIB).to_str().unwrap())
    );
    assert_eq!(canonical(&fields[3]), canonical(&fields[2]));
}

#[test]
fn the_documented_per_repo_variables_are_the_ones_a_child_receives() {
    let w = Weave::standard();
    #[cfg(unix)]
    let program = ["env"];
    #[cfg(windows)]
    let program = ["cmd.exe", "/C", "set"];
    let args = [
        &["--json", "-j", "1", "--repo", APP, "--"][..],
        &program[..],
    ]
    .concat();
    let out = w.each(&args);
    let env = envelope(&out);
    let mut set: Vec<String> = outcome_of(&env, APP)["stdout"]
        .as_str()
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.to_owned()))
        .filter(|k| k.starts_with("RWV_REPO"))
        .collect();
    set.sort();
    assert_eq!(set, ["RWV_REPO", "RWV_REPO_PATH", "RWV_REPO_ROLE"]);
    let reference = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/rwv-each.md"),
    )
    .unwrap();
    for var in set {
        assert!(
            reference.contains(&format!("`{var}`")),
            "{var} is set for every child but docs/reference/rwv-each.md does not document it"
        );
    }
}

#[test]
fn arguments_are_values_never_re_split_or_expanded() {
    let w = Weave::standard();
    let out = w.each(&[
        "--json",
        "-j",
        "1",
        "--repo",
        APP,
        "--",
        "git",
        "-c",
        "each.note=two words $HOME;|*",
        "config",
        "each.note",
    ]);
    let env = envelope(&out);
    assert_eq!(
        outcome_of(&env, APP)["stdout"].as_str().unwrap().trim(),
        "two words $HOME;|*"
    );
}

#[test]
fn shell_runs_the_command_string_through_the_platform_shell() {
    let w = Weave::standard();
    let out = w.each(&[
        "--json",
        "-j",
        "1",
        "--repo",
        APP,
        "--shell",
        "echo one&& echo two",
    ]);
    let env = envelope(&out);
    let lines: Vec<&str> = outcome_of(&env, APP)["stdout"]
        .as_str()
        .unwrap()
        .lines()
        .map(str::trim_end)
        .collect();
    assert_eq!(lines, ["one", "two"]);
}

#[test]
fn a_missing_checkout_is_not_run_and_fails_naming_rwv_fetch() {
    let gone = "github/org/gone";
    let w = Weave::with_missing(&[(APP, "owned")], &[(gone, "owned")]);
    let out = w.each(&["--", "git", "status"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains(&format!("{gone}: not present on disk; run `rwv fetch`")),
        "{}",
        stderr(&out)
    );
    let out = w.each(&["--json", "-j", "1", "--", "git", "status"]);
    let env = envelope(&out);
    assert_eq!(outcome_of(&env, gone)["status"], "not-present");
    assert_eq!(outcome_of(&env, APP)["status"], "ok");
}

#[test]
fn fail_fast_starts_no_repo_after_the_first_failure() {
    let w = Weave::standard();
    let out = w.each(&[
        "-j",
        "1",
        "--fail-fast",
        "--role",
        "owned",
        "--role",
        "dependency",
        "--shell",
        "git config each.ran yes && exit 1",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(w.ran(APP));
    assert!(!w.ran(LIB), "no repo starts after the first failure");
    assert!(
        stderr(&out).contains(&format!("  {LIB}: not run")),
        "{}",
        stderr(&out)
    );
}

#[test]
fn without_fail_fast_a_failure_stops_nothing() {
    let w = Weave::standard();
    w.each(&["-j", "1", "--shell", "git config each.ran yes && exit 1"]);
    assert!(w.ran(APP) && w.ran(LIB));
}

#[test]
fn dry_run_runs_nothing_and_prints_the_plan() {
    let w = Weave::standard();
    let out = w.each(&args_then(&["--dry-run", "--"], MARK));
    assert_eq!(out.status.code(), Some(0));
    assert!(!w.ran(APP) && !w.ran(LIB));
    let text = stdout(&out);
    for path in [APP, LIB] {
        assert!(
            text.contains(&format!("{path}: would run `git config each.ran yes` in ")),
            "{text}"
        );
    }
    assert!(!text.contains(DOCS), "{text}");
}

#[test]
fn json_under_one_job_is_one_envelope_with_captured_output_even_on_failure() {
    let w = Weave::standard();
    let out = w.each(&[
        "--json",
        "-j",
        "1",
        "--shell",
        "echo out&& echo err 1>&2&& exit 2",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let env = envelope(&out);
    assert!(env["$schema"]
        .as_str()
        .unwrap()
        .ends_with("/docs/reference/schemas/each.json"));
    assert_eq!(paths_in(&env), [APP, LIB]);
    let app = outcome_of(&env, APP);
    assert_eq!(app["status"], "failed");
    assert_eq!(app["exit_code"], 2);
    assert_eq!(app["role"], "owned");
    assert_eq!(app["stdout"].as_str().unwrap().trim(), "out");
    assert_eq!(app["stderr"].as_str().unwrap().trim(), "err");
    assert!(app["duration_ms"].is_u64());
}

#[test]
fn json_is_one_envelope_whatever_the_worker_count() {
    let w = Weave::standard();
    let shape_under = |jobs: &str| {
        let out = w.each(&["--json", "-j", jobs, "--", "git", "status", "--short"]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        common::envelope_shape(&out.stdout, EACH_SCHEMA_URL, "outcomes")
    };
    let serial = shape_under("1");
    assert_eq!(serial.1, [APP, LIB]);
    assert_eq!(shape_under("2"), serial);
}

#[test]
fn a_command_not_on_path_fails_before_any_repo_runs() {
    let w = Weave::standard();
    let out = w.each(&["--", "rwv-each-test-no-such-program"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("not found on PATH"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_program_found_through_a_relative_path_entry_is_the_same_file_in_every_repo() {
    let w = Weave::new(&[(APP, "owned"), (LIB, "owned")]);
    for (dir, says) in [
        (w.ws.join("tools"), "invocation"),
        (w.ws.join(APP).join("tools"), "checkout"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        common::write_plugin(
            &dir,
            "mytool",
            &format!("echo {says}\n"),
            &format!("echo {says}\n"),
        );
    }
    let path = std::env::join_paths(
        std::iter::once(PathBuf::from("tools")).chain(std::env::split_paths(&path_with_bins())),
    )
    .unwrap();
    let out = w
        .each_cmd(&["-j", "1", "--json", "--", "mytool"])
        .env("PATH", path)
        .output()
        .expect("rwv each runs");
    let env = envelope(&out);
    for repo in [APP, LIB] {
        let outcome = outcome_of(&env, repo);
        assert_eq!(outcome["status"], "ok", "{repo}: {outcome}");
        assert_eq!(
            outcome["stdout"].as_str().unwrap().trim(),
            "invocation",
            "{repo}"
        );
    }
}

#[test]
fn no_command_is_a_usage_error() {
    let w = Weave::standard();
    let out = w.each(&["--role", "owned"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn joblog_has_a_header_and_one_line_per_repo() {
    let w = Weave::standard();
    let log = w.ws.join("job.log");
    let out = w.each(&[
        "-j",
        "2",
        "--joblog",
        log.to_str().unwrap(),
        "--shell",
        "exit 5",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let text = std::fs::read_to_string(&log).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("Seq\tRepo\tStarttime\tJobRuntime\tExitval\tSignal\tCommand")
    );
    let mut rows: Vec<Vec<&str>> = lines.map(|l| l.split('\t').collect()).collect();
    rows.sort_by_key(|r| r[0].to_owned());
    assert_eq!(rows.len(), 2, "{text}");
    for (row, (seq, path)) in rows.iter().zip([("1", APP), ("2", LIB)]) {
        assert_eq!(row[0], seq);
        assert_eq!(row[1], path);
        assert!(row[2].parse::<f64>().unwrap() > 0.0);
        assert!(row[3].parse::<f64>().is_ok());
        assert_eq!(row[4], "5");
        assert_eq!(row[5], "0");
        assert_eq!(row[6], "exit 5");
    }
}

/// The plan line and the joblog's `Command` column quote the command the way
/// POSIX `sh` reads words, on every platform.
#[test]
fn the_plan_and_the_joblog_show_the_command_posix_quoted() {
    const COMMAND: &[&str] = &["git", "config", "each.note", "two words, $HOME's"];
    let shown = r#"git config each.note 'two words, $HOME'\''s'"#;
    let w = Weave::standard();

    let plan = stdout(&w.each(&args_then(&["--dry-run", "--"], COMMAND)));
    assert!(
        plan.contains(&format!("{APP}: would run `{shown}` in ")),
        "{plan}"
    );

    let log = w.ws.join("job.log");
    let mut args = vec!["-j", "1", "--joblog", log.to_str().unwrap(), "--"];
    args.extend(COMMAND);
    let out = w.each(&args);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = std::fs::read_to_string(&log).unwrap();
    let commands: Vec<&str> = text
        .lines()
        .skip(1)
        .map(|l| l.split('\t').nth(6).unwrap())
        .collect();
    assert_eq!(commands, [shown, shown], "{text}");
}

#[test]
fn results_writes_each_repos_output_and_outcome_and_leaves_other_repos_alone() {
    let w = Weave::standard();
    let dir = w.ws.join("results");
    let stale = dir.join(DOCS);
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("exit"), "untouched\n").unwrap();
    std::fs::create_dir_all(dir.join(APP)).unwrap();
    std::fs::write(dir.join(APP).join("stdout"), "stale output that must go\n").unwrap();

    let out = w.each(&[
        "--results",
        dir.to_str().unwrap(),
        "--shell",
        "echo hello&& echo warn 1>&2",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    for path in [APP, LIB] {
        let read = |f: &str| std::fs::read_to_string(dir.join(path).join(f)).unwrap();
        assert_eq!(read("stdout").trim(), "hello");
        assert_eq!(read("stderr").trim(), "warn");
        assert_eq!(read("exit"), "ok\n");
    }
    assert_eq!(
        std::fs::read_to_string(stale.join("exit")).unwrap(),
        "untouched\n"
    );
    assert!(
        stdout(&out).contains("hello"),
        "terminal output is unchanged by --results: {}",
        stdout(&out)
    );
}

#[test]
fn relative_results_and_joblog_under_c_land_under_the_c_directory() {
    let w = Weave::standard();
    let shell_cwd = common::tempdir().unwrap();
    let out = common::rwv_std()
        .arg("-C")
        .arg(&w.ws)
        .args([
            "each",
            "--results",
            "out",
            "--joblog",
            "job.log",
            "--shell",
            "echo hello",
        ])
        .current_dir(shell_cwd.path())
        .env("PATH", path_with_bins())
        .stdin(Stdio::null())
        .output()
        .expect("rwv each runs");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    for path in [APP, LIB] {
        let written = w.ws.join("out").join(path).join("stdout");
        assert_eq!(std::fs::read_to_string(&written).unwrap().trim(), "hello");
    }
    assert!(w.ws.join("job.log").is_file());
    assert!(
        std::fs::read_dir(shell_cwd.path())
            .unwrap()
            .next()
            .is_none(),
        "rwv each wrote under the shell's cwd instead of the -C directory"
    );
}

#[test]
fn progress_draws_nothing_when_stderr_is_not_a_terminal() {
    let w = Weave::standard();
    let plain = w.each(&["-j", "2", "--shell", "echo x&& exit 1"]);
    let with = w.each(&["-j", "2", "--progress", "--shell", "echo x&& exit 1"]);
    let sorted = |s: String| {
        let mut l: Vec<String> = s.lines().map(str::to_owned).collect();
        l.sort();
        l
    };
    assert_eq!(sorted(stderr(&with)), sorted(stderr(&plain)));
    assert!(!stderr(&with).contains("running"), "{}", stderr(&with));
}

#[test]
fn group_prints_each_repos_output_whole_under_its_header() {
    let w = Weave::standard();
    let out = w.each(&["-j", "2", "--group", "--", "git", "log", "--format=%s"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    for path in [APP, LIB] {
        let header = format!("[{path}]\ninit\n");
        assert!(text.contains(&header), "{text}");
    }
    assert!(
        !text.contains("] init"),
        "grouped output is not prefixed: {text}"
    );
}

#[test]
fn many_jobs_prefix_every_line_with_its_repo() {
    let w = Weave::standard();
    let out = w.each(&["-j", "2", "--", "git", "log", "--format=%s"]);
    let text = stdout(&out);
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort();
    assert_eq!(lines, [format!("[{APP}] init"), format!("[{LIB}] init")]);
}

#[test]
fn one_job_prints_a_header_before_each_repo_and_leaves_its_output_unprefixed() {
    let w = Weave::standard();
    let out = w.each(&["-j", "1", "--", "git", "log", "--format=%s"]);
    assert_eq!(stdout(&out), format!("[{APP}]\ninit\n[{LIB}]\ninit\n"));
}

#[test]
fn a_workweave_runs_in_its_own_checkouts_never_the_primarys() {
    let pw = common::build_workspace("demo", &[(APP, "owned")]);
    let created = common::rwv()
        .args(["workweave", "demo", "create", "seat"])
        .current_dir(&pw.workspace)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let mut cmd = common::rwv_std();
    let out = cmd
        .args(["-w", "demo--seat", "each", "--json", "-j", "1", "--"])
        .args(["git", "rev-parse", "--show-toplevel"])
        .current_dir(&pw.workspace)
        .env("PATH", path_with_bins())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let env = envelope(&out);
    let ran_in = PathBuf::from(outcome_of(&env, APP)["stdout"].as_str().unwrap().trim());

    let status: Value = serde_json::from_slice(
        &common::rwv()
            .args(["-w", "demo--seat", "status", "--json"])
            .current_dir(&pw.workspace)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let seat_dir = PathBuf::from(status["repos"][0]["absolute_path"].as_str().unwrap());
    let canonical = |p: &Path| repoweave::path_spelling::resolve(p).unwrap();
    assert_eq!(canonical(&ran_in), canonical(&seat_dir));
    assert_ne!(canonical(&ran_in), canonical(&pw.workspace.join(APP)));
}

/// The command an error message spells between its first pair of backticks,
/// with each `<placeholder>` replaced from `fill`.
fn advised_command(message: &str, fill: &[(&str, &str)]) -> Vec<String> {
    let quoted = message
        .split('`')
        .nth(1)
        .unwrap_or_else(|| panic!("no backticked command in: {message}"));
    quoted
        .split_whitespace()
        .map(|word| {
            fill.iter()
                .find(|(placeholder, _)| *placeholder == word)
                .map_or(word, |(_, value)| *value)
                .to_owned()
        })
        .collect()
}

fn follow_the_activate_advice(w: &Weave) {
    let refused = w.each(&args_then(&["--"], MARK));
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    assert!(!w.ran(APP), "nothing runs without a project");

    let advice = advised_command(&stderr(&refused), &[("<project>", "demo")]);
    assert_eq!(advice[..2], ["rwv", "activate"], "{}", stderr(&refused));
    let followed = common::rwv_std()
        .args(&advice[1..])
        .current_dir(&w.ws)
        .output()
        .unwrap();
    assert!(followed.status.success(), "{}", stderr(&followed));

    let out = w.each(&args_then(&["--"], MARK));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(w.ran(APP) && w.ran(LIB));
}

#[test]
fn a_primary_with_no_project_selected_names_the_step_that_selects_one() {
    let w = Weave::standard();
    std::fs::remove_file(w.ws.join(".rwv-active")).unwrap();
    follow_the_activate_advice(&w);
}

#[test]
fn the_state_doctor_leaves_after_dropping_an_unreadable_pointer_names_the_step_that_selects_one() {
    let w = Weave::standard();
    std::fs::write(w.ws.join(".rwv-active"), "x y\n").unwrap();
    let fixed = common::rwv_std()
        .args(["doctor", "--fix"])
        .current_dir(&w.ws)
        .output()
        .unwrap();
    assert!(
        !w.ws.join(".rwv-active").exists(),
        "doctor --fix keeps the unreadable pointer:\n{}",
        stdout(&fixed)
    );
    follow_the_activate_advice(&w);
}

#[test]
fn outside_any_workspace_addressing_one_with_minus_c_runs_there() {
    let w = Weave::standard();
    let outside = w.ws.parent().unwrap();
    let refused = common::rwv_std()
        .arg("each")
        .args(args_then(&["--"], MARK))
        .current_dir(outside)
        .env("PATH", path_with_bins())
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("-C <path>"),
        "{}",
        stderr(&refused)
    );
    assert!(
        !stderr(&refused).contains("-w "),
        "-w looks its name up in the workspace it starts in, so from outside one it is no advice: {}",
        stderr(&refused)
    );

    let out = common::rwv_std()
        .arg("-C")
        .arg(&w.ws)
        .arg("each")
        .args(args_then(&["--"], MARK))
        .current_dir(outside)
        .env("PATH", path_with_bins())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(w.ran(APP) && w.ran(LIB));
}

/// docs/reference/rwv-each.md, Synopsis: "`--` is accepted but never
/// required"; everything from the first non-option word on, options included,
/// belongs to the command.
#[test]
fn the_command_needs_no_double_dash_and_keeps_its_own_options() {
    let w = Weave::standard();
    let out = w.each(&["--repo", APP, "git", "log", "--format=%s", "-1"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains(&format!("[{APP}] init")),
        "{}",
        stdout(&out)
    );

    let out = w.each(&["--repo", APP, "--", "git", "log", "--format=%s", "-1"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

/// docs/reference/rwv-each.md, Running: "-j N ... Default `min(nproc, 8)`",
/// and the how-to's "default: min(nproc, 8)". Ten repos, so the cap is what
/// limits the concurrency whenever the machine has eight or more cores.
#[test]
fn the_default_job_count_is_the_core_count_capped_at_eight() {
    let repos: Vec<(String, &str)> = (0..10).map(|i| (format!("r/{i}"), "owned")).collect();
    let refs: Vec<(&str, &str)> = repos.iter().map(|(p, r)| (p.as_str(), *r)).collect();
    let w = Weave::new(&refs);
    let log = w.ws.join("concurrency.log");
    let line = format!(
        "echo start >> '{log}'; sleep 1; echo end >> '{log}'",
        log = log.display()
    );
    let out = w.each(&["--shell", &line]);
    assert!(out.status.success(), "{}", stderr(&out));
    let (mut running, mut peak) = (0usize, 0usize);
    for event in std::fs::read_to_string(&log).unwrap().lines() {
        match event {
            "start" => running += 1,
            _ => running -= 1,
        }
        peak = peak.max(running);
    }
    let cores = std::thread::available_parallelism().unwrap().get();
    assert_eq!(peak, cores.min(8));
}

/// docs/reference/rwv-each.md, Running: under `--fail-fast` "repos already
/// running finish".
#[test]
fn fail_fast_lets_repos_already_running_finish() {
    let w = Weave::new(&[(APP, "owned"), (LIB, "owned")]);
    let script =
        format!(r#"if [ "$RWV_REPO" = {APP} ]; then exit 1; fi; sleep 1; git config each.ran yes"#);
    let out = w.each(&["-j", "2", "--fail-fast", "--shell", &script]);
    assert_eq!(out.status.code(), Some(1));
    assert!(w.ran(LIB), "the repo running beside the failure finishes");
    assert!(
        !stderr(&out).contains(&format!("{LIB}:")),
        "{}",
        stderr(&out)
    );
}

/// docs/reference/rwv-each.md, Running: a command that could not be started
/// "is failed", and Records and logs: `exit_code` is `null` when it does not
/// apply.
#[test]
fn a_command_that_cannot_be_started_is_failed_with_no_exit_code() {
    let w = Weave::new(&[(APP, "owned")]);
    let out = w.each(&["--json", "-j", "1", "--", "./no-such-program"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let envelope = envelope(&out);
    let outcome = outcome_of(&envelope, APP);
    assert_eq!(outcome["status"], "failed");
    assert_eq!(outcome["exit_code"], Value::Null);
    assert_eq!(outcome["signal"], Value::Null);
}

/// docs/reference/rwv-each.md, `--joblog`: "`Exitval` is `-1` when the command
/// left no exit code".
#[test]
fn the_joblog_records_exitval_minus_one_when_the_command_left_no_exit_code() {
    let w = Weave::new(&[(APP, "owned")]);
    let log = w.ws.join("job.log");
    let out = w.each(&[
        "-j",
        "1",
        "--joblog",
        log.to_str().unwrap(),
        "--",
        "./no-such-program",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let text = std::fs::read_to_string(&log).unwrap();
    let row: Vec<&str> = text.lines().nth(1).expect("a row").split('\t').collect();
    assert_eq!(row[4], "-1", "{text}");
}

/// docs/reference/rwv-each.md, `--joblog`: "Repos that never started have no
/// line" — neither those a `--fail-fast` halt skipped nor a checkout absent
/// from disk — and "FILE is replaced if it exists".
#[test]
fn the_joblog_has_no_line_for_repos_that_never_started_and_replaces_the_file() {
    let gone = "github/org/gone";
    let w = Weave::with_missing(&[(APP, "owned"), (LIB, "owned")], &[(gone, "owned")]);
    let log = w.ws.join("job.log");
    let log_arg = log.to_str().unwrap();
    let repos_logged = || -> Vec<String> {
        let text = std::fs::read_to_string(&log).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some(JOBLOG_HEADER), "{text}");
        lines
            .map(|l| l.split('\t').nth(1).unwrap().to_owned())
            .collect()
    };

    std::fs::write(&log, "left over from an earlier run\n").unwrap();
    w.each(&["-j", "1", "--joblog", log_arg, "--shell", "exit 0"]);
    assert_eq!(repos_logged(), [APP, LIB]);

    w.each(&[
        "-j",
        "1",
        "--fail-fast",
        "--joblog",
        log_arg,
        "--shell",
        "exit 1",
    ]);
    assert_eq!(repos_logged(), [APP]);
}

/// docs/reference/rwv-each.md, Exit status: exit `1` when "the selection could
/// not be resolved".
#[test]
fn a_selection_that_cannot_be_resolved_exits_1_and_runs_nothing() {
    let w = Weave::standard();
    let out = w.each(&args_then(&["--repo", "re:(", "--"], MARK));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(!w.ran(APP) && !w.ran(LIB) && !w.ran(DOCS));
}

/// docs/reference/rwv-each.md, Which checkout, which repos: "The project repo
/// is not a manifest member and is never run in."
#[test]
fn the_project_repo_is_never_run_in() {
    let w = Weave::standard();
    let out = w.each(&["--repo", "re:.*", "--dry-run", "--", "true"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for repo in [APP, LIB, DOCS] {
        assert!(text.contains(&format!("  {repo}: would run")), "{text}");
    }
    assert_eq!(text.matches("would run").count(), 3, "{text}");
    assert!(!text.contains("projects/demo"), "{text}");
}

#[test]
fn job_helper() {
    common::job::serve();
}

impl Weave {
    /// `rwv each` running a [`common::job::TREE_ROOT`] in each repo: a
    /// process with a child of its own that ignores interrupts and would
    /// outlive it by a minute.
    fn each_tree(&self, flags: &[&str]) -> Command {
        let exe = common::job::helper_exe();
        let mut args: Vec<&str> = flags.to_vec();
        args.push("--");
        args.push(exe.to_str().expect("a UTF-8 test binary path"));
        args.extend(common::job::helper_args());
        let mut cmd = self.each_cmd(&args);
        cmd.env(common::job::ROLE, common::job::TREE_ROOT);
        cmd
    }

    /// [`Weave::each_tree`] with a [`common::job::ORPHANING_ROOT`]: the leaf
    /// holds none of the command's output, so only its tree reaches it.
    fn each_orphaning_tree(&self, flags: &[&str]) -> Command {
        let mut cmd = self.each_tree(flags);
        cmd.env(common::job::ROLE, common::job::ORPHANING_ROOT);
        cmd
    }
}

#[test]
fn a_timeout_kills_the_whole_tree_grandchildren_included() {
    let w = Weave::standard();
    let start = Instant::now();
    let out = w
        .each_tree(&["--json", "-j", "2", "--timeout", "2"])
        .output()
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(30));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let outcomes = envelope(&out)["outcomes"].as_array().unwrap().clone();
    assert_eq!(outcomes.len(), 2, "{}", stdout(&out));
    for outcome in outcomes {
        assert_eq!(outcome["status"], "timed-out", "{outcome}");
    }
    for path in [APP, LIB] {
        let pid = common::job::leaf_pid(&w.ws.join(path), Duration::ZERO);
        common::job::wait_dead(pid);
    }
}

#[test]
fn an_interrupt_reaches_every_running_tree_starts_no_more_and_exits_130() {
    let w = Weave::new(&[("a/one", "owned"), ("a/two", "owned"), ("a/three", "owned")]);
    let mut cmd = w.each_tree(&["-j", "2"]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = common::job::as_foreground_job(&mut cmd).spawn().unwrap();
    let pids: Vec<u32> = ["a/one", "a/three"]
        .iter()
        .map(|p| common::job::leaf_pid(&w.ws.join(p), Duration::from_secs(20)))
        .collect();
    common::job::interrupt_foreground_job(&child);
    let interrupted_at = Instant::now();
    let out = common::job::wait_job_with_output(child, common::job::JOB_BOUND)
        .unwrap_or_else(|e| panic!("the interrupted job never ended: {e}"));
    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
    assert!(
        interrupted_at.elapsed() < Duration::from_secs(20),
        "rwv each waited out a grandchild that ignores interrupts instead of ending its tree"
    );
    for pid in pids {
        common::job::wait_dead(pid);
    }
    assert!(
        !w.ws.join("a/two").join(common::job::LEAF_PID_FILE).exists(),
        "no repo starts after an interrupt"
    );
    assert!(stderr(&out).contains("a/two: not run"), "{}", stderr(&out));
}

#[test]
fn after_an_interrupt_what_a_command_left_running_is_ended_even_when_it_holds_no_output() {
    for jobs in ["2", "1"] {
        let w = Weave::new(&[(APP, "owned")]);
        let mut cmd = w.each_orphaning_tree(&["-j", jobs]);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = common::job::as_foreground_job(&mut cmd).spawn().unwrap();
        let leaf = common::job::leaf_pid(&w.ws.join(APP), Duration::from_secs(20));
        common::job::interrupt_foreground_job(&child);
        let out = common::job::wait_job_with_output(child, common::job::JOB_BOUND)
            .unwrap_or_else(|e| panic!("-j {jobs}: the interrupted job never ended: {e}"));
        assert_eq!(out.status.code(), Some(130), "-j {jobs}: {}", stderr(&out));
        common::job::wait_dead(leaf);
    }
}

#[test]
fn a_lone_dash_for_the_joblog_or_results_is_refused_and_nothing_runs() {
    let w = Weave::new(&[(APP, "owned")]);
    for flag in ["--joblog", "--results"] {
        let out = w.each(&args_then(&[flag, "-", "--"], MARK));
        assert_eq!(out.status.code(), Some(2), "{flag}: {}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains("stdout carries the commands' output"), "{err}");
        assert!(err.contains("`./-`"), "{err}");
        assert!(!w.ws.join("-").exists(), "{flag} - wrote a file named -");
        assert!(!w.ran(APP), "{flag} -: a repo ran");
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::Write;

    /// `rwv each` as the foreground job of a terminal of its own, which the
    /// test types into through `master`.
    fn on_a_terminal(mut cmd: Command) -> (std::process::Child, std::fs::File) {
        use std::os::unix::process::CommandExt;
        let pty = nix::pty::openpty(None, None).expect("a pty");
        cmd.stdin(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
            .stderr(Stdio::from(pty.slave));
        // SAFETY: setsid and ioctl are async-signal-safe, and nothing else
        // runs between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid()?;
                if nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("rwv each starts on the terminal");
        drop(cmd);
        let master = std::fs::File::from(pty.master);
        let mut screen = master.try_clone().unwrap();
        std::thread::spawn(move || std::io::copy(&mut screen, &mut std::io::sink()));
        (child, master)
    }

    #[test]
    fn ctrl_c_typed_under_one_job_ends_what_the_command_left_running() {
        let w = Weave::new(&[(APP, "owned")]);
        let (mut child, mut master) = on_a_terminal(w.each_orphaning_tree(&["-j", "1"]));
        let leaf = common::job::leaf_pid(&w.ws.join(APP), Duration::from_secs(20));
        master.write_all(b"\x03").unwrap();
        let status = common::job::wait_job(&mut child, common::job::JOB_BOUND)
            .unwrap_or_else(|e| panic!("the interrupted job never ended: {e}"));
        assert_eq!(status.code(), Some(130));
        common::job::wait_dead(leaf);
    }

    #[test]
    fn concurrent_output_lines_never_interleave_mid_line() {
        let repos: Vec<(String, &str)> = (0..6).map(|i| (format!("r/{i}"), "owned")).collect();
        let refs: Vec<(&str, &str)> = repos.iter().map(|(p, r)| (p.as_str(), *r)).collect();
        let w = Weave::new(&refs);
        let body = "x".repeat(3000);
        let line = format!(
            r#"i=0; while [ $i -lt 300 ]; do echo "$RWV_REPO:{body}:$i"; echo "$RWV_REPO:{body}:$i" >&2; i=$((i+1)); done"#
        );
        let out = w.each(&["-j", "6", "--shell", &line]);
        assert_eq!(out.status.code(), Some(0));
        for (stream, text) in [("stdout", stdout(&out)), ("stderr", stderr(&out))] {
            let mut count = 0;
            for l in text.lines() {
                let (prefix, rest) = l.split_once("] ").unwrap_or_else(|| {
                    panic!("{stream} line without a prefix: {}", &l[..l.len().min(80)])
                });
                let repo = prefix.trim_start_matches('[');
                let mut parts = rest.split(':');
                assert_eq!(
                    parts.next(),
                    Some(repo),
                    "{stream}: a line carries another repo's text"
                );
                assert_eq!(parts.next(), Some(body.as_str()), "{stream}: torn line");
                assert!(parts.next().unwrap().parse::<u32>().is_ok());
                count += 1;
            }
            assert_eq!(count, 6 * 300, "{stream}");
        }
    }

    #[test]
    fn one_job_hands_the_command_this_process_stdin_and_many_jobs_give_it_none() {
        let w = Weave::new(&[(APP, "owned")]);
        let script = "read line; echo \"got:$line\"";
        let run = |jobs: &str| {
            let mut child = w
                .each_cmd(&["-j", jobs, "--shell", script])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(b"typed\n").unwrap();
            stdout(&child.wait_with_output().unwrap())
        };
        assert!(run("1").contains("got:typed"), "{}", run("1"));
        assert!(run("2").contains("got:\n"), "{}", run("2"));
    }

    /// docs/reference/rwv-each.md, Synopsis: everything after the first word
    /// that is not an option "is passed to the command unchanged", including
    /// words `rwv each` itself spells as options.
    #[test]
    fn options_after_the_command_word_belong_to_the_command() {
        let w = Weave::new(&[(APP, "owned")]);
        let out = w.each(&[
            "--repo",
            APP,
            "sh",
            "-c",
            "echo \"$@\"",
            "_",
            "-j",
            "1",
            "--json",
            "--group",
        ]);
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(stdout(&out), format!("[{APP}] -j 1 --json --group\n"));
    }

    /// docs/reference/rwv-each.md, Output: "Under `-j 1`, `--results` and
    /// `--progress` also detach the terminal: output is read line by line,
    /// under the header, unprefixed."
    #[test]
    fn one_job_with_results_or_progress_gives_the_command_no_stdin_and_a_plain_header() {
        let w = Weave::new(&[(APP, "owned")]);
        let results = w.ws.join("results");
        let script = "read line; echo \"got:$line\"";
        for flags in [
            vec!["--results", results.to_str().unwrap()],
            vec!["--progress"],
        ] {
            let mut args = vec!["-j", "1"];
            args.extend(&flags);
            args.extend(["--shell", script]);
            let mut child = w
                .each_cmd(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(b"typed\n").unwrap();
            let out = stdout(&child.wait_with_output().unwrap());
            assert_eq!(out, format!("[{APP}]\ngot:\n"), "{flags:?}");
        }
    }

    /// docs/reference/rwv-each.md, Exit status and interrupts: "A command that
    /// leaves something running after a normal exit keeps it."
    #[test]
    fn a_process_left_running_after_a_normal_exit_is_kept() {
        struct Reap(u32);
        impl Drop for Reap {
            fn drop(&mut self) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(self.0 as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
        let w = Weave::new(&[(APP, "owned")]);
        let pid_file = w.ws.join("left.pid");
        let script = format!(
            "sleep 60 >/dev/null 2>&1 </dev/null & echo $! > '{}'",
            pid_file.display()
        );
        let out = w.each(&["--shell", &script]);
        assert!(out.status.success(), "{}", stderr(&out));
        let left = Reap(common::job::recorded_pid(
            &pid_file,
            Duration::from_secs(10),
        ));
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            common::job::alive(left.0),
            "the left-behind process was ended"
        );
    }

    /// docs/reference/rwv-each.md, Output and the how-to's `-j 1` recipe: under
    /// `-j 1` on a terminal, "colors, prompts and pagers work" because the
    /// command's process group is handed the terminal. A command that changes
    /// the terminal's modes and reads it is stopped by the kernel, and never
    /// answers, when it is not the foreground group.
    #[test]
    fn one_job_on_a_terminal_lets_the_command_prompt_and_set_terminal_modes() {
        use std::os::unix::process::CommandExt;
        use std::sync::{Arc, Mutex};

        let w = Weave::new(&[(APP, "owned")]);
        let pty = nix::pty::openpty(None, None).expect("a pseudo-terminal");
        let script = "stty -echo; printf 'ok? '; read a; echo \"answer=$a\"";
        let mut cmd = w.each_cmd(&["-j", "1", "--shell", script]);
        cmd.stdin(pty.slave.try_clone().unwrap())
            .stdout(pty.slave.try_clone().unwrap())
            .stderr(pty.slave.try_clone().unwrap());
        // SAFETY: only async-signal-safe calls run between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid()?;
                if nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().unwrap();
        drop(cmd);
        drop(pty.slave);

        let screen = Arc::new(Mutex::new(String::new()));
        let mut master = std::fs::File::from(pty.master);
        let mut typing = master.try_clone().unwrap();
        let mirror = Arc::clone(&screen);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = std::io::Read::read(&mut master, &mut buf) {
                if n == 0 {
                    break;
                }
                mirror
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });

        let deadline = Instant::now() + Duration::from_secs(30);
        let wait_for = |what: &str| {
            while !screen.lock().unwrap().contains(what) {
                if Instant::now() > deadline {
                    let _ = nix::sys::signal::killpg(
                        nix::unistd::Pid::from_raw(child.id() as i32),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                    panic!(
                        "never saw {what:?}; terminal showed {:?}",
                        screen.lock().unwrap()
                    );
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait_for("ok? ");
        typing.write_all(b"yes\n").unwrap();
        wait_for("answer=yes");
        assert!(child.wait().unwrap().success());
    }
}
