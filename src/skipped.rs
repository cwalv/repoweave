//! What a verb left undone on this run that it does on others, and the verb
//! that does it.
//!
//! Which verb authors files and which runs install hooks depends on where it
//! runs and how it was called, and readers mispredict it. So a verb that skips
//! one of those side effects says so in one line, naming the remedy, and puts
//! the same fact in its `--json` envelope as a record. A run that skipped
//! nothing prints nothing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One materialization side effect a verb skipped on this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkippedEffect {
    pub effect: Effect,
    /// The integration whose part of `effect` was skipped; absent when the
    /// whole effect was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration: Option<String>,
    pub reason: SkipReason,
    /// The command that performs the effect.
    pub remedy: String,
}

/// The side effect that did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    /// The integrations' install hooks (`npm install`, `uv sync`, `cargo
    /// fetch`, …).
    InstallHooks,
    /// Regenerating the integrations' managed and generated files.
    IntegrationFiles,
    /// Refreshing the weave root's links onto the project's files.
    WeaveRootLinks,
    /// Activating a fetched project: selecting it, surfacing its links and
    /// running its install hooks.
    Activation,
}

/// Why the effect did not happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    /// `--no-materialize` was passed.
    NoMaterialize,
    /// An authoring verb ran inside a workweave, where it runs no hooks.
    InsideWorkweave,
    /// The CLI the integration's hook drives is not on `PATH`.
    ToolMissing { tool: String },
    /// The verb acted on a project the weave root does not present.
    NotPresented { project: String },
    /// `rwv update` was narrowed by `--role` or `--repo`.
    Narrowed,
    /// A project was already active, so the fetch selected none.
    ProjectAlreadyActive { project: String },
}

impl SkippedEffect {
    /// The install hooks, all of them, skipped for `reason`.
    pub fn install_hooks(reason: SkipReason) -> Self {
        let remedy = match &reason {
            SkipReason::NotPresented { project } => format!("rwv activate {project}"),
            _ => "rwv materialize".to_string(),
        };
        Self {
            effect: Effect::InstallHooks,
            integration: None,
            reason,
            remedy,
        }
    }

    /// `integration`'s hook, skipped because `tool` is not on `PATH`.
    pub fn tool_missing(integration: &str, tool: &str) -> Self {
        Self {
            effect: Effect::InstallHooks,
            integration: Some(integration.to_string()),
            reason: SkipReason::ToolMissing {
                tool: tool.to_string(),
            },
            remedy: "rwv materialize".to_string(),
        }
    }

    /// The links and the hooks of `project`, which the weave root does not
    /// present, so an authoring verb regenerated its files and stopped there.
    pub fn not_presented(project: &str) -> [Self; 2] {
        let reason = SkipReason::NotPresented {
            project: project.to_string(),
        };
        [
            Self {
                effect: Effect::WeaveRootLinks,
                integration: None,
                reason: reason.clone(),
                remedy: format!("rwv activate {project}"),
            },
            Self::install_hooks(reason),
        ]
    }

    /// The regeneration and the hooks a narrowed `rwv update` leaves alone.
    pub fn narrowed() -> [Self; 2] {
        [
            Self {
                effect: Effect::IntegrationFiles,
                integration: None,
                reason: SkipReason::Narrowed,
                remedy: "rwv doctor --fix".to_string(),
            },
            Self::install_hooks(SkipReason::Narrowed),
        ]
    }

    /// A bootstrap fetch of `fetched` that activated nothing because `active`
    /// already was.
    pub fn activation_while_active(active: &str, fetched: &str) -> Self {
        Self {
            effect: Effect::Activation,
            integration: None,
            reason: SkipReason::ProjectAlreadyActive {
                project: active.to_string(),
            },
            remedy: format!("rwv activate {fetched}"),
        }
    }

    /// An in-place fetch that ran no hooks because `active` was already
    /// active and it selected nothing.
    pub fn hooks_while_active(active: &str) -> Self {
        Self::install_hooks(SkipReason::ProjectAlreadyActive {
            project: active.to_string(),
        })
    }

    /// The one line an operator reads for this skip.
    pub fn line(&self) -> String {
        let remedy = &self.remedy;
        let why = match &self.reason {
            SkipReason::ToolMissing { tool } => {
                return format!(
                    "[warning] {}: {tool} is not on PATH, so this integration's install hook \
                     did not run; install {tool}, then run `{remedy}`",
                    self.integration.as_deref().unwrap_or("core")
                )
            }
            SkipReason::ProjectAlreadyActive { project } if self.effect == Effect::Activation => {
                return format!(
                    "rwv fetch: skipping auto-activate (project '{project}' already active); \
                     `{remedy}` selects it and runs its install hooks"
                )
            }
            SkipReason::ProjectAlreadyActive { project } => {
                format!("project `{project}` was already active, so this fetch activated nothing")
            }
            SkipReason::NoMaterialize => "--no-materialize".to_string(),
            SkipReason::InsideWorkweave => {
                "an authoring verb runs none inside a workweave".to_string()
            }
            SkipReason::NotPresented { project } => {
                format!("`{project}` is not the project this weave root presents")
            }
            SkipReason::Narrowed => "the update was narrowed by --role or --repo".to_string(),
        };
        let (what, does) = match self.effect {
            Effect::InstallHooks => ("the install hooks did not run", "runs them"),
            Effect::IntegrationFiles => (
                "the integration files were not regenerated",
                "regenerates them",
            ),
            Effect::WeaveRootLinks => ("the weave-root links were not refreshed", "refreshes them"),
            Effect::Activation => ("the fetched project was not activated", "activates it"),
        };
        format!("[skipped] core: {what} ({why}); `{remedy}` {does}")
    }

    /// Print [`Self::line`] on stderr, where a verb's stdout may be a machine's.
    pub fn report(&self) {
        eprintln!("{}", self.line());
    }
}
