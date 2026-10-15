//! A clone sitting under a top-level directory no built-in registry names
//! and no project's `rwv.toml` names either is invisible to
//! `scan_repos_on_disk`'s walk entirely — not orphan-reported, its lock and
//! HEAD never compared against anything, because the walk never enumerates
//! that directory's name in the first place. `unknown-segment-clone` is the
//! finding that covers it; see `docs/internals/path-url-mapping.md` for the
//! residue this leaves.
//!
//! What this scan must NOT report is as load-bearing as what it must:
//! `projects/`, a dot-prefixed directory, and a directory shaped like build
//! output (`target/`, `node_modules/@scope/pkg`) all sit at the weave root
//! and must stay silent even though some share the three-segment shape a
//! registry path has.

use std::path::{Path, PathBuf};

mod common;

/// A weave with one project and no manifest entries outside the default
/// `projects/` tree — so every stray this file plants sits somewhere
/// nothing on disk or in a manifest explains.
fn bare_weave(tmp: &tempfile::TempDir) -> PathBuf {
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    common::rwv()
        .args(["init", "demo"])
        .current_dir(&ws)
        .assert()
        .success();
    ws
}

/// Clone a fresh bare commit into `dest`, so `Vcs::is_repo` sees a real repo
/// there rather than an empty directory.
fn plant_clone(tmp: &tempfile::TempDir, dest: &Path, origin_name: &str) {
    let origin = tmp.path().join("origins").join(origin_name);
    std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&origin);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    common::git_in(
        dest.parent().unwrap(),
        &["clone", origin.to_str().unwrap(), dest.to_str().unwrap()],
    );
}

/// `(kind, path)` pairs for every violation `rwv doctor --json` reports.
fn doctor_findings(ws: &Path, args: &[&str]) -> Vec<(String, String)> {
    let out = common::rwv()
        .arg("doctor")
        .args(args)
        .arg("--json")
        .current_dir(ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{stdout}"));
    report["violations"]
        .as_array()
        .expect("the report carries a violations array")
        .iter()
        .map(|v| {
            (
                v["kind"].as_str().unwrap_or_default().to_owned(),
                v["path"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// A clone under a segment nothing names is reported whether or not `--all`
/// is passed: which top-level directories are registry segments is already
/// a fact about every project's manifest under the weave root, unlike
/// `orphaned-clone`'s `known_repos` comparison, which needs every project
/// loaded to be meaningful.
#[test]
fn a_clone_under_a_wholly_unnamed_segment_is_reported_with_or_without_all() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);

    plant_clone(
        &tmp,
        &ws.join("selfhosted.example").join("acme").join("stray"),
        "stray.git",
    );

    let manifest = std::fs::read_to_string(ws.join("projects").join("demo").join("rwv.toml"))
        .expect("the project manifest is readable");
    assert!(
        !manifest.contains("selfhosted.example"),
        "the pin needs a manifest naming nothing under this segment, got:\n{manifest}"
    );

    for args in [&[][..], &["--all"][..]] {
        let findings = doctor_findings(&ws, args);
        assert!(
            findings.contains(&(
                "unknown-segment-clone".to_owned(),
                "selfhosted.example/acme/stray".to_owned()
            )),
            "a clone in a segment no manifest and no builtin names must be reported \
             regardless of --all (args={args:?}), got: {findings:?}"
        );
        assert!(
            !findings.iter().any(|(k, _)| k == "dangling-reference"),
            "nothing in the manifest names this path, so it cannot be a dangling \
             reference (args={args:?}), got: {findings:?}"
        );
    }
}

/// A repo nested inside `projects/` belongs to the project it sits under —
/// a vendored submodule, say — not to an unknown registry segment, however
/// deep it sits.
#[test]
fn a_nested_repo_under_projects_is_never_reported_as_unknown_segment() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);

    plant_clone(
        &tmp,
        &ws.join("projects")
            .join("demo")
            .join("vendor")
            .join("nested"),
        "nested.git",
    );

    let findings = doctor_findings(&ws, &["--all"]);
    assert!(
        !findings.iter().any(|(k, _)| k == "unknown-segment-clone"),
        "a repo nested inside the project directory is not an unknown registry \
         segment: {findings:?}"
    );
}

/// A dot-prefixed top-level directory is never a candidate segment — every
/// registry segment this tree mints is a bare host or `local`, never
/// dot-prefixed.
#[test]
fn a_repo_under_a_dot_prefixed_directory_is_never_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);

    plant_clone(
        &tmp,
        &ws.join(".cache").join("acme").join("hidden"),
        "hidden.git",
    );

    let findings = doctor_findings(&ws, &["--all"]);
    assert!(
        !findings.iter().any(|(k, _)| k == "unknown-segment-clone"),
        "a dot-prefixed top-level directory is never a registry segment: {findings:?}"
    );
}

