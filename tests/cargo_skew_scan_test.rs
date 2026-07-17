//! Version-skew observatory + same-key patch shadowing scanner tests
//! (fo-t9x0l1.1).
//!
//! Two read-only scans in `cargo_workspace.rs` feed `rwv doctor`:
//!
//! 1. **Version skew** — same crate required at differing version-req
//!    strings across workspace members, post `workspace = true`
//!    indirection.
//! 2. **Same-key patch shadowing** — a member's own `.cargo/config.toml`
//!    silently defeats a weave-level `[patch.<reg>].<crate>` per cargo's
//!    closest-config-wins per-key merge (probe P5b).
//!
//! Both scans emit `CheckViolation`s with `Warning` severity: they surface
//! in `rwv doctor` output (and `--json`) but never fail exit-status. Both
//! are report-not-mandate per Finding 3 of
//! `docs/repoweave/grok-build-export-findings.md`.

use repoweave::integration::IntegrationContext;
use repoweave::integrations::cargo_workspace::{
    CargoSkewOccurrence, CargoWorkspace, PatchShadowingRecord,
};
use repoweave::manifest::{IntegrationConfig, Manifest, ProjectName, Role};
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn write_file(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
}

fn make_manifest(repos: Vec<(&str, Role)>) -> Manifest {
    let mut yaml = String::from("repositories:\n");
    for (path, role) in &repos {
        let last = path.split('/').next_back().unwrap();
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: https://github.com/test/{last}.git\n    version: main\n    role: {}\n",
            role.as_str()
        ));
    }
    Manifest::from_yaml_str(&yaml).unwrap()
}

fn make_ctx<'a>(
    root: &'a Path,
    project: &'a ProjectName,
    manifest: &'a Manifest,
    config: &'a IntegrationConfig,
    cache: &'a HashMap<String, Vec<String>>,
) -> IntegrationContext<'a> {
    IntegrationContext {
        output_dir: root,
        workspace_root: root,
        project,
        repos: manifest
            .iter_entries()
            .map(|(rp, e)| (rp.clone(), e.clone()))
            .collect(),
        config,
        all_repos_on_disk: &[],
        all_project_paths: &[],
        detection_cache: cache,
        workweave: None,
    }
}

// ---------------------------------------------------------------------------
// Version-skew: direct requirements
// ---------------------------------------------------------------------------

#[test]
fn version_skew_detects_bare_string_requirement_difference() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = \"1.0\"\n",
    );
    write_file(
        root,
        "github/acme/bar/Cargo.toml",
        "[package]\nname = \"bar\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = \"2.0\"\n",
    );

    let members = vec!["github/acme/foo".to_string(), "github/acme/bar".to_string()];
    let out = CargoWorkspace::scan_version_skew(root, &members);
    assert_eq!(
        out.len(),
        1,
        "expected exactly one skew record; got {out:?}"
    );
    let (crate_name, occurrences) = &out[0];
    assert_eq!(crate_name, "serde");
    let versions: Vec<&str> = occurrences.iter().map(|o| o.requirement.as_str()).collect();
    assert!(versions.contains(&"1.0"));
    assert!(versions.contains(&"2.0"));
}

#[test]
fn version_skew_silent_when_all_agree() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = \"1.0\"\ntokio = \"1.30\"\n",
    );
    write_file(
        root,
        "github/acme/bar/Cargo.toml",
        "[package]\nname = \"bar\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = \"1.0\"\ntokio = \"1.30\"\n",
    );

    let members = vec!["github/acme/foo".to_string(), "github/acme/bar".to_string()];
    let out = CargoWorkspace::scan_version_skew(root, &members);
    assert!(out.is_empty(), "no skew expected; got {out:?}");
}

#[test]
fn version_skew_inline_table_uses_version_field() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = { version = \"1.0\", features = [\"derive\"] }\n",
    );
    write_file(
        root,
        "github/acme/bar/Cargo.toml",
        "[package]\nname = \"bar\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = { version = \"2.0\" }\n",
    );

    let members = vec!["github/acme/foo".to_string(), "github/acme/bar".to_string()];
    let out = CargoWorkspace::scan_version_skew(root, &members);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "serde");
}

