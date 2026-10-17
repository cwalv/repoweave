//! A verb that skips a materialization side effect it performs on other runs
//! says so: one line per skipped effect, naming the verb that performs it,
//! and a typed record in `--json` where the verb has an envelope. A run that
//! skipped nothing says nothing.
//!
//! Each skip path is asserted beside its control, the same verb on a run where
//! the effect happened, so the line's absence there is measured rather than
//! assumed.
//!
//! Unix only: the `npm` stand-in is a shebang script a child resolves off
//! `PATH`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

mod common;

const SKIPPED: &str = "[skipped] core:";
const NO_MATERIALIZE: &str =
    "[skipped] core: the install hooks did not run (--no-materialize); `rwv materialize` runs them";
const INSIDE_WORKWEAVE: &str = "[skipped] core: the install hooks did not run (an authoring verb \
                                runs none inside a workweave); `rwv materialize` runs them";

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

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

/// A weave whose committed project `app` lists one npm member, on a `PATH`
/// holding `git` and, unless removed, an `npm` that records each run.
struct Weave {
    tmp: tempfile::TempDir,
    ws: PathBuf,
    bin: PathBuf,
    witness: PathBuf,
    member_url: String,
}

struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Run {
    fn report(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout).expect("the verb printed a JSON envelope")
    }
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
        weave.install_npm();
        weave.ok_in(&weave.ws, &["doctor", "--fix"]);
        weave.ok_in(&weave.ws, &["lock"]);
        weave.commit_project();
        weave
    }

    fn install_npm(&self) {
        let npm = self.bin.join("npm");
        std::fs::write(
            &npm,
            format!(
                "#!/bin/sh\necho \"$PWD\" >> {}\nexit 0\n",
                self.witness.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn commit_project(&self) {
        let project = self.ws.join("projects/app");
        common::git_in(&project, &["add", "-A"]);
        common::git_in(
            &project,
            &["commit", "-q", "--allow-empty", "-m", "fixture"],
        );
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Run {
        let _ = std::fs::remove_file(&self.witness);
        let out = common::rwv()
            .args(args)
            .current_dir(cwd)
            .env("PATH", &self.bin)
            .output()
            .expect("rwv should run");
        Run {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn ok_in(&self, cwd: &Path, args: &[&str]) -> Run {
        let run = self.run_in(cwd, args);
        assert!(run.ok, "`rwv {}` failed:\n{}", args.join(" "), run.report());
        run
    }

    fn npm_ran(&self) -> bool {
        std::fs::read_to_string(&self.witness).is_ok_and(|w| !w.is_empty())
    }

    fn seat(&self) -> PathBuf {
        self.ok_in(&self.ws, &["workweave", "app", "create", "seat"]);
        self.tmp.path().join(".workweaves/app--seat")
    }
}

/// The `skipped` records of a `--json` envelope.
fn skipped(doc: &serde_json::Value) -> Vec<serde_json::Value> {
    doc["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("the envelope carries a skipped array: {doc}"))
        .clone()
}

fn assert_says_nothing_skipped(run: &Run) {
    assert!(
        !run.stderr.contains(SKIPPED) && !run.stdout.contains(SKIPPED),
        "a run that skipped nothing says nothing:\n{}",
        run.report()
    );
}

#[test]
fn no_materialize_names_materialize_and_a_hooked_run_says_nothing() {
    let weave = Weave::new();

    let run = weave.ok_in(&weave.ws, &["activate", "app", "--no-materialize"]);
    assert!(
        run.stderr.contains(NO_MATERIALIZE) && !weave.npm_ran(),
        "the skipped hooks are named, with the verb that runs them:\n{}",
        run.report()
    );
    assert_eq!(
        run.stderr.matches(SKIPPED).count(),
        1,
        "one line per skipped effect:\n{}",
        run.report()
    );

    let run = weave.ok_in(&weave.ws, &["activate", "app"]);
    assert!(weave.npm_ran(), "control: the hooks ran:\n{}", run.report());
    assert_says_nothing_skipped(&run);

    let run = weave.ok_in(
        &weave.ws,
        &["workweave", "app", "create", "bare", "--no-materialize"],
    );
    assert!(run.stderr.contains(NO_MATERIALIZE), "{}", run.report());
    let run = weave.ok_in(&weave.ws, &["workweave", "app", "create", "hooked"]);
    assert!(
        weave.npm_ran(),
        "control: create ran the hooks:\n{}",
        run.report()
    );
    assert_says_nothing_skipped(&run);

    let url = remote(
        weave.tmp.path(),
        "web2",
        &[("package.json", &package("web2"))],
    );
    let run = weave.ok_in(&weave.ws, &["add", &url, "--no-materialize"]);
    assert!(run.stderr.contains(NO_MATERIALIZE), "{}", run.report());
    let run = weave.ok_in(&weave.ws, &["add", &url]);
    assert!(weave.npm_ran(), "control: the hooks ran:\n{}", run.report());
    assert_says_nothing_skipped(&run);
}

#[test]
fn an_authoring_verb_inside_a_workweave_names_materialize() {
    let weave = Weave::new();
    let seat = weave.seat();

    let run = weave.ok_in(&seat, &["role", "github/acme/web", "owned"]);
    assert!(
        run.stderr.contains(INSIDE_WORKWEAVE) && !weave.npm_ran(),
        "the hooks an authoring verb skips in a workweave are named:\n{}",
        run.report()
    );

    let run = weave.ok_in(&weave.ws, &["role", "github/acme/web", "owned"]);
    assert!(
        weave.npm_ran(),
        "control: at primary the hooks ran:\n{}",
        run.report()
    );
    assert_says_nothing_skipped(&run);
}

#[test]
fn an_authoring_verb_on_a_project_the_root_does_not_present_names_activate() {
    let weave = Weave::new();
    let other = weave.ws.join("projects/other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("rwv.toml"),
        manifest(&[("github/acme/web", &weave.member_url)]),
    )
    .unwrap();
    git_init_with_commit(&other);

    let run = weave.ok_in(
        &weave.ws,
        &["role", "github/acme/web", "owned", "--project", "other"],
    );
    for line in [
        "[skipped] core: the weave-root links were not refreshed (`other` is not the project \
         this weave root presents); `rwv activate other` refreshes them",
        "[skipped] core: the install hooks did not run (`other` is not the project this weave \
         root presents); `rwv activate other` runs them",
    ] {
        assert!(
            run.stderr.contains(line),
            "missing `{line}`:\n{}",
            run.report()
        );
    }
    assert!(!weave.npm_ran(), "fixture: no hook ran for `other`");

    let run = weave.ok_in(&weave.ws, &["role", "github/acme/web", "owned"]);
    assert!(
        weave.npm_ran(),
        "control: the presented project's hooks ran"
    );
    assert_says_nothing_skipped(&run);
}

#[test]
fn a_narrowed_update_names_doctor_fix_and_materialize_in_text_and_json() {
    let weave = Weave::new();

    let run = weave.ok_in(
        &weave.ws,
        &["update", "--repo", "github/acme/web", "--json", "-j", "1"],
    );
    for line in [
        "[skipped] core: the integration files were not regenerated (the update was narrowed \
         by --role or --repo); `rwv doctor --fix` regenerates them",
        "[skipped] core: the install hooks did not run (the update was narrowed by --role or \
         --repo); `rwv materialize` runs them",
    ] {
        assert!(
            run.stderr.contains(line),
            "missing `{line}`:\n{}",
            run.report()
        );
    }
    assert_eq!(
        skipped(&run.json()),
        vec![
            serde_json::json!({"effect": "integration-files", "reason": "narrowed", "remedy": "rwv doctor --fix"}),
            serde_json::json!({"effect": "install-hooks", "reason": "narrowed", "remedy": "rwv materialize"}),
        ]
    );
    assert!(!weave.npm_ran(), "fixture: a narrowed update runs no hook");

    let run = weave.ok_in(&weave.ws, &["update", "--json", "-j", "1"]);
    assert!(
        weave.npm_ran(),
        "control: an unnarrowed update ran the hooks"
    );
    assert_says_nothing_skipped(&run);
    assert_eq!(skipped(&run.json()), Vec::<serde_json::Value>::new());
}

#[test]
fn update_records_the_hooks_it_skipped_in_json() {
    let weave = Weave::new();

    let run = weave.ok_in(
        &weave.ws,
        &["update", "--no-materialize", "--json", "-j", "1"],
    );
    assert!(run.stderr.contains(NO_MATERIALIZE), "{}", run.report());
    assert_eq!(
        skipped(&run.json()),
        vec![
            serde_json::json!({"effect": "install-hooks", "reason": "no-materialize", "remedy": "rwv materialize"})
        ]
    );

    let seat = weave.seat();
    let run = weave.ok_in(&seat, &["update", "--json", "-j", "1"]);
    assert!(run.stderr.contains(INSIDE_WORKWEAVE), "{}", run.report());
    assert_eq!(
        skipped(&run.json()),
        vec![
            serde_json::json!({"effect": "install-hooks", "reason": "inside-workweave", "remedy": "rwv materialize"})
        ]
    );

    std::fs::remove_file(weave.bin.join("npm")).unwrap();
    let run = weave.ok_in(&weave.ws, &["update", "--json", "-j", "1"]);
    assert!(
        run.stderr.contains(
            "[warning] npm-workspaces: npm is not on PATH, so this integration's install hook \
             did not run; install npm, then run `rwv materialize`"
        ),
        "{}",
        run.report()
    );
    assert_eq!(
        skipped(&run.json()),
        vec![serde_json::json!({
            "effect": "install-hooks",
            "integration": "npm-workspaces",
            "reason": {"tool-missing": {"tool": "npm"}},
            "remedy": "rwv materialize",
        })]
    );
}

#[test]
fn fetch_names_what_its_activation_skipped_in_text_and_json() {
    let weave = Weave::new();
    let source = weave.tmp.path().join("app.git");
    common::git_in(
        weave.tmp.path(),
        &[
            "clone",
            "--bare",
            &weave.ws.join("projects/app").to_string_lossy(),
            &source.to_string_lossy(),
        ],
    );
    let source = common::file_url(&source);
    let fresh = |name: &str| {
        let dir = weave.tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };

    let run = weave.ok_in(
        &fresh("flagged"),
        &["fetch", &source, "--no-materialize", "--json", "-j", "1"],
    );
    assert!(run.stderr.contains(NO_MATERIALIZE), "{}", run.report());
    assert_eq!(
        skipped(&run.json()),
        vec![
            serde_json::json!({"effect": "install-hooks", "reason": "no-materialize", "remedy": "rwv materialize"})
        ]
    );

    let occupied = fresh("occupied");
    std::fs::create_dir_all(occupied.join("projects")).unwrap();
    std::fs::write(occupied.join(".rwv-active"), "elsewhere\n").unwrap();
    let run = weave.ok_in(
        &occupied,
        &[
            "fetch",
            &source,
            "--allow-non-empty-dir",
            "--json",
            "-j",
            "1",
        ],
    );
    assert!(
        run.stderr.contains(
            "rwv fetch: skipping auto-activate (project 'elsewhere' already active); \
             `rwv activate app` selects it and runs its install hooks"
        ),
        "{}",
        run.report()
    );
    assert_eq!(
        skipped(&run.json()),
        vec![serde_json::json!({
            "effect": "activation",
            "reason": {"project-already-active": {"project": "elsewhere"}},
            "remedy": "rwv activate app",
        })]
    );

    let run = weave.ok_in(&fresh("control"), &["fetch", &source, "--json", "-j", "1"]);
    assert!(
        weave.npm_ran(),
        "control: the first activation ran the hooks"
    );
    assert_says_nothing_skipped(&run);
    assert_eq!(skipped(&run.json()), Vec::<serde_json::Value>::new());

    std::fs::remove_file(weave.ws.join(".rwv-active")).unwrap();
    let run = weave.ok_in(&weave.ws, &["fetch"]);
    assert!(
        weave.npm_ran(),
        "control: selecting the only project ran its hooks"
    );
    assert_says_nothing_skipped(&run);
}

/// An in-place fetch that activates nothing says the hooks did not run only
/// when it moved or created a checkout: a no-op fetch leaves nothing for them
/// to do and says nothing.
#[test]
fn an_in_place_fetch_names_materialize_only_when_it_moved_or_created_a_checkout() {
    let weave = Weave::new();
    let line = "[skipped] core: the install hooks did not run (project `app` was already active, \
                so this fetch activated nothing); `rwv materialize` runs them";
    let record = serde_json::json!({
        "effect": "install-hooks",
        "reason": {"project-already-active": {"project": "app"}},
        "remedy": "rwv materialize",
    });
    let member = weave.ws.join("github/acme/web");

    for args in [&["fetch"][..], &["fetch", "--json", "-j", "1"][..]] {
        let run = weave.ok_in(&weave.ws, args);
        assert_says_nothing_skipped(&run);
        if args.contains(&"--json") {
            assert_eq!(skipped(&run.json()), Vec::<serde_json::Value>::new());
        }
    }

    for args in [&["fetch"][..], &["fetch", "--json", "-j", "1"][..]] {
        std::fs::remove_dir_all(&member).unwrap();
        let run = weave.ok_in(&weave.ws, args);
        assert!(
            member.join(".git").exists(),
            "fixture: fetch re-cloned the member"
        );
        assert!(
            run.stderr.contains(line),
            "`rwv {}` re-created a member, so it says the hooks did not run:\n{}",
            args.join(" "),
            run.report()
        );
        if args.contains(&"--json") {
            assert_eq!(skipped(&run.json()), vec![record.clone()]);
        }
    }

    std::fs::write(member.join("moved.txt"), "moved\n").unwrap();
    common::git_in(&member, &["add", "-A"]);
    common::git_in(&member, &["commit", "-q", "-m", "ahead of the lock"]);
    common::git_in(&member, &["checkout", "-q", "--detach"]);
    let run = weave.ok_in(&weave.ws, &["fetch", "--json", "-j", "1"]);
    assert!(
        !member.join("moved.txt").exists(),
        "fixture: fetch moved the member back to its pin:\n{}",
        run.report()
    );
    assert!(run.stderr.contains(line), "{}", run.report());
    assert_eq!(skipped(&run.json()), vec![record]);
}

/// In a workweave the clones a fetch moves are the primary's, so only a member
/// worktree it creates there makes it say the hooks did not run.
#[test]
fn an_in_place_fetch_in_a_workweave_names_materialize_only_for_a_worktree_it_created() {
    let weave = Weave::new();
    let seat = weave.seat();

    let run = weave.ok_in(&seat, &["fetch", "--json", "-j", "1"]);
    assert_says_nothing_skipped(&run);
    assert_eq!(skipped(&run.json()), Vec::<serde_json::Value>::new());

    let canonical = weave.ws.join("github/acme/web");
    std::fs::write(canonical.join("moved.txt"), "moved\n").unwrap();
    common::git_in(&canonical, &["add", "-A"]);
    common::git_in(&canonical, &["commit", "-q", "-m", "ahead of the lock"]);
    common::git_in(&canonical, &["checkout", "-q", "--detach"]);
    let run = weave.ok_in(&seat, &["fetch", "--json", "-j", "1"]);
    assert!(
        !canonical.join("moved.txt").exists(),
        "fixture: the fetch moved the primary's clone back to the pin:\n{}",
        run.report()
    );
    assert_says_nothing_skipped(&run);
    assert_eq!(skipped(&run.json()), Vec::<serde_json::Value>::new());

    let member = seat.join("github/acme/web");
    common::git_in(
        weave.ws.join("github/acme/web"),
        &["worktree", "remove", "--force", &member.to_string_lossy()],
    );
    let run = weave.ok_in(&seat, &["fetch", "--json", "-j", "1"]);
    assert!(
        member.join(".git").exists(),
        "fixture: fetch re-created the worktree"
    );
    assert!(
        run.stderr.contains(
            "[skipped] core: the install hooks did not run (project `app` was already active, \
             so this fetch activated nothing); `rwv materialize` runs them"
        ),
        "{}",
        run.report()
    );
    assert_eq!(skipped(&run.json()).len(), 1, "{}", run.report());
}
