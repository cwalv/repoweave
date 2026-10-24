//! A parked `sync-to` whose owner record stops parsing is left in a usable
//! state by following, verbatim, the advice its errors print: no lease
//! survives, no checkout stays mid-rebase, `rwv sync-to` lands again, and
//! `rwv doctor` reports nothing of the op.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const PROJECT: &str = "app";
const LIB: &str = "local/owner/lib";

struct Side {
    root: PathBuf,
    project: PathBuf,
    lib: PathBuf,
}

struct Parked {
    _tmp: tempfile::TempDir,
    primary: Side,
    seat: Side,
}

fn side(root: PathBuf) -> Side {
    Side {
        project: root.join("projects").join(PROJECT),
        lib: common::under(&root, LIB),
        root,
    }
}

fn head(repo: &Path) -> String {
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn commit_file(repo: &Path, file: &str, text: &str, message: &str) {
    std::fs::write(repo.join(file), text).unwrap();
    common::git_in(repo, &["add", file]);
    common::git_in(repo, &["commit", "-m", message]);
}

fn rwv(dir: &Path, args: &[&str]) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn transcript(out: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn succeed(dir: &Path, args: &[&str]) -> Output {
    let out = rwv(dir, args);
    assert!(
        out.status.success(),
        "rwv {args:?} in {} failed:\n{}",
        dir.display(),
        transcript(&out)
    );
    out
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A primary weave with one member and a seat forked from it. Both commit to
/// the same file of the member and the primary relocks, so the seat's
/// `sync-to` replays the primary's project repo onto the seat and parks on
/// the member's rebase conflict.
fn park() -> Parked {
    let tmp = common::tempdir().unwrap();
    let upstream = tmp.path().join("upstream/owner/lib");
    std::fs::create_dir_all(&upstream).unwrap();
    common::git_in(&upstream, &["init", "-b", "main"]);
    commit_file(&upstream, "README.md", "init\n", "initial");

    let primary = side(tmp.path().join("ws"));
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &common::file_url(&upstream),
            &primary.lib.to_string_lossy(),
        ],
    );
    std::fs::create_dir_all(&primary.project).unwrap();
    common::git_in(&primary.project, &["init", "-b", "main"]);
    std::fs::write(
        primary.project.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    std::fs::write(primary.project.join(".gitignore"), "*.code-workspace\n").unwrap();
    std::fs::write(
        primary.project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::file_url(&upstream)
        ),
    )
    .unwrap();
    std::fs::write(primary.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    succeed(&primary.root, &["lock"]);
    common::git_in(&primary.project, &["add", "-A"]);
    common::git_in(&primary.project, &["commit", "-m", "initial"]);

    succeed(&primary.root, &["workweave", PROJECT, "create", "s1"]);
    let seat = side(
        tmp.path()
            .join(".workweaves")
            .join(format!("{PROJECT}--s1")),
    );
    commit_file(&seat.lib, "c.txt", "seat\n", "seat: lib");
    commit_file(&primary.lib, "c.txt", "primary\n", "primary: lib");
    succeed(&primary.root, &["lock"]);
    common::git_in(&primary.project, &["add", "rwv.lock"]);
    common::git_in(&primary.project, &["commit", "-m", "primary: relock"]);

    let parked = Parked {
        _tmp: tmp,
        primary,
        seat,
    };
    let out = rwv(&parked.seat.root, &["sync-to"]);
    assert!(
        !out.status.success(),
        "the member conflict must park sync-to:\n{}",
        transcript(&out)
    );
    assert!(parked.seat.root.join(".rwv-op").exists());
    assert!(parked.primary.root.join(".rwv-op-lease").exists());
    parked
}

fn record_path(parked: &Parked) -> PathBuf {
    parked.seat.root.join(".rwv-op")
}

fn rewrite_record(parked: &Parked, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = record_path(parked);
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    edit(&mut record);
    std::fs::write(&path, serde_json::to_string_pretty(&record).unwrap()).unwrap();
}

fn mid_op(repo: &Path) -> bool {
    let git_dir = PathBuf::from(common::git_in(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    ["rebase-merge", "rebase-apply", "MERGE_HEAD"]
        .iter()
        .any(|state| git_dir.join(state).exists())
}

/// Resolves the member conflict a fresh `sync-to` parks on and resumes it, so
/// the landing runs to completion.
fn land(parked: &Parked) {
    let out = rwv(&parked.seat.root, &["sync-to"]);
    let stderr = stderr_of(&out);
    assert!(
        !stderr.contains("rwv explain op-in-progress")
            && !stderr.contains("rwv explain dead-op-lease"),
        "no state of the discarded op may refuse a new sync-to:\n{}",
        transcript(&out)
    );
    if !out.status.success() {
        assert!(
            mid_op(&parked.seat.lib),
            "a new sync-to may stop only on the member's genuine conflict:\n{}",
            transcript(&out)
        );
        std::fs::write(parked.seat.lib.join("c.txt"), "both\n").unwrap();
        common::git_in(&parked.seat.lib, &["add", "c.txt"]);
        common::git_in(
            &parked.seat.lib,
            &["-c", "core.editor=true", "rebase", "--continue"],
        );
        succeed(&parked.seat.root, &["sync-to", "--continue"]);
    }
    assert_eq!(
        head(&parked.primary.lib),
        head(&parked.seat.lib),
        "the landing must advance the target's member to the seat's tip"
    );
}

fn assert_no_op_residue(parked: &Parked) {
    assert!(!record_path(parked).exists());
    assert!(!parked.primary.root.join(".rwv-op-lease").exists());
    for repo in [
        &parked.seat.lib,
        &parked.seat.project,
        &parked.primary.lib,
        &parked.primary.project,
    ] {
        assert!(!mid_op(repo), "{} is left mid-op", repo.display());
    }
}

fn assert_doctor_reports_no_op_state(parked: &Parked) {
    for dir in [&parked.seat.root, &parked.primary.root] {
        let out = rwv(dir, &["doctor", "--json"]);
        let report: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("doctor --json: {e}\n{}", transcript(&out)));
        let kinds: Vec<&str> = report["violations"]
            .as_array()
            .expect("violations array")
            .iter()
            .filter_map(|v| v["kind"].as_str())
            .collect();
        for kind in ["stale-op-state", "dead-op-lease", "orphaned-savepoint"] {
            assert!(
                !kinds.contains(&kind),
                "doctor in {} still reports {kind}:\n{}",
                dir.display(),
                transcript(&out)
            );
        }
    }
}

/// A recorded tip that is a tag name fails the record's parse while its op
/// id and workspaces still parse. The error sends the operator to `rwv
/// abort`, abort refuses the one repo it can no longer attribute and names
/// the flag that settles it, and that flag ends the op.
#[test]
fn a_record_whose_header_parses_is_discarded_by_the_abort_its_error_names() {
    let parked = park();
    rewrite_record(&parked, |record| {
        record["advanced_tips"]["(project)"] = "v1.0".into();
    });

    let out = rwv(&parked.seat.root, &["sync-to", "--continue"]);
    let stderr = stderr_of(&out);
    assert!(!out.status.success(), "{}", transcript(&out));
    assert!(
        stderr.contains("`rwv abort` still discards it"),
        "the parse error must name abort as the way out:\n{}",
        transcript(&out)
    );

    let out = rwv(&parked.seat.root, &["abort"]);
    let stderr = stderr_of(&out);
    assert!(!out.status.success(), "{}", transcript(&out));
    assert!(
        stderr.contains("  (project): foreign-tip violation"),
        "with its recorded tip unreadable, the replayed project repo is foreign:\n{}",
        transcript(&out)
    );
    assert!(
        stderr.contains("`rwv abort --abandon-foreign-tip=<repo>`"),
        "the refusal must name the flag that settles it:\n{}",
        transcript(&out)
    );
    assert!(
        !mid_op(&parked.seat.lib),
        "abort must end the member's rebase, which needs no recorded tip"
    );

    succeed(
        &parked.seat.root,
        &["abort", "--abandon-foreign-tip=(project)"],
    );

    assert_no_op_residue(&parked);
    land(&parked);
    assert_no_op_residue(&parked);
    assert_doctor_reports_no_op_state(&parked);
}

/// A record that names no op cannot be aborted. Its error tells the operator
/// to remove it and names each thing removal leaves behind with the command
/// that clears it; following each in turn ends in a usable weave, and the
/// lease left behind refuses with that same remedy rather than sending the
/// operator back to `rwv abort`.
#[test]
fn removing_a_record_that_names_no_op_leaves_only_what_its_error_names() {
    let parked = park();
    let target = parked.primary.root.clone();
    std::fs::write(record_path(&parked), "{ not json").unwrap();

    let out = rwv(&parked.seat.root, &["abort"]);
    let stderr = stderr_of(&out);
    assert!(!out.status.success(), "{}", transcript(&out));
    for advice in [
        "Removing the file discards the op",
        "`rwv status` marks",
        "`rebase --abort`",
        "the workspace the record names as `target`, which `rwv doctor --fix` run there releases",
        "`rwv doctor` lists",
    ] {
        assert!(
            stderr.contains(advice),
            "the error must say {advice:?}:\n{}",
            transcript(&out)
        );
    }

    std::fs::remove_file(record_path(&parked)).unwrap();

    for (dir, args) in [
        (&parked.seat.root, &["sync-to"][..]),
        (&target, &["abort"][..]),
    ] {
        let out = rwv(dir, args);
        let stderr = stderr_of(&out);
        assert!(!out.status.success(), "{}", transcript(&out));
        assert!(
            stderr.contains("rwv explain dead-op-lease")
                && stderr.contains(&format!(
                    "`rwv doctor --fix` run in {} releases the lease",
                    target.display()
                )),
            "rwv {args:?} must name the lease's own remedy, not `rwv abort`:\n{}",
            transcript(&out)
        );
    }

    let status = succeed(&parked.seat.root, &["status"]);
    let marked: Vec<String> = String::from_utf8_lossy(&status.stdout)
        .lines()
        .filter(|line| line.contains("[mid-rebase]"))
        .map(|line| line.split_whitespace().next().unwrap().to_owned())
        .collect();
    assert_eq!(marked, vec![LIB.to_owned()], "{}", transcript(&status));
    for repo in &marked {
        common::git_in(
            common::under(&parked.seat.root, repo),
            &["rebase", "--abort"],
        );
    }

    succeed(&target, &["doctor", "--fix"]);

    assert_no_op_residue(&parked);
    let out = rwv(&parked.seat.root, &["sync-to"]);
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("rwv explain stale-lock")
            && stderr.contains("bless the current HEAD with `rwv lock --commit`"),
        "the replay the removal left in place pins the member at the target's tip, \
         which the next sync-to reports with its own remedy:\n{}",
        transcript(&out)
    );
    succeed(&parked.seat.root, &["lock", "--commit"]);
    land(&parked);
    assert_no_op_residue(&parked);
    assert_doctor_reports_no_op_state(&parked);
}