#[test]
fn version_skew_ignores_path_and_git_deps() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // A path/git dep is not a registry requirement — skew comparison would be
    // misleading. Even if bar's `foo` dep uses a totally different version
    // string, the scanner must not fire.
    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nsibling = { path = \"../bar\" }\n\
         upstream = { git = \"https://example.com/x.git\" }\n",
    );
    write_file(
        root,
        "github/acme/bar/Cargo.toml",
        "[package]\nname = \"bar\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nsibling = \"9.9\"\nupstream = \"1.0\"\n",
    );

    let members = vec!["github/acme/foo".to_string(), "github/acme/bar".to_string()];
    let out = CargoWorkspace::scan_version_skew(root, &members);
    // Neither `sibling` nor `upstream` should surface: foo's declaration is
    // not a registry requirement.
    assert!(
        out.is_empty(),
        "path/git deps must not produce skew records; got {out:?}"
    );
}

#[test]
fn version_skew_includes_dev_and_build_dependencies() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dev-dependencies]\ntest_crate = \"1.0\"\n",
    );
    write_file(
        root,
        "github/acme/bar/Cargo.toml",
        "[package]\nname = \"bar\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [build-dependencies]\ntest_crate = \"2.0\"\n",
    );

    let members = vec!["github/acme/foo".to_string(), "github/acme/bar".to_string()];
    let out = CargoWorkspace::scan_version_skew(root, &members);
    assert_eq!(out.len(), 1, "expected dev/build-dep skew; got {out:?}");
    assert_eq!(out[0].0, "test_crate");
}

// ---------------------------------------------------------------------------
// Version-skew: `workspace = true` indirection
// ---------------------------------------------------------------------------
//
// This is the day-one grok-build requirement: members declare deps as
// `serde.workspace = true` and the effective requirement lives in the
// repo's own `[workspace.dependencies]`. The scanner must resolve
// through it before comparing.

#[test]
fn version_skew_resolves_workspace_true_via_repo_workspace_deps() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Nested-workspace repo A: root has [workspace.dependencies].serde = "1.0",
    // sub-crate uses workspace = true.
    write_file(
        root,
        "github/acme/big-repo/Cargo.toml",
        "[workspace]\nmembers = [\"crate-a\"]\nresolver = \"2\"\n\n\
         [workspace.dependencies]\nserde = \"1.0\"\n",
    );
    write_file(
        root,
        "github/acme/big-repo/crate-a/Cargo.toml",
        "[package]\nname = \"crate-a\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = { workspace = true, features = [\"derive\"] }\n",
    );

    // Repo B: bare string version.
    write_file(
        root,
        "github/acme/bar/Cargo.toml",
        "[package]\nname = \"bar\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = \"2.0\"\n",
    );

    // Scan against both the sub-crate (grok-build shape) and the sibling.
    let members = vec![
        "github/acme/big-repo/crate-a".to_string(),
        "github/acme/bar".to_string(),
    ];
    let out = CargoWorkspace::scan_version_skew(root, &members);
    assert_eq!(out.len(), 1, "expected serde skew; got {out:?}");
    let (name, occs) = &out[0];
    assert_eq!(name, "serde");
    let versions: Vec<&str> = occs.iter().map(|o| o.requirement.as_str()).collect();
    // crate-a's requirement is post-indirection "1.0"; bar's is "2.0".
    assert!(
        versions.contains(&"1.0"),
        "expected resolved 1.0; got {versions:?}"
    );
    assert!(versions.contains(&"2.0"));
}

#[test]
fn version_skew_workspace_true_on_workspace_root_member() {
    // Case where the member IS the workspace root (grok-build shape:
    // `members.<repo>.include` includes the root, but here we test the
    // simpler case where the member path is the workspace root itself).
    // The root's own `[workspace.dependencies]` is where deps live; there
    // is nothing else to compare against, but the scan should not panic
    // and should silently produce no skew for a single member.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "github/acme/big-repo/Cargo.toml",
        "[workspace]\nmembers = []\nresolver = \"2\"\n\n\
         [workspace.dependencies]\nserde = \"1.0\"\n",
    );

    let members = vec!["github/acme/big-repo".to_string()];
    let out = CargoWorkspace::scan_version_skew(root, &members);
    // Only one member observing "serde" at "1.0" → not skew, silent.
    assert!(out.is_empty());
}

