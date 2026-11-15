//! `rwv integration set-enabled` and `rwv integration set` — change one key of
//! an `[integrations.<name>]` table in this workspace's manifest, and settle
//! what the change reaches.

use crate::activate::ActivateOptions;
use crate::add_remove::{find_project, settle_manifest};
use crate::manifest::{Enablement, Manifest};
use crate::refusal::RefusalKind;
use crate::workspace::WorkspaceContext;
use anyhow::Context;
use std::collections::BTreeSet;

/// Commit `apply` to the manifest as HEAD has it, with what the verb's
/// regeneration wrote and nothing else the project repo holds uncommitted.
fn commit_change(
    ctx: &WorkspaceContext,
    project: &crate::manifest::ProjectName,
    project_dir: &std::path::Path,
    before: &Manifest,
    apply: impl Fn(&mut Manifest),
    message: String,
) -> anyhow::Result<()> {
    let on_disk = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    let authored: BTreeSet<String> =
        crate::manifest_commit::authored_across(ctx, project, &[before, &on_disk]);
    crate::manifest_commit::commit(&crate::manifest_commit::MemberCommit {
        project,
        project_dir,
        edit_manifest: &apply,
        edit_lock: None,
        authored: &authored,
        message: &message,
        naming: crate::repin::Naming::Unnamed,
    })?;
    Ok(())
}

/// Refuse a name or key no built-in integration takes, with the parser's own
/// message, which lists the accepted ones, and a value that is not TOML.
fn judge_setting(name: &str, key: &str, value: &toml::Value) -> anyhow::Result<()> {
    let mut table = toml::Table::new();
    table.insert(key.to_owned(), value.clone());
    let mut integrations = toml::Table::new();
    integrations.insert(name.to_owned(), toml::Value::Table(table));
    let mut root = toml::Table::new();
    root.insert(
        "repositories".to_owned(),
        toml::Value::Table(toml::Table::new()),
    );
    root.insert("integrations".to_owned(), toml::Value::Table(integrations));
    let text = toml::to_string(&root).context("failed to spell the setting")?;
    if let Err(e) = Manifest::from_toml_str(&text) {
        let said = format!("{e:#}");
        let reason = said
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(&said);
        crate::refuse!(
            RefusalKind::UnknownIntegrationSetting,
            "`[integrations.{name}] {key}`: {reason}"
        );
    }
    Ok(())
}

/// Execute `rwv integration set-enabled <name> <true|false>`.
///
/// Enabling authors the managed files the integration declares; disabling
/// deletes nothing, and names `rwv materialize` as the step that strips what
/// the integration authored before. A value `rwv.toml` already holds is not
/// rewritten, but the generated files are regenerated, so a hand edit of the
/// key is finished by re-running this.
pub fn run_set_enabled(
    name: &str,
    enabled: bool,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    judge_setting(
        name,
        crate::manifest::IntegrationConfig::ENABLED_KEY,
        &toml::Value::Boolean(enabled),
    )?;
    let (project, project_dir) = find_project(ctx)?;
    crate::diagnosis::settle_project_drift(&project, &project_dir)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;
    let was = manifest.integration_enablement(name);
    let before = manifest.clone();
    crate::manifest_commit::judge(&project_dir)?;
    let stated = if enabled {
        Enablement::On
    } else {
        Enablement::Off
    };
    let message =
        format!("integration set-enabled {name} {enabled}\n\n  - {name}: enabled {enabled}");
    let name_for_edit = name.to_owned();
    let apply = move |m: &mut Manifest| m.set_integration_enabled(&name_for_edit, enabled);
    if was == stated {
        settle_manifest(ctx, &project, None, &[], opts)?;
        commit_change(ctx, &project, &project_dir, &before, apply, message)?;
        eprintln!("[integrations.{name}] already has enabled = {enabled}; nothing to change");
    } else {
        manifest.set_integration_enabled(name, enabled);
        let landed = settle_manifest(ctx, &project, Some(&manifest), &[], opts);
        let committed = if landed.is_ok() {
            commit_change(ctx, &project, &project_dir, &before, apply, message)
        } else {
            Ok(())
        };
        landed?;
        committed?;
        eprintln!("Set enabled = {enabled} for the integration {name}");
    }
    if !enabled {
        eprintln!(
            "{name} is disabled and nothing was deleted; `rwv materialize` strips what it \
             authored before"
        );
    }
    Ok(())
}

/// Execute `rwv integration set <name> <key> <value>`: `value` is a TOML value,
/// so a string is quoted.
pub fn run_set(
    name: &str,
    key: &str,
    value: &str,
    ctx: &WorkspaceContext,
    opts: ActivateOptions,
) -> anyhow::Result<()> {
    if key == crate::manifest::IntegrationConfig::ENABLED_KEY {
        crate::refuse!(
            RefusalKind::InapplicableFlag,
            "`enabled` is set by `rwv integration set-enabled {name} <true|false>`"
        );
    }
    let parsed: toml::Table = match toml::from_str(&format!("v = {value}")) {
        Ok(table) => table,
        Err(_) => crate::refuse!(
            RefusalKind::UnknownIntegrationSetting,
            "`{value}` is not a TOML value; quote a string, as in '\"sccache\"', and write a \
             list as '[\"a\", \"b\"]'"
        ),
    };
    let value = parsed
        .get("v")
        .cloned()
        .expect("the value was just spelled");
    judge_setting(name, key, &value)?;
    let (project, project_dir) = find_project(ctx)?;
    crate::diagnosis::settle_project_drift(&project, &project_dir)?;
    let manifest_path = project_dir.join(Manifest::FILE_NAME);
    let mut manifest = Manifest::from_path(&manifest_path)
        .with_context(|| format!("failed to load manifest at {}", manifest_path.display()))?;
    let held = manifest.integration_setting(name, key).cloned();
    let before = manifest.clone();
    crate::manifest_commit::judge(&project_dir)?;
    let (name_for_edit, key_for_edit, value_for_edit) =
        (name.to_owned(), key.to_owned(), value.clone());
    let apply = move |m: &mut Manifest| {
        m.set_integration_setting(&name_for_edit, &key_for_edit, value_for_edit.clone())
    };
    let message = format!("integration set {name} {key}\n\n  - {name}: {key} = {value}");
    if held.as_ref() == Some(&value) {
        settle_manifest(ctx, &project, None, &[], opts)?;
        commit_change(ctx, &project, &project_dir, &before, apply, message)?;
        eprintln!("[integrations.{name}] already has {key} = {value}; nothing to change");
        return Ok(());
    }
    manifest.set_integration_setting(name, key, value.clone());
    let landed = settle_manifest(ctx, &project, Some(&manifest), &[], opts);
    let committed = if landed.is_ok() {
        commit_change(ctx, &project, &project_dir, &before, apply, message)
    } else {
        Ok(())
    };
    landed?;
    committed?;
    eprintln!("Set {key} = {value} for the integration {name}");
    Ok(())
}
