//! A project is a directory under `projects/` that holds an `rwv.toml`, named
//! by its path relative to `projects/` — at **any** depth.
//!
//! The enumeration used to read the immediate children of `projects/` and stop,
//! so `chatly/web-app` enumerated as `chatly`. Everything downstream inherited
//! that: doctor's registry reconciliation reported every multi-segment
//! project's workweaves as unregistered, on a weave where `create` had recorded
//! them correctly, and `--fix` re-recorded them into an index the next run
//! could still not read — a finding that returns forever.
//!
//! Depth is the part that was argued rather than measured. Nothing restricts a
//! project name to one `/`, and every multi-segment fixture in the suite was
//! exactly one, so the fixtures here run to three segments and drive the
//! shipped binary at that depth.
//!
//! Two states sit where a project would without being one, and both are
//! reported rather than walked past in silence: a directory holding no
//! manifest anywhere below it, and one holding a manifest under a path the
//! project-name validator refuses. The second is the divergence this closes —
//! the same directory used to be a project on the orientation surfaces and not
//! a project in the registry passes.

use repoweave::workspace::{discover_projects, scan_projects};
use std::path::{Path, PathBuf};

mod common;

/// A weave root with `projects/` and a registry directory, no projects yet.
fn make_weave(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    std::fs::create_dir_all(ws.join("github")).unwrap();
    ws
}

/// Mint `projects/<name>/` as a project: the directory plus the manifest that
/// makes it one.
fn make_project(ws: &Path, name: &str) {
    let dir = ws.join("projects").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("rwv.toml"), "[repositories]\n").unwrap();
}

fn names(ws: &Path) -> Vec<String> {
    discover_projects(ws)
        .into_iter()
        .map(|p| p.as_str().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// The rule: a manifest below `projects/`, at any depth
// ---------------------------------------------------------------------------

/// One, two and three segments, from one walk. The three-segment case is the
/// one the old enumeration would still have missed after a single extra level
/// of recursion, and it is missed the same silent way — a prefix enumerated in
/// place of the project.
#[test]
fn a_project_is_enumerated_at_any_depth() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    make_project(&ws, "alpha");
    make_project(&ws, "chatly/web-app");
    make_project(&ws, "a/b/c");

    assert_eq!(
        names(&ws),
        vec!["a/b/c", "alpha", "chatly/web-app"],
        "every project must be named by its whole path below projects/"
    );
}

/// The nested-manifest rule, which the mint agrees with: the outermost
/// manifest is the project, and everything below it is that project's own
/// working tree.
#[test]
fn descent_stops_at_the_first_manifest() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    make_project(&ws, "acme");
    make_project(&ws, "acme/inner");

    assert_eq!(
        names(&ws),
        vec!["acme"],
        "a manifest inside a project's working tree is one of its files, not a \
         second project"
    );
    assert!(
        scan_projects(&ws).projectless.is_empty(),
        "nothing below a project is a stray directory"
    );
}

/// `.git` and its neighbours are host state, and the ref-name validator
/// refuses a component starting with `.` anyway — so descending into one could
/// only mint a name no project can carry.
#[test]
fn a_dot_directory_is_neither_walked_nor_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    make_project(&ws, "alpha");
    let hidden = ws.join("projects").join(".cache");
    std::fs::create_dir_all(hidden.join("inner")).unwrap();
    std::fs::write(hidden.join("rwv.toml"), "[repositories]\n").unwrap();

    let scan = scan_projects(&ws);
    assert_eq!(names(&ws), vec!["alpha"]);
    assert!(
        scan.projectless.is_empty() && scan.unnameable.is_empty(),
        "a dot-directory is not walked, so it is not reported either: {scan:?}"
    );
}

// ---------------------------------------------------------------------------
// What the walk finds that is not a project
// ---------------------------------------------------------------------------

/// A hand-made directory ahead of its manifest. `rwv init` cannot repair it —
/// it mints the directory and refuses one already there — which is why the
/// finding names writing the manifest.
#[test]
fn a_directory_with_no_manifest_below_it_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    make_project(&ws, "alpha");
    std::fs::create_dir_all(ws.join("projects").join("scaffold")).unwrap();

    let scan = scan_projects(&ws);
    assert_eq!(
        scan.projectless,
        vec![ws.join("projects").join("scaffold")],
        "the directory holding nothing must be named"
    );
    assert_eq!(
        names(&ws),
        vec!["alpha"],
        "and it must not be enumerated as a project"
    );
}

