//! A project name renders as ONE path segment everywhere a flat address is
//! needed, with `/` written as `+`.
//!
//! The property that carries the whole scheme is **injectivity**: two distinct
//! `(project, workweave)` pairs must never render the same directory name.
//! It is asserted here over the namespace the validators define rather than
//! over examples, and it is the validators that make it hold — `+` is
//! unmintable in a project name, so every `+` in a rendered segment is an
//! encoded `/`. Loosen `validate_project_name` and the corpus below grows to
//! contain `a+b` beside `a/b`, which render the same segment, and the property
//! test reports the collision.
//!
//! The four consumers the flat rendering exists for are driven end to end
//! against the shipped binary: the workweave directory, the ephemeral ref, the
//! `-w` address, and the `.code-workspace` filename.

use assert_cmd::Command;
use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::workspace::{parse_weave_dir_name, weave_dir_name};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

// ---------------------------------------------------------------------------
// Injectivity, over the namespace the validators define
// ---------------------------------------------------------------------------

/// Every string of length 1..=3 over an alphabet chosen for the characters the
/// grammar turns on: the segment separator, its encoding, the `-` the `--`
/// separator is built from, a `.` that ref-name validation has opinions about,
/// and two ordinary letters.
fn candidates() -> Vec<String> {
    const ALPHABET: [char; 6] = ['a', 'b', '-', '/', '+', '.'];
    let mut out: Vec<String> = ALPHABET.iter().map(|c| c.to_string()).collect();
    let singles = out.clone();
    for a in &singles {
        for b in &singles {
            out.push(format!("{a}{b}"));
            for c in &singles {
                out.push(format!("{a}{b}{c}"));
            }
        }
    }
    out
}

/// Every valid project name in the candidate set — the corpus is the
/// validator's own answer, so loosening the validator widens what is tested
/// rather than leaving a typed list behind.
fn valid_projects() -> Vec<ProjectName> {
    candidates()
        .into_iter()
        .filter_map(|s| ProjectName::new(s).ok())
        .collect()
}

fn valid_workweaves() -> Vec<WorkweaveName> {
    candidates()
        .into_iter()
        .filter_map(|s| WorkweaveName::new(s).ok())
        .collect()
}

/// The corpus really is a corpus, and really does contain the shapes the
/// property turns on. A validator that started rejecting everything would
/// leave every assertion below vacuously true.
#[test]
fn the_namespace_corpus_covers_what_the_encoding_turns_on() {
    let projects = valid_projects();
    let workweaves = valid_workweaves();

    assert!(
        projects.len() > 20 && workweaves.len() > 20,
        "corpus too small to be a namespace: {} projects, {} workweaves",
        projects.len(),
        workweaves.len()
    );
    assert!(
        projects.iter().any(|p| p.as_str().contains('/')),
        "a multi-segment project name must be in the corpus, or the encoding is never exercised"
    );
    assert!(
        candidates().iter().any(|c| c.contains('+')),
        "the generator must offer `+`-bearing candidates for the validator to reject"
    );
    assert!(
        !projects.iter().any(|p| p.as_str().contains('+')),
        "a `+`-bearing project name must not validate: it is what makes the decode ambiguous"
    );
}

/// Two distinct pairs never render the same directory name.
///
/// This is the load-bearing property, and it is also what fails if `+` becomes
/// mintable: `a/b` and `a+b` would both be valid project names rendering
/// `a+b`, and the collision below names them.
#[test]
fn the_rendered_directory_name_is_injective_over_the_namespace() {
    let projects = valid_projects();
    let workweaves = valid_workweaves();

    let mut seen: HashMap<String, (String, String)> = HashMap::new();
    let mut pairs = 0usize;
    for project in &projects {
        for workweave in &workweaves {
            let rendered = weave_dir_name(project, workweave);
            pairs += 1;
            let this = (project.as_str().to_owned(), workweave.as_str().to_owned());
            if let Some(other) = seen.insert(rendered.clone(), this.clone()) {
                panic!("`{rendered}` is rendered by two distinct pairs: {other:?} and {this:?}");
            }
        }
    }
    assert_eq!(
        seen.len(),
        pairs,
        "every pair must render a name of its own"
    );
    assert!(pairs > 400, "only {pairs} pairs — the corpus collapsed");
}

