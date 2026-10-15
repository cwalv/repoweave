//! A relock whose commit the project repo refuses. At op start the op does not
//! begin: the refusal carries the cause, and the project repo's index and work
//! tree are left as the op found them. In Phase 3 the op parks, and it resumes
//! once the cause is gone.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const PROJECT: &str = "app";
const LIB: &str = "local/owner/lib";
const HOOK_TEXT: &str = "refused-by-the-fixture-hook";

struct Weave {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    hooks: PathBuf,
}

fn project_dir(workspace: &Path) -> PathBuf {
    workspace.join("projects").join(PROJECT)
}

fn run(dir: &Path, args: &[&str]) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn succeed(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "rwv {args:?} in {} failed:\nstdout:\n{}\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A primary weave whose project names one owned member. The project repo's
/// hooks directory is wired up empty; [`Weave::refuse_commits`] fills it.
fn weave() -> Weave {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    let upstream = tmp.path().join("upstream").join("lib");
    std::fs::create_dir_all(&upstream).unwrap();
    common::git_in(&upstream, &["init", "-b", "main"]);
    std::fs::write(upstream.join("README.md"), "init\n").unwrap();
    common::git_in(&upstream, &["add", "."]);
    common::git_in(&upstream, &["commit", "-m", "initial"]);
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &common::file_url(&upstream),
            &common::under(&root, LIB).to_string_lossy(),
        ],
    );

    let project = project_dir(&root);
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    std::fs::write(project.join(".gitignore"), "*.code-workspace\ngita/\n").unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::file_url(&upstream)
        ),
    )
    .unwrap();
    std::fs::write(root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    succeed(&root, &["lock"]);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);

    let hooks = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    common::git_in(
        &project,
        &["config", "core.hooksPath", &hooks.to_string_lossy()],
    );
    Weave {
        _tmp: tmp,
        root,
        hooks,
    }
}

impl Weave {
    fn seat(&self, name: &str) -> PathBuf {
        succeed(&self.root, &["workweave", PROJECT, "create", name]);
        self.root
            .parent()
            .unwrap()
            .join(".workweaves")
            .join(format!("{PROJECT}--{name}"))
    }

    fn refuse_commits(&self) {
        let hook = self.hooks.join("commit-msg");
        std::fs::write(&hook, format!("#!/bin/sh\necho {HOOK_TEXT} >&2\nexit 1\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn accept_commits(&self) {
        std::fs::remove_file(self.hooks.join("commit-msg")).unwrap();
    }
}

fn commit_file(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), format!("{name}\n")).unwrap();
    common::git_in(repo, &["add", name]);
    common::git_in(repo, &["commit", "-m", name]);
}

fn head(repo: &Path) -> String {
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn savepoints(repo: &Path) -> String {
    common::git_in(repo, &["for-each-ref", "refs/rwv/pre-op"])
}

/// The seat's member is ahead of its lock, so `sync-to` relocks at op start,
/// and that commit is refused.
#[test]
fn a_refused_op_start_relock_refuses_the_op_and_leaves_the_project_repo_as_it_was() {
    let weave = weave();
    let s1 = weave.seat("s1");
    commit_file(&common::under(&s1, LIB), "work.txt");
    let project = project_dir(&s1);
    let project_head = head(&project);
    let lock_before = std::fs::read(project.join("rwv.lock")).unwrap();
    let target_head = head(&project_dir(&weave.root));
    weave.refuse_commits();

    let out = run(&s1, &["sync-to"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the op must refuse:\n{stderr}");
    assert!(
        stderr.contains(HOOK_TEXT),
        "the refusal must carry the hook's own text:\n{stderr}"
    );
    assert!(
        !stderr.contains("op-parked") && !stderr.contains("index contains uncommitted changes"),
        "the op must not run on to a later failure:\n{stderr}"
    );

    assert_eq!(
        common::git_in(&project, &["status", "--porcelain"]),
        "",
        "the project repo's index and work tree must be as the op found them"
    );
    assert_eq!(
        std::fs::read(project.join("rwv.lock")).unwrap(),
        lock_before
    );
    assert_eq!(head(&project), project_head);
    assert_eq!(head(&project_dir(&weave.root)), target_head);
    assert!(
        !s1.join(".rwv-op").exists() && !weave.root.join(".rwv-op-lease").exists(),
        "an op that did not start leaves no op record"
    );
    assert_eq!(savepoints(&project), "", "and no savepoint");
    assert_eq!(savepoints(&common::under(&s1, LIB)), "", "and no savepoint");

    weave.accept_commits();
    succeed(&s1, &["sync-to"]);
}

/// The seat and the primary both moved the member, so the replay rebases it
/// and Phase 3 must commit a lock pinning the rebased tip.
fn park_in_relock(weave: &Weave) -> PathBuf {
    let s1 = weave.seat("s1");
    commit_file(&common::under(&weave.root, LIB), "primary.txt");
    succeed(&weave.root, &["lock", "--commit"]);
    commit_file(&common::under(&s1, LIB), "seat.txt");
    succeed(&s1, &["lock", "--commit"]);
    weave.refuse_commits();

    let out = run(&s1, &["sync", "primary", "--strategy", "rebase"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the relock must park the op:\n{stderr}"
    );
    assert!(stderr.contains(HOOK_TEXT), "{stderr}");
    assert!(
        s1.join(".rwv-op").exists(),
        "a Phase 3 refusal parks the op:\n{stderr}"
    );
    assert_eq!(
        common::git_in(project_dir(&s1), &["diff", "--cached", "--name-only"]),
        "",
        "a refused lock commit must not leave rwv.lock staged"
    );
    s1
}

#[test]
fn a_refused_phase3_relock_parks_and_resumes() {
    let weave = weave();
    let s1 = park_in_relock(&weave);

    weave.accept_commits();
    succeed(&s1, &["sync", "--continue"]);
    let lib_tip = head(&common::under(&s1, LIB));
    let lock = std::fs::read_to_string(project_dir(&s1).join("rwv.lock")).unwrap();
    assert!(lock.contains(&lib_tip), "{lock}");
    assert_eq!(
        common::git_in(project_dir(&s1), &["status", "--porcelain"]),
        ""
    );
}

#[test]
fn abort_after_a_refused_phase3_relock_leaves_no_staged_lock() {
    let weave = weave();
    let s1 = park_in_relock(&weave);

    succeed(&s1, &["abort"]);
    assert_eq!(
        common::git_in(project_dir(&s1), &["diff", "--cached", "--name-only"]),
        "",
        "abort must not leave rwv.lock staged"
    );
}
