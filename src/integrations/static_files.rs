//! Static-files integration.
//!
//! Symlinks declared files from the project directory to the workspace root on
//! activate, and removes them on deactivate. Configured in the
//! `[integrations.static-files]` table of `rwv.toml` with a list of entries.
//! Default disabled.
//!
//! Example config:
//!
//! ```toml
//! [integrations.static-files]
//! enabled = true
//! files = ["turbo.json", ".prettierrc", { path = ".beads", from = "primary" }]
//! ```
//!
//! A plain entry links the weave's own project checkout. An entry with
//! `from = "primary"` links the primary's, so every workweave of the project
//! shares one copy.
//!
//! # Collision with `workweave.link`
//!
//! A name that appears in BOTH `integrations.static-files.files` (in either
//! form) AND `workweave.link` is rejected as a hard `Severity::Error`. The two
//! sections give one root name two different answers in a workweave:
//!
//! - `workweave.link` creates an absolute symlink to the canonical path of the
//!   source weave's root slot, once, at creation.
//! - `static-files.files` surfaces a project checkout's copy, and the
//!   surfacing check verifies and repairs it.
//!
//! Silently picking one of the two is a footgun. The fix has two layers: the
//! framework's owner-scoped removal predicate preserves the `workweave.link`
//! symlink at activation time (defensive), and this integration raises a loud
//! Severity::Error pre-activate so an operator who wrote the conflicting
//! config sees a clear message rather than relying on the framework's
//! tie-breaking (defense in depth).

use crate::integration::{
    Integration, IntegrationContext, Issue, IssueKind, IssueSource, Severity, SurfacedFile,
};
use crate::workspace::ContainerKind;
use serde::Deserialize;
use std::fmt;
use std::path::Path;

/// Integration-specific settings for the `static-files` integration.
#[derive(Deserialize, Default)]
struct StaticFilesConfig {
    #[serde(default)]
    files: Vec<StaticFileEntry>,
}

/// One `files` entry: a bare path, or `{ path = "...", from = "primary" }`.
#[derive(Debug, PartialEq, Eq)]
struct StaticFileEntry {
    path: String,
    from: Option<CopyFrom>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CopyFrom {
    Primary,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticFileTable {
    path: String,
    #[serde(default)]
    from: Option<CopyFrom>,
}

impl StaticFileEntry {
    fn reads_primary(&self) -> bool {
        self.from == Some(CopyFrom::Primary)
    }

    fn surfaced(&self) -> SurfacedFile {
        let file = SurfacedFile::written_at_source(self.path.clone());
        if self.reads_primary() {
            file.from_primary()
        } else {
            file
        }
    }
}

impl<'de> Deserialize<'de> for StaticFileEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = StaticFileEntry;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(
                    f,
                    "a path string, or a table `{{ path = \"...\", from = \"primary\" }}`"
                )
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StaticFileEntry {
                    path: v.to_string(),
                    from: None,
                })
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                let table = StaticFileTable::deserialize(
                    serde::de::value::MapAccessDeserializer::new(map),
                )?;
                Ok(StaticFileEntry {
                    path: table.path,
                    from: table.from,
                })
            }
        }
        deserializer.deserialize_any(V)
    }
}

pub struct StaticFiles;

/// Names declared in BOTH `static-files.files` and `workweave.link`, sorted.
///
/// Operators who write this combination almost certainly meant only one of
/// the two; the integration refuses to guess. Returns the sorted unique
/// collision set so callers can emit one Issue per collision or fail with a
/// single aggregate error message.
fn collision_names(cfg: &StaticFilesConfig, ctx: &IntegrationContext) -> Vec<String> {
    let Some(workweave) = ctx.workweave() else {
        return Vec::new();
    };
    if workweave.link.is_empty() || cfg.files.is_empty() {
        return Vec::new();
    }
    let link_set: std::collections::BTreeSet<&str> =
        workweave.link.iter().map(|s| s.as_str()).collect();
    let mut hits: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for f in &cfg.files {
        if link_set.contains(f.path.as_str()) {
            hits.insert(f.path.clone());
        }
    }
    hits.into_iter().collect()
}

/// Human-readable message for a single colliding name. Names both integrations
/// so the operator can fix the rwv.toml without re-reading the doc string.
fn collision_message(name: &str) -> String {
    format!(
        "name '{name}' is declared in both static-files.files and workweave.link; \
         pick one (workweave.link absolutizes to the primary, static-files surfaces project content)"
    )
}

/// The entries whose copy is the one under `output_dir`. In a workweave a
/// primary-sourced entry names the primary's checkout instead, which may hold
/// what the workweave's own checkout does not.
fn checked_at_output_dir<'c>(
    cfg: &'c StaticFilesConfig,
    ctx: &IntegrationContext,
) -> impl Iterator<Item = &'c str> {
    let in_workweave = ctx.container_kind() == ContainerKind::Workweave;
    cfg.files
        .iter()
        .filter(move |f| !(in_workweave && f.reads_primary()))
        .map(|f| f.path.as_str())
}

impl Integration for StaticFiles {
    fn name(&self) -> &str {
        "static-files"
    }