/// The decode is the same seam's other direction, so it recovers exactly what
/// the render was given. Delete the `+`→`/` decode and this reports the
/// project half coming back with the encoding still in it.
#[test]
fn every_rendered_name_round_trips_through_the_decode() {
    let projects = valid_projects();
    let workweaves = valid_workweaves();
    let mut checked = 0usize;
    for project in &projects {
        for workweave in &workweaves {
            let rendered = weave_dir_name(project, workweave);
            let (decoded_project, decoded_workweave) = parse_weave_dir_name(&rendered)
                .unwrap_or_else(|| panic!("`{rendered}` must parse back"));
            assert_eq!(
                decoded_project,
                project.as_str(),
                "project half of `{rendered}` must decode to what rendered it"
            );
            assert_eq!(&decoded_workweave, workweave);
            checked += 1;
        }
    }
    assert!(checked > 400, "only {checked} round-trips exercised");
}

/// The rendered project half is one path segment. Everything downstream — a
/// directory name, a ref component, a filename — assumes it.
#[test]
fn the_rendered_project_half_is_one_segment() {
    let workweave = WorkweaveName::new("w").unwrap();
    for project in valid_projects() {
        let rendered = weave_dir_name(&project, &workweave);
        let (left, _) = rendered
            .split_once("--")
            .expect("the render always carries the separator");
        assert!(
            !left.contains('/') && !left.contains('\\'),
            "`{left}` (from project `{}`) is a path, not a segment",
            project.as_str()
        );
    }
}

/// The prohibition the decode rests on, and the decode that rests on it,
/// asserted together. They may not ship apart: a project name carrying `+`
/// that predated the decode would come back as a nesting it never had.
#[test]
fn the_unmintable_plus_and_the_decode_hold_together() {
    let err =
        ProjectName::new("chatly+web-app").expect_err("`+` must not be mintable in a project name");
    let rendered = format!("{err}");
    assert!(
        rendered.contains('+') && rendered.contains("not a valid project name"),
        "the refusal must name the character it refuses: {rendered}"
    );

    let (project, _) =
        parse_weave_dir_name("chatly+web-app--wtest").expect("the flat address must parse");
    assert_eq!(
        project, "chatly/web-app",
        "`+` in the project half must decode as the segment separator"
    );
}

// ---------------------------------------------------------------------------
// The four consumers, driven against the shipped binary
// ---------------------------------------------------------------------------

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

/// A weave whose only project is multi-segment, with `wtest` created in it.
/// Returns `(weave root, workweave dir)`.
fn make_nested_project_weave(tmp: &Path) -> (PathBuf, PathBuf) {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    rwv()
        .args(["init", "chatly/web-app"])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/chatly/web-app");
    git(&["init", "--initial-branch=main"], &project_dir);
    git(&["config", "user.email", "t@t"], &project_dir);
    git(&["config", "user.name", "T"], &project_dir);
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "initial"], &project_dir);

    rwv()
        .args(["workweave", "chatly/web-app", "create", "wtest"])
        .current_dir(&ws)
        .assert()
        .success();

    let container = tmp.join(".workweaves");
    (ws, container.join("chatly+web-app--wtest"))
}

fn branch_names(repo: &Path) -> Vec<String> {
    let out = common::git()
        .args([
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/heads/",
        ])
        .current_dir(repo)
        .output()
        .expect("git should be available");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Consumer 1: the workweave directory is a child of the container, not a
/// grandchild — so no intermediate directory exists for doctor to read as a
/// stray.
#[test]
fn a_multi_segment_project_places_its_workweave_flat() {
    let tmp = common::tempdir().unwrap();
    let (ws, ww) = make_nested_project_weave(tmp.path());

    assert!(
        ww.join(".rwv-workweave").exists(),
        "the workweave must be at {}",
        ww.display()
    );
    assert!(
        !tmp.path().join(".workweaves/chatly").exists(),
        "no intermediate directory may be left in the container"
    );

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("has no `.rwv-workweave` marker"),
        "doctor must not report a stray container directory: {combined}"
    );
}

