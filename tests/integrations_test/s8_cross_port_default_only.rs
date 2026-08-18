// ===========================================================================
// Cross-port DefaultOnly regression battery
//
// For each port that adopts Ownership::DefaultOnly, two tests:
//   (a) s8_<port>_default_only_preserves_user_value — an existing value set by
//       the user (present in the file before activate) is not overwritten.
//   (b) s8_<port>_default_only_seeds_on_greenfield — a fresh / no-file case
//       gets a sensible non-literal default seeded.
//
// These tests use the same fixture-setup style (TempDir, write_file, make_ctx,
// Integration.activate()) and the same assertion shape across all ports, making
// the cross-port contract visible at a glance.  Each test also notes the
// port-specific equivalent added by the per-port spec, so reviewers can see
// that no coverage is duplicated — only the s8_ naming convention is new.
//
// Contract being tested (`Ownership::DefaultOnly` in src/integrations/merge.rs):
//   - merge_activate sets the key only when absent; never overwrites.
//   - strip_deactivate does NOT remove DefaultOnly keys.
//   - DefaultOnly drift is CLEAN in verify().
//   - DefaultOnly keys never appear in MergeResult::authored.
// ===========================================================================

use super::*;

// -----------------------------------------------------------------------
// npm — `name` and `private`
//
// Port-specific equivalents:
//   (a) → regression_name_and_scripts_survive_activate
//         default_only_private_false_survives_activate
//   (b) → greenfield_name_set_from_context_project_name
//
// The s8 versions follow the uniform cross-port shape: a single DefaultOnly
// key per test, minimal fixture, same assertion wording across ports.
// -----------------------------------------------------------------------

/// (a) npm — user-set `name` and `private: false` survive re-activate.
///
/// The file already has the x-repoweave marker (indicating rwv previously
/// authored the file).  DefaultOnly must NOT overwrite the existing values.
#[test]
fn s8_npm_default_only_preserves_user_value() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/api/package.json");

    // Pre-existing file: marker present, user-chosen name + private: false.
    write_file(
        root,
        "package.json",
        r#"{
  "x-repoweave": {"managed": true},
  "name": "acme-monorepo",
  "private": false,
  "workspaces": ["github/acme/api"]
}"#,
    );

    let manifest = make_manifest(vec![("github/acme/api", Role::Owned)]);
    let project = ProjectName::new("different-project-name").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    NpmWorkspaces.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    // DefaultOnly: existing values must survive — no overwrite.
    assert_eq!(
        parsed["name"], "acme-monorepo",
        "name (DefaultOnly) must not be overwritten on re-activate"
    );
    assert_eq!(
        parsed["private"], false,
        "private: false (DefaultOnly) must not be overwritten on re-activate"
    );
}

/// (b) npm — greenfield seeds `name` from project name and `private: true`.
///
/// No pre-existing package.json.  DefaultOnly seeds sensible defaults.
#[test]
fn s8_npm_default_only_seeds_on_greenfield() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/api/package.json");

    // No root package.json — greenfield.
    assert!(!root.join("package.json").exists());

    let manifest = make_manifest(vec![("github/acme/api", Role::Owned)]);
    let project = ProjectName::new("my-workspace").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    NpmWorkspaces.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("package.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    // DefaultOnly seeds from project name (not a hardcoded literal).
    assert_eq!(
        parsed["name"], "my-workspace",
        "greenfield name must be seeded from ctx.project (DefaultOnly)"
    );
    // DefaultOnly seeds private: true as the sensible default.
    assert_eq!(
        parsed["private"], true,
        "greenfield private must be seeded as true (DefaultOnly)"
    );
}

// -----------------------------------------------------------------------
// uv — `[tool.uv].package`
//
// Port-specific equivalents:
//   (a) → default_only_does_not_overwrite_user_set_package_true
//   (b) → default_only_sets_package_false_on_greenfield
// -----------------------------------------------------------------------

