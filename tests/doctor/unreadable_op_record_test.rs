//! A parked `land` whose owner record stops parsing is left in a usable
//! state by following, verbatim, the advice its errors print: no lease
//! survives, no checkout stays mid-rebase, `rwv land` lands again, and
//! `rwv doctor` reports nothing of the op.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::common;

use repoweave::diagnosis::State;
use repoweave::refusal::RefusalKind;

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

fn assert_routes_to(stderr: &str, token: &str) {
    let routed: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routed, vec![token], "stderr was:\n{stderr}");
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A primary weave with one member and a seat forked from it. Both commit to
/// the same file of the member and the primary relocks, so the seat's
/// `land` replays the primary's project repo onto the seat and parks on
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
    common::set_up_lock_driver(&primary.project);
    std::fs::write(primary.project.join(".gitignore"), "*.code-workspace\n").unwrap();
    std::fs::write(
        primary.project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nbranch = \"main\"\nrole = \"owned\"\n",
            common::file_url(&upstream)
        ),
    )
    .unwrap();
    std::fs::write(primary.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    common::record_trunk(&primary.project);
    common::pin_every_member(&primary.root);
    common::git_in(&primary.project, &["add", "-A"]);
    common::git_in(&primary.project, &["commit", "-m", "initial"]);

    succeed(
        &primary.root,
        &["workweave", "create", "--project", PROJECT, "s1"],
    );
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
    let out = rwv(&parked.seat.root, &["land", "--history", "linear"]);
    assert!(
        !out.status.success(),
        "the member conflict must park land:\n{}",
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

/// Resolves the member conflict a fresh `land` parks on and resumes it, so
/// the landing runs to completion.
fn land(parked: &Parked) {
    let out = rwv(&parked.seat.root, &["land", "--history", "linear"]);
    let stderr = stderr_of(&out);
    assert!(
        !stderr.contains("rwv explain op-in-progress")
            && !stderr.contains("rwv explain dead-op-lease"),
        "no state of the discarded op may refuse a new land:\n{}",
        transcript(&out)
    );
    if !out.status.success() {
        assert!(
            mid_op(&parked.seat.lib),
            "a new land may stop only on the member's genuine conflict:\n{}",
            transcript(&out)
        );
        std::fs::write(parked.seat.lib.join("c.txt"), "both\n").unwrap();
        common::git_in(&parked.seat.lib, &["add", "c.txt"]);
        common::git_in(
            &parked.seat.lib,
            &["-c", "core.editor=true", "rebase", "--continue"],
        );
        succeed(&parked.seat.root, &["land", "--continue"]);
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

/// A record whose op id and workspaces read while the rest does not is one
/// unreadable state whatever failed in it: a missing field, a variant this
/// release does not know, a value of the wrong type, a tip that is not a
/// commit id. Plain `rwv abort` from either side refuses it naming the
/// discard and moves nothing, so a commit another seat made on the target's
/// branch since the stop stays where it is.
#[test]
fn a_record_that_does_not_read_whole_is_refused_by_abort_whatever_failed_in_it() {
    type Edit = fn(&mut serde_json::Value);
    let edits: [(&str, Edit); 4] = [
        ("a missing field", |r| {
            r["take_in"]
                .as_object_mut()
                .unwrap()
                .remove("merge_upstream")
                .expect("the record carries the field the edit removes");
        }),
        ("an unknown variant", |r| {
            r["phase"] = "no-such-phase".into()
        }),
        ("a wrong type", |r| r["take_in"]["merges"] = 7.into()),
        ("a tag for a tip", |r| {
            r["converged_tips"]["(project)"] = "v1.0".into()
        }),
    ];
    for (what, edit) in edits {
        let parked = park();
        let target = parked.primary.root.clone();
        commit_file(&parked.primary.lib, "other.txt", "x\n", "another seat");
        let target_lib = head(&parked.primary.lib);
        let seat_lib = head(&parked.seat.lib);
        let seat_project = head(&parked.seat.project);
        rewrite_record(&parked, edit);
        let discard = format!(
            "rwv -C {} abort --discard-unreadable-record",
            parked.seat.root.display()
        );
        for (dir, args) in [
            (&parked.seat.root, &["abort"][..]),
            (&target, &["abort"][..]),
            (&parked.seat.root, &["land", "--continue"][..]),
        ] {
            let out = rwv(dir, args);
            let stderr = common::cli::refused(&out, RefusalKind::UnreadableOpRecord);
            assert!(
                stderr.contains(&discard),
                "{what}: rwv {args:?} must name the discard:\n{}",
                transcript(&out)
            );
        }
        assert_eq!(head(&parked.primary.lib), target_lib, "{what}");
        assert_eq!(head(&parked.seat.lib), seat_lib, "{what}");
        assert_eq!(head(&parked.seat.project), seat_project, "{what}");
        assert!(record_path(&parked).exists(), "{what}");
        succeed(&parked.seat.root, &["abort", "--discard-unreadable-record"]);
        assert!(!record_path(&parked).exists(), "{what}");
    }
}

/// A record that names no op refuses every verb that reads it, `rwv abort`
/// included, naming the verb that ends it; `rwv status` reports it on both
/// sides. That verb, run from the workspace the landing leases, removes the
/// record, the lease and the claim, moves nothing back and names the member
/// left stopped in the landing's rebase; following what it prints ends in a
/// usable weave.
#[test]
fn a_record_that_names_no_op_is_ended_by_the_verb_its_error_names() {
    let parked = park();
    let target = parked.primary.root.clone();
    std::fs::write(record_path(&parked), "{ not json").unwrap();
    let discard = format!(
        "rwv -C {} abort --discard-unreadable-record",
        parked.seat.root.display()
    );

    for (dir, args) in [
        (&parked.seat.root, &["abort"][..]),
        (&parked.seat.root, &["land", "--continue"][..]),
        (&target, &["abort"][..]),
    ] {
        let out = rwv(dir, args);
        let stderr = stderr_of(&out);
        assert!(!out.status.success(), "{}", transcript(&out));
        assert_routes_to(&stderr, "unreadable-op-record");
        assert!(
            stderr.contains(&discard),
            "rwv {args:?} must name the verb that ends the op:\n{}",
            transcript(&out)
        );
    }
    for dir in [&parked.seat.root, &target] {
        let out = succeed(dir, &["status"]);
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&discard),
            "{}",
            transcript(&out)
        );
    }

    let lib_tip = head(&parked.seat.lib);
    let out = succeed(&target, &["abort", "--discard-unreadable-record"]);
    let stderr = stderr_of(&out);
    for said in [
        "Nothing the operation moved was put back",
        "stopped in a rebase",
        "released the lease on",
        "savepoint",
    ] {
        assert!(
            stderr.contains(said),
            "it must say {said:?}:\n{}",
            transcript(&out)
        );
    }
    assert!(
        !stderr.contains("`rwv status` marks"),
        "the project repo it lists is one `rwv status` does not mark:\n{}",
        transcript(&out)
    );
    assert!(!record_path(&parked).exists());
    assert!(!target.join(".rwv-op-lease").exists());
    assert!(
        mid_op(&parked.seat.lib),
        "nothing is canceled without the record"
    );
    assert_eq!(head(&parked.seat.lib), lib_tip, "nothing is moved back");

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

    let out = succeed(&target, &["doctor", "--fix"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("orphaned-savepoint"),
        "the savepoints it left are doctor's to drop once no record claims them:\n{}",
        transcript(&out)
    );

    assert_no_op_residue(&parked);
    let out = rwv(&parked.seat.root, &["land", "--history", "linear"]);
    common::cli::refused(&out, State::PinBackwards);
    succeed(
        &parked.seat.root,
        &["member", "pin", LIB, "HEAD", "--commit"],
    );
    land(&parked);
    assert_no_op_residue(&parked);
    assert_doctor_reports_no_op_state(&parked);
}

/// What an earlier release leaves of a parked landing: a record with no
/// schema, the seat's marker with no name, the index at the project
/// checkout's old path, and manifests keyed `version`. The record's refusal
/// lists the migrations before the discard, in the order they run; each runs
/// verbatim, and the discard then names the member left mid-rebase.
#[test]
fn a_record_an_earlier_release_left_is_ended_by_the_lines_its_refusal_prints_in_order() {
    let parked = park();
    let target = parked.primary.root.clone();
    rewrite_record(&parked, |record| {
        let record = record.as_object_mut().unwrap();
        record.remove("schema");
        record.remove("release");
    });
    let marker = parked.seat.root.join(".rwv-workweave");
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("name").unwrap();
    std::fs::write(&marker, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    let index = common::index_file_of(&parked.primary.project);
    std::fs::remove_file(&index).unwrap();
    std::fs::write(
        parked.primary.project.join(".rwv-workweave-index"),
        serde_json::json!({
            "workweaves": { "s1": parked.seat.root.to_string_lossy() },
            "receipts": {},
        })
        .to_string(),
    )
    .unwrap();
    for project in [&parked.primary.project, &parked.seat.project] {
        let manifest = project.join("rwv.toml");
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(&manifest, text.replace("branch = ", "version = ")).unwrap();
    }

    let out = rwv(&target, &["status"]);
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    let lines: Vec<&str> = said
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("rwv -C "))
        .collect();
    let expected = [
        format!("rwv -C {} doctor --fix", target.display()),
        format!("rwv -C {} doctor --fix", parked.seat.root.display()),
        format!(
            "rwv -C {} abort --discard-unreadable-record",
            parked.seat.root.display()
        ),
    ];
    assert_eq!(lines, expected, "{}", transcript(&out));

    let mut last = None;
    for line in &expected {
        let words: Vec<&str> = line.split_whitespace().collect();
        last = Some(succeed(&target, &words[1..]));
    }
    let stderr = stderr_of(&last.unwrap());
    assert!(
        stderr.contains(&format!(
            "{}: left stopped in a rebase",
            parked.seat.lib.display()
        )),
        "the discard must name the member it leaves mid-rebase:\n{stderr}"
    );
    assert!(!record_path(&parked).exists());
    assert!(!target.join(".rwv-op-lease").exists());
}
