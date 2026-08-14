//! An activation hook materializes; it never moves a pin.
//!
//! Each test here is a prohibition: an ecosystem lockfile holding a
//! deliberately non-newest version survives a hooked activation byte for byte.
//! A hook fires on paths where no operator asked for a dependency update —
//! `rwv activate`, `rwv doctor --fix`, any verb that reaches activation — so
//! "the hook advanced a dependency" is silent dependency movement wherever it
//! happens.
//!
//! **Every test carries a control**, because a pin that never had anywhere to
//! move proves nothing: after asserting survival, the same fixture is driven by
//! the ecosystem's own re-resolve and must show the pin moving. A green
//! survival assertion with a red control is a fixture that cannot fail.
//!
//! The cargo fixture is hermetic — a directory source holding two versions of
//! one package, wired in by a `.cargo/config.toml` that replaces crates.io, so
//! real cargo resolves against local files and no registry is reachable or
//! needed. The npm and uv fixtures use their real registries: neither tool has
//! an equivalent of a two-version local source that costs less than the
//! network, and both are skipped when their tool is absent.

use std::collections::HashMap;
use std::path::Path;

mod common;

use repoweave::integration::{Integration, IntegrationContext};
use repoweave::integrations::merge::stamp_owned_digest;
use repoweave::integrations::{CargoWorkspace, NpmWorkspaces, UvWorkspace};
use repoweave::manifest::{IntegrationConfig, Manifest, ProjectName, Role};
use repoweave::workspace::ContainerKind;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn make_manifest(repos: &[(&str, Role)]) -> Manifest {
    let mut toml = String::new();
    for (path, role) in repos {
        let last = path.split('/').next_back().unwrap();
        toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"https://github.com/test/{last}.git\"\nversion = \"main\"\nrole = \"{}\"\n",
            role.as_str()
        ));
    }
    Manifest::from_toml_str(&toml).unwrap()
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
        container_kind: ContainerKind::Primary,
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

