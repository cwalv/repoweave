pub mod cargo_workspace;
pub mod gita;
pub mod go_work;
pub mod npm_workspaces;
pub mod pnpm_workspaces;
pub mod uv_workspace;
pub mod vscode_workspace;

use crate::integration::Integration;

pub use cargo_workspace::CargoWorkspace;
pub use gita::Gita;
pub use go_work::GoWork;
pub use npm_workspaces::NpmWorkspaces;
pub use pnpm_workspaces::PnpmWorkspaces;
pub use uv_workspace::UvWorkspace;
pub use vscode_workspace::VscodeWorkspace;

/// Returns all built-in integrations.
pub fn builtin_integrations() -> Vec<Box<dyn Integration>> {
    vec![
        Box::new(NpmWorkspaces),
        Box::new(PnpmWorkspaces),
        Box::new(GoWork),
        Box::new(UvWorkspace),
        Box::new(CargoWorkspace),
        Box::new(Gita),
        Box::new(VscodeWorkspace),
    ]
}
