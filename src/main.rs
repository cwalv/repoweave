use repoweave::add_remove;
use repoweave::activate;
use repoweave::check;
use repoweave::fetch;
use repoweave::init;
use repoweave::lock;
use repoweave::manifest;
use repoweave::prime;
use repoweave::setup;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use repoweave::manifest::WorkweaveName;
use repoweave::workspace::WorkspaceContext;

#[derive(Parser)]
#[command(name = "rwv", version = option_env!("RWV_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")), about = "A cross-repo workspace manager")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Create, delete, or list workweaves
    Workweave {
        /// Project name (not required when --claude-hook is set)
        #[arg(required_unless_present = "claude_hook")]
        project: Option<String>,
        /// Optional workweave name
        name: Option<String>,
        /// Delete the named workweave
        #[arg(long)]
        delete: bool,
        /// List existing workweaves
        #[arg(long)]
        list: bool,
        /// Sync workweave with current manifest
        #[arg(long)]
        sync: bool,
        /// Hook mode: print only the workweave path to stdout (for Claude Code WorktreeCreate hook)
        #[arg(long)]
        hook_mode: bool,
        /// Claude Code hook mode: read JSON from stdin, handle create/remove automatically
        #[arg(long, conflicts_with_all = ["delete", "list", "sync", "hook_mode"])]
        claude_hook: bool,
    },
    /// Clone a project and its repos
    Fetch {
        /// Source to fetch from
        source: String,
        /// Check out exact revisions from rwv.lock (reproducible builds)
        #[arg(long, conflicts_with = "frozen")]
        locked: bool,
        /// Like --locked, but error if lock file is missing or stale (CI mode)
        #[arg(long, conflicts_with = "locked")]
        frozen: bool,
        /// Bootstrap into a non-empty directory that is not a workspace
        #[arg(long)]
        force: bool,
    },
    /// Add a repo to the active project
    Add {
        /// Repository URL or path (with --new)
        url: String,
        /// Role for the repo (primary, fork, dependency, reference)
        #[arg(long, default_value = "dependency")]
        role: String,
        /// Create a new repo (git init) at the canonical path instead of cloning
        #[arg(long)]
        new: bool,
    },
    /// Remove a repo from the active project
    Remove {
        /// Path of the repo to remove
        path: String,
        /// Delete the clone directory
        #[arg(long)]
        delete: bool,
        /// Skip confirmation when deleting
        #[arg(long)]
        force: bool,
    },
    /// Snapshot repo versions
    Lock {
        /// Allow locking repos with uncommitted changes
        #[arg(long)]
        dirty: bool,
    },
    /// Convention enforcement
    Check,
    /// Print workspace root path
    Resolve,
    /// Initialize a new project
    Init {
        /// Project name (or URL / shorthand when --adopt is used)
        project: String,
        /// Provider in registry/owner format (e.g., github/myorg)
        #[arg(long, conflicts_with = "adopt")]
        provider: Option<String>,
        /// Adopt an existing repo: clone from URL or shorthand instead of git init
        #[arg(long)]
        adopt: bool,
    },
    /// Activate a project (generate ecosystem files, create symlinks)
    Activate {
        /// Project name
        project: String,
    },
    /// Print structured workspace context for agent system prompts
    Prime,
    /// Generate workspace-level configuration files
    Setup {
        #[command(subcommand)]
        action: SetupAction,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum SetupAction {
    /// Generate AGENTS.md at the workspace root
    AgentsMd,
    /// Register rwv prime as a Claude Code hook (SessionStart + PreCompact)
    Claude {
        /// Remove all rwv hooks from Claude Code settings
        #[arg(long)]
        uninstall: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            let cwd = std::env::current_dir()?;
            let ctx = WorkspaceContext::resolve(&cwd, None)?;
            println!("{}", ctx.display());
        }
        Some(Commands::Workweave { project, name, delete, list, sync, hook_mode, claude_hook }) => {
            if claude_hook {
                repoweave::workweave::handle_claude_hook()?;
            } else {
                let project = project.expect("project is required unless --claude-hook is set");
                let cwd = std::env::current_dir()?;
                let ctx = WorkspaceContext::resolve(&cwd, None)?;
                let ws_root = &ctx.root;

                if list {
                    let names = repoweave::workweave::list_workweaves(ws_root)?;
                    for n in &names {
                        println!("{}", n);
                    }
                } else if delete {
                    let name = name.ok_or_else(|| anyhow::anyhow!("--delete requires a workweave name"))?;
                    repoweave::workweave::delete_workweave(ws_root, &project, &WorkweaveName::new(name))?;
                } else if sync {
                    let name = name.ok_or_else(|| anyhow::anyhow!("--sync requires a workweave name"))?;
                    repoweave::workweave::sync_workweave(ws_root, &project, &WorkweaveName::new(name))?;
                } else {
                    let name = name.ok_or_else(|| anyhow::anyhow!("workweave create requires a name argument"))?;
                    let workweave_path = repoweave::workweave::create_workweave(ws_root, &project, &WorkweaveName::new(name))?;
                    if hook_mode {
                        println!("{}", workweave_path.display());
                    }
                }
            }
        }
        Some(Commands::Fetch { source, locked, frozen, force }) => {
            let cwd = std::env::current_dir()?;
            repoweave::workspace::require_workspace_or_empty(&cwd, force)?;
            let mode = if frozen {
                fetch::FetchMode::Frozen
            } else if locked {
                fetch::FetchMode::Locked
            } else {
                fetch::FetchMode::Default
            };
            fetch::run_fetch(&source, &cwd, mode)?;
        }
        Some(Commands::Add { url, role, new }) => {
            let cwd = std::env::current_dir()?;
            if new {
                add_remove::run_add_new(&url, &cwd)?;
            } else {
                let parsed_role: manifest::Role = serde_yaml::from_str(&role)
                    .map_err(|_| anyhow::anyhow!("Invalid role '{}'. Valid roles: primary, fork, dependency, reference", role))?;
                add_remove::run_add(&url, parsed_role, &cwd)?;
            }
        }
        Some(Commands::Remove { path, delete, force }) => {
            let cwd = std::env::current_dir()?;
            add_remove::run_remove(&path, delete, force, &cwd)?;
        }
        Some(Commands::Lock { dirty }) => {
            let cwd = std::env::current_dir()?;
            lock::lock(&cwd, dirty)?;
        }
        Some(Commands::Check) => {
            let cwd = std::env::current_dir()?;
            let has_errors = check::run_check(&cwd)?;
            if has_errors {
                std::process::exit(1);
            }
        }
        Some(Commands::Resolve) => {
            let cwd = std::env::current_dir()?;
            let ctx = WorkspaceContext::resolve(&cwd, None)?;
            println!("{}", ctx.resolve_path().display());
        }
        Some(Commands::Init { project, provider, adopt }) => {
            let cwd = std::env::current_dir()?;
            if adopt {
                init::init_adopt(&project, &cwd)?;
            } else {
                init::init(&project, provider.as_deref(), &cwd)?;
            }
        }
        Some(Commands::Activate { project }) => {
            let cwd = std::env::current_dir()?;
            activate::activate(&project, &cwd)?;
        }
        Some(Commands::Prime) => {
            let cwd = std::env::current_dir()?;
            prime::prime(&cwd)?;
        }
        Some(Commands::Setup { action }) => {
            let cwd = std::env::current_dir()?;
            match action {
                SetupAction::AgentsMd => setup::agents_md(&cwd)?,
                SetupAction::Claude { uninstall } => {
                    if uninstall {
                        setup::claude_uninstall()?;
                    } else {
                        setup::claude()?;
                    }
                }
            }
        }
        Some(Commands::Completions { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "rwv", &mut std::io::stdout());
        }
    }

    Ok(())
}
