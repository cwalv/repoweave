//! `rwv prime` lists the integrations the runner runs — no more, no fewer.
//!
//! The regression it exists for: prime read an `[integrations.<name>]` table
//! without an `enabled` key as on, while the runner read the same table as the
//! integration's own default. `[integrations.gita]` alone made prime tell an
//! agent gita was enabled while `rwv doctor` raised no gita finding. Prime also
//! walked only the tables the manifest wrote, so an integration that runs by
//! default with no table at all was never listed.
//!
//! Every builtin integration is driven through each state its table can take —
//! no table, a table without an `enabled` key, `enabled = true`, `enabled =
//! false` — in a manifest that holds only that one table, so no other
//! integration's table can stand in for the one under test. The expectation is
//! derived from the integration's own default and the operator's key, never
//! from either reader, and both readers are held to it: prime's rendered
//! section, and the runner's own enabled set.
//!
//! **Residue**: the runner side is the set `enabled_integrations` yields, which
//! every runner driver iterates; an integration run by a path that does not go
//! through it is invisible here.

use std::path::Path;

use repoweave::integration::Integration;
use repoweave::integration_runner::enabled_integrations;
use repoweave::integrations::builtin_integrations;
use repoweave::manifest::{IntegrationConfig, Manifest};
use repoweave::prime::render_context;
use repoweave::workspace::WorkspaceContext;

mod common;

#[derive(Clone, Copy, Debug)]
enum TableState {
    NoTable,
    TableWithoutKey,
    EnabledTrue,
    EnabledFalse,
}

const TABLE_STATES: [TableState; 4] = [
    TableState::NoTable,
    TableState::TableWithoutKey,
    TableState::EnabledTrue,
    TableState::EnabledFalse,
];

impl TableState {
    fn table(self, integration: &str) -> String {
        match self {
            TableState::NoTable => String::new(),
            TableState::TableWithoutKey => format!("\n[integrations.{integration}]\n"),
            TableState::EnabledTrue => {
                format!("\n[integrations.{integration}]\nenabled = true\n")
            }
            TableState::EnabledFalse => {
                format!("\n[integrations.{integration}]\nenabled = false\n")
            }
        }
    }

    fn runs(self, integration: &dyn Integration) -> bool {
        match self {
            TableState::NoTable | TableState::TableWithoutKey => integration.default_enabled(),
            TableState::EnabledTrue => true,
            TableState::EnabledFalse => false,
        }
    }
}

const REPOSITORIES: &str = "[repositories.\"github/acme/lib\"]\n\
    type = \"git\"\n\
    url = \"https://github.com/acme/lib.git\"\n\
    version = \"main\"\n\
    role = \"owned\"\n";

fn weave(root: &Path) {
    std::fs::create_dir_all(root.join("projects/app")).unwrap();
    std::fs::write(root.join(".rwv-active"), "app\n").unwrap();
}

fn write_manifest(root: &Path, text: &str) {
    std::fs::write(root.join("projects/app/rwv.toml"), text).unwrap();
}

fn primed_integrations(root: &Path) -> Vec<String> {
    let ctx = WorkspaceContext::resolve_invocation(root, None).unwrap();
    let output = render_context(&ctx);
    let Some((_, section)) = output.split_once("\n## Integrations\n") else {
        return Vec::new();
    };
    section
        .split("\n## ")
        .next()
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .map(str::to_string)
        .collect()
}

fn run_integrations(manifest_text: &str) -> Vec<String> {
    let manifest = Manifest::from_toml_str(manifest_text).unwrap();
    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
    let default_config = IntegrationConfig::default();
    enabled_integrations(&integrations, &manifest, &default_config)
        .map(|(integration, _)| integration.name().to_string())
        .collect()
}

#[test]
fn prime_lists_an_integration_exactly_when_the_runner_runs_it() {
    let builtin = builtin_integrations();
    assert!(
        builtin.iter().any(|i| i.default_enabled()) && builtin.iter().any(|i| !i.default_enabled()),
        "the matrix must hold integrations that default on and that default off, \
         or a reader that ignores the default passes"
    );

    let tmp = common::tempdir().unwrap();
    weave(tmp.path());

    let mut disagreements = Vec::new();
    for integration in &builtin {
        for state in TABLE_STATES {
            let text = format!("{REPOSITORIES}{}", state.table(integration.name()));
            write_manifest(tmp.path(), &text);

            let name = integration.name();
            let expected = state.runs(integration.as_ref());
            let runner_runs = run_integrations(&text).iter().any(|n| n == name);
            let prime_lists = primed_integrations(tmp.path()).iter().any(|n| n == name);

            if runner_runs != expected {
                disagreements.push(format!(
                    "{name} with {state:?}: the runner runs it = {runner_runs}, expected {expected}"
                ));
            }
            if prime_lists != expected {
                disagreements.push(format!(
                    "{name} with {state:?}: prime lists it = {prime_lists}, expected {expected}"
                ));
            }
        }
    }

    assert!(
        disagreements.is_empty(),
        "prime and the runner disagree about which integrations run:\n{}",
        disagreements.join("\n")
    );
}
