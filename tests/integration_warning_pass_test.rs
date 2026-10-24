//! The integration warnings a verb prints about the files it leaves behind.
//!
//! `rwv activate`, `rwv materialize` and `rwv workweave create` author nothing
//! and report a missing managed file the same way, once per file. `rwv init
//! <name>` authors what its empty manifest implies before it activates, so a
//! new project starts with nothing to report.

use std::path::{Path, PathBuf};

mod common;

const MISSING: &str = "managed file missing: ";

struct Run {
    stderr: String,
}

fn run(args: &[&str], cwd: &Path) -> Run {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "`rwv {}` should succeed; stderr:\n{stderr}",
        args.join(" ")
    );
    Run { stderr }
}

fn warning_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.to_lowercase().contains("warning"))
        .collect()
}

fn assert_no_warning(verb: &str, run: &Run) {
    let warnings = warning_lines(&run.stderr);
    assert!(
        warnings.is_empty(),
        "`{verb}` should print no warning; got:\n{}",
        run.stderr
    );
}

/// `rwv doctor --json`'s integration findings, as `(kind, integration)` pairs.
fn doctor_issues(ws: &Path) -> Vec<(String, String)> {
    let output = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(ws)
        .output()
        .expect("rwv doctor should run");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor --json prints one JSON document");
    report["issues"]
        .as_array()
        .expect("doctor --json carries an `issues` array")
        .iter()
        .map(|issue| {
            (
                issue["kind"].as_str().unwrap_or_default().to_string(),
                issue["integration"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

#[test]
fn a_fresh_init_prints_no_warning_and_leaves_doctor_nothing_to_report() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let init = run(&["init", "myapp"], &ws);

    assert_no_warning("rwv init myapp", &init);
    assert_eq!(
        doctor_issues(&ws),
        Vec::<(String, String)>::new(),
        "a fresh project should leave no integration finding behind"
    );
}

#[test]
fn init_add_fetch_activate_prints_no_warning_and_refuses_nothing() {
    let tmp = common::tempdir().unwrap();
    let remote = tmp.path().join("remotes/lib.git");
    std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&remote);
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let url = common::file_url(&remote);
    for args in [
        &["init", "myapp"][..],
        &["add", &url],
        &["fetch"],
        &["activate", "myapp"],
    ] {
        let verb = format!("rwv {}", args.join(" "));
        assert_no_warning(&verb, &run(args, &ws));
    }
}

/// A weave whose one project declares no repos and has never been authored, so
/// `vscode-workspace`'s file is the one managed file it lacks.
fn unauthored_project(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    let project_dir = ws.join("projects/myapp");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), "[repositories]\n").unwrap();
    std::fs::create_dir_all(tmp.join(".workweaves")).unwrap();
    ws
}

/// The one line `run` printed naming a missing managed file, split into the
/// text around the path and the path itself.
fn the_missing_file_line(verb: &str, run: &Run) -> (String, String) {
    let lines: Vec<&str> = run
        .stderr
        .lines()
        .filter(|line| line.contains(MISSING))
        .collect();
    let [line] = lines.as_slice() else {
        panic!(
            "`{verb}` should report the missing file on exactly one line; got:\n{}",
            run.stderr
        );
    };
    let (before, rest) = line.split_once(MISSING).unwrap();
    let (path, after) = rest
        .split_once("; ")
        .unwrap_or_else(|| panic!("`{verb}` printed no remedy after the path: {line}"));
    (
        format!("{before}{MISSING}<path>; {after}"),
        path.to_string(),
    )
}

#[test]
fn activate_materialize_and_workweave_create_report_a_missing_managed_file_alike() {
    let tmp = common::tempdir().unwrap();
    let ws = unauthored_project(tmp.path());
    let file = "projects/myapp/myapp.code-workspace";

    let activate = run(&["activate", "myapp", "--no-materialize"], &ws);
    let materialize = run(&["materialize"], &ws);
    let create = run(&["workweave", "myapp", "create", "feat"], &ws);

    let seat = common::under(tmp.path(), ".workweaves/myapp--feat");
    let reports = [
        ("rwv activate", &activate, common::under(&ws, file)),
        ("rwv materialize", &materialize, common::under(&ws, file)),
        ("rwv workweave create", &create, common::under(&seat, file)),
    ];
    let expected_shape = "[warning (drift)] vscode-workspace: vscode-workspace managed file \
                          missing: <path>; run rwv doctor --fix to regenerate";
    for (verb, run, path) in reports {
        let (shape, named) = the_missing_file_line(verb, run);
        assert_eq!(
            shape, expected_shape,
            "`{verb}` words the report differently"
        );
        assert_eq!(
            named,
            path.display().to_string(),
            "`{verb}` should name the file in the checkout it acted on"
        );
        assert!(!path.exists(), "`{verb}` authors nothing");
    }
}

#[test]
fn vscode_workspace_reports_its_missing_file_once() {
    let tmp = common::tempdir().unwrap();
    let ws = unauthored_project(tmp.path());

    let activate = run(&["activate", "myapp", "--no-materialize"], &ws);
    let vscode_warnings: Vec<&str> = warning_lines(&activate.stderr)
        .into_iter()
        .filter(|line| line.contains("vscode-workspace"))
        .collect();
    assert_eq!(
        vscode_warnings.len(),
        1,
        "`rwv activate` should report the missing .code-workspace once; got:\n{}",
        activate.stderr
    );

    let missing_in_doctor = doctor_issues(&ws)
        .into_iter()
        .filter(|(kind, integration)| {
            kind == "managed-file-missing" && integration == "vscode-workspace"
        })
        .count();
    assert_eq!(
        missing_in_doctor, 1,
        "`rwv doctor` should carry one managed-file-missing finding for the .code-workspace"
    );
}

/// A one-crate source repo with no dependencies, so `cargo` writes its lock
/// without reaching a registry.
fn crate_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "").unwrap();
    common::git_in(dir, &["init", "-q"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(
        dir,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "lib",
        ],
    );
}

/// The install hook is what writes a missing `Cargo.lock`, so a report taken
/// after it has nothing to say about that file.
#[test]
fn activate_does_not_report_the_lock_its_install_hook_writes() {
    if common::skip_without_tool("cargo") {
        return;
    }
    let tmp = common::tempdir().unwrap();
    let source = tmp.path().join("src/lib");
    crate_repo(&source);
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    run(&["init", "myapp"], &ws);
    run(&["add", &common::file_url(&source)], &ws);
    let lock = ws.join("projects/myapp/Cargo.lock");
    assert!(lock.exists(), "`rwv add` should leave a Cargo.lock behind");
    std::fs::remove_file(&lock).unwrap();

    let activate = run(&["activate", "myapp"], &ws);

    assert!(
        lock.exists(),
        "`rwv activate`'s install hook writes the lock"
    );
    let lock_warnings: Vec<&str> = warning_lines(&activate.stderr)
        .into_iter()
        .filter(|line| line.contains("Cargo.lock"))
        .collect();
    assert!(
        lock_warnings.is_empty(),
        "`rwv activate` should not report the lock its hook writes; got:\n{}",
        activate.stderr
    );
}