#[test]
fn version_skew_record_is_sorted_and_stable() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Three members with three different requirements — the record must
    // list them sorted by member path.
    write_file(
        root,
        "github/acme/c/Cargo.toml",
        "[package]\nname = \"c\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\ntokio = \"1.2\"\n",
    );
    write_file(
        root,
        "github/acme/a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\ntokio = \"1.0\"\n",
    );
    write_file(
        root,
        "github/acme/b/Cargo.toml",
        "[package]\nname = \"b\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\ntokio = \"1.1\"\n",
    );

    let members = vec![
        "github/acme/c".to_string(),
        "github/acme/a".to_string(),
        "github/acme/b".to_string(),
    ];
    let out = CargoWorkspace::scan_version_skew(root, &members);
    assert_eq!(out.len(), 1);
    let occs = &out[0].1;
    let members_in_order: Vec<&str> = occs.iter().map(|o| o.member.as_str()).collect();
    assert_eq!(
        members_in_order,
        vec!["github/acme/a", "github/acme/b", "github/acme/c"],
        "occurrences must be sorted by member path for stable output"
    );
}

// ---------------------------------------------------------------------------
// Same-key patch shadowing
// ---------------------------------------------------------------------------

#[test]
fn patch_shadowing_detects_member_config_overriding_weave() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Weave-level Cargo.toml with a patch entry.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"github/acme/foo\"]\nresolver = \"2\"\n\n\
         [patch.crates-io]\nserde = { path = \"github/acme/serde-fork\" }\n",
    );

    // Member with its own .cargo/config.toml carrying the same key —
    // cargo's closest-config-wins per key silently voids the weave entry.
    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n",
    );
    write_file(
        root,
        "github/acme/foo/.cargo/config.toml",
        "[patch.crates-io]\nserde = { path = \"../serde-different\" }\n",
    );

    let members = vec!["github/acme/foo".to_string()];
    let records = CargoWorkspace::scan_patch_shadowing(root, &members);
    assert_eq!(
        records.len(),
        1,
        "expected exactly one shadowing; got {records:?}"
    );
    let r = &records[0];
    assert_eq!(r.registry, "crates-io");
    assert_eq!(r.crate_name, "serde");
    // Both files must be named — the finding is useless without them.
    assert!(
        r.weave_config.ends_with("Cargo.toml"),
        "weave_config should name the weave-root Cargo.toml; got {}",
        r.weave_config.display()
    );
    assert!(
        r.member_config
            .ends_with(Path::new("github/acme/foo/.cargo/config.toml")),
        "member_config should name the member's .cargo/config.toml; got {}",
        r.member_config.display()
    );
}

#[test]
fn patch_shadowing_silent_when_keys_do_not_collide() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Weave has a patch for `serde`, member has a patch for `tokio` — no
    // shadowing (cargo merges per-key across configs; disjoint keys stack
    // cleanly).
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"github/acme/foo\"]\nresolver = \"2\"\n\n\
         [patch.crates-io]\nserde = { path = \"github/acme/serde-fork\" }\n",
    );
    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n",
    );
    write_file(
        root,
        "github/acme/foo/.cargo/config.toml",
        "[patch.crates-io]\ntokio = { path = \"../tokio-fork\" }\n",
    );

    let members = vec!["github/acme/foo".to_string()];
    let records = CargoWorkspace::scan_patch_shadowing(root, &members);
    assert!(
        records.is_empty(),
        "disjoint patch keys must not fire; got {records:?}"
    );
}

#[test]
fn patch_shadowing_silent_when_member_has_no_cargo_config() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"github/acme/foo\"]\nresolver = \"2\"\n\n\
         [patch.crates-io]\nserde = { path = \"github/acme/serde-fork\" }\n",
    );
    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n",
    );
    // No member .cargo/config.toml → no shadowing possible.

    let members = vec!["github/acme/foo".to_string()];
    let records = CargoWorkspace::scan_patch_shadowing(root, &members);
    assert!(records.is_empty());
}

#[test]
fn patch_shadowing_silent_when_weave_has_no_patches() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Weave Cargo.toml has no [patch.*]; a member's .cargo/config.toml
    // patches are then not shadowing anything.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"github/acme/foo\"]\nresolver = \"2\"\n",
    );
    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n",
    );
    write_file(
        root,
        "github/acme/foo/.cargo/config.toml",
        "[patch.crates-io]\nserde = { path = \"../serde-fork\" }\n",
    );

    let members = vec!["github/acme/foo".to_string()];
    let records = CargoWorkspace::scan_patch_shadowing(root, &members);
    assert!(records.is_empty());
}