/// Write a two-version directory source under `dir` and the `.cargo/config.toml`
/// at `weave_root` that replaces crates.io with it.
///
/// A directory source is the cheapest thing real cargo will resolve a semver
/// range against without a network: each subdirectory is a package, and cargo
/// picks the newest matching one exactly as it would from an index. The source
/// lives outside the weave so its packages are never candidates for the
/// workspace membership rwv authors.
fn write_local_crate_source(source_dir: &Path, weave_root: &Path, versions: &[&str]) {
    for version in versions {
        let pkg = source_dir.join(format!("pinnable-{version}"));
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(
            pkg.join("Cargo.toml"),
            format!(
                "[package]\nname = \"pinnable\"\nversion = \"{version}\"\nedition = \"2021\"\n"
            ),
        )
        .unwrap();
        std::fs::write(pkg.join("src/lib.rs"), "pub fn pinned() {}\n").unwrap();
        std::fs::write(pkg.join(".cargo-checksum.json"), r#"{"files":{}}"#).unwrap();
    }
    std::fs::create_dir_all(weave_root.join(".cargo")).unwrap();
    std::fs::write(
        weave_root.join(".cargo/config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"local\"\n\n[source.local]\ndirectory = \"{}\"\n",
            common::json_escaped(source_dir)
        ),
    )
    .unwrap();
}

/// The `version = "x.y.z"` line of the `pinnable` package in a `Cargo.lock`.
fn locked_pinnable_version(lock_text: &str) -> Option<String> {
    let mut lines = lock_text.lines();
    while let Some(line) = lines.next() {
        if line.trim() == r#"name = "pinnable""# {
            return lines
                .next()
                .and_then(|v| v.trim().strip_prefix(r#"version = ""#).map(str::to_string))
                .and_then(|v| v.strip_suffix('"').map(str::to_string));
        }
    }
    None
}

fn cargo(args: &[&str], dir: &Path) -> std::process::Output {
    std::process::Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("cargo should run")
}

// ---------------------------------------------------------------------------
// cargo
// ---------------------------------------------------------------------------

/// A `Cargo.lock` rwv accepted, holding the older of the two versions the
/// source offers, is byte-identical after the hook that fires on every
/// activation.
///
/// The control at the end re-resolves the same fixture and must move the pin:
/// the fixture is only evidence if it is capable of showing movement.
#[test]
fn cargo_activation_leaves_a_non_newest_pin_byte_identical() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("weave");
    let source = tmp.path().join("crate-source");
    std::fs::create_dir_all(&root).unwrap();
    write_local_crate_source(&source, &root, &["0.1.0", "0.1.1"]);

    write_file(
        &root,
        "github/acme/server/Cargo.toml",
        "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\npinnable = \"0.1\"\n",
    );
    write_file(&root, "github/acme/server/src/lib.rs", "");

    let manifest = make_manifest(&[("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(&root, &project, &manifest, &config, &cache);

    let integration = CargoWorkspace;
    integration.activate(&ctx).unwrap();

    if which::which("cargo").is_err() {
        assert!(
            integration.activate_hook(&ctx).is_err(),
            "with cargo absent the hook has nothing to run and must say so"
        );
        return;
    }

    // The first activation is the first resolve: no lock exists, so nothing is
    // being discarded and the newest match is the right answer.
    integration
        .activate_hook(&ctx)
        .expect("first activation should produce a lock");
    let lock_path = root.join("Cargo.lock");
    let first = std::fs::read_to_string(&lock_path).unwrap();
    assert_eq!(
        locked_pinnable_version(&first).as_deref(),
        Some("0.1.1"),
        "fixture: a first resolve should pick the newest matching version"
    );

    // The operator pins back to the older version and rwv accepts it — the
    // state every fork of this weave then carries.
    let pinned = first.replace(r#"version = "0.1.1""#, r#"version = "0.1.0""#);
    assert_ne!(pinned, first, "fixture: the downgrade must change the lock");
    std::fs::write(&lock_path, &pinned).unwrap();
    stamp_owned_digest(&root, "Cargo.lock", pinned.as_bytes()).unwrap();

    integration
        .activate_hook(&ctx)
        .expect("activation over an attested lock should succeed");

    let after = std::fs::read_to_string(&lock_path).unwrap();
    assert_eq!(
        after,
        pinned,
        "activation must leave an attested lock byte-identical; \
         pinned {:?} became {:?}",
        locked_pinnable_version(&pinned),
        locked_pinnable_version(&after)
    );

    // Control: the same fixture, re-resolved, moves the pin. Without this the
    // assertion above passes for a fixture in which nothing could ever move.
    let out = cargo(&["generate-lockfile"], &root);
    assert!(
        out.status.success(),
        "control: generate-lockfile should run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let control = std::fs::read_to_string(&lock_path).unwrap();
    assert_eq!(
        locked_pinnable_version(&control).as_deref(),
        Some("0.1.1"),
        "control: a re-resolve must be able to move this pin, or the survival \
         assertion above is vacuous"
    );
}

/// Membership growth still reaches the lock: the hook is not a no-op, it is a
/// materialization. A member added after the lock was accepted contributes its
/// dependency, and the pin that was already there does not move.
#[test]
fn cargo_activation_adds_a_new_member_without_moving_an_existing_pin() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }

    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("weave");
    let source = tmp.path().join("crate-source");
    std::fs::create_dir_all(&root).unwrap();
    write_local_crate_source(&source, &root, &["0.1.0", "0.1.1"]);

    write_file(
        &root,
        "github/acme/server/Cargo.toml",
        "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\npinnable = \"0.1\"\n",
    );
    write_file(&root, "github/acme/server/src/lib.rs", "");

    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let integration = CargoWorkspace;

    let manifest = make_manifest(&[("github/acme/server", Role::Owned)]);
    let ctx = make_ctx(&root, &project, &manifest, &config, &cache);
    integration.activate(&ctx).unwrap();
    integration.activate_hook(&ctx).unwrap();

    let lock_path = root.join("Cargo.lock");
    let pinned = std::fs::read_to_string(&lock_path)
        .unwrap()
        .replace(r#"version = "0.1.1""#, r#"version = "0.1.0""#);
    std::fs::write(&lock_path, &pinned).unwrap();
    stamp_owned_digest(&root, "Cargo.lock", pinned.as_bytes()).unwrap();

    // A second Rust repo joins the project.
    write_file(
        &root,
        "github/acme/client/Cargo.toml",
        "[package]\nname = \"client\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write_file(&root, "github/acme/client/src/lib.rs", "");
    let grown = make_manifest(&[
        ("github/acme/client", Role::Owned),
        ("github/acme/server", Role::Owned),
    ]);
    let grown_ctx = make_ctx(&root, &project, &grown, &config, &cache);
    integration.activate(&grown_ctx).unwrap();
    integration
        .activate_hook(&grown_ctx)
        .expect("activation after a membership change should succeed");

    let after = std::fs::read_to_string(&lock_path).unwrap();
    assert!(
        after.contains(r#"name = "client""#),
        "the new member should have reached the lock:\n{after}"
    );
    assert_eq!(
        locked_pinnable_version(&after).as_deref(),
        Some("0.1.0"),
        "a membership change must not re-resolve the pins already recorded"
    );
}

// ---------------------------------------------------------------------------
// npm
// ---------------------------------------------------------------------------

/// npm's install already honours `package-lock.json`; this pins that it stays
/// the command rwv runs. `npm update` in its place would advance every
/// dependency that a range allows.
#[test]
fn npm_activation_leaves_a_non_newest_pin_byte_identical() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();

    write_file(
        &root,
        "github/acme/server/package.json",
        "{\"name\":\"server\",\"version\":\"0.1.0\",\"dependencies\":{\"lodash\":\"^4.17.0\"}}",
    );

    let manifest = make_manifest(&[("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(&root, &project, &manifest, &config, &cache);

    let integration = NpmWorkspaces;
    integration.activate(&ctx).unwrap();

    if which::which("npm").is_err() {
        assert!(
            integration.activate_hook(&ctx).is_err(),
            "with npm absent the hook has nothing to run and must say so"
        );
        return;
    }

    integration
        .activate_hook(&ctx)
        .expect("first activation should produce a package-lock");
    let lock_path = root.join("package-lock.json");
    let newest = locked_npm_version(&std::fs::read_to_string(&lock_path).unwrap());

    // Reach the pinned state the way an operator would: ask for the older
    // release exactly, then restore the range that still admits a newer one.
    // The lock that comes out is npm's own, and it records a version the range
    // no longer forces.
    let npm_install = |member: &str| {
        write_file(&root, "github/acme/server/package.json", member);
        let out = std::process::Command::new(common::node_tool("npm"))
            .args(["install"])
            .current_dir(&root)
            .output()
            .expect("npm should run");
        assert!(
            out.status.success(),
            "fixture: npm install failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::remove_file(&lock_path).unwrap();
    npm_install(r#"{"name":"server","version":"0.1.0","dependencies":{"lodash":"4.17.20"}}"#);
    npm_install(r#"{"name":"server","version":"0.1.0","dependencies":{"lodash":"^4.17.0"}}"#);

    let pinned = std::fs::read_to_string(&lock_path).unwrap();
    assert_eq!(
        locked_npm_version(&pinned).as_deref(),
        Some("4.17.20"),
        "fixture: the lock should hold the deliberately older version"
    );
    assert_ne!(
        locked_npm_version(&pinned),
        newest,
        "fixture: the pin must differ from what a fresh resolve picks"
    );
    stamp_owned_digest(&root, "package-lock.json", pinned.as_bytes()).unwrap();

    integration
        .activate_hook(&ctx)
        .expect("activation over an attested lock should succeed");
    let after = std::fs::read_to_string(&lock_path).unwrap();
    assert_eq!(
        locked_npm_version(&after).as_deref(),
        Some("4.17.20"),
        "activation must not advance a locked npm dependency"
    );
    assert_eq!(
        after, pinned,
        "the attested lock must survive byte-identical"
    );

    // Control: the fixture can show movement.
    assert!(
        std::process::Command::new(common::node_tool("npm"))
            .args(["update"])
            .current_dir(&root)
            .output()
            .expect("npm should run")
            .status
            .success(),
        "control: npm update should run"
    );
    let control = std::fs::read_to_string(&lock_path).unwrap();
    assert_ne!(
        locked_npm_version(&control).as_deref(),
        Some("4.17.20"),
        "control: an update must be able to move this pin, or the survival \
         assertion above is vacuous"
    );
}

/// The `"version"` recorded for the `lodash` entry of a `package-lock.json`.
///
/// npm keys the entry by install location, which is the hoisted
/// `node_modules/lodash` for a tree it built in one pass and a nested path for
/// one it converged on — the suffix is the part that identifies the package.
fn locked_npm_version(lock_text: &str) -> Option<String> {
    let mut lines = lock_text.lines();
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with('"') && line.contains("node_modules/lodash\":") {
            for entry in lines.by_ref().take(4) {
                if let Some(rest) = entry.trim().strip_prefix("\"version\": \"") {
                    return rest.split('"').next().map(str::to_string);
                }
            }
            return None;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// uv
// ---------------------------------------------------------------------------

/// `uv sync` already honours `uv.lock`; this pins that it stays the command
/// rwv runs, rather than an upgrading resolve.
#[test]
fn uv_activation_leaves_a_non_newest_pin_byte_identical() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();

    write_file(
        &root,
        "github/acme/server/pyproject.toml",
        "[project]\nname = \"server\"\nversion = \"0.1.0\"\nrequires-python = \">=3.9\"\n\
         dependencies = [\"six>=1.10\"]\n",
    );

    let manifest = make_manifest(&[("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(&root, &project, &manifest, &config, &cache);

    let integration = UvWorkspace;
    integration.activate(&ctx).unwrap();

    if which::which("uv").is_err() {
        assert!(
            integration.activate_hook(&ctx).is_err(),
            "with uv absent the hook has nothing to run and must say so"
        );
        return;
    }

    integration
        .activate_hook(&ctx)
        .expect("first activation should produce a uv.lock");
    let lock_path = root.join("uv.lock");
    let first = std::fs::read_to_string(&lock_path).unwrap();
    let newest = locked_uv_version(&first);

    // Ask uv itself for an older release, so the pinned lock is one uv wrote.
    let downgrade = std::process::Command::new("uv")
        .args(["lock", "--upgrade-package", "six==1.15.0"])
        .current_dir(&root)
        .output()
        .expect("uv should run");
    assert!(
        downgrade.status.success(),
        "fixture: uv should pin the older release: {}",
        String::from_utf8_lossy(&downgrade.stderr)
    );
    let pinned = std::fs::read_to_string(&lock_path).unwrap();
    assert_eq!(
        locked_uv_version(&pinned).as_deref(),
        Some("1.15.0"),
        "fixture: the lock should hold the deliberately older version"
    );
    assert_ne!(
        locked_uv_version(&pinned),
        newest,
        "fixture: the pin must differ from what a fresh resolve picks"
    );
    stamp_owned_digest(&root, "uv.lock", pinned.as_bytes()).unwrap();

    integration
        .activate_hook(&ctx)
        .expect("activation over an attested lock should succeed");
    let after = std::fs::read_to_string(&lock_path).unwrap();
    assert_eq!(
        after, pinned,
        "activation must leave an attested uv.lock byte-identical"
    );

    // Control: the fixture can show movement.
    let control_run = std::process::Command::new("uv")
        .args(["lock", "--upgrade"])
        .current_dir(&root)
        .output()
        .expect("uv should run");
    assert!(
        control_run.status.success(),
        "control: uv lock --upgrade should run"
    );
    let control = std::fs::read_to_string(&lock_path).unwrap();
    assert_ne!(
        locked_uv_version(&control).as_deref(),
        Some("1.15.0"),
        "control: an upgrading resolve must be able to move this pin, or the \
         survival assertion above is vacuous"
    );
}

/// The `version = "x.y.z"` recorded for the `six` package in a `uv.lock`.
fn locked_uv_version(lock_text: &str) -> Option<String> {
    let mut lines = lock_text.lines();
    while let Some(line) = lines.next() {
        if line.trim() == r#"name = "six""# {
            return lines
                .next()
                .and_then(|v| v.trim().strip_prefix(r#"version = ""#).map(str::to_string))
                .and_then(|v| v.strip_suffix('"').map(str::to_string));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// The `doctor --fix` door
// ---------------------------------------------------------------------------

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

fn git_init_with_commit(dir: &Path) {
    git(&["init", "--initial-branch=main"], dir);
    git(&["config", "user.email", "test@test.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "-A"], dir);
    git(&["commit", "-m", "init"], dir);
}

/// `rwv doctor --fix`, run for a finding that has nothing to do with the lock,
/// routes through a hooked activation. That door is the one an operator walks
/// through without asking for anything to be resolved, so what it must not do
/// is move a pin.
#[test]
fn doctor_fix_for_an_unrelated_finding_leaves_a_pin_byte_identical() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }

    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let source = tmp.path().join("crate-source");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    write_local_crate_source(&source, &ws, &["0.1.0", "0.1.1"]);

    let server = ws.join("github/acme/server");
    std::fs::create_dir_all(server.join("src")).unwrap();
    std::fs::write(
        server.join("Cargo.toml"),
        "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\npinnable = \"0.1\"\n",
    )
    .unwrap();
    std::fs::write(server.join("src/lib.rs"), "").unwrap();
    git_init_with_commit(&server);

    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/server\"]\ntype = \"git\"\nurl = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    std::fs::write(project_dir.join(".gitignore"), "/Cargo.lock\n").unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve(&ws, None).unwrap();
    repoweave::activate::activate_intent("app", &ctx).expect("activation should succeed");

    let lock_path = project_dir.join("Cargo.lock");
    let pinned = std::fs::read_to_string(&lock_path)
        .unwrap()
        .replace(r#"version = "0.1.1""#, r#"version = "0.1.0""#);
    std::fs::write(&lock_path, &pinned).unwrap();
    stamp_owned_digest(&project_dir, "Cargo.lock", pinned.as_bytes()).unwrap();

    // The unrelated finding: the managed member list no longer matches
    // `rwv.toml`. It is safe-to-fix, which is what puts `--fix` on the
    // activation path in the first place.
    let managed = project_dir.join("Cargo.toml");
    let drifted = std::fs::read_to_string(&managed)
        .unwrap()
        .replace("members = [", "members = [\n    \"github/acme/ghost\",");
    std::fs::write(&managed, drifted).unwrap();

    let output = common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    let report = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        !std::fs::read_to_string(&managed)
            .unwrap()
            .contains("github/acme/ghost"),
        "precondition: --fix should have repaired the member drift, or it never \
         reached activation:\n{report}"
    );
    let after = std::fs::read_to_string(&lock_path).unwrap();
    assert_eq!(
        after,
        pinned,
        "`doctor --fix` for an unrelated finding must leave the lock \
         byte-identical; pinned {:?} became {:?}\n{report}",
        locked_pinnable_version(&pinned),
        locked_pinnable_version(&after)
    );
}

/// `rwv materialize` is the verb an operator is told to run after a sync, and
/// the only way to run the hooks inside a workweave. It is safe to name as a
/// remedy exactly because it cannot move a pin.
#[test]
fn materialize_leaves_a_pin_byte_identical() {
    if which::which("cargo").is_err() {
        eprintln!("skipping: `cargo` not found on PATH");
        return;
    }

    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let source = tmp.path().join("crate-source");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    write_local_crate_source(&source, &ws, &["0.1.0", "0.1.1"]);

    let server = ws.join("github/acme/server");
    std::fs::create_dir_all(server.join("src")).unwrap();
    std::fs::write(
        server.join("Cargo.toml"),
        "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\npinnable = \"0.1\"\n",
    )
    .unwrap();
    std::fs::write(server.join("src/lib.rs"), "").unwrap();
    git_init_with_commit(&server);

    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/server\"]\ntype = \"git\"\nurl = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve(&ws, None).unwrap();
    repoweave::activate::activate_intent("app", &ctx).expect("activation should succeed");

    let lock_path = project_dir.join("Cargo.lock");
    let pinned = std::fs::read_to_string(&lock_path)
        .unwrap()
        .replace(r#"version = "0.1.1""#, r#"version = "0.1.0""#);
    std::fs::write(&lock_path, &pinned).unwrap();
    stamp_owned_digest(&project_dir, "Cargo.lock", pinned.as_bytes()).unwrap();

    let output = common::rwv()
        .args(["materialize"])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    let report = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "materialize should succeed:\n{report}"
    );
    assert_eq!(
        std::fs::read_to_string(&lock_path).unwrap(),
        pinned,
        "`rwv materialize` must leave an attested lock byte-identical\n{report}"
    );
}

// ---------------------------------------------------------------------------
// Every integration: which commands a hooked activation is allowed to run
// ---------------------------------------------------------------------------

/// Shim `name` into `bin_dir` so it records its argv in `log` and otherwise
/// does as little as it can get away with.
///
/// The shim is a shebang script that `rwv` must find on `PATH` and spawn
/// itself. That is a strictly harder thing to ask for than a git hook: git
/// reads the `#!` line and looks the interpreter up on its own, whereas an
/// ordinary process spawn on Windows does not, and an extensionless file is
/// not a candidate there at all because lookup selects on `PATHEXT`. So this
/// fixture needs both a Windows spelling for the script and a decision about
/// what an executable's name means there before it can port.
#[cfg(unix)]
fn write_shim(bin_dir: &Path, name: &str, log: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = bin_dir.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' {name} \"$*\" >> {}\n{body}\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The complete set of ecosystem commands one `rwv activate` may run.
///
/// This is the per-ecosystem audit as an assertion rather than a reading: every
/// invocation is matched against the list, so an integration that starts
/// shelling out to something new fails here until someone decides whether the
/// new command can move a pin. `go work use` is on the list because it edits
/// membership without resolving anything; `go get`, `go mod tidy`,
/// `npm update`, `pnpm update`, `uv lock --upgrade` and a `cargo
/// generate-lockfile` over an existing lock are the shapes it exists to keep
/// out.
///
/// Gated on the fixture, not the subject: which commands an activation may run
/// is a portable contract, but the instrument that observes them is a shebang
/// shim on `PATH`, which Windows will neither find nor spawn.
#[cfg(unix)]
#[test]
fn a_hooked_activation_runs_only_materializing_commands() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let bin = tmp.path().join("bin");
    let log = tmp.path().join("argv.log");
    std::fs::create_dir_all(ws.join("projects/app")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();

    // A single repo that every ecosystem integration detects.
    let repo = ws.join("github/acme/everything");
    std::fs::create_dir_all(&repo).unwrap();
    for (name, content) in [
        (
            "Cargo.toml",
            "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "package.json",
            "{\"name\":\"server\",\"version\":\"0.1.0\"}",
        ),
        (
            "pyproject.toml",
            "[project]\nname = \"server\"\nversion = \"0.1.0\"\n",
        ),
        ("go.mod", "module example.com/server\n\ngo 1.21\n"),
    ] {
        std::fs::write(repo.join(name), content).unwrap();
    }
    git_init_with_commit(&repo);

    std::fs::write(
        ws.join("projects/app/rwv.toml"),
        "[repositories.\"github/acme/everything\"]\ntype = \"git\"\nurl = \"https://github.com/acme/everything.git\"\nversion = \"main\"\nrole = \"owned\"\n\n\
         [integrations.pnpm-workspaces]\nenabled = true\n\n[integrations.gita]\nenabled = true\n",
    )
    .unwrap();
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    // The lock the cargo hook must not re-resolve exists before activation, so
    // the run under audit is the one with a resolve to preserve.
    std::fs::write(
        ws.join("projects/app/Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"server\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    write_shim(&bin, "cargo", &log, "");
    write_shim(&bin, "npm", &log, "");
    write_shim(&bin, "pnpm", &log, "");
    write_shim(&bin, "uv", &log, "");
    write_shim(&bin, "gita", &log, "");
    // `go work` authors the file the go integration owns, so its shim has to
    // leave one behind or activation fails for a reason unrelated to argv.
    write_shim(
        &bin,
        "go",
        &log,
        "case \"$*\" in \
         'work init') printf 'go 1.21\\n' > go.work ;; \
         'work use '*) printf 'use %s\\n' \"${3}\" >> go.work ;; \
         esac",
    );

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let rwv_with_shims = |args: &[&str]| {
        let output = common::rwv()
            .args(args)
            .current_dir(&ws)
            .env("PATH", &path)
            .output()
            .expect("rwv should run");
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };

    // Activate is a context verb: it materializes what an authoring verb wrote,
    // so the managed files have to exist before the run under audit means
    // anything.
    let authored = rwv_with_shims(&["doctor", "--fix"]);
    assert!(
        ws.join("projects/app/Cargo.toml").exists(),
        "fixture: the authoring pass should have written the managed files:\n{authored}"
    );
    std::fs::write(&log, "").unwrap();

    let report = rwv_with_shims(&["activate", "app"]);

    let invocations: Vec<String> = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    // Non-vacuity: a log nobody wrote to would satisfy every prohibition below.
    assert!(
        invocations.len() >= 4,
        "the audit read {} ecosystem invocations, which is too few to be \
         auditing anything:\n{invocations:#?}\n{report}",
        invocations.len()
    );

    let allowed = |call: &str| {
        call == "cargo fetch"
            || call == "npm install"
            || call == "pnpm install"
            || call == "uv sync"
            || call.starts_with("go work ")
            || call == "gita"
    };
    let unexpected: Vec<&String> = invocations.iter().filter(|c| !allowed(c)).collect();
    assert!(
        unexpected.is_empty(),
        "a hooked activation ran commands outside the materializing set: \
         {unexpected:#?}\nall invocations:\n{invocations:#?}\n{report}"
    );

    for tool in ["cargo", "npm", "uv"] {
        assert!(
            invocations.iter().any(|c| c.starts_with(tool)),
            "{tool} should have been reached by this activation:\n{invocations:#?}\n{report}"
        );
    }
}

/// go materializes membership and nothing else: `go work use` records which
/// modules are in the workspace and resolves no version, so a go activation
/// leaves no `go.sum` behind to have pinned anything in.
#[test]
fn go_activation_records_membership_without_resolving() {
    if which::which("go").is_err() {
        eprintln!("skipping: `go` not found on PATH");
        return;
    }

    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    write_file(
        &root,
        "github/acme/server/go.mod",
        "module example.com/server\n\ngo 1.21\n",
    );

    let manifest = make_manifest(&[("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::default();
    let cache = HashMap::new();
    let ctx = make_ctx(&root, &project, &manifest, &config, &cache);

    let integration = repoweave::integrations::GoWork;
    integration
        .activate(&ctx)
        .expect("go activation should succeed");
    integration
        .activate_hook(&ctx)
        .expect("go hook should succeed");

    assert!(
        root.join("go.work").exists(),
        "go activation should record membership in go.work"
    );
    assert!(
        !root.join("go.sum").exists(),
        "go activation must not resolve module versions"
    );
    assert!(
        !root.join("github/acme/server/go.sum").exists(),
        "go activation must not resolve the member's module versions either"
    );
}

/// gita's activation writes its own CSV inventory and runs no ecosystem tool,
/// so there is no pin for it to move.
#[test]
fn gita_activation_produces_no_lockfile() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    write_file(&root, "github/acme/server/Cargo.toml", "");

    let manifest = make_manifest(&[("github/acme/server", Role::Owned)]);
    let project = ProjectName::new("test-project").unwrap();
    let config = IntegrationConfig::from_toml("enabled = true");
    let cache = HashMap::new();
    let ctx = make_ctx(&root, &project, &manifest, &config, &cache);

    let integration = repoweave::integrations::Gita;
    integration
        .activate(&ctx)
        .expect("gita activation should succeed");
    integration
        .activate_hook(&ctx)
        .expect("gita hook should succeed");

    for lock in ["Cargo.lock", "package-lock.json", "uv.lock", "go.sum"] {
        assert!(
            !root.join(lock).exists(),
            "gita activation produced {lock}, which it has no business writing"
        );
    }
}
