//! A lock file a tool wrote at the weave root where its link should have
//! taken the write is the tool's output, and reaches the project directory.
//!
//! The link stands before the file so a tool's write lands in the project, and
//! the tools measured keep it. What leaves a real file at the root instead is a
//! renaming writer meeting a link that dangles (`pnpm`'s first lock) or any
//! writer meeting a link that is missing. The next verb used to find an
//! occupant there and refuse, every time, with the lock's changes stranded
//! where no repo tracks them. Now a verb that may write takes the file in and
//! puts the link back; one that may not names it and carries on.
//!
//! Unix only: the runs get stand-in tools, scripts resolved off `PATH`, for
//! the reason `common::cargo_stand_in_path` states.
#![cfg(unix)]

use std::path::PathBuf;

mod common;

struct Weave {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    ws: PathBuf,
    bin: PathBuf,
    path: std::ffi::OsString,
}

impl Weave {
    fn new() -> Weave {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let ws = root.join("ws");
        std::fs::create_dir_all(ws.join("projects")).unwrap();
        let bin = root.join("stand-in-bin");
        let path = common::cargo_stand_in_path(&bin);
        let weave = Weave {
            _tmp: tmp,
            root,
            ws,
            bin,
            path,
        };
        let init = weave.rwv(&["init", "demo"]);
        assert!(init.status.success(), "{}", stderr_of(&init));
        weave
    }

    fn rwv(&self, args: &[&str]) -> std::process::Output {
        common::rwv()
            .args(args)
            .current_dir(&self.ws)
            .env("PATH", &self.path)
            .output()
            .unwrap()
    }

    fn origin(&self, name: &str, files: &[(&str, &str)]) -> String {
        let bare = self.root.join(format!("origin/acme/{name}.git"));
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        common::git_in(
            bare.parent().unwrap(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                bare.to_str().unwrap(),
            ],
        );
        let seed = self.root.join(format!("seed-{name}"));
        common::git_in(
            &self.root,
            &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
        );
        common::git_in(&seed, &["config", "user.email", "test@test.com"]);
        common::git_in(&seed, &["config", "user.name", "Test"]);
        std::fs::write(seed.join("README"), "seed").unwrap();
        for (path, content) in files {
            let file = common::under(&seed, path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, content).unwrap();
        }
        common::git_in(&seed, &["add", "."]);
        common::git_in(&seed, &["commit", "-m", "initial"]);
        common::git_in(&seed, &["push", "origin", "main"]);
        common::file_url(&bare)
    }

