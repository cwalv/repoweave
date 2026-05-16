use repoweave::activate;
use repoweave::add_remove;
use repoweave::check;
use repoweave::fetch;
use repoweave::init;
use repoweave::lock;
use repoweave::manifest;
use repoweave::prime;
use repoweave::setup;
use repoweave::status;
use repoweave::sync;
use repoweave::sync::{SyncSource, SyncStrategy};

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
    /// Clone a project and its repos (also re-pins already-cloned repos with --locked/--frozen)
    Fetch {
        /// Source to fetch from
        source: String,
        /// Check out each repo at the SHA recorded in rwv.lock — works for existing repos too;
        /// effectively a per-repo `git checkout` to bring tips back in line with the lock
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
        /// Commit rwv.lock after writing it
        #[arg(long)]
        commit: bool,
    },
    /// Convention enforcement and lock-freshness checking
    #[command(alias = "check")]
    Doctor {
        /// Zero exit iff every repo's tip matches its rwv.lock entry (scriptable precondition for rwv sync)
        #[arg(long)]
        locked: bool,
        /// Auto-fix safely-fixable index drift and working-tree drift (see `rwv doctor` description for classification rules)
        #[arg(long, conflicts_with = "locked")]
        fix: bool,
    },
    /// Show per-repo state of the CWD workspace
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Align CWD workspace with another workspace's committed rwv.lock
    Sync {
        /// Source workspace: `primary`, a bare workweave name, or a path
        /// (absolute, or relative to the primary workspace)
        source: SyncSource,
        /// Sync strategy: ff (default), rebase, or merge
        #[arg(long, default_value = "ff", value_enum)]
        strategy: SyncStrategy,
        /// Bypass the lock-freshness precondition
        #[arg(long)]
        force: bool,
    },
    /// Restore CWD workspace to its pre-sync state using savepoint refs
    Abort,
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
    Prime {
        /// Always emit output, even when CWD is not inside a weave or workweave
        #[arg(long)]
        no_suppress: bool,
    },
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
        /// Destroy an existing workweave at this path before recreating.
        ///
        /// Without this flag, re-invoking `create` against an existing
        /// workweave preserves non-git state in place (the idempotent
        /// path). Use `--force` for explicit rebuild scenarios.
        #[arg(long)]
        force: bool,
        /// Workspace to fork the new workweave from. Accepts `primary`, an
        /// absolute or relative path, or is omitted to fork from CWD's
        /// active workspace (the workweave when invoked from inside one,
        /// otherwise primary). Relative paths resolve against primary, so
        /// peer workweaves can be referenced by directory name (e.g.
        /// `--from .workweaves/foundations--fo-city`).
        #[arg(long)]
        from: Option<String>,
    },
    /// Delete a workweave
    Delete {
        /// Workweave name
        name: String,
    },
    /// List existing workweaves
    List,
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
                let primary_root = ctx.primary_path();

                match action {
                    Some(WorkweaveAction::List) | None => {
                        let names = repoweave::workweave::list_workweaves(primary_root, &project)?;
                        for n in &names {
                            println!("{}", n);
                        }
                    }
                    Some(WorkweaveAction::Delete { name }) => {
                        repoweave::workweave::delete_workweave(
                            primary_root,
                            &project,
                            &WorkweaveName::new(name),
                        )?;
                    }
                    Some(WorkweaveAction::Create { name, force, from }) => {
                        let source_root = match from.as_deref() {
                            None => ctx.active_path().to_path_buf(),
                            Some("primary") => primary_root.to_path_buf(),
                            Some(s) => {
                                let p = std::path::PathBuf::from(s);
                                if p.is_absolute() {
                                    p
                                } else {
                                    primary_root.join(s)
                                }
                            }
                        };
                        let workweave_path = repoweave::workweave::create_workweave(
                            primary_root,
                            &source_root,
                            &project,
                            &WorkweaveName::new(name),
                            force,
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
        Some(Commands::Lock { dirty, commit }) => {
            let cwd = std::env::current_dir()?;
            lock::lock(&cwd, dirty, commit)?;
        }
        Some(Commands::Doctor { locked, fix }) => {
            let cwd = std::env::current_dir()?;
            if locked {
                let has_drift = check::run_check_locked(&cwd)?;
                if has_drift {
                    std::process::exit(1);
                }
            } else {
                let has_errors = check::run_check(&cwd, fix)?;
                if has_errors {
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Status { json }) => {
            let cwd = std::env::current_dir()?;
            status::run_status(&cwd, json)?;
        }
        Some(Commands::Sync {
            source,
            strategy,
            force,
        }) => {
            let cwd = std::env::current_dir()?;
            sync::run_sync(&cwd, &source, strategy, force)?;
        }
        Some(Commands::Abort) => {
            let cwd = std::env::current_dir()?;
            sync::run_abort(&cwd)?;
        }
        Some(Commands::Resolve) => {
            let cwd = std::env::current_dir()?;
            let ctx = WorkspaceContext::resolve(&cwd, None)?;
            println!("{}", ctx.active_path().display());
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
        Some(Commands::Prime { no_suppress }) => {
            let cwd = std::env::current_dir()?;
            prime::prime(&cwd, no_suppress)?;
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
