// ===========================================================================
// vscode-workspace: residual-bug scenarios
// ===========================================================================
//
// Each scenario pins one promise of the activate/deactivate merge over a
// `.code-workspace`: what the user put there survives both, and rwv's own
// region — the `folders` entry whose path is ".", the marker, and the
// `files.exclude` keys the marker records — is the only thing either touches.

use super::*;

// -------------------------------------------------------------------------
// Scenario 1 — User adds a personal `files.exclude` entry; sync must not
// eat it.
// -------------------------------------------------------------------------
#[test]
fn scenario1_user_files_exclude_survives_reactivation() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Seed: an rwv-generated workspace file (marker present, primary folder,
    // git.* settings, rwv-derived exclude keys) PLUS two user-added exclude
    // entries that rwv should never touch.
    write_file(
        root,
        "foundations.code-workspace",
        r#"{
  "rwv.generated": true,
  "folders": [{"path": ".", "name": "foundations (primary)"}],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {
      ".*": true,
      "github/acme": true,
      "**/target": true,
      "dist": true
    }
  }
}"#,
    );

    // Activate again with a new repo on disk (github/chatly/api joins).
    // github/acme is still excluded (not in manifest).
    let manifest = make_manifest(vec![
        ("github/cwalv/repoweave", Role::Owned),
        ("github/chatly/api", Role::Owned),
    ]);
    let project = ProjectName::new("foundations").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();

    let all_repos_on_disk: Vec<RepoPath> = vec![
        RepoPath::new("github/cwalv/repoweave").expect("known-safe literal"),
        RepoPath::new("github/chatly/api").expect("known-safe literal"),
        RepoPath::new("github/acme/server").expect("known-safe literal"),
        RepoPath::new("github/acme/web").expect("known-safe literal"),
    ];

    let ctx = IntegrationContext {
        output_dir: root,
        workspace_root: root,
        container_kind: ContainerKind::Primary,
        project: &project,
        repos: manifest
            .iter_entries()
            .map(|(rp, e)| (rp.clone(), e.clone()))
            .collect(),
        config: &config,
        all_repos_on_disk: &all_repos_on_disk,
        all_project_paths: &[],
        detection_cache: &cache,
        workweave: None,
    };

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("foundations.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let exclude = &parsed["settings"]["files.exclude"];

    // User-added keys MUST survive (this was the bug: they were wiped).
    assert_eq!(
        exclude["**/target"],
        serde_json::Value::Bool(true),
        "user-added **/target must survive reactivation"
    );
    assert_eq!(
        exclude["dist"],
        serde_json::Value::Bool(true),
        "user-added dist must survive reactivation"
    );

    // rwv-derived keys should be correct for the new state.
    // github/acme is still excluded (both repos excluded → collapses to owner).
    assert_eq!(
        exclude["github/acme"],
        serde_json::Value::Bool(true),
        "rwv-derived exclude for github/acme must be present"
    );

    // The marker and git.* keys must still be present.
    assert_eq!(
        parsed["rwv.generated"]["managed"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        parsed["settings"]["git.autoRepositoryDetection"],
        "subFolders"
    );
}

// -------------------------------------------------------------------------
// Scenario 2 — User adds `extensions`/`launch`/`tasks`/`compounds`; they
// survive activate AND deactivate.
// -------------------------------------------------------------------------
#[test]
fn scenario2_user_blocks_survive_activate_and_deactivate() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Seed: rwv-generated workspace + the four user-added top-level blocks.
    write_file(
        root,
        "myproject.code-workspace",
        r#"{
  "rwv.generated": true,
  "folders": [{"path": ".", "name": "myproject (primary)"}],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {".*": true}
  },
  "extensions": {
    "recommendations": ["rust-analyzer", "vadimcn.vscode-lldb"]
  },
  "launch": {
    "version": "0.2.0",
    "configurations": [{"type": "lldb", "request": "launch", "name": "Debug"}]
  },
  "tasks": {
    "version": "2.0.0",
    "tasks": [{"label": "build", "type": "shell", "command": "cargo build"}]
  },
  "compounds": [{"name": "Full debug", "configurations": ["Debug"]}]
}"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("myproject").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    // Activate: all four user blocks must survive.
    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("myproject.code-workspace")).unwrap();
    let after_activate: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert!(
        after_activate["extensions"]["recommendations"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("rust-analyzer")),
        "extensions must survive activate"
    );
    assert!(
        after_activate["launch"]["version"].as_str() == Some("0.2.0"),
        "launch must survive activate"
    );
    assert!(
        after_activate["tasks"]["version"].as_str() == Some("2.0.0"),
        "tasks must survive activate"
    );
    assert!(
        after_activate["compounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "Full debug"),
        "compounds must survive activate"
    );

    // Deactivate: file must NOT be deleted; owned keys stripped but user
    // blocks survive.
    VscodeWorkspace.deactivate(root).unwrap();

    assert!(
        root.join("myproject.code-workspace").exists(),
        "file must NOT be deleted — user content remains"
    );

    let content = std::fs::read_to_string(root.join("myproject.code-workspace")).unwrap();
    let after_deactivate: serde_json::Value = serde_json::from_str(&content).unwrap();

    // Owned keys stripped.
    assert!(
        after_deactivate.get("rwv.generated").is_none(),
        "marker must be stripped on deactivate"
    );
    assert!(
        after_deactivate.get("folders").is_none(),
        "folders must be stripped on deactivate"
    );
    assert!(
        after_deactivate["settings"]
            .as_object()
            .map(|m| m.get("files.exclude").is_none())
            .unwrap_or(true),
        "files.exclude must be stripped on deactivate"
    );

    // User blocks preserved.
    assert!(
        after_deactivate["extensions"]["recommendations"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("rust-analyzer")),
        "extensions must survive deactivate"
    );
    assert!(
        after_deactivate["launch"]["version"].as_str() == Some("0.2.0"),
        "launch must survive deactivate"
    );
    assert!(
        after_deactivate["tasks"]["version"].as_str() == Some("2.0.0"),
        "tasks must survive deactivate"
    );
    assert!(
        after_deactivate["compounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "Full debug"),
        "compounds must survive deactivate"
    );
}

// -------------------------------------------------------------------------
// Scenario 3 — User converts to multi-root; rwv keeps the extra folder.
// -------------------------------------------------------------------------
#[test]
fn scenario3_user_extra_folder_survives_reactivation() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // Seed: primary folder + a user-added extra folder (shared-notes).
    write_file(
        root,
        "foundations.code-workspace",
        r#"{
  "rwv.generated": true,
  "folders": [
    {"path": ".", "name": "foundations (primary)"},
    {"name": "notes", "path": "../shared-notes"}
  ],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {".*": true}
  }
}"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("foundations").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    VscodeWorkspace.activate(&ctx).unwrap();

    let content = std::fs::read_to_string(root.join("foundations.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let folders = parsed["folders"].as_array().unwrap();

    // BOTH folders must be present.
    assert_eq!(
        folders.len(),
        2,
        "both folders must be present after reactivation; got: {folders:?}"
    );

    // Element 0 must be the rwv-managed primary.
    assert_eq!(folders[0]["path"], ".", "primary folder must be at index 0");
    assert_eq!(
        folders[0]["name"], "foundations (primary)",
        "primary folder name must be updated"
    );

    // Element 1 must be the user-added extra folder, preserved unchanged.
    assert_eq!(
        folders[1]["path"], "../shared-notes",
        "user-added folder path must survive"
    );
    assert_eq!(
        folders[1]["name"], "notes",
        "user-added folder name must survive"
    );

    // Marker still present (object form).
    assert_eq!(
        parsed["rwv.generated"]["managed"],
        serde_json::Value::Bool(true)
    );
}

// -------------------------------------------------------------------------
// Scenario 4 — Deactivate of a purely-rwv file deletes it; hand-written
// file (no marker) is untouched.
// -------------------------------------------------------------------------
#[test]
fn scenario4_deactivate_deletes_pure_rwv_file_leaves_handwritten() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // (a) A purely-rwv .code-workspace: marker + owned keys only. The
    // marker records the exclude keys, so all of them are rwv's own.
    write_file(
        root,
        "proj.code-workspace",
        r#"{
  "rwv.generated": {"managed": true, "files.exclude": [".*"]},
  "folders": [{"path": ".", "name": "proj (primary)"}],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {".*": true}
  }
}"#,
    );

    // (b) A hand-written .code-workspace with no rwv marker.
    write_file(
        root,
        "mine.code-workspace",
        r#"{
  "folders": [{"path": "."}],
  "settings": {"editor.tabSize": 2}
}"#,
    );

    VscodeWorkspace.deactivate(root).unwrap();

    // (a) Purely-rwv file: all content was owned → delete it.
    assert!(
        !root.join("proj.code-workspace").exists(),
        "purely-rwv file must be deleted on deactivate"
    );

    // (b) Hand-written file: no marker → must not be touched.
    assert!(
        root.join("mine.code-workspace").exists(),
        "hand-written file must survive deactivate"
    );
    let content = std::fs::read_to_string(root.join("mine.code-workspace")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        parsed["settings"]["editor.tabSize"],
        serde_json::Value::Number(2.into()),
        "hand-written file content must be byte-identical"
    );
}