#[test]
fn patch_shadowing_reads_weave_level_cargo_config_too() {
    // Finding 2 forward-looking: when a weave-level `.cargo/config.toml`
    // is introduced (or authored by the operator today), its `[patch.*]`
    // keys are just as vulnerable to shadowing as the manifest's.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(
        root,
        ".cargo/config.toml",
        "[patch.crates-io]\nserde = { path = \"github/acme/serde-fork\" }\n",
    );
    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n",
    );
    write_file(
        root,
        "github/acme/foo/.cargo/config.toml",
        "[patch.crates-io]\nserde = { path = \"../serde-different\" }\n",
    );

    let members = vec!["github/acme/foo".to_string()];
    let records = CargoWorkspace::scan_patch_shadowing(root, &members);
    assert_eq!(records.len(), 1);
    let r = &records[0];
    // weave_config should point at the .cargo/config.toml, not the missing
    // manifest.
    assert!(r.weave_config.ends_with(".cargo/config.toml"));
}

#[test]
fn patch_shadowing_names_ancestor_config_between_member_and_weave() {
    // Cargo's discovery walks upward. A `.cargo/config.toml` sitting in
    // an intermediate ancestor (e.g. `github/acme/.cargo/config.toml`)
    // still shadows the weave root per closest-wins.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"github/acme/foo\"]\nresolver = \"2\"\n\n\
         [patch.crates-io]\nserde = { path = \"github/acme/serde-fork\" }\n",
    );
    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n",
    );
    write_file(
        root,
        "github/acme/.cargo/config.toml",
        "[patch.crates-io]\nserde = { path = \"./other-serde\" }\n",
    );

    let members = vec!["github/acme/foo".to_string()];
    let records = CargoWorkspace::scan_patch_shadowing(root, &members);
    assert_eq!(records.len(), 1);
    assert!(
        records[0]
            .member_config
            .ends_with(Path::new("github/acme/.cargo/config.toml")),
        "got {}",
        records[0].member_config.display()
    );
}

// ---------------------------------------------------------------------------
// Doctor JSON wire shape — round-trip through ViolationOutput
// ---------------------------------------------------------------------------

#[test]
fn version_skew_wire_shape_has_stable_kind_tag_and_fields() {
    use repoweave::check::{CheckViolation, ViolationOutput};
    use repoweave::manifest::WorkweaveName;

    let ws = std::path::PathBuf::from("/ws");
    let ww: HashMap<WorkweaveName, std::path::PathBuf> = HashMap::new();
    let v = CheckViolation::CargoVersionSkew {
        crate_name: "serde".to_string(),
        occurrences: vec![
            CargoSkewOccurrence {
                member: "github/acme/foo".into(),
                requirement: "1.0".into(),
            },
            CargoSkewOccurrence {
                member: "github/acme/bar".into(),
                requirement: "2.0".into(),
            },
        ],
    };
    let json = serde_json::to_value(ViolationOutput::from_violation(v, &ws, &ww)).unwrap();
    assert_eq!(
        json.get("kind").and_then(|k| k.as_str()),
        Some("cargo-version-skew"),
    );
    assert_eq!(
        json.get("crate_name").and_then(|s| s.as_str()),
        Some("serde"),
    );
    let occurrences = json.get("occurrences").and_then(|v| v.as_array()).unwrap();
    assert_eq!(occurrences.len(), 2);
    let member_names: Vec<&str> = occurrences
        .iter()
        .map(|o| o.get("member").and_then(|m| m.as_str()).unwrap())
        .collect();
    assert!(member_names.contains(&"github/acme/foo"));
    assert!(member_names.contains(&"github/acme/bar"));
    let reqs: Vec<&str> = occurrences
        .iter()
        .map(|o| o.get("requirement").and_then(|m| m.as_str()).unwrap())
        .collect();
    assert!(reqs.contains(&"1.0"));
    assert!(reqs.contains(&"2.0"));
}