/// (a) uv — user-set `[tool.uv].package = true` survives re-activate.
///
/// DefaultOnly must not inject `package = false` when `package` already exists.
#[test]
fn s8_uv_default_only_preserves_user_value() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/astral/protocol/pyproject.toml");

    // Pre-existing file: marker on members, user-set package = true.
    write_file(
        root,
        "pyproject.toml",
        concat!(
            "[tool.uv.workspace]\n",
            "# managed by rwv\n",
            "members = [\"github/astral/protocol\"]\n",
            "\n",
            "[tool.uv]\n",
            "package = true\n",
        ),
    );

    let manifest = make_manifest(vec![("github/astral/protocol", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    UvWorkspace.activate(&ctx).unwrap();

    let after = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();

    // DefaultOnly: user-set value must survive.
    assert!(
        after.contains("package = true"),
        "user-set package=true must survive activate (DefaultOnly never overwrites); got:\n{after}"
    );
    assert!(
        !after.contains("package = false"),
        "DefaultOnly must not inject package=false when key is present; got:\n{after}"
    );
}

/// (b) uv — greenfield seeds `[tool.uv].package = false`.
///
/// No pre-existing pyproject.toml.  DefaultOnly seeds `package = false`
/// so `uv sync` accepts a non-package root.
#[test]
fn s8_uv_default_only_seeds_on_greenfield() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/astral/protocol/pyproject.toml");

    // No root pyproject.toml — greenfield.
    assert!(!root.join("pyproject.toml").exists());

    let manifest = make_manifest(vec![("github/astral/protocol", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    UvWorkspace.activate(&ctx).unwrap();

    let after = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();

    // DefaultOnly seeds package = false on a fresh file.
    assert!(
        after.contains("package = false") || after.contains("package=false"),
        "greenfield pyproject.toml must get package=false from DefaultOnly; got:\n{after}"
    );
}

// -----------------------------------------------------------------------
// cargo — `[workspace].resolver`
//
// Port-specific equivalents:
//   (a) → resolver_default_only_operator_override_preserved
//         (in s7_cargo_doctor mod)
//   (b) → resolver_default_only_greenfield_sets_resolver_2
//         (in s7_cargo_doctor mod)
// -----------------------------------------------------------------------

/// (a) cargo — user-set `resolver = "1"` survives re-activate.
///
/// DefaultOnly must not overwrite an existing resolver value,
/// even when the rwv marker is present on members.
#[test]
fn s8_cargo_default_only_preserves_user_value() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/Cargo.toml");

    // Pre-existing Cargo.toml: marker on members, user-set resolver = "1".
    write_file(
        root,
        "Cargo.toml",
        "[workspace]\n# managed by rwv\nmembers = [\"github/acme/server\"]\n\
             # managed by rwv\nresolver = \"1\"\n",
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    CargoWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();

    // DefaultOnly: operator's resolver = "1" must survive.
    assert!(
        content.contains("resolver = \"1\""),
        "resolver = \"1\" (user-set DefaultOnly) must survive activate; got:\n{content}"
    );
    assert!(
        !content.contains("resolver = \"2\""),
        "DefaultOnly must not overwrite resolver to \"2\"; got:\n{content}"
    );
}

/// (b) cargo — greenfield seeds `resolver = "2"`.
///
/// No pre-existing Cargo.toml.  DefaultOnly seeds resolver = "2".
#[test]
fn s8_cargo_default_only_seeds_on_greenfield() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    touch(root, "github/acme/server/Cargo.toml");

    // No root Cargo.toml — greenfield.
    assert!(!root.join("Cargo.toml").exists());

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    CargoWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();

    // DefaultOnly seeds resolver = "2" on a fresh file.
    assert!(
        content.contains("resolver = \"2\""),
        "greenfield Cargo.toml must get resolver = \"2\" from DefaultOnly; got:\n{content}"
    );
}

// -----------------------------------------------------------------------
// go.work — `go <version>`
//
// Port-specific equivalents: live in src/integrations/go_work.rs (unit
// tests internal to the port module).  These s8 tests exercise the same
// contract from the integration-test layer using GoWork.activate() directly,
// mirroring the cross-port shape.
//
//   (a) → go_work.rs::regression_no_downgrade_defaultonly_preserves_existing_go_line
//   (b) → go_work.rs::greenfield_go_line_written_from_max_go_version
//
// Note: because FORCE_GOWORK_FALLBACK is a thread-local private to the
// go_work module, these tests go through the public activate() entrypoint.
// If `go` is on PATH the primary path is used; otherwise the hand-parse
// fallback.  Both paths honour the DefaultOnly contract.
// -----------------------------------------------------------------------

/// (a) go.work — existing `go 1.20` line survives re-activate.
///
/// The member go.mod also declares `go 1.20`, so `max_go_version` computes
/// 1.20 whether `go` is on PATH (primary path: `go work edit -go=1.20`) or
/// not (fallback path: DefaultOnly preserves the existing 1.20).  In both
/// cases the go-line in the output must still be `go 1.20`.
///
/// 1.20 (not this file's usual 1.26): this test goes through activate()
/// with `go` on PATH, and 1.21 is the oldest go release with GOTOOLCHAIN
/// switching, so a fixture at or below that never makes `go work` reach
/// the network for a toolchain download.
///
/// Note: the deeper DefaultOnly contract (preserving user-set version even
/// when it differs from max_go_version) is fully tested in the fallback-
/// path unit tests inside go_work.rs
/// (`regression_no_downgrade_defaultonly_preserves_existing_go_line`), which
/// force the fallback via a thread-local.  The s8 test here validates the
/// cross-port shape through the public Integration::activate() entrypoint.
#[test]
fn s8_go_work_default_only_preserves_user_value() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Member go.mod declares go 1.20 — same version as the go.work.
    // max_go_version will compute 1.20, so both primary and fallback paths
    // produce "go 1.20" and neither downgrades it.
    write_file(
        root,
        "github/acme/server/go.mod",
        "module github.com/acme/server\n\ngo 1.20\n",
    );

    // Pre-existing go.work with go 1.20.
    write_file(
        root,
        "go.work",
        "go 1.20\n\n// managed by repoweave\nuse (\n\t./github/acme/server\n)\n",
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    GoWork.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("go.work")).unwrap();

    // go 1.20 must be present after activate (not downgraded, not removed).
    assert!(
        content.contains("go 1.20"),
        "go 1.20 must survive activate; got:\n{content}"
    );
    // Confirm the marker is still present (Author key managed correctly).
    assert!(
        content.contains("// managed by repoweave"),
        "ownership marker must be present after activate; got:\n{content}"
    );
}

/// (b) go.work — greenfield seeds `go <version>` from member go.mod files.
///
/// No pre-existing go.work.  DefaultOnly seeds the go-line from
/// `max_go_version` across member go.mod files (a sensible non-literal default).
///
/// 1.20 (not this file's usual 1.26): this test goes through activate()
/// with `go` on PATH, and 1.21 is the oldest go release with GOTOOLCHAIN
/// switching, so a fixture at or below that never makes `go work` reach
/// the network for a toolchain download.
#[test]
fn s8_go_work_default_only_seeds_on_greenfield() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Member go.mod declares go 1.20.
    write_file(
        root,
        "github/acme/server/go.mod",
        "module github.com/acme/server\n\ngo 1.20\n",
    );

    // No go.work — greenfield.
    assert!(!root.join("go.work").exists());

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    GoWork.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("go.work")).unwrap();

    // DefaultOnly seeds go version from max_go_version (not a hardcoded
    // literal): restore_go_directive puts the value rwv computed back after
    // `go work use`, so the seed is exactly what the go.mod reported, on
    // both the tool and hand-edit paths.
    assert!(
        content.contains("go 1.20"),
        "greenfield go.work must seed go 1.20 from max_go_version; got:\n{content}"
    );
}

