//! An integration condition with a published `IssueKind` must reach
//! `rwv doctor --json` under that kind, never under `integration-failed`.
//!
//! `for_each_enabled` captures a hook's `Err` into
//! `Issue{kind: IntegrationFailed}`, which is the only kind the runner can
//! mint. So a condition reported by returning `Err` from a hook loses whatever
//! kind it would have carried, and `docs/reference/doctor-findings.md` routes
//! the operator to the vaguest entry it publishes instead of the one naming the
//! remedy. The conditions below each have a precise kind and each are reported
//! by returning an `Issue`, not by bailing — moving one onto the bail path is
//! the regression these red on.
//!
//! Every hook a condition can reach is driven, because a condition can be
//! routed correctly out of one and still bail out of another: `check()`,
//! `verify()` and `activate()` are separate `for_each_enabled` passes, and
//! each captures its own errors.
//!
//! Asserted through the serialized `--json` payload rather than the `IssueKind`
//! value, because the wire token is what an operator looks up and what a
//! consumer routes on. The text renderer prints no kind at all — that is the
//! published behaviour, stated on the findings page under "Finding your entry"
//! — so `--json` is the whole of the kind surface.

use repoweave::check::build_doctor_json;
use repoweave::integration::Integration;
use repoweave::integration_runner::{
    build_detection_cache, run_activations, run_checks, run_verifications, IntegrationContextBase,
};
use repoweave::integrations::{CargoWorkspace, StaticFiles, VscodeWorkspace};
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

/// Which of the three `Vec<Issue>` hooks to drive. They are separate
/// `for_each_enabled` passes, so routing a condition correctly out of one says
/// nothing about the others.
enum Hook {
    Check,
    Verify,
    Activate,
}

fn hook_kinds(
    hook: Hook,
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
    issue_kinds(match hook {
        Hook::Check => run_checks(&integrations, manifest, &ctx_base),
        Hook::Verify => run_verifications(&integrations, manifest, &ctx_base),
        Hook::Activate => run_activations(&integrations, manifest, &ctx_base),
    })
}

fn check_kinds(
    integration: &dyn Integration,
    root: &Path,
    manifest: &Manifest,
    workweave: Option<&repoweave::manifest::WorkweaveConfig>,
) -> Vec<String> {
    hook_kinds(Hook::Check, integration, root, manifest, workweave)
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

/// A settings block that does not deserialize is its own condition, not the
/// runner's capture of a hook that gave up.
///
/// The value never parsed, so no predicate ran and nothing was asked of the
/// workspace — which is what separates it from `config-rejected`, where rwv
/// understood the request and could not meet it. `integration-failed` names
/// neither the field nor a remedy; this kind's message carries the
/// deserializer's own, which names both.
#[test]
fn malformed_settings_reach_json_under_their_own_kind() {
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

    for hook in [Hook::Check, Hook::Verify, Hook::Activate] {
        let kinds = hook_kinds(hook, &CargoWorkspace, root, &manifest, None);
        assert_eq!(
            kinds,
            vec!["malformed-settings".to_string()],
            "a settings block that does not deserialize must reach --json under its own kind"
        );
    }
}

/// The doctor path (`check()`/`verify()`) and the activate path must report
/// the *same* kind for the *same* malformed block — not merely a kind each,
/// independently chosen. Before `activate()` had an `Issue` channel this could
/// not even be stated: its bail reached the operator as `integration-failed`
/// while `check()`/`verify()` already reported `malformed-settings` for the
/// identical `rwv.toml`, so which kind you got depended on which verb you ran.
#[test]
fn malformed_settings_reach_the_same_kind_from_activate_as_from_doctor() {
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

    let doctor_check = hook_kinds(Hook::Check, &CargoWorkspace, root, &manifest, None);
    let doctor_verify = hook_kinds(Hook::Verify, &CargoWorkspace, root, &manifest, None);
    let activate = hook_kinds(Hook::Activate, &CargoWorkspace, root, &manifest, None);

    assert_eq!(
        activate,
        vec!["malformed-settings".to_string()],
        "rwv activate must report the same kind rwv doctor does for identical malformed \
         settings, got: {activate:?}"
    );
    assert_eq!(
        doctor_check, activate,
        "check() and activate() disagree on the kind for the same malformed input"
    );
    assert_eq!(
        doctor_verify, activate,
        "verify() and activate() disagree on the kind for the same malformed input"
    );
}

/// The same condition on the two other integrations that read settings inside
/// a `Vec<Issue>` hook, across every hook half that reads them — including
/// `activate()`, so a settings-parsing integration cannot drift back to
/// bailing on just one verb.
///
/// Named rather than derived: an integration that starts reading settings in a
/// hook is a site this file has to be told about, and there is no handle that
/// enumerates them.
#[test]
fn malformed_settings_carry_their_kind_out_of_every_hook_that_reads_them() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();
    write(root, "NOTES.md", "notes\n");

    let static_files = Manifest::from_toml_str(
        "[repositories.\"github/acme/plain\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/plain.git\"\nversion = \"main\"\nrole = \"owned\"\n\
         [integrations.static-files]\nenabled = true\nfiles = \"NOTES.md\"\n",
    )
    .unwrap();
    assert_eq!(
        check_kinds(&StaticFiles, root, &static_files, None),
        vec!["malformed-settings".to_string()],
        "static-files reads its settings in check()"
    );
    assert_eq!(
        hook_kinds(Hook::Activate, &StaticFiles, root, &static_files, None),
        vec!["malformed-settings".to_string()],
        "static-files reads the same settings in activate()"
    );

    let vscode = Manifest::from_toml_str(
        "[repositories.\"github/acme/plain\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/plain.git\"\nversion = \"main\"\nrole = \"owned\"\n\
         [integrations.vscode-workspace]\nhide-dotfiles = \"yes\"\n",
    )
    .unwrap();
    assert_eq!(
        hook_kinds(Hook::Verify, &VscodeWorkspace, root, &vscode, None),
        vec!["malformed-settings".to_string()],
        "vscode-workspace reads its settings in verify(), ahead of the MISSING arm — a \
         missing file must not mask the reason it cannot be regenerated"
    );
    assert_eq!(
        hook_kinds(Hook::Activate, &VscodeWorkspace, root, &vscode, None),
        vec!["malformed-settings".to_string()],
        "vscode-workspace reads the same settings in activate()"
    );
}

/// The runner's capture really does mint `integration-failed`, so the
/// assertions above are refusing something reachable rather than something the
/// payload cannot express.
///
/// The seeded failure is an unreadable member manifest — a directory where
/// `partition` expects a file — because that is an error with no kind of its
/// own to lose, which is the population the capture is for. Every condition
/// this file names a kind for is by construction unavailable here.
#[test]
fn a_hook_error_does_reach_json_as_integration_failed() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    std::fs::create_dir_all(root.join("github/acme/plain/Cargo.toml")).unwrap();

    let manifest = Manifest::from_toml_str(
        "[repositories.\"github/acme/plain\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/plain.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();

    let kinds = check_kinds(&CargoWorkspace, root, &manifest, None);

    assert_eq!(
        kinds,
        vec!["integration-failed".to_string()],
        "a check() that returns Err is the runner's only kind"
    );
}
