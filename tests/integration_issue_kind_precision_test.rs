//! An integration condition with a published `IssueKind` must reach
//! `rwv doctor --json` under that kind, never under `integration-failed`.
//!
//! `for_each_enabled` captures a hook's `Err` into
//! `Issue{kind: IntegrationFailed}`, which is the only kind the runner can
//! mint. So a condition reported by returning `Err` from a hook loses whatever
//! kind it would have carried, and `docs/reference/doctor-findings.md` hands the
//! operator the vaguest row in its table instead of the one naming the remedy.
//! The conditions below each have a precise kind and each are reported by
//! returning an `Issue`, not by bailing — moving one onto the bail path is the
//! regression these red on.
//!
//! Asserted through the serialized `--json` payload rather than the `IssueKind`
//! value, because the wire token is what an operator looks up and what a
//! consumer routes on. The text renderer prints no kind at all — that is the
//! published behaviour, stated on the findings page under "Finding your entry"
//! — so `--json` is the whole of the kind surface.

use repoweave::check::build_doctor_json;
use repoweave::integration::Integration;
use repoweave::integration_runner::{build_detection_cache, run_checks, IntegrationContextBase};
use repoweave::integrations::{CargoWorkspace, StaticFiles};
use repoweave::manifest::{Manifest, ProjectName};
use repoweave::workspace::ContainerKind;
use std::collections::HashMap;
use std::path::Path;

mod common;

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// The `kind` token of every entry on `--json`'s `issues` array, in order.
///
/// A kind carrying fields serializes as a single-key object; both shapes are
/// read here so a variant that grows fields does not silently drop out.
fn issue_kinds(issues: Vec<repoweave::integration::Issue>) -> Vec<String> {
    let payload = serde_json::to_value(build_doctor_json(
        Vec::new(),
        issues,
        Path::new("/ws"),
        &HashMap::new(),
        None,
        Vec::new(),
        Vec::new(),
    ))
    .expect("doctor payload serializes");
    payload["issues"]
        .as_array()
        .expect("issues is an array")
        .iter()
        .map(|issue| match &issue["kind"] {
            serde_json::Value::String(tag) => tag.clone(),
            serde_json::Value::Object(map) => {
                map.keys().next().expect("tagged kind has a key").clone()
            }
            other => panic!("unrenderable kind: {other}"),
        })
        .collect()
}

fn check_kinds(
    integration: &dyn Integration,
    root: &Path,
    manifest: &Manifest,
    workweave: Option<&repoweave::manifest::WorkweaveConfig>,
) -> Vec<String> {
    let integrations: Vec<&dyn Integration> = vec![integration];
    let project = ProjectName::new("test-project").unwrap();
    let detection_cache = build_detection_cache(&integrations, root, manifest.iter_entries());
    let ctx_base = IntegrationContextBase {
        output_dir: root.to_path_buf(),
        workspace_root: root,
        container_kind: ContainerKind::Primary,
        project: &project,
        all_repos_on_disk: &[],
        all_project_paths: &[],
        detection_cache: &detection_cache,
        workweave,
    };
    issue_kinds(run_checks(&integrations, manifest, &ctx_base))
}

#[test]
fn nested_workspace_reaches_json_as_config_rejected() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write(
        root,
        "github/acme/plain/Cargo.toml",
        "[package]\nname = \"plain\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        root,
        "github/acme/forked/Cargo.toml",
        "[package]\nname = \"forked\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [workspace]\nmembers = [\"crates/*\"]\n",
    );

    let manifest = Manifest::from_toml_str(
        "[repositories.\"github/acme/plain\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/plain.git\"\nversion = \"main\"\nrole = \"owned\"\n\
         [repositories.\"github/acme/forked\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/forked.git\"\nversion = \"main\"\nrole = \"fork\"\n",
    )
    .unwrap();

    let kinds = check_kinds(&CargoWorkspace, root, &manifest, None);

    assert!(
        kinds.contains(&"config-rejected".to_string()),
        "the nested [workspace] must reach --json as config-rejected, got: {kinds:?}"
    );
    assert!(
        !kinds.contains(&"integration-failed".to_string()),
        "the nested [workspace] was flattened into integration-failed: {kinds:?}"
    );
}

#[test]
fn static_files_name_collision_reaches_json_as_config_rejected() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write(root, "NOTES.md", "notes\n");

    let mut manifest = Manifest::from_toml_str(
        "[repositories.\"github/acme/plain\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/plain.git\"\nversion = \"main\"\nrole = \"owned\"\n\
         [integrations.static-files]\nenabled = true\nfiles = [\"NOTES.md\"]\n\
         [workweave]\nlink = [\"NOTES.md\"]\n",
    )
    .unwrap();
    let workweave = manifest.workweave.take().expect("workweave section parses");

    let kinds = check_kinds(&StaticFiles, root, &manifest, Some(&workweave));

    assert!(
        kinds.contains(&"config-rejected".to_string()),
        "the twice-claimed name must reach --json as config-rejected, got: {kinds:?}"
    );
    assert!(
        !kinds.contains(&"integration-failed".to_string()),
        "the twice-claimed name was flattened into integration-failed: {kinds:?}"
    );
}

/// The runner's capture really does mint `integration-failed`, so the two
/// assertions above are refusing something reachable rather than something the
/// payload cannot express.
#[test]
fn a_hook_error_does_reach_json_as_integration_failed() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write(
        root,
        "github/acme/plain/Cargo.toml",
        "[package]\nname = \"p\"\n",
    );

    let manifest = Manifest::from_toml_str(
        "[repositories.\"github/acme/plain\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/plain.git\"\nversion = \"main\"\nrole = \"owned\"\n\
         [integrations.cargo-workspace]\nexclude = \"not-a-list\"\n",
    )
    .unwrap();

    let kinds = check_kinds(&CargoWorkspace, root, &manifest, None);

    assert_eq!(
        kinds,
        vec!["integration-failed".to_string()],
        "a check() that returns Err is the runner's only kind"
    );
}