// -----------------------------------------------------------------------
// vscode — `git.autoRepositoryDetection`
//
// Port-specific equivalents:
//   (a) → git_settings_user_values_preserved_on_reactivate
//   (b) → git_settings_seeded_on_fresh_workspace
//         (both in vscode_workspace mod of this file)
// -----------------------------------------------------------------------

/// (a) vscode — user-customized `git.autoRepositoryDetection` survives re-activate.
///
/// The user has set `git.autoRepositoryDetection` to "always" (not the rwv
/// default "subFolders").  DefaultOnly must not overwrite it.
#[test]
fn s8_vscode_default_only_preserves_user_value() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Pre-existing workspace: marker present, user-set git.* values.
    write_file(
        root,
        "test-project.code-workspace",
        r#"{
  "rwv.generated": { "managed": true, "files.exclude": [] },
  "folders": [{ "path": ".", "name": "test-project (primary)" }],
  "settings": {
    "git.autoRepositoryDetection": "always",
    "git.repositoryScanMaxDepth": 10,
    "files.exclude": { ".*": true }
  }
}"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    // DefaultOnly: user-set values must survive.
    assert_eq!(
        parsed["settings"]["git.autoRepositoryDetection"], "always",
        "user-set git.autoRepositoryDetection (DefaultOnly) must not be overwritten"
    );
    assert_eq!(
        parsed["settings"]["git.repositoryScanMaxDepth"], 10,
        "user-set git.repositoryScanMaxDepth (DefaultOnly) must not be overwritten"
    );
}

/// (b) vscode — greenfield seeds `git.autoRepositoryDetection = "subFolders"`.
///
/// No pre-existing .code-workspace.  DefaultOnly seeds the git.* settings
/// to their sensible defaults.
#[test]
fn s8_vscode_default_only_seeds_on_greenfield() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // No .code-workspace — greenfield.
    assert!(!root.join("test-project.code-workspace").exists());

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("test-project.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    // DefaultOnly seeds the expected defaults on a fresh workspace.
    assert_eq!(
        parsed["settings"]["git.autoRepositoryDetection"], "subFolders",
        "greenfield workspace must get git.autoRepositoryDetection = \"subFolders\" from DefaultOnly"
    );
    assert_eq!(
        parsed["settings"]["git.repositoryScanMaxDepth"], 3,
        "greenfield workspace must get git.repositoryScanMaxDepth = 3 from DefaultOnly"
    );
}