/// The control the finding above needs: a directory that exists only to hold
/// projects is a namespace, not a stray, and reporting it would fire on every
/// weave that spells a project with a `/`.
///
/// Reported at the outermost barren directory, not at every level below it: a
/// finding per level names one state three times.
#[test]
fn a_namespace_is_not_reported_and_a_barren_branch_is_reported_once() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    make_project(&ws, "acme/web-app");
    std::fs::create_dir_all(ws.join("projects").join("acme").join("junk").join("deep")).unwrap();

    assert_eq!(
        scan_projects(&ws).projectless,
        vec![ws.join("projects").join("acme").join("junk")],
        "`acme` holds a project so it is a namespace; `acme/junk/deep` is inside \
         a directory already reported"
    );
}

/// The divergence this closes. `bad--name` contains the `--` that joins
/// project to workweave, so the validator refuses it — and the old
/// enumeration showed it on the orientation surfaces while the registry
/// passes silently dropped it. Now neither reads it as a project and doctor
/// says why.
#[test]
fn a_manifest_under_a_refused_name_is_reported_not_dropped() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    make_project(&ws, "alpha");
    make_project(&ws, "bad--name");

    let scan = scan_projects(&ws);
    assert_eq!(names(&ws), vec!["alpha"]);
    assert_eq!(scan.unnameable.len(), 1, "one refused name: {scan:?}");
    assert_eq!(scan.unnameable[0].derived, "bad--name");
    assert_eq!(
        scan.unnameable[0].dir,
        ws.join("projects").join("bad--name")
    );
    assert!(
        scan.projectless.is_empty(),
        "a directory that holds a manifest is not manifest-less, whatever its \
         name: {scan:?}"
    );
}

// ---------------------------------------------------------------------------
// The operator surfaces, driven
// ---------------------------------------------------------------------------