// -------------------------------------------------------------------------
// Scenario 5 — Deactivate strips rwv's own keys *within* the managed maps
// and leaves everything the user put there.
//
// rwv owns keys within a managed map, never the whole map: the `folders`
// entry whose path is ".", and the files.exclude keys the marker records.
// -------------------------------------------------------------------------
#[test]
fn scenario5_deactivate_preserves_user_excludes_and_folders() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "foundations.code-workspace",
        r#"{
  "rwv.generated": {"managed": true, "files.exclude": [".*", "github/acme"]},
  "folders": [
    {"path": ".", "name": "foundations (primary)"},
    {"path": "../shared-notes", "name": "notes"}
  ],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3,
    "files.exclude": {
      ".*": true,
      "github/acme": true,
      "**/target": true,
      "dist": false
    },
    "editor.tabSize": 2
  }
}"#,
    );

    VscodeWorkspace.deactivate(root).unwrap();

    let path = root.join("foundations.code-workspace");
    assert!(path.exists(), "file with user content must not be deleted");
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    // The two rwv-derived exclude keys go; the two user keys stay, values
    // intact.
    let exclude = &parsed["settings"]["files.exclude"];
    assert!(
        exclude.get(".*").is_none() && exclude.get("github/acme").is_none(),
        "rwv-derived exclude keys must be stripped; got: {exclude}"
    );
    assert_eq!(
        exclude["**/target"],
        serde_json::Value::Bool(true),
        "user-added **/target must survive deactivate; got: {exclude}"
    );
    assert_eq!(
        exclude["dist"],
        serde_json::Value::Bool(false),
        "user-added dist must survive deactivate with its value; got: {exclude}"
    );

    // The primary folder entry goes; the user's extra root stays.
    let folders = parsed["folders"].as_array().unwrap();
    assert_eq!(
        folders.len(),
        1,
        "only the rwv primary entry may be stripped; got: {folders:?}"
    );
    assert_eq!(folders[0]["path"], "../shared-notes");
    assert_eq!(folders[0]["name"], "notes");

    // DefaultOnly git.* settings are never stripped, and unrelated
    // settings are untouched.
    assert_eq!(
        parsed["settings"]["git.autoRepositoryDetection"],
        "subFolders"
    );
    assert_eq!(parsed["settings"]["git.repositoryScanMaxDepth"], 3);
    assert_eq!(parsed["settings"]["editor.tabSize"], 2);

    assert!(
        parsed.get("rwv.generated").is_none(),
        "marker must be stripped; got: {parsed}"
    );
}