/// `target/` (cargo build output can land at the weave root because
/// `Cargo.toml` surfaces there) and `node_modules/@scope/pkg` (the same
/// three-segment shape a registry path has) hold no real repo at the leaf,
/// so the shape alone must not be enough to report them.
#[test]
fn build_output_shaped_directories_with_no_real_repo_are_not_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);

    std::fs::create_dir_all(ws.join("target").join("debug").join("deps")).unwrap();
    std::fs::create_dir_all(ws.join("node_modules").join("@scope").join("pkg")).unwrap();

    let findings = doctor_findings(&ws, &["--all"]);
    assert!(
        !findings.iter().any(|(k, _)| k == "unknown-segment-clone"),
        "neither target/ nor node_modules/ holds a real repo at the leaf: {findings:?}"
    );
}

/// A weave sitting inside an enclosing git repository (a `$HOME` dotfiles
/// repo, say) must not read its own build output as a repo: `git rev-parse
/// --git-dir` succeeds from any directory under the enclosing repo by
/// walking up to its `.git`, and `Vcs::is_repo` must not mistake that for
/// the candidate itself being a repo.
#[test]
fn build_output_inside_an_enclosing_repo_is_still_not_reported() {
    let tmp = common::tempdir().unwrap();
    common::git_in(tmp.path(), &["init"]);

    let ws = bare_weave(&tmp);
    std::fs::create_dir_all(ws.join("target").join("debug").join("deps")).unwrap();
    std::fs::create_dir_all(ws.join("node_modules").join("@scope").join("pkg")).unwrap();

    let findings = doctor_findings(&ws, &["--all"]);
    assert!(
        !findings.iter().any(|(k, _)| k == "unknown-segment-clone"),
        "an enclosing repo must not make build output read as a repo at member \
         depth: {findings:?}"
    );
}

/// Once one member under a segment is declared, that segment is known —
/// `scan_repos_on_disk` already walks it — so a stray sibling in the same
/// segment is `orphaned-clone`, and declaring the one member must not also
/// surface the stray a second time under `unknown-segment-clone`.
#[test]
fn a_stray_clone_in_a_partially_known_segment_is_orphaned_not_unknown() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);

    let declared_origin = tmp.path().join("origins").join("lib.git");
    std::fs::create_dir_all(declared_origin.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&declared_origin);

    let segment = ws.join("codeberg.org").join("acme");
    std::fs::create_dir_all(&segment).unwrap();
    common::git_in(
        &segment,
        &[
            "clone",
            declared_origin.to_str().unwrap(),
            segment.join("lib").to_str().unwrap(),
        ],
    );
    plant_clone(&tmp, &segment.join("stray"), "stray.git");

    let manifest_path = ws.join("projects").join("demo").join("rwv.toml");
    let mut manifest = std::fs::read_to_string(&manifest_path).unwrap();
    manifest.push_str(&format!(
        "\n[repositories.\"codeberg.org/acme/lib\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
        common::file_url(&declared_origin)
    ));
    std::fs::write(&manifest_path, manifest).unwrap();

    common::rwv()
        .args(["activate", "demo"])
        .current_dir(&ws)
        .assert()
        .success();

    let findings = doctor_findings(&ws, &["--all"]);
    assert!(
        findings.contains(&(
            "orphaned-clone".to_owned(),
            "codeberg.org/acme/stray".to_owned()
        )),
        "a stray beside a declared member in the same segment is a known-segment \
         orphan: {findings:?}"
    );
    assert!(
        !findings
            .iter()
            .any(|(k, p)| k == "unknown-segment-clone" && p == "codeberg.org/acme/stray"),
        "declaring one member under a segment must not also surface its sibling \
         as unknown-segment: {findings:?}"
    );
}