#[test]
fn patch_shadowing_wire_shape_has_stable_kind_tag_and_fields() {
    use repoweave::check::{CheckViolation, ViolationOutput};
    use repoweave::manifest::WorkweaveName;

    let ws = std::path::PathBuf::from("/ws");
    let ww: HashMap<WorkweaveName, std::path::PathBuf> = HashMap::new();
    let v = CheckViolation::CargoPatchShadowing {
        weave_config: std::path::PathBuf::from("/ws/Cargo.toml"),
        member_config: std::path::PathBuf::from("/ws/github/acme/foo/.cargo/config.toml"),
        registry: "crates-io".into(),
        crate_name: "serde".into(),
    };
    let json = serde_json::to_value(ViolationOutput::from_violation(v, &ws, &ww)).unwrap();
    assert_eq!(
        json.get("kind").and_then(|k| k.as_str()),
        Some("cargo-patch-shadowing"),
    );
    assert_eq!(
        json.get("registry").and_then(|s| s.as_str()),
        Some("crates-io"),
    );
    assert_eq!(
        json.get("crate_name").and_then(|s| s.as_str()),
        Some("serde"),
    );
    assert_eq!(
        json.get("weave_config").and_then(|s| s.as_str()),
        Some("/ws/Cargo.toml"),
    );
    assert_eq!(
        json.get("member_config").and_then(|s| s.as_str()),
        Some("/ws/github/acme/foo/.cargo/config.toml"),
    );
}

// ---------------------------------------------------------------------------
// End-to-end via scan_cargo_ecosystem: exercises the IntegrationContext
// path used by both `run_check` and `collect_doctor_violations`.
// ---------------------------------------------------------------------------

#[test]
fn scan_cargo_ecosystem_produces_both_skew_and_shadowing_violations() {
    use repoweave::check::{scan_cargo_ecosystem, CheckViolation};

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // A weave with two Rust members, a skewed dep, and a shadowing
    // .cargo/config.toml at one of them.
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"github/acme/foo\", \"github/acme/bar\"]\nresolver = \"2\"\n\n\
         [patch.crates-io]\nserde = { path = \"github/acme/serde-fork\" }\n",
    );
    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = \"1.0\"\n",
    );
    write_file(
        root,
        "github/acme/bar/Cargo.toml",
        "[package]\nname = \"bar\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = \"2.0\"\n",
    );
    write_file(
        root,
        "github/acme/foo/.cargo/config.toml",
        "[patch.crates-io]\nserde = { path = \"../serde-different\" }\n",
    );

    let manifest = make_manifest(vec![
        ("github/acme/foo", Role::Owned),
        ("github/acme/bar", Role::Owned),
    ]);
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    cache.insert(
        "Cargo.toml".to_string(),
        vec!["github/acme/bar".to_string(), "github/acme/foo".to_string()],
    );
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let violations = scan_cargo_ecosystem(&ctx).unwrap();
    let mut saw_skew = false;
    let mut saw_shadow = false;
    for v in &violations {
        match v {
            CheckViolation::CargoVersionSkew { crate_name, .. } if crate_name == "serde" => {
                saw_skew = true;
            }
            CheckViolation::CargoPatchShadowing {
                crate_name,
                registry,
                ..
            } if crate_name == "serde" && registry == "crates-io" => {
                saw_shadow = true;
            }
            _ => {}
        }
    }
    assert!(
        saw_skew,
        "expected version-skew violation; got {violations:?}"
    );
    assert!(
        saw_shadow,
        "expected patch-shadowing violation; got {violations:?}"
    );
}

#[test]
fn scan_cargo_ecosystem_silent_when_no_cargo_members() {
    use repoweave::check::scan_cargo_ecosystem;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // No Cargo.toml files anywhere — scan must silently produce no findings
    // rather than erroring.
    let manifest = make_manifest(vec![]);
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let cache: HashMap<String, Vec<String>> = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let violations = scan_cargo_ecosystem(&ctx).unwrap();
    assert!(violations.is_empty());
}

// ---------------------------------------------------------------------------
// Adversarial: does the scan handle the grok-build shape (nested workspace
// repo as a scan target) without hard-erroring the way activation does?
// ---------------------------------------------------------------------------