// -------------------------------------------------------------------------
// Scenario 6 — A git.* value the user changed is a user choice: it keeps
// the file alive where the seeded value would not have.
// -------------------------------------------------------------------------
#[test]
fn scenario6_deactivate_keeps_file_holding_user_changed_git_setting() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "proj.code-workspace",
        r#"{
  "rwv.generated": {"managed": true, "files.exclude": [".*"]},
  "folders": [{"path": ".", "name": "proj (primary)"}],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 10,
    "files.exclude": {".*": true}
  }
}"#,
    );

    VscodeWorkspace.deactivate(root).unwrap();

    let path = root.join("proj.code-workspace");
    assert!(
        path.exists(),
        "a git.* value the user changed must keep the file"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed["settings"]["git.repositoryScanMaxDepth"], 10);
    assert!(parsed.get("rwv.generated").is_none());
    assert!(parsed.get("folders").is_none());
}

// -------------------------------------------------------------------------
// Scenario 7 — A marker predating the recorded exclude list cannot say
// which keys were rwv's, so it leaves all of them.
// -------------------------------------------------------------------------
#[test]
fn scenario7_deactivate_leaves_excludes_when_marker_records_none() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "proj.code-workspace",
        r#"{
  "rwv.generated": true,
  "folders": [{"path": ".", "name": "proj (primary)"}],
  "settings": {
    "files.exclude": {".*": true, "dist": true}
  }
}"#,
    );

    VscodeWorkspace.deactivate(root).unwrap();

    let path = root.join("proj.code-workspace");
    assert!(path.exists(), "unattributable excludes must keep the file");
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        parsed["settings"]["files.exclude"],
        serde_json::json!({".*": true, "dist": true}),
        "no exclude key may be guessed at; got: {parsed}"
    );
    assert!(parsed.get("folders").is_none());
    assert!(parsed.get("rwv.generated").is_none());
}

