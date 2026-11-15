//! The lock merge driver: `rwv lock --merge-driver` as git runs it in merges
//! rwv does not make, what git leaves when it cannot run it, the refusals
//! that hold every verb off an unsettled lock or an unmigrated setup, and the
//! verbs that set it up.
//!
//! Every merge here is a real `git merge`, `git cherry-pick` or `git rebase`
//! in a weave built on disk. `common::git()` puts the `rwv` under test first
//! on `PATH`, so the driver git runs is this build's.

use std::path::{Path, PathBuf};

use repoweave::diagnosis::State;

use crate::common;

const APP: &str = "github/acme/app";
const PROJECT: &str = "proj";

struct Weave {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Weave {
    /// A weave whose project `proj` lists `app` on `main`, with the lock
    /// merge driver set up and `app` pinned at its first commit.
    fn new() -> Weave {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path().join("ws");
        let weave = Weave { _tmp: tmp, root };
        let app = weave.app();
        std::fs::create_dir_all(&app).unwrap();
        common::git_in(&app, &["init", "-b", "main"]);
        common::git_in(&app, &["commit", "--allow-empty", "-m", "a1"]);

        let project = weave.project();
        std::fs::create_dir_all(&project).unwrap();
        common::git_in(&project, &["init", "-b", "main"]);
        weave.declare("main");
        common::set_up_lock_driver(&project);
        weave.pin(&weave.app_tip());
        common::git_in(&project, &["add", "."]);
        common::git_in(&project, &["commit", "-m", "base"]);
        common::record_trunk(&project);
        std::fs::write(weave.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
        weave
    }

    fn app(&self) -> PathBuf {
        self.root.join(APP)
    }

    fn project(&self) -> PathBuf {
        self.root.join("projects").join(PROJECT)
    }

    fn app_tip(&self) -> String {
        common::git_in(self.app(), &["rev-parse", "HEAD"])
    }

    /// A new `app` commit on `branch`, created from `from`.
    fn app_commit(&self, branch: &str, from: &str, message: &str) -> String {
        common::git_in(self.app(), &["checkout", "-q", "-B", branch, from]);
        common::git_in(self.app(), &["commit", "--allow-empty", "-m", message]);
        let tip = self.app_tip();
        common::git_in(self.app(), &["checkout", "-q", "main"]);
        tip
    }

    fn declare(&self, branch: &str) {
        std::fs::write(
            self.project().join("rwv.toml"),
            format!(
                "[repositories.\"{APP}\"]\ntype = \"git\"\nurl = \"https://example.com/acme/app.git\"\nbranch = \"{branch}\"\nrole = \"owned\"\n"
            ),
        )
        .unwrap();
    }

    fn pin(&self, sha: &str) {
        common::fixture_lock(
            &self.project(),
            &[(APP, "https://example.com/acme/app.git", sha)],
        );
    }

    fn project_git(&self, args: &[&str]) -> std::process::Output {
        common::git()
            .args(args)
            .current_dir(self.project())
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .unwrap()
    }

    /// Commit the working project files on a new branch `name` from `from`.
    fn project_branch(&self, name: &str, from: &str, edit: impl FnOnce(&Weave)) {
        common::git_in(self.project(), &["checkout", "-q", "-b", name, from]);
        edit(self);
        common::git_in(self.project(), &["commit", "-qam", name]);
    }

    fn pinned(&self) -> String {
        let text = std::fs::read_to_string(self.project().join("rwv.lock")).unwrap();
        let lock: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("rwv.lock does not parse ({e}):\n{text}"));
        lock["repositories"][APP]["version"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn lock_text(&self) -> String {
        std::fs::read_to_string(self.project().join("rwv.lock")).unwrap()
    }

    fn lock_stages(&self) -> Vec<String> {
        common::git_in(self.project(), &["ls-files", "-s", "--", "rwv.lock"])
            .lines()
            .map(|line| line.split_whitespace().nth(2).unwrap().to_owned())
            .collect()
    }

    fn rwv(&self, args: &[&str]) -> std::process::Output {
        common::rwv()
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap()
    }
}

fn path_without_rwv() -> std::ffi::OsString {
    let paths = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|dir| !dir.join("rwv").exists() && !dir.join("rwv.exe").exists())
        .collect::<Vec<_>>();
    std::env::join_paths(paths).unwrap()
}

// ---------------------------------------------------------------------------
// The driver's arms, through a real merge
// ---------------------------------------------------------------------------

#[test]
fn a_pin_one_side_moved_merges_to_that_side() {
    let weave = Weave::new();
    let a2 = weave.app_commit("main", "main", "a2");
    weave.project_branch("feat", "main", |w| w.pin(&a2));
    common::git_in(weave.project(), &["checkout", "-q", "main"]);
    std::fs::write(weave.project().join("notes"), "n\n").unwrap();
    common::git_in(weave.project(), &["add", "notes"]);
    common::git_in(weave.project(), &["commit", "-qm", "notes"]);

    let out = weave.project_git(&["merge", "--no-edit", "feat"]);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(weave.pinned(), a2);
}

#[test]
fn a_pin_both_sides_moved_alike_merges_to_it() {
    let weave = Weave::new();
    let a2 = weave.app_commit("main", "main", "a2");
    weave.project_branch("feat", "main", |w| {
        w.pin(&a2);
        std::fs::write(w.project().join("feat-file"), "f\n").unwrap();
        common::git_in(w.project(), &["add", "feat-file"]);
    });
    weave.project_branch("other", "main", |w| w.pin(&a2));

    let out = weave.project_git(&["merge", "--no-edit", "feat"]);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(weave.pinned(), a2);
}

#[test]
fn a_member_one_side_added_takes_that_sides_pin() {
    let weave = Weave::new();
    let base = weave.app_tip();
    weave.project_branch("feat", "main", |w| {
        let mut manifest = std::fs::read_to_string(w.project().join("rwv.toml")).unwrap();
        manifest.push_str(
            "\n[repositories.\"github/acme/lib\"]\ntype = \"git\"\nurl = \"https://example.com/acme/lib.git\"\nbranch = \"main\"\nrole = \"owned\"\n",
        );
        std::fs::write(w.project().join("rwv.toml"), manifest).unwrap();
        common::fixture_lock(
            &w.project(),
            &[
                (APP, "https://example.com/acme/app.git", &base),
                ("github/acme/lib", "https://example.com/acme/lib.git", &"c".repeat(40)),
            ],
        );
    });
    weave.project_branch("other", "main", |w| {
        std::fs::write(w.project().join("notes"), "n\n").unwrap();
        common::git_in(w.project(), &["add", "notes"]);
    });

    let out = weave.project_git(&["merge", "--no-edit", "feat"]);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    let lock: serde_json::Value = serde_json::from_str(&weave.lock_text()).unwrap();
    assert_eq!(
        lock["repositories"]["github/acme/lib"]["version"],
        "c".repeat(40)
    );
    assert_eq!(weave.pinned(), base);
}

#[test]
fn a_member_the_sides_declare_differently_keeps_the_kept_branchs_pin() {
    let weave = Weave::new();
    let base = weave.app_tip();
    let on_main = weave.app_commit("main", "main", "a2");
    let on_dev = weave.app_commit("dev", &base, "d1");
    weave.project_branch("feat", "main", |w| {
        w.declare("dev");
        w.pin(&on_dev);
    });
    weave.project_branch("other", "main", |w| w.pin(&on_main));

    let out = weave.project_git(&["merge", "--no-edit", "feat"]);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(
        weave.pinned(),
        on_dev,
        "only feat changed the declaration, so the merge keeps dev, and dev's pin"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("declare it differently"),
        "the pin not carried is reported: {}",
        common::cli::describe(&out)
    );
}

#[test]
fn a_pin_diverged_under_one_branch_is_left_conflicted_with_both_pins_marked() {
    let weave = Weave::new();
    let base = weave.app_tip();
    let ours = weave.app_commit("main", "main", "a2");
    let theirs = weave.app_commit("side", &base, "a3");
    weave.project_branch("feat", "main", |w| w.pin(&theirs));
    weave.project_branch("other", "main", |w| w.pin(&ours));

    let out = weave.project_git(&["merge", "--no-edit", "feat"]);
    assert!(!out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(weave.lock_stages(), ["1", "2", "3"]);
    let text = weave.lock_text();
    assert!(text.starts_with("<<<<<<< "), "{text}");
    assert!(text.contains(&ours) && text.contains(&theirs), "{text}");

    let status = weave.rwv(&["status"]);
    common::cli::refused(&status, State::EditsLockUnresolved);
    let lock = weave.rwv(&["lock"]);
    common::cli::refused(&lock, State::EditsLockUnresolved);
}

#[test]
fn a_copy_whose_sides_are_not_two_heads_leaves_a_diverged_pin_conflicted() {
    let weave = Weave::new();
    let base = weave.app_tip();
    let ours = weave.app_commit("main", "main", "a2");
    let theirs = weave.app_commit("dev", &base, "d1");
    weave.project_branch("feat", "main", |w| {
        w.declare("dev");
        w.pin(&theirs);
    });
    weave.project_branch("other", "main", |w| w.pin(&ours));

    let out = weave.project_git(&["cherry-pick", "feat"]);
    assert!(!out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(
        weave.lock_stages(),
        ["1", "2", "3"],
        "a cherry-pick names no two heads, so which branch the result keeps is not known \
         and no side is taken"
    );
    assert!(weave.lock_text().starts_with("<<<<<<< "));
}

#[test]
fn a_driver_that_cannot_run_leaves_gits_conflict_and_no_verb_reads_the_side_left_in_the_file() {
    let weave = Weave::new();
    let a2 = weave.app_commit("main", "main", "a2");
    weave.project_branch("feat", "main", |w| w.pin(&a2));
    weave.project_branch("other", "main", |w| {
        std::fs::write(w.project().join("notes"), "n\n").unwrap();
        common::git_in(w.project(), &["add", "notes"]);
        w.pin(&"d".repeat(40));
    });

    let out = common::git()
        .args(["merge", "--no-edit", "feat"])
        .current_dir(weave.project())
        .env("PATH", path_without_rwv())
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(
        weave.lock_stages(),
        ["1", "2", "3"],
        "git records the conflict in the index"
    );
    assert_eq!(
        weave.pinned(),
        "d".repeat(40),
        "git leaves this side in the working file, unmarked: a lock that parses"
    );
    for args in [&["status"][..], &["lock"], &["lock", "--commit"], &["log"]] {
        common::cli::refused(&weave.rwv(args), State::EditsLockUnresolved);
    }
}

// ---------------------------------------------------------------------------
// Setup and migration
// ---------------------------------------------------------------------------

/// Leave `weave`'s project on the setup an earlier release wrote.
fn unmigrate(weave: &Weave) {
    std::fs::write(
        weave.project().join(".gitattributes"),
        "*.png binary\nrwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    common::git_in(weave.project(), &["add", ".gitattributes"]);
    common::git_in(
        weave.project(),
        &["commit", "-qm", "the old attribute line"],
    );
    common::git_in(
        weave.project(),
        &["config", "--unset", "merge.rwv-lock.driver"],
    );
    common::git_in(
        weave.project(),
        &["config", "merge.rwv-ours.driver", "true"],
    );
}

#[test]
fn an_old_setup_refuses_every_lock_writing_verb_before_anything_is_written() {
    let weave = Weave::new();
    unmigrate(&weave);
    let head = common::git_in(weave.project(), &["rev-parse", "HEAD"]);
    let lock = weave.lock_text();

    for args in [
        &["lock"][..],
        &["lock", "--commit"],
        &["member", "pin", APP, "HEAD", "--commit"],
        &["branch", "create", "feat-x"],
        &["sync"],
        &["pull"],
        &["update", "--remote"],
    ] {
        let out = weave.rwv(args);
        common::cli::refused(&out, State::MissingLockMergeDriverAttributeLegacy);
        common::cli::refused(&out, State::MissingLockMergeDriverConfigLegacy);
        assert_eq!(
            common::git_in(weave.project(), &["rev-parse", "HEAD"]),
            head,
            "{args:?} wrote the project repo"
        );
        assert_eq!(weave.lock_text(), lock, "{args:?} wrote the lock");
    }

    let refused = weave.rwv(&["lock"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    let advised = stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("rwv doctor"))
        .unwrap_or_else(|| panic!("no fix is printed:\n{stderr}"));
    assert_eq!(advised, "rwv doctor --fix");
    assert!(common::run_advised(advised, &weave.root).status.success());
    let lock = weave.rwv(&["lock"]);
    assert!(lock.status.success(), "{}", common::cli::describe(&lock));
}

#[test]
fn doctor_reports_each_part_and_fix_commits_only_the_attribute_line() {
    let weave = Weave::new();
    unmigrate(&weave);
    std::fs::write(
        weave.project().join(".gitattributes"),
        "*.png binary\nrwv.lock merge=rwv-ours\n*.jpg binary\n",
    )
    .unwrap();
    let findings = common::doctor_findings(&weave.root);
    for sub_kind in ["attribute-legacy", "config-legacy"] {
        assert!(
            findings
                .iter()
                .any(|f| f.starts_with(&format!("missing-lock-merge-driver {sub_kind}"))),
            "{sub_kind} not reported: {findings:?}"
        );
    }
    std::fs::write(weave.project().join("staged.txt"), "s\n").unwrap();
    common::git_in(weave.project(), &["add", "staged.txt"]);
    let before = common::git_in(weave.project(), &["rev-parse", "HEAD"]);

    let fix = weave.rwv(&["doctor", "--fix"]);
    assert!(fix.status.success(), "{}", common::cli::describe(&fix));

    let changed = common::git_in(
        weave.project(),
        &["diff", "--name-only", &format!("{before}..HEAD")],
    );
    assert_eq!(
        changed.trim(),
        ".gitattributes",
        "the commit holds only the line"
    );
    assert_eq!(
        common::git_in(weave.project(), &["diff", "--cached", "--name-only"]),
        "staged.txt",
        "what else was staged stays staged"
    );
    let parent = common::git_in(weave.project(), &["rev-parse", "HEAD~1"]);
    assert_eq!(parent, before, "one commit");
    assert_eq!(
        common::git_in(weave.project(), &["show", "HEAD:.gitattributes"]),
        "*.png binary\nrwv.lock merge=rwv-lock"
    );
    assert_eq!(
        std::fs::read_to_string(weave.project().join(".gitattributes")).unwrap(),
        "*.png binary\nrwv.lock merge=rwv-lock\n*.jpg binary\n",
        "the working file keeps its other edit, uncommitted"
    );
    assert_eq!(
        common::git_in(
            weave.project(),
            &["config", "--get", "merge.rwv-lock.driver"]
        ),
        common::LOCK_DRIVER_COMMAND
    );
    assert!(
        !common::doctor_findings(&weave.root)
            .iter()
            .any(|f| f.starts_with("missing-lock-merge-driver")),
        "doctor --fix converges"
    );
}

#[test]
fn lock_driver_migration() {
    let weave = Weave::new();
    let a2 = weave.app_commit("main", "main", "a2");
    weave.project_branch("feat", "main", |w| w.pin(&a2));
    common::git_in(weave.project(), &["checkout", "-q", "main"]);
    unmigrate(&weave);

    common::cli::refused(
        &weave.rwv(&["lock", "--commit"]),
        State::MissingLockMergeDriverAttributeLegacy,
    );
    assert!(weave.rwv(&["doctor", "--fix"]).status.success());

    let out = weave.project_git(&["merge", "--no-edit", "feat"]);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(weave.pinned(), a2, "the pins merged three-way");

    common::git_in(weave.project(), &["reset", "-q", "--hard", "HEAD~1"]);
    let a3 = weave.app_commit("main", "main", "a3");
    weave.project_branch("ahead", "feat", |w| w.pin(&a3));
    weave.project_branch("ours", "main", |w| w.pin(&a2));
    let out = common::git()
        .args(["merge", "--no-edit", "ahead"])
        .current_dir(weave.project())
        .env("PATH", path_without_rwv())
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "the driver would settle this merge on a3; without it git must not either: {}",
        common::cli::describe(&out)
    );
    assert_eq!(
        weave.lock_stages(),
        ["1", "2", "3"],
        "git's ordinary conflict"
    );
    common::cli::refused(&weave.rwv(&["lock"]), State::EditsLockUnresolved);
}

#[test]
fn init_sets_the_driver_up_and_a_linked_worktree_needs_nothing_more() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    let out = common::rwv()
        .args(["init", "web"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    let project = root.join("projects/web");
    assert_eq!(
        common::git_in(&project, &["show", "HEAD:.gitattributes"]),
        common::LOCK_DRIVER_ATTRIBUTE.trim_end(),
        "init's first commit holds the line"
    );
    assert_eq!(
        common::git_in(&project, &["config", "--get", "merge.rwv-lock.driver"]),
        common::LOCK_DRIVER_COMMAND
    );
    assert!(
        !common::doctor_findings(&root)
            .iter()
            .any(|f| f.starts_with("missing-lock-merge-driver")),
        "a new project starts set up"
    );

    let linked = tmp.path().join("linked");
    common::git_in(
        &project,
        &["worktree", "add", "-b", "other", linked.to_str().unwrap()],
    );
    assert_eq!(
        common::git_in(&linked, &["config", "--get", "merge.rwv-lock.driver"]),
        common::LOCK_DRIVER_COMMAND
    );
}

fn bare_project(tmp: &Path, name: &str, attributes: Option<&str>) -> PathBuf {
    let bare = tmp.join(format!("{name}.git"));
    common::git_in(
        tmp,
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    let work = tmp.join(format!("{name}-work"));
    common::git_in(
        tmp,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    std::fs::write(work.join("rwv.toml"), "[repositories]\n").unwrap();
    if let Some(text) = attributes {
        std::fs::write(work.join(".gitattributes"), text).unwrap();
    }
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-qm", "project source"]);
    common::git_in(&work, &["push", "-q", "origin", "main"]);
    bare
}

#[test]
fn clone_defines_the_driver_whatever_the_project_declares() {
    for (name, attributes, expected) in [
        ("declared", Some(common::LOCK_DRIVER_ATTRIBUTE), None),
        (
            "undeclared",
            None,
            Some("missing-lock-merge-driver attribute-absent"),
        ),
    ] {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        let bare = bare_project(tmp.path(), name, attributes);
        let out = common::rwv()
            .args(["clone", &common::file_url(&bare)])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", common::cli::describe(&out));
        let project = root.join("projects").join(name);
        assert_eq!(
            common::git_in(&project, &["config", "--get", "merge.rwv-lock.driver"]),
            common::LOCK_DRIVER_COMMAND
        );
        let findings: Vec<String> = common::doctor_findings(&root)
            .into_iter()
            .filter(|f| f.starts_with("missing-lock-merge-driver"))
            .collect();
        match expected {
            None => assert!(findings.is_empty(), "{name}: {findings:?}"),
            Some(finding) => assert!(
                findings.iter().any(|f| f.starts_with(finding)),
                "{name}: {findings:?}"
            ),
        }
    }
}

#[test]
fn the_driver_run_by_hand_with_files_that_are_not_there_refuses_naming_its_form() {
    let tmp = common::tempdir().unwrap();
    let out = common::rwv()
        .args(["lock", "--merge-driver", "no-base", "no-ours", "no-theirs"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        common::cli::describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no-base") && stderr.contains("the command a VCS runs to merge rwv.lock"),
        "{stderr}"
    );
    let two = common::rwv()
        .args(["lock", "--merge-driver", "a", "b"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(two.status.code(), Some(2), "clap refuses a wrong count");
}