/// The finding behaves the same from inside a workweave as from the
/// primary: the path reported is relative to the workweave's own root, not
/// the primary's.
#[test]
fn an_unknown_segment_clone_is_reported_from_inside_a_workweave() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let member_path = ws.join("github").join("org").join("repo");
    std::fs::create_dir_all(&member_path).unwrap();
    common::git_in(&member_path, &["init", "--initial-branch=main"]);
    common::git_in(&member_path, &["config", "user.email", "test@test.com"]);
    common::git_in(&member_path, &["config", "user.name", "Test"]);
    std::fs::write(member_path.join("README"), "init").unwrap();
    common::git_in(&member_path, &["add", "."]);
    common::git_in(&member_path, &["commit", "-m", "initial"]);

    let project_dir = ws.join("projects").join("demo");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::file_url(&member_path)
        ),
    )
    .unwrap();

    let container = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&container).unwrap();

    common::rwv()
        .args(["workweave", "demo", "create", "feat"])
        .current_dir(&ws)
        .assert()
        .success();
    let ww_dir = container.join("demo--feat");
    assert!(
        ww_dir.is_dir(),
        "workweave create must materialize its directory"
    );

    plant_clone(
        &tmp,
        &ww_dir.join("selfhosted.example").join("acme").join("stray"),
        "ww-stray.git",
    );

    let findings = doctor_findings(&ww_dir, &[]);
    assert!(
        findings.contains(&(
            "unknown-segment-clone".to_owned(),
            "selfhosted.example/acme/stray".to_owned()
        )),
        "a stray in an unnamed segment inside a workweave must be reported against \
         the workweave's own root, got: {findings:?}"
    );
}

/// Plants a clone under `codeberg.org/acme/lib`, declares it in `project`'s
/// manifest, then replaces that manifest with text that does not parse. The
/// segment is now named only by a manifest nothing can read.
fn member_of_broken_project(tmp: &tempfile::TempDir, ws: &Path, project: &str) {
    let origin = tmp.path().join("origins").join("lib.git");
    std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&origin);
    let dest = ws.join("codeberg.org").join("acme").join("lib");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    common::git_in(
        dest.parent().unwrap(),
        &["clone", origin.to_str().unwrap(), dest.to_str().unwrap()],
    );

    let manifest_path = ws.join("projects").join(project).join("rwv.toml");
    std::fs::write(&manifest_path, "[repositories.\"codeberg.org/acme/lib\"\n").unwrap();
}

fn assert_broken_manifest_never_reads_as_unknown_segment(ws: &Path, args: &[&str]) {
    let findings = doctor_findings(ws, args);
    assert!(
        !findings.iter().any(|(k, _)| k == "unknown-segment-clone"),
        "a member of a project whose manifest does not parse must not read as an \
         unknown-segment clone: {findings:?}"
    );
    assert!(
        findings.iter().any(|(k, _)| k == "unparseable-project"),
        "the unparseable manifest is reported in the same run: {findings:?}"
    );
}

#[test]
fn an_unparseable_active_manifest_never_yields_unknown_segment_clone() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);
    member_of_broken_project(&tmp, &ws, "demo");

    assert_broken_manifest_never_reads_as_unknown_segment(&ws, &[]);
    assert_broken_manifest_never_reads_as_unknown_segment(&ws, &["--all"]);
}

#[test]
fn an_unparseable_non_active_manifest_never_yields_unknown_segment_clone() {
    let tmp = common::tempdir().unwrap();
    let ws = bare_weave(&tmp);
    common::rwv()
        .args(["init", "other"])
        .current_dir(&ws)
        .assert()
        .success();
    common::rwv()
        .args(["activate", "demo"])
        .current_dir(&ws)
        .assert()
        .success();
    member_of_broken_project(&tmp, &ws, "other");

    assert_broken_manifest_never_reads_as_unknown_segment(&ws, &[]);
}