// -------------------------------------------------------------------------
// Scenario 8 — Activate is marker-gated too: a hand-authored workspace is
// left byte-for-byte alone, not converted to an rwv-owned file.
//
// This is the take-the-pen escape hatch: delete rwv's marker and the file
// is yours. Without this, `rwv doctor` reports the file USER-HELD and the
// next intent verb silently seizes it.
// -------------------------------------------------------------------------
#[test]
fn scenario8_activate_leaves_hand_authored_workspace_untouched() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    // A workspace a user wrote by hand: no rwv.generated marker, a folder
    // layout and excludes that are entirely their own.
    write_file(
        root,
        "foundations.code-workspace",
        r#"{
  "folders": [
    {"path": "github/acme/server", "name": "server"},
    {"path": ".", "name": "my own name for the root"}
  ],
  "settings": {
    "git.repositoryScanMaxDepth": 7,
    "files.exclude": {"**/target": true},
    "editor.tabSize": 2
  }
}"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("foundations").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    contract::assert_activate_leaves_user_held_untouched(
        &root.join("foundations.code-workspace"),
        || {
            VscodeWorkspace.activate(&ctx).unwrap();
        },
    );

    // Specifically: no marker was stamped, so the file does not become
    // rwv-owned on the run after next.
    let parsed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("foundations.code-workspace")).unwrap(),
    )
    .unwrap();
    assert!(
        parsed.get("rwv.generated").is_none(),
        "activate must not stamp the marker on a user-held file; got: {parsed}"
    );
}

// -------------------------------------------------------------------------
// Scenario 9 — The gate is the owned region, not the file. A file with no
// `folders` has nothing rwv could be taking, so rwv creates the key and
// manages from that point forward — preserving the blocks already there.
// -------------------------------------------------------------------------
#[test]
fn scenario9_activate_adopts_unmarked_file_without_the_owned_region() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path();

    write_file(
        root,
        "foundations.code-workspace",
        r#"{
  "extensions": {"recommendations": ["rust-lang.rust-analyzer"]},
  "settings": {"editor.tabSize": 2}
}"#,
    );

    let manifest = make_manifest(vec![("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("foundations").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(root, &project, &manifest, &config, &cache);

    VscodeWorkspace.activate(&ctx).unwrap();

    let parsed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("foundations.code-workspace")).unwrap(),
    )
    .unwrap();

    assert_eq!(
        parsed["rwv.generated"]["managed"], true,
        "an absent owned region is rwv's to create; got: {parsed}"
    );
    assert_eq!(parsed["folders"][0]["path"], ".");
    assert_eq!(parsed["folders"][0]["name"], "foundations (primary)");

    // The user's existing blocks are merged around, not replaced.
    assert_eq!(parsed["settings"]["editor.tabSize"], 2);
    assert_eq!(
        parsed["extensions"]["recommendations"][0],
        "rust-lang.rust-analyzer"
    );
}