    fn settings_keys(&self) -> &'static [&'static str] {
        crate::integration::settings_struct_keys::<StaticFilesConfig>()
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        // Static files live in the project directory and are symlinked to the
        // workspace root by the activation framework (via `generated_files()`).
        // The activate hook itself is a no-op — it does not need to generate
        // any files. The files are expected to already exist in the project dir.
        //
        // We validate here that declared files actually exist so that the user
        // gets early feedback (activation still succeeds — missing files are
        // simply skipped by the symlink machinery in activate.rs).
        let cfg: StaticFilesConfig = match ctx.settings_or_issue(self.name()) {
            Ok(cfg) => cfg,
            Err(issue) => return Ok(vec![issue]),
        };

        // Defense in depth: even though `run_checks` runs in Context-mode
        // activation (and `run_activations` itself drives `check`-then-bail in
        // Intent mode via report_and_check_activation_issues), a hand-edited
        // rwv.toml mid-session or a future call site that skips checks would
        // silently fall through to the framework predicate. A loud bail here
        // keeps the contract explicit: static-files refuses to author symlinks
        // for a name owned by workweave.link.
        let collisions = collision_names(&cfg, ctx);
        if !collisions.is_empty() {
            // bail with the first collision's message — operators almost
            // always have a single offending entry, so a focused message is
            // more actionable than a comma-joined list. check() reports them
            // all individually for `rwv doctor`.
            anyhow::bail!("{}", collision_message(&collisions[0]));
        }

        for file in checked_at_output_dir(&cfg, ctx) {
            if !ctx
                .output_dir()
                .join(crate::path_spelling::native_relative(file))
                .exists()
            {
                eprintln!(
                    "[warning] static-files: declared file '{}' not found in project directory",
                    file
                );
            }
        }
        Ok(Vec::new())
    }

    fn deactivate(&self, root: &Path) -> anyhow::Result<()> {
        // Symlink removal is handled by the activation framework
        // (remove_activation_symlinks in activate.rs), which removes any
        // symlink at the workspace root whose target points into `projects/`.
        // We don't need to do anything extra here since the static files are
        // plain symlinks into the project directory.
        let _ = root;
        Ok(())
    }

    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> {
        let cfg: StaticFilesConfig = match ctx.settings_or_issue(self.name()) {
            Ok(cfg) => cfg,
            Err(issue) => return Ok(vec![issue]),
        };
        let mut issues = Vec::new();

        // Collision with workweave.link: hard Severity::Error so
        // `rwv doctor` surfaces it pre-activation. The same predicate is
        // re-run in `activate()` as a defense-in-depth bail.
        for name in collision_names(&cfg, ctx) {
            issues.push(Issue {
                source: IssueSource::integration(self.name()),
                severity: Severity::Error,
                message: collision_message(&name).into(),
                kind: IssueKind::ConfigRejected,
                safe_to_fix: true,
            });
        }

        for file in checked_at_output_dir(&cfg, ctx) {
            if !ctx
                .output_dir()
                .join(crate::path_spelling::native_relative(file))
                .exists()
            {
                issues.push(Issue {
                    source: IssueSource::integration(self.name()),
                    severity: Severity::Warning,
                    message: format!("declared file '{}' not found in project directory", file)
                        .into(),
                    kind: IssueKind::ConfigRejected,
                    safe_to_fix: true,
                });
            }
        }
        Ok(issues)
    }

    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<SurfacedFile> {
        ctx.config()
            .settings::<StaticFilesConfig>()
            .unwrap_or_default()
            .files
            .iter()
            .map(StaticFileEntry::surfaced)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::SurfacedCopy;
    use crate::manifest::IntegrationConfig;

    fn parse(toml: &str) -> Result<StaticFilesConfig, toml::de::Error> {
        IntegrationConfig::from_toml(toml).settings()
    }

    #[test]
    fn a_plain_string_and_a_table_without_from_surface_the_weaves_copy() {
        let cfg = parse("files = [\"turbo.json\", { path = \"nx.json\" }]").unwrap();
        let copies: Vec<_> = cfg.files.iter().map(|f| f.surfaced().copy()).collect();
        assert_eq!(copies, [SurfacedCopy::Weave, SurfacedCopy::Weave]);
    }

    #[test]
    fn from_primary_surfaces_the_primarys_copy() {
        let cfg = parse("files = [{ path = \".beads\", from = \"primary\" }]").unwrap();
        assert_eq!(
            cfg.files[0].surfaced(),
            SurfacedFile::written_at_source(".beads").from_primary()
        );
    }

    #[test]
    fn a_misspelt_key_or_value_is_refused_rather_than_read_as_plain() {
        let key = parse("files = [{ path = \".beads\", form = \"primary\" }]")
            .err()
            .expect("an unknown key must not parse");
        assert!(key.to_string().contains("form"), "{key}");
        let value = parse("files = [{ path = \".beads\", from = \"primry\" }]")
            .err()
            .expect("an unknown value must not parse");
        assert!(value.to_string().contains("primry"), "{value}");
    }
}
