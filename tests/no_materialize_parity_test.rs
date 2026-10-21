//! Which verbs run the integration install hooks, and that each of them takes
//! `--no-materialize` to skip them.
//!
//! `rwv workweave create` runs them in the new seat by default; so do the
//! authoring verbs at primary, a SOURCE fetch's first activation, and `init
//! --adopt`. Each run below is paired with its control: the same verb without
//! the flag, which must reach the hook, so a fixture that never reached the
//! hooks cannot pass for one whose flag suppressed them.
//!
//! Unix only: the `npm` stand-in is a shebang script a child resolves off
//! `PATH`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

mod common;

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// A bare repo holding `files`, returned with its `file://` URL.
fn remote(tmp: &Path, name: &str, files: &[(&str, &str)]) -> String {
    let work = tmp.join("src").join(name);
    for (path, body) in files {
        let file = work.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    git_init_with_commit(&work);
    let bare = tmp.join(format!("{name}.git"));
    common::git_in(
        tmp,
        &[
            "clone",
            "--bare",
            &work.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    common::file_url(&bare)
}

fn package(name: &str) -> String {
    format!("{{\"name\": \"{name}\", \"version\": \"0.1.0\"}}\n")
}

fn manifest(members: &[(&str, &str)]) -> String {
    let mut toml = String::new();
    for (path, url) in members {
        toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\n\
             version = \"main\"\nrole = \"owned\"\n\n"
        ));
    }
    toml.push_str(
        "[integrations.npm-workspaces]\nenabled = true\n\n\
         [integrations.vscode-workspace]\nenabled = false\n",
    );
    toml
}

/// A weave whose committed project `app` lists one npm member, with a `PATH`
/// holding `git` and an `npm` that records each run (its working directory)
/// and prints to stdout.
struct Weave {
    tmp: tempfile::TempDir,
    ws: PathBuf,
    bin: PathBuf,
    witness: PathBuf,
    member_url: String,
}

impl Weave {
    fn new() -> Self {
        let tmp = common::tempdir().unwrap();
        let url = remote(tmp.path(), "web", &[("package.json", &package("web"))]);
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join("projects/app")).unwrap();
        common::git_in(
            tmp.path(),
            &["clone", &url, &ws.join("github/acme/web").to_string_lossy()],
        );
        std::fs::write(
            ws.join("projects/app/rwv.toml"),
            manifest(&[("github/acme/web", &url)]),
        )
        .unwrap();
        git_init_with_commit(&ws.join("projects/app"));
        std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(which::which("git").unwrap(), bin.join("git")).unwrap();
        let witness = tmp.path().join("npm-ran");
        let weave = Weave {
            tmp,
            ws,
            bin,
            witness,
            member_url: url,
        };
        weave.npm(0);
        weave.ok(&["doctor", "--fix"]);
        weave.commit_project("authored");
        weave
    }

    fn npm(&self, exit: i32) {
        let npm = self.bin.join("npm");
        std::fs::write(
            &npm,
            format!(
                "#!/bin/sh\necho \"$PWD\" >> {}\necho npm-noise-on-stdout\nexit {exit}\n",
                self.witness.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn commit_project(&self, message: &str) {
        let project = self.ws.join("projects/app");
        common::git_in(&project, &["add", "-A"]);
        common::git_in(&project, &["commit", "-q", "-m", message]);
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> (bool, String, String) {
        self.run_with_stdin(cwd, args, "")
    }

    fn run_with_stdin(&self, cwd: &Path, args: &[&str], stdin: &str) -> (bool, String, String) {
        let out = common::rwv()
            .args(args)
            .current_dir(cwd)
            .env("PATH", &self.bin)
            .write_stdin(stdin)
            .output()
            .expect("rwv should run");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, stdout, stderr) = self.run_in(&self.ws, args);
        assert!(ok, "`rwv {}` failed:\n{stdout}\n{stderr}", args.join(" "));
        format!("{stdout}\n{stderr}")
    }

    /// The working directories `npm` ran in since the last call.
    fn npm_runs(&self) -> Vec<PathBuf> {
        let runs = std::fs::read_to_string(&self.witness)
            .unwrap_or_default()
            .lines()
            .map(PathBuf::from)
            .collect();
        let _ = std::fs::remove_file(&self.witness);
        runs
    }

    /// Run `args` with `--no-materialize`, then without it, asserting the hook
    /// ran only the second time.
    fn assert_flag_suppresses_hooks(&self, args: &[&str]) {
        let _ = self.npm_runs();
        let mut flagged = args.to_vec();
        flagged.push("--no-materialize");
        let report = self.ok(&flagged);
        assert_eq!(
            self.npm_runs(),
            Vec::<PathBuf>::new(),
            "`rwv {}` must run no install hook:\n{report}",
            flagged.join(" ")
        );
        let report = self.ok(args);
        assert!(
            !self.npm_runs().is_empty(),
            "control: `rwv {}` must reach the install hook in this fixture:\n{report}",
            args.join(" ")
        );
    }
}

#[test]
fn the_authoring_verbs_take_no_materialize() {
    let weave = Weave::new();
    let url = remote(
        weave.tmp.path(),
        "web2",
        &[("package.json", &package("web2"))],
    );

    weave.assert_flag_suppresses_hooks(&["add", &url]);
    let added = std::fs::read_to_string(weave.ws.join("projects/app/rwv.toml")).unwrap();
    let member = added
        .lines()
        .filter_map(|l| l.strip_prefix("[repositories.\""))
        .filter_map(|l| l.strip_suffix("\"]"))
        .find(|path| *path != "github/acme/web")
        .expect("fixture: `rwv add` should have recorded the new member")
        .to_string();

    let adopted = remote(
        weave.tmp.path(),
        "web3",
        &[("package.json", &package("web3"))],
    );
    common::git_in(
        weave.tmp.path(),
        &[
            "clone",
            &adopted,
            &weave.ws.join("github/acme/web3").to_string_lossy(),
        ],
    );
    weave.assert_flag_suppresses_hooks(&["add", "--adopt", "github/acme/web3"]);

    weave.assert_flag_suppresses_hooks(&["role", &member, "owned"]);
    weave.assert_flag_suppresses_hooks(&["update"]);
    weave.assert_flag_suppresses_hooks(&["remove", &member]);
}

#[test]
fn workweave_create_runs_the_install_hooks_in_the_new_seat() {
    let weave = Weave::new();
    let _ = weave.npm_runs();

    weave.ok(&["workweave", "app", "create", "seat"]);
    let seat = weave.tmp.path().join(".workweaves/app--seat");
    let runs = weave.npm_runs();
    assert!(
        runs.iter()
            .any(|cwd| repoweave::path_spelling::resolve(cwd).ok()
                == repoweave::path_spelling::resolve(&seat).ok()),
        "create must run the install hook from the new seat's root; npm ran in {runs:?}"
    );

    weave.ok(&["workweave", "app", "create", "bare", "--no-materialize"]);
    assert_eq!(
        weave.npm_runs(),
        Vec::<PathBuf>::new(),
        "`create --no-materialize` must run no install hook"
    );
}

#[test]
fn hook_mode_create_prints_only_the_path_while_the_hooks_write_to_stdout() {
    let weave = Weave::new();
    let _ = weave.npm_runs();

    let (ok, stdout, stderr) = weave.run_in(
        &weave.ws,
        &["workweave", "app", "--hook-mode", "create", "seat"],
    );
    assert!(ok, "hook-mode create failed:\n{stdout}\n{stderr}");
    assert!(
        !weave.npm_runs().is_empty(),
        "fixture: the hook must have run for its stdout to matter"
    );
    let seat = weave.tmp.path().join(".workweaves/app--seat");
    assert_eq!(
        repoweave::path_spelling::resolve(&PathBuf::from(stdout.trim_end())).ok(),
        repoweave::path_spelling::resolve(&seat).ok(),
        "stdout must be the bare path the agent runtime reads; got:\n{stdout}"
    );
    assert!(
        stderr.contains("npm-noise-on-stdout"),
        "the tool's own output still reaches the operator, on stderr:\n{stderr}"
    );
}

#[test]
fn a_hook_that_fails_at_create_keeps_the_seat_and_names_materialize() {
    let weave = Weave::new();
    weave.npm(1);

    let (ok, stdout, stderr) = weave.run_in(&weave.ws, &["workweave", "app", "create", "seat"]);
    assert!(
        !ok,
        "a failed install hook still fails the verb:\n{stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("only its install hooks did not complete")
            && stderr.contains("rwv materialize"),
        "the failure must say the seat exists and name the verb that finishes it:\n{stderr}"
    );
    let seat = weave.tmp.path().join(".workweaves/app--seat");
    assert!(
        seat.join(".rwv-workweave").is_file(),
        "the seat must survive a failure at its hooks"
    );
    assert!(
        weave.ok(&["workweave", "app", "list"]).contains("seat"),
        "and stay registered, so it can be addressed and finished"
    );
}

/// Under `--hook-mode` an agent runtime is waiting for the seat's path, and a
/// seat whose hooks failed is still a seat: the create exits 0, prints the
/// path as usual, and the failure is a warning naming `rwv materialize`.
#[test]
fn hook_mode_create_hands_over_a_seat_whose_hooks_failed() {
    let weave = Weave::new();
    weave.npm(1);

    let (ok, stdout, stderr) = weave.run_in(
        &weave.ws,
        &["workweave", "app", "--hook-mode", "create", "seat"],
    );
    assert!(
        !weave.npm_runs().is_empty(),
        "fixture: the failing hook must have run:\n{stderr}"
    );
    assert!(
        ok,
        "hook-mode create must not fail over its hooks:\n{stdout}\n{stderr}"
    );
    assert_hook_failure_handed_over(&weave, "seat", &stdout, &stderr);
}

/// The hook `rwv setup claude` registers for `WorktreeCreate` reads its
/// request from stdin, and hands the seat over on the same terms.
#[test]
fn claude_hook_create_hands_over_a_seat_whose_hooks_failed() {
    let weave = Weave::new();
    weave.npm(1);

    let payload = format!(
        "{{\"hook_event_name\": \"WorktreeCreate\", \"cwd\": {}, \"branch_name\": \"seat\"}}",
        serde_json::to_string(&weave.ws.to_string_lossy()).unwrap()
    );
    let (ok, stdout, stderr) =
        weave.run_with_stdin(&weave.ws, &["workweave", "--claude-hook"], &payload);
    assert!(
        !weave.npm_runs().is_empty(),
        "fixture: the failing hook must have run:\n{stderr}"
    );
    assert!(
        ok,
        "the WorktreeCreate hook must not fail over its hooks:\n{stdout}\n{stderr}"
    );
    assert_hook_failure_handed_over(&weave, "seat", &stdout, &stderr);
}

fn assert_hook_failure_handed_over(weave: &Weave, name: &str, stdout: &str, stderr: &str) {
    let seat = weave.tmp.path().join(format!(".workweaves/app--{name}"));
    assert_eq!(
        repoweave::path_spelling::resolve(&PathBuf::from(stdout.trim_end())).ok(),
        repoweave::path_spelling::resolve(&seat).ok(),
        "stdout must be the bare path, as for a clean create; got:\n{stdout}"
    );
    assert!(
        stderr.contains("warning")
            && stderr.contains("only its install hooks did not complete")
            && stderr.contains("rwv materialize"),
        "the failure must be warned about, naming the verb that finishes it:\n{stderr}"
    );
    assert!(
        weave.ok(&["workweave", "app", "list"]).contains(name),
        "the seat is registered"
    );
}

#[test]
fn a_source_fetch_takes_no_materialize_for_its_first_activation() {
    let weave = Weave::new();
    let project = weave.ws.join("projects/app");
    let source = weave.tmp.path().join("app.git");
    common::git_in(
        weave.tmp.path(),
        &[
            "clone",
            "--bare",
            &project.to_string_lossy(),
            &source.to_string_lossy(),
        ],
    );
    let source = common::file_url(&source);

    for (dir, flagged) in [("flagged", true), ("control", false)] {
        let _ = weave.npm_runs();
        let fresh = weave.tmp.path().join(dir);
        std::fs::create_dir_all(&fresh).unwrap();
        let mut args = vec!["fetch", source.as_str()];
        if flagged {
            args.push("--no-materialize");
        }
        let (ok, stdout, stderr) = weave.run_in(&fresh, &args);
        assert!(ok, "`rwv {}` failed:\n{stdout}\n{stderr}", args.join(" "));
        assert_eq!(
            weave.npm_runs().is_empty(),
            flagged,
            "`rwv {}`: the first activation's hooks ran iff the flag was absent:\n{stderr}",
            args.join(" ")
        );
        assert_eq!(
            std::fs::read_to_string(fresh.join(".rwv-active"))
                .unwrap_or_default()
                .trim(),
            "app",
            "the flag skips the hooks, not the activation"
        );
    }
}

/// An in-place fetch activates the weave's only project when none is active,
/// and that activation's hooks honor the flag as a bootstrap fetch's do.
#[test]
fn an_in_place_fetch_takes_no_materialize_for_the_project_it_selects() {
    let weave = Weave::new();

    for flagged in [true, false] {
        std::fs::remove_file(weave.ws.join(".rwv-active")).unwrap();
        let _ = weave.npm_runs();
        let mut args = vec!["fetch"];
        if flagged {
            args.push("--no-materialize");
        }
        let report = weave.ok(&args);
        assert_eq!(
            weave.npm_runs().is_empty(),
            flagged,
            "`rwv {}`: the selected project's hooks ran iff the flag was absent:\n{report}",
            args.join(" ")
        );
        assert_eq!(
            std::fs::read_to_string(weave.ws.join(".rwv-active"))
                .unwrap_or_default()
                .trim(),
            "app",
            "the flag skips the hooks, not the selection:\n{report}"
        );
    }
}

#[test]
fn init_adopt_takes_no_materialize() {
    let weave = Weave::new();

    for (name, flagged) in [("adopted", true), ("control", false)] {
        let _ = weave.npm_runs();
        let source = remote(
            weave.tmp.path(),
            name,
            &[(
                "rwv.toml",
                &manifest(&[("github/acme/web", &weave.member_url)]),
            )],
        );
        let mut args = vec!["init", "--adopt", source.as_str()];
        if flagged {
            args.push("--no-materialize");
        }
        weave.ok(&args);
        assert_eq!(
            weave.npm_runs().is_empty(),
            flagged,
            "`rwv {}`: the adopted project's hooks ran iff the flag was absent",
            args.join(" ")
        );
    }
}

#[test]
fn init_no_materialize_without_adopt_is_a_usage_error() {
    let weave = Weave::new();

    let (ok, _, stderr) = weave.run_in(&weave.ws, &["init", "other", "--no-materialize"]);
    assert!(
        !ok && stderr.contains("--adopt"),
        "`init --no-materialize` without `--adopt` must be a usage error naming it:\n{stderr}"
    );
    assert!(
        !weave.ws.join("projects/other").exists(),
        "and must create nothing"
    );
}
