use repoweave::activate;
use repoweave::add_remove;
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
        /// Hook mode: print only the workweave path to stdout (for Claude Code WorktreeCreate hook)
        #[arg(long)]
        hook_mode: bool,
        /// Claude Code hook mode: read JSON from stdin, handle create/remove automatically
        #[arg(long, conflicts_with = "hook_mode")]
        claude_hook: bool,
        #[command(subcommand)]
        action: Option<WorkweaveAction>,
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
        /// Role for the repo
        #[arg(long, default_value = "dependency", value_enum)]
        role: manifest::Role,
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
enum WorkweaveAction {
    /// Create a new workweave
    Create {
        /// Workweave name
        name: String,
    },
    /// Delete a workweave
    Delete {
        /// Workweave name
        name: String,
    },
    /// List existing workweaves
    List,
    /// Sync workweave with current manifest
    Sync {
        /// Workweave name
        name: String,
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
        Some(Commands::Workweave {
            project,
            hook_mode,
            claude_hook,
            action,
        }) => {
            if claude_hook {
                repoweave::workweave::handle_claude_hook()?;
            } else {
                let project = project.expect("project is required unless --claude-hook is set");
                let cwd = std::env::current_dir()?;
                let ctx = WorkspaceContext::resolve(&cwd, None)?;
                let ws_root = &ctx.root;

                match action {
                    Some(WorkweaveAction::List) | None => {
                        let names = repoweave::workweave::list_workweaves(ws_root)?;
                        for n in &names {
                            println!("{}", n);
                        }
                    }
                    Some(WorkweaveAction::Delete { name }) => {
                        repoweave::workweave::delete_workweave(
                            ws_root,
                            &project,
                            &WorkweaveName::new(name),
                        )?;
                    }
                    Some(WorkweaveAction::Sync { name }) => {
                        repoweave::workweave::sync_workweave(
                            ws_root,
                            &project,
                            &WorkweaveName::new(name),
                        )?;
                    }
                    Some(WorkweaveAction::Create { name }) => {
                        let workweave_path = repoweave::workweave::create_workweave(
                            ws_root,
                            &project,
                            &WorkweaveName::new(name),
                        )?;
                        if hook_mode {
                            println!("{}", workweave_path.display());
                        }
                    }
                }
            }
        }
        Some(Commands::Fetch {
            source,
            locked,
            frozen,
            force,
        }) => {
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
                add_remove::run_add(&url, role, &cwd)?;
            }
        }
        Some(Commands::Remove {
            path,
            delete,
            force,
        }) => {
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
        Some(Commands::Init {
            project,
            provider,
            adopt,
        }) => {
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