#[test]
fn scan_members_includes_nested_workspace_repos_grok_build_shape() {
    // The activation-time `partition` hard-errors on a repo that declares
    // its own [workspace]. The scan-time `scan_members` must not — the
    // scanner still wants to read the repo's [workspace.dependencies]. The
    // grok-build fixture is exactly this shape (85 crates under one
    // nested workspace).
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Nested-workspace repo — activation would bail on this.
    write_file(
        root,
        "github/xai/grok-build/Cargo.toml",
        "[workspace]\nmembers = [\"crate-1\"]\nresolver = \"2\"\n\n\
         [workspace.dependencies]\nserde = \"1.0\"\n",
    );
    write_file(
        root,
        "github/xai/grok-build/crate-1/Cargo.toml",
        "[package]\nname = \"crate-1\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = { workspace = true }\n",
    );
    // Sibling repo without a workspace.
    write_file(
        root,
        "github/acme/other/Cargo.toml",
        "[package]\nname = \"other\"\nversion = \"0.1\"\nedition = \"2021\"\n\
         [dependencies]\nserde = \"2.0\"\n",
    );

    let manifest = make_manifest(vec![
        ("github/xai/grok-build", Role::Owned),
        ("github/acme/other", Role::Owned),
    ]);
    let project = ProjectName::new("test-project");
    let config = IntegrationConfig::default();
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    cache.insert(
        "Cargo.toml".to_string(),
        vec![
            "github/acme/other".to_string(),
            "github/xai/grok-build".to_string(),
        ],
    );
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    let cfg = repoweave::manifest::CargoWorkspaceConfig::default();
    let members = CargoWorkspace::scan_members(&ctx, &cfg).unwrap();
    // The nested-workspace repo must be present in the scan list. Sub-crates
    // are NOT emitted unless `members.<repo>.include` is configured — the
    // scanner emits the repo root and the version resolver walks up from
    // there.
    assert!(members.contains(&"github/xai/grok-build".to_string()));
    assert!(members.contains(&"github/acme/other".to_string()));

    // Version skew should be detected: grok-build's workspace-deps serde=1.0
    // vs other's direct 2.0.
    let out = CargoWorkspace::scan_version_skew(root, &members);
    // Since only the grok-build root is emitted (no sub-crates), the direct
    // `serde` entry in workspace-deps table isn't reached by the "[dependencies]"
    // scan. This is the intentional v1 scope: version-skew observation
    // requires either configuring `members.<repo>` to emit sub-crates or the
    // repo to have direct [dependencies]. Assert the skew is silent here so
    // the behavior is documented and any future change is caught.
    assert!(
        out.iter().all(|(name, _)| name != "serde"),
        "grok-build root without members.<repo> config surfaces no direct \
         serde skew (its [workspace.dependencies] is read only when sub-crate \
         members are emitted); got {out:?}"
    );

    // But if the operator DOES configure members.<repo> to include the
    // sub-crate, skew is visible.
    let members_with_sub = vec![
        "github/xai/grok-build/crate-1".to_string(),
        "github/acme/other".to_string(),
    ];
    let out_with_sub = CargoWorkspace::scan_version_skew(root, &members_with_sub);
    assert_eq!(out_with_sub.len(), 1);
    assert_eq!(out_with_sub[0].0, "serde");
    // crate-1's serde requirement resolves through
    // github/xai/grok-build/Cargo.toml [workspace.dependencies] to "1.0".
    let versions: Vec<&str> = out_with_sub[0]
        .1
        .iter()
        .map(|o| o.requirement.as_str())
        .collect();
    assert!(versions.contains(&"1.0"));
    assert!(versions.contains(&"2.0"));
}

// ---------------------------------------------------------------------------
// Adversarial: dedupe and record shape
// ---------------------------------------------------------------------------

#[test]
fn patch_shadowing_dedupes_repeated_findings_across_members() {
    // If two members share the same `.cargo/config.toml` (via an ancestor
    // dir like github/acme/.cargo/config.toml), the finding must be
    // reported once — one file has one shadowing key.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"github/acme/foo\", \"github/acme/bar\"]\nresolver = \"2\"\n\n\
         [patch.crates-io]\nserde = { path = \"vendor/serde\" }\n",
    );
    write_file(
        root,
        "github/acme/foo/Cargo.toml",
        "[package]\nname = \"foo\"\nversion = \"0.1\"\nedition = \"2021\"\n",
    );
    write_file(
        root,
        "github/acme/bar/Cargo.toml",
        "[package]\nname = \"bar\"\nversion = \"0.1\"\nedition = \"2021\"\n",
    );
    // Shared ancestor config that shadows both members.
    write_file(
        root,
        "github/acme/.cargo/config.toml",
        "[patch.crates-io]\nserde = { path = \"./shared-serde\" }\n",
    );

    let members = vec!["github/acme/foo".to_string(), "github/acme/bar".to_string()];
    let records: Vec<PatchShadowingRecord> = CargoWorkspace::scan_patch_shadowing(root, &members);
    assert_eq!(
        records.len(),
        1,
        "one config, one key → one record; got {records:?}"
    );
}