/// Consumer 2: the ephemeral ref is one component, so no ref directory is
/// created — a namespace git will not also let be a ref file.
#[test]
fn a_multi_segment_project_mints_a_single_component_ref() {
    let tmp = common::tempdir().unwrap();
    let (ws, _) = make_nested_project_weave(tmp.path());

    let branches = branch_names(&ws.join("projects/chatly/web-app"));
    assert!(
        branches.iter().any(|b| b == "chatly+web-app--wtest"),
        "the minted ref must be one component; branches: {branches:?}"
    );
    assert!(
        !branches.iter().any(|b| b.contains('/')),
        "no ref may make `chatly` a ref directory; branches: {branches:?}"
    );
}

/// Consumer 3: `-w` accepts the identity rwv itself mints. The regression is
/// that it did not — it refused its own output as a path.
#[test]
fn the_minted_identity_is_addressable_by_w() {
    let tmp = common::tempdir().unwrap();
    let (ws, _) = make_nested_project_weave(tmp.path());

    let out = rwv()
        .args(["-w", "chatly+web-app--wtest", "status"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "`-w` must accept the identity rwv mints: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Consumer 4: the `.code-workspace` filename is one segment, so the name
/// contributes no directory of its own — the file lands in the project's own
/// directory, and the surfacing link beside it at the weave root.
///
/// The generated file is asserted present rather than left to whatever the
/// walk happens to turn up: until a multi-segment project was enumerable at
/// all, `doctor --fix` regenerated nothing here and the only `.code-workspace`
/// this found was the dangling surfacing symlink `activate` had planted.
#[test]
fn the_code_workspace_filename_is_one_segment() {
    let tmp = common::tempdir().unwrap();
    let (ws, _) = make_nested_project_weave(tmp.path());

    let _ = rwv().args(["doctor", "--fix"]).current_dir(&ws).output();

    let project_dir = ws.join("projects").join("chatly").join("web-app");
    let mut found = Vec::new();
    collect_by_extension(&ws, "code-workspace", &mut found);
    assert!(
        found.contains(&project_dir.join("chatly+web-app.code-workspace")),
        "`doctor --fix` must regenerate the managed file in the project's own \
         directory; found {found:?}"
    );
    for path in &found {
        let name = path.file_name().unwrap().to_string_lossy();
        assert_eq!(
            name,
            "chatly+web-app.code-workspace",
            "the filename must be one segment; found {}",
            path.display()
        );
        let parent = path.parent().unwrap();
        assert!(
            parent == ws || parent == project_dir,
            "the name must create no directory of its own — a `.code-workspace` \
             may sit in the project directory or at the weave root as the \
             surfacing link, nowhere else; found {}",
            path.display()
        );
    }
}

fn collect_by_extension(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
        if path.is_dir() {
            collect_by_extension(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// The corrected `-w` advice, read where the operator reads it
// ---------------------------------------------------------------------------

/// The old refusal offered `-C <the same string>`, which for a nested project
/// is not the path either — the path is `<container>/chatly/web-app--wtest`.
/// The advice now names the address that works, and no longer names one that
/// does not.
#[test]
fn the_w_refusal_offers_the_address_that_works() {
    let tmp = common::tempdir().unwrap();
    let (ws, _) = make_nested_project_weave(tmp.path());

    let out = rwv()
        .args(["-w", "chatly/web-app--wtest", "status"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "a slashed address must refuse");
    assert!(
        stderr.contains("rwv -w chatly+web-app--wtest"),
        "the refusal must offer the flat address: {stderr}"
    );
    assert!(
        !stderr.contains("-C chatly/web-app--wtest"),
        "the refusal must not offer a -C argument that is not the path: {stderr}"
    );

    // The advice is a claim about the tool, so run it.
    rwv()
        .args(["-w", "chatly+web-app--wtest", "status"])
        .current_dir(&ws)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// Legacy nested directories: reported, and never renamed
// ---------------------------------------------------------------------------

/// Rebuild the pre-encoding placement by hand: the workweave one level down,
/// its first segment left behind as a marker-less directory. This is the shape
/// rwv wrote before the flat rendering, and the only way to obtain it now.
fn make_legacy_nested_workweave(tmp: &Path) -> (PathBuf, PathBuf) {
    let (ws, flat) = make_nested_project_weave(tmp);
    let nested = tmp.join(".workweaves/chatly/web-app--wtest");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    std::fs::rename(&flat, &nested).unwrap();

    let index_path = ws.join("projects/chatly/web-app/.rwv-workweave-index");
    let mut index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
    index["workweaves"]["wtest"] = serde_json::json!(nested.canonicalize().unwrap());
    std::fs::write(&index_path, serde_json::to_string(&index).unwrap()).unwrap();
    (ws, nested)
}

/// The finding names the directory, the single-segment name the records now
/// spell, and the remedy — and it replaces the stray-directory report, whose
/// advice ("inspect and remove it") pointed at a directory holding a live
/// workweave.
#[test]
fn a_legacy_nested_workweave_is_reported_with_the_retire_remedy() {
    let tmp = common::tempdir().unwrap();
    let (ws, nested) = make_legacy_nested_workweave(tmp.path());

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("sits below its container"),
        "doctor must report the nested placement: {combined}"
    );
    assert!(
        combined.contains("chatly+web-app--wtest"),
        "the finding must name the single-segment spelling: {combined}"
    );
    assert!(
        combined.contains("Retire this workweave and create it again"),
        "the remedy must be retire-and-recreate: {combined}"
    );
    assert!(
        !combined.contains(&format!(
            "{}: directory under workweaves parent has no",
            nested.parent().unwrap().display()
        )),
        "the stray-directory report must not stand beside it: {combined}"
    );
}

/// The prohibition. `--fix` may report this and must not move it: the rename
/// crosses a directory boundary, which strands the worktrees inside and the
/// recorded path that found them.
///
/// Revert `NestedWorkweaveDir`'s `ReportOnly` arm in
/// `CheckViolation::fix_disposition` to `Auto` and this is the assertion that
/// stands between that change and a repair nobody wrote.
#[test]
fn doctor_fix_never_renames_a_legacy_nested_workweave() {
    let tmp = common::tempdir().unwrap();
    let (ws, nested) = make_legacy_nested_workweave(tmp.path());
    let flat = tmp.path().join(".workweaves/chatly+web-app--wtest");

    let _ = rwv().args(["doctor", "--fix"]).current_dir(&ws).output();

    assert!(
        nested.join(".rwv-workweave").exists(),
        "the workweave must still be where it was: {}",
        nested.display()
    );
    assert!(
        !flat.exists(),
        "`--fix` must not have moved it to {}",
        flat.display()
    );

    let index_path = ws.join("projects/chatly/web-app/.rwv-workweave-index");
    let index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
    assert_eq!(
        index["workweaves"]["wtest"].as_str().map(PathBuf::from),
        Some(nested.canonicalize().unwrap()),
        "the recorded path must still name the directory on disk"
    );

    // Reported again on the next run: a report-only finding does not clear
    // itself by being read.
    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("sits below its container"),
        "the finding must survive `--fix`: {combined}"
    );
}

/// `--fix` reports the disposition register as its own authority, so the
/// prohibition above is also readable there. Both are asserted: the register
/// entry is what a future change would edit, and the behaviour above is what
/// an operator would notice.
#[test]
fn the_nested_finding_is_declared_report_only() {
    use repoweave::check::{CheckViolation, FixDisposition, WorkweaveTreeIntegrityKind};
    let violation = CheckViolation::WorkweaveTreeIntegrity {
        workweave_dir: PathBuf::from("/ws/.workweaves/chatly/web-app--wtest"),
        sub_kind: WorkweaveTreeIntegrityKind::NestedWorkweaveDir {
            project: "chatly/web-app".into(),
            workweave_name: "wtest".into(),
            expected_dir_name: "chatly+web-app--wtest".into(),
        },
    };
    assert_eq!(
        violation.fix_disposition(),
        FixDisposition::ReportOnly,
        "a nested workweave is never repaired in place"
    );
}