fn doctor_kinds(ws: &Path) -> Vec<String> {
    let out = common::rwv()
        .args(["doctor", "--all", "--json"])
        .current_dir(ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let doc: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("`doctor --json` did not emit JSON ({e}):\n{stdout}"));
    doc["violations"]
        .as_array()
        .expect("violations array")
        .iter()
        .map(|v| v["kind"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Both findings reach the wire, and a weave holding neither state reports
/// neither — a check that fires on a clean tree is a check nobody keeps.
#[test]
fn doctor_reports_both_states_and_only_when_they_are_there() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    make_project(&ws, "alpha");

    let clean = doctor_kinds(&ws);
    assert!(
        !clean.contains(&"projectless-dir".to_string())
            && !clean.contains(&"unnameable-project".to_string()),
        "a weave whose only directory under projects/ is a project must raise \
         neither finding; got {clean:?}"
    );

    std::fs::create_dir_all(ws.join("projects").join("scaffold")).unwrap();
    make_project(&ws, "bad--name");

    let seeded = doctor_kinds(&ws);
    assert!(
        seeded.contains(&"projectless-dir".to_string()),
        "the manifest-less directory must be reported; got {seeded:?}"
    );
    assert!(
        seeded.contains(&"unnameable-project".to_string()),
        "the refused name must be reported; got {seeded:?}"
    );
}

/// The text report carries the remedy, and the remedy is not one the state
/// blocks: `rwv init` mints the directory and refuses one that is there, so
/// the line names writing the manifest instead.
#[test]
fn the_projectless_report_names_a_remedy_the_state_allows() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    make_project(&ws, "alpha");
    std::fs::create_dir_all(ws.join("projects").join("scaffold")).unwrap();

    let out = common::rwv()
        .args(["doctor", "--all"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text
        .lines()
        .find(|l| l.contains("scaffold"))
        .unwrap_or_else(|| panic!("the text report must name the directory:\n{text}"));
    assert!(
        line.contains("rwv.toml") && line.contains("remove the directory"),
        "the line must say what to do next: {line}"
    );
    assert!(
        !line.contains("rwv init"),
        "`rwv init` refuses a directory that already exists, so naming it as \
         the repair sends the operator into a refusal: {line}"
    );
}

/// The orientation surfaces and the registry passes name the same set. Bare
/// `rwv` used to list a name the registry passes dropped, which is how
/// `bad--name` reached an agent's orientation text as a project.
#[test]
fn the_orientation_listing_names_what_the_walk_enumerates() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    make_project(&ws, "chatly/web-app");
    make_project(&ws, "bad--name");

    let out = common::rwv().current_dir(&ws).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let listing = text
        .lines()
        .find(|l| l.starts_with("Projects:"))
        .unwrap_or_else(|| panic!("bare `rwv` must list projects:\n{text}"))
        .to_owned();

    assert!(
        listing.contains("chatly/web-app"),
        "a multi-segment project must be listed by its whole name: {listing}"
    );
    assert!(
        !listing.contains("bad--name"),
        "a name the validator refuses is not a project on any surface: {listing}"
    );
    assert_eq!(names(&ws), vec!["chatly/web-app"]);
}

// ---------------------------------------------------------------------------
// The P1 mechanism, driven end to end
// ---------------------------------------------------------------------------

fn git(args: &[&str], dir: &Path) {
    let out = common::git().args(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A weave whose only project is three segments deep, with one workweave
/// created in it the ordinary way.
fn make_deep_project_weave(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    common::rwv()
        .args(["init", "a/b/c"])
        .current_dir(&ws)
        .output()
        .unwrap();

    let project_dir = ws.join("projects").join("a").join("b").join("c");
    git(&["init", "--initial-branch=main"], &project_dir);
    git(&["config", "user.email", "t@t"], &project_dir);
    git(&["config", "user.name", "T"], &project_dir);
    git(&["add", "-A"], &project_dir);
    git(&["commit", "-m", "initial"], &project_dir);

    let created = common::rwv()
        .args(["workweave", "a/b/c", "create", "wtest"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "workweave create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    ws
}

/// The P1 symptom. `create` recorded the workweave in its project's index;
/// the reconciliation pass could not read that index, because the project it
/// enumerated was the path prefix. Every such workweave was reported
/// unregistered on a weave nothing had gone wrong in.
#[test]
fn a_deep_projects_workweave_is_not_reported_unregistered() {
    let tmp = common::tempdir().unwrap();
    let ws = make_deep_project_weave(tmp.path());

    let kinds = doctor_kinds(&ws);
    assert!(
        !kinds.contains(&"workweave-tree-integrity".to_string()),
        "a workweave `create` recorded is registered; got {kinds:?}"
    );
}

/// And the repair writes the index the marker names, never one beside a path
/// prefix — a `.rwv-workweave-index` at `projects/a/` would be a record
/// nothing reads, since `a` is not a project.
///
/// **This one guards; it does not pin.** Reverting the enumeration to its
/// immediate-children form leaves it green: the finding's project comes from
/// each workweave's own marker, so the repair addressed the right file even
/// while the scan that raised it could not read that file. What the revert
/// does red is the assertion above — the finding itself. Measured, because
/// the defect was filed as a repair writing to the wrong index and that half
/// of it does not reproduce.
#[test]
fn doctor_fix_records_no_index_against_a_path_prefix() {
    let tmp = common::tempdir().unwrap();
    let ws = make_deep_project_weave(tmp.path());

    common::rwv()
        .args(["doctor", "--all", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();

    let projects = ws.join("projects");
    for prefix in ["a", "a/b"] {
        assert!(
            !projects.join(prefix).join(".rwv-workweave-index").exists(),
            "`projects/{prefix}/` is not a project, so nothing may record an \
             index there"
        );
    }
    let index = std::fs::read_to_string(
        projects
            .join("a")
            .join("b")
            .join("c")
            .join(".rwv-workweave-index"),
    )
    .expect("the project's own index must be the one that holds the entry");
    assert!(
        index.contains("wtest"),
        "the recorded entry must survive the run: {index}"
    );
}

// ---------------------------------------------------------------------------
// The mint agrees with the enumeration
// ---------------------------------------------------------------------------

/// Enumeration stops at the first manifest, so a project minted below another
/// project would exist on disk and never be listed. The mint refuses it rather
/// than creating the state the walk has already decided not to see.
#[test]
fn init_refuses_a_project_inside_a_project() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    make_project(&ws, "acme");

    let out = common::rwv()
        .args(["init", "acme/web-app"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success(),
        "the mint must refuse: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("acme") && stderr.contains("already a project"),
        "the refusal must name the project that encloses the requested one: {stderr}"
    );
    assert!(
        !ws.join("projects").join("acme").join("web-app").exists(),
        "a refused mint leaves nothing behind"
    );
}

/// The control. A directory that merely holds projects is not one, so a
/// project below it is legal — which is the whole of `chatly/web-app`.
#[test]
fn init_accepts_a_project_under_a_bare_namespace_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = make_weave(tmp.path());
    std::fs::create_dir_all(ws.join("projects").join("acme")).unwrap();

    let out = common::rwv()
        .args(["init", "acme/web-app"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a namespace directory must not block the mint: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(names(&ws), vec!["acme/web-app"]);
}