    /// Add a member whose manifest makes cargo-workspace declare `Cargo.lock`;
    /// the stand-in `cargo` generates the lock, and the hook records it.
    fn add_cargo_member(&self) {
        let server = self.origin(
            "server",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                ),
                ("src/lib.rs", ""),
            ],
        );
        let add = self.rwv(&["add", &server]);
        assert!(add.status.success(), "fixture add: {}", stderr_of(&add));
        assert!(
            self.is_linked("Cargo.lock"),
            "fixture: the lock should be surfaced through its link"
        );
    }

    /// Commit the project's lock, so history can give it back.
    fn commit_project_lock(&self) {
        let project = self.ws.join("projects/demo");
        common::git_in(&project, &["add", "Cargo.lock"]);
        common::git_in(&project, &["commit", "-qm", "lock"]);
    }

    /// Replace the lock's link with a real file holding `bytes`: a tool that
    /// ran while the link was missing.
    fn write_past_the_link(&self, bytes: &str) {
        let path = self.ws.join("Cargo.lock");
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, bytes).unwrap();
    }

    fn is_linked(&self, name: &str) -> bool {
        std::fs::read_link(self.ws.join(name)).ok()
            == Some(PathBuf::from(format!("projects/demo/{name}")))
    }

    fn is_real_file(&self, name: &str) -> bool {
        self.ws
            .join(name)
            .symlink_metadata()
            .is_ok_and(|m| m.is_file())
    }

    fn project_file(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.ws.join("projects/demo").join(name)).ok()
    }
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `pnpm install` on a first generation: the link dangles, the tool renames
/// its new lock over it, and the verb that ran the tool takes the file in.
#[test]
fn a_lock_renamed_over_its_dangling_link_is_adopted_by_the_verb_that_ran_the_tool() {
    let weave = Weave::new();
    let mv = which::which("mv").expect("mv must be resolvable to run this test");
    let npm = weave.bin.join("npm");
    std::fs::write(
        &npm,
        format!(
            "#!/bin/sh\n\
             printf '{{\"lockfileVersion\": 3}}\\n' > package-lock.json.tmp\n\
             {} package-lock.json.tmp package-lock.json\n",
            mv.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let web = weave.origin(
        "web",
        &[(
            "package.json",
            "{\"name\": \"web\", \"version\": \"1.0.0\"}\n",
        )],
    );

    let add = weave.rwv(&["add", &web]);

    assert!(add.status.success(), "{}", stderr_of(&add));
    assert_eq!(
        weave.project_file("package-lock.json").as_deref(),
        Some("{\"lockfileVersion\": 3}\n"),
        "the tool's lock must reach the project directory: {}",
        stderr_of(&add)
    );
    assert!(
        weave.is_linked("package-lock.json"),
        "and the link must stand again, so the next write goes through it"
    );
}

/// A tool that ran while the link was missing: history holds the project
/// copy, so the next intent verb takes the tool's in and relinks, without
/// stopping on it.
#[test]
fn a_lock_written_where_its_link_was_missing_is_adopted_by_the_next_intent_verb() {
    let weave = Weave::new();
    weave.add_cargo_member();
    weave.commit_project_lock();
    weave.write_past_the_link("version = 4\n# regenerated\n");
    let plain = weave.origin("plain", &[]);

    let add = weave.rwv(&["add", &plain]);

    assert!(add.status.success(), "{}", stderr_of(&add));
    assert!(
        !stderr_of(&add).contains("surfacing-path-occupied"),
        "{}",
        stderr_of(&add)
    );
    assert_eq!(
        weave.project_file("Cargo.lock").as_deref(),
        Some("version = 4\n# regenerated\n")
    );
    assert!(weave.is_linked("Cargo.lock"));
}

/// The project copy holds an edit no commit has, so neither side may be taken
/// over the other: the verb names both, leaves both, and carries on without
/// the link.
#[test]
fn a_lock_changed_at_the_root_and_in_the_project_is_named_and_both_are_left() {
    let weave = Weave::new();
    weave.add_cargo_member();
    weave.commit_project_lock();
    weave.write_past_the_link("# the tool's\n");
    std::fs::write(weave.ws.join("projects/demo/Cargo.lock"), "# edited\n").unwrap();
    let plain = weave.origin("plain", &[]);

    let add = weave.rwv(&["add", &plain]);
    let stderr = stderr_of(&add);

    assert!(
        add.status.success(),
        "a conflict is named, not refused: {stderr}"
    );
    assert!(
        stderr.contains("version control does not hold")
            && stderr.contains("projects/demo/Cargo.lock"),
        "the finding must name both copies: {stderr}"
    );
    assert_eq!(
        stderr.matches("version control does not hold").count(),
        1,
        "named once, though surfacing runs before and after the hooks: {stderr}"
    );
    assert!(weave.is_real_file("Cargo.lock"));
    assert_eq!(
        std::fs::read_to_string(weave.ws.join("Cargo.lock")).unwrap(),
        "# the tool's\n"
    );
    assert_eq!(
        weave.project_file("Cargo.lock").as_deref(),
        Some("# edited\n")
    );
}

/// A context verb never writes the project: it names the file and the repair,
/// and the repair it names adopts it.
#[test]
fn activate_reports_a_lock_written_past_its_link_and_doctor_fix_adopts_it() {
    let weave = Weave::new();
    weave.add_cargo_member();
    weave.commit_project_lock();
    let recorded = weave.project_file("Cargo.lock").unwrap();
    weave.write_past_the_link("version = 4\n# regenerated\n");

    let activate = weave.rwv(&["activate", "demo"]);
    let stderr = stderr_of(&activate);

    assert!(activate.status.success(), "{stderr}");
    assert!(
        stderr.contains("not yet taken into") && stderr.contains("rwv doctor --fix"),
        "the verb must name the file and the repair: {stderr}"
    );
    assert_eq!(
        stderr.matches("not yet taken into").count(),
        1,
        "named once, though the hooks ran between the two passes that read it: {stderr}"
    );
    assert!(
        stderr.contains("the generation is not recorded until"),
        "the hook ran cargo over the root file, and must not attest the project copy \
         beside it: {stderr}"
    );
    assert!(weave.is_real_file("Cargo.lock"));
    assert_eq!(
        weave.project_file("Cargo.lock").as_deref(),
        Some(recorded.as_str()),
        "a context verb must not write the project copy"
    );

    let fix = weave.rwv(&["doctor", "--fix"]);

    assert_eq!(
        weave.project_file("Cargo.lock").as_deref(),
        Some("version = 4\n# regenerated\n"),
        "{}",
        String::from_utf8_lossy(&fix.stdout)
    );
    assert!(weave.is_linked("Cargo.lock"));
}

/// `rwv materialize` reads a tool's write that missed its link as the drift
/// it is: the same consent fork the write would have raised through the link.
#[test]
fn materialize_reads_a_lock_written_past_its_link_as_drift_to_settle() {
    let weave = Weave::new();
    weave.add_cargo_member();
    weave.commit_project_lock();
    weave.write_past_the_link("version = 4\n# out of band\n");

    let materialize = weave.rwv(&["materialize"]);
    let stderr = stderr_of(&materialize);

    assert!(
        !materialize.status.success(),
        "an unaccepted lock is a fork: {stderr}"
    );
    assert!(
        stderr.contains("--adopt-drifted") && stderr.contains("--regenerate-drifted"),
        "{stderr}"
    );
    assert_eq!(
        weave.project_file("Cargo.lock").as_deref(),
        Some("version = 4\n# out of band\n"),
        "the drift check reads the project copy, which must hold the tool's write"
    );
}
