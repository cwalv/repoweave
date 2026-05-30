use repoweave::activate;
use repoweave::add_remove;
use repoweave::check;
use repoweave::explain;
use repoweave::fetch;
use repoweave::init;
use repoweave::lock;
use repoweave::manifest;
use repoweave::prime;
use repoweave::push;
use repoweave::setup;
use repoweave::status;
use repoweave::sync;
use repoweave::sync::{SyncSource, SyncStrategy};
use repoweave::update;

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
    /// Clone a project and align repos to rwv.lock (no network bump). Use `rwv update` to advance to branch HEAD.
    Fetch {
        /// Source to fetch from
        source: String,
        /// Error if the lock file is missing or stale (CI mode)
        #[arg(long)]
        frozen: bool,
        /// Bootstrap into a non-empty directory that is not a workspace
        #[arg(long)]
        force: bool,
        /// Skip cloning/fetching repositories with role: reference
        #[arg(long)]
        no_reference: bool,
        /// Limit the operation to repos with this role. Repeat to union multiple roles. Combined as a union with --repo.
        #[arg(long = "role")]
        roles: Vec<String>,
        /// Limit the operation to repos matching this selector. Bare strings match exactly; `re:<pat>` matches as regex; `glob:<pat>` matches as glob. Repeat for union. Combined as a union with --role.
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// Number of parallel per-repo workers. Default: min(nproc, 8). `-j 1` is explicit serial.
        #[arg(short = 'j', long = "jobs")]
        jobs: Option<usize>,
    },
    /// Advance each repo to its branch HEAD and re-snapshot the lock (network bump; analogous to `cargo update` / `npm update`). Use `rwv fetch` for the read-only counterpart that aligns clones to the existing lock.
    Update {
        /// Allow update with uncommitted changes in repos when relocking
        #[arg(long)]
        dirty: bool,
        /// Commit rwv.lock after writing it
        #[arg(long)]
        commit: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Limit the operation to repos with this role. Repeat to union multiple roles. Combined as a union with --repo.
        #[arg(long = "role")]
        roles: Vec<String>,
        /// Limit the operation to repos matching this selector. Bare strings match exactly; `re:<pat>` matches as regex; `glob:<pat>` matches as glob. Repeat for union. Combined as a union with --role.
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// Number of parallel per-repo workers. Default: min(nproc, 8). `-j 1` is explicit serial.
        #[arg(short = 'j', long = "jobs")]
        jobs: Option<usize>,
    },
    /// Push manifest repos and then the project repo, in that order. Refuses from a workweave. Manifest pushes are attempt-all-and-collect; project repo is gated on every manifest repo succeeding.
    Push {
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Print the push plan without executing
        #[arg(long)]
        dry_run: bool,
        /// Force-push every repo in the operation (manifest repos and the project repo). Default deny.
        #[arg(long)]
        force: bool,
        /// Limit the push to repos with this role. Repeat to union multiple roles. Combined as a union with --repo. Lock-precondition still runs against the full manifest.
        #[arg(long = "role")]
        roles: Vec<String>,
        /// Limit the push to repos matching this selector. Bare strings match exactly; `re:<pat>` matches as regex; `glob:<pat>` matches as glob. Repeat for union. Combined as a union with --role. Lock-precondition still runs against the full manifest.
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// Number of parallel per-repo workers for manifest-repo pushes. Default: min(nproc, 8). `-j 1` is explicit serial. Project-repo push always runs serially as the last step.
        #[arg(short = 'j', long = "jobs")]
        jobs: Option<usize>,
    },
    /// Add a repo to the active project
    Add {
        /// Repository URL or path (with --new)
        url: String,
        /// Role for the repo
        #[arg(long, default_value = "owned", value_enum)]
        role: manifest::Role,
        /// Create a new repo (git init) at the canonical path instead of cloning
        #[arg(long)]
        new: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
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
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Snapshot repo versions (pure git SHA snapshot — no integration hooks fire). Run `rwv activate` after lock changes the workspace membership to refresh node_modules / .venv / etc.
    Lock {
        /// Allow locking repos with uncommitted changes
        #[arg(long)]
        dirty: bool,
        /// Commit rwv.lock after writing it
        #[arg(long)]
        commit: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Convention enforcement and lock-freshness checking
    Doctor {
        /// Zero exit iff every repo's tip matches its rwv.lock entry (scriptable precondition for rwv sync)
        #[arg(long)]
        locked: bool,
        /// Auto-fix safely-fixable index drift and working-tree drift (see `rwv doctor` description for classification rules)
        #[arg(long, conflicts_with = "locked")]
        fix: bool,
        /// Emit violations as JSON (array-of-records with stable per-variant `kind`). See `rwv explain doctor`.
        #[arg(long, conflicts_with_all = ["locked", "fix"])]
        json: bool,
        /// Scan all projects and run weave-wide checks (orphan detection, cross-project stale locks, etc.).
        /// By default only the active project is checked.
        #[arg(long)]
        all: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Show per-repo state of the CWD workspace
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Align CWD workspace with another workspace's committed rwv.lock
    Sync {
        /// Source workspace: `primary`, a bare workweave name, or a path
        /// (absolute, or relative to the primary workspace). Required unless
        /// `--continue` is passed (source is then read from the in-progress
        /// op-state file). If you meant to land work upward, use `rwv sync-to`.
        #[arg(required_unless_present = "do_continue")]
        source: Option<SyncSource>,
        /// Sync strategy: ff (default), rebase, or merge
        #[arg(long, default_value = "ff", value_enum, conflicts_with = "do_continue")]
        strategy: SyncStrategy,
        /// Bypass the lock-freshness precondition
        #[arg(long, conflicts_with = "do_continue")]
        force: bool,
        /// Emit per-repo outcomes as JSON (array-of-records with stable per-variant `kind`). See `rwv explain sync`.
        #[arg(long, conflicts_with = "do_continue")]
        json: bool,
        /// Run up to N per-repo manifest syncs in parallel. Under `-j > 1` with `--json`,
        /// output switches to NDJSON (one JSON record per repo, streamed as repos finish).
        #[arg(short = 'j', long = "jobs", conflicts_with = "do_continue")]
        jobs: Option<usize>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Resume a sync that was interrupted mid-op (e.g. after resolving a conflict).
        /// All parameters (source, strategy, force, etc.) are read from the in-progress
        /// op-state file. No other flags may be passed alongside `--continue` (except
        /// `--project`). To change parameters mid-op, run `rwv abort` and re-invoke.
        #[arg(long = "continue")]
        do_continue: bool,
    },
    /// Advance target workspace to CWD's tip (3-step orchestration: rebase CWD against target,
    /// auto-relock, then fast-forward target to CWD's converged tip). CWD's unique commits land
    /// on top of target's prior history; target absorbs CWD's state with CWD as the newest
    /// contribution.
    SyncTo {
        /// Target workspace: `primary`, a bare workweave name, or a path
        /// (absolute, or relative to the primary workspace root). Omit to target
        /// the recorded parent from `.rwv-workweave`; errors if not in a workweave.
        /// Must not be passed alongside `--continue` (target is then read from op-state).
        #[arg(conflicts_with = "do_continue")]
        target: Option<SyncSource>,
        /// Sync strategy for step 1 (rebase CWD against target). Default: rebase.
        /// ff means CWD must already be strictly ahead of target (no-op step 1).
        /// rebase replays CWD's unique commits onto target's tip.
        /// merge merges target into CWD with an auto-generated commit.
        /// Step 3 (FF-advance target) is always ff regardless of this flag.
        #[arg(
            long,
            default_value = "rebase",
            value_enum,
            conflicts_with = "do_continue"
        )]
        strategy: SyncStrategy,
        /// Bypass the lock-freshness precondition
        #[arg(long, conflicts_with = "do_continue")]
        force: bool,
        /// Land work then delete the workweave on success (requires clean worktree and
        /// manifest repos converged with target after sync-to completes).
        #[arg(long, conflicts_with = "do_continue")]
        retire: bool,
        /// Emit per-repo outcomes as JSON (array-of-records with stable per-variant `kind`). See `rwv explain sync-to`.
        #[arg(long, conflicts_with = "do_continue")]
        json: bool,
        /// Run up to N per-repo manifest syncs in parallel. Under `-j > 1` with `--json`,
        /// output switches to NDJSON (one JSON record per repo, streamed as repos finish).
        #[arg(short = 'j', long = "jobs", conflicts_with = "do_continue")]
        jobs: Option<usize>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Resume a sync-to that was interrupted mid-op (e.g. after resolving a conflict).
        /// All parameters (target, strategy, retire, force, etc.) are read from the
        /// in-progress op-state file. No other flags may be passed alongside `--continue`
        /// (except `--project`). To change parameters mid-op, run `rwv abort` and re-invoke.
        #[arg(long = "continue")]
        do_continue: bool,
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
    /// Activate a project (generate ecosystem files, create symlinks, then run integration install hooks like `npm install` / `uv sync` / `cargo generate-lockfile`)
    Activate {
        /// Project name
        project: String,
        /// Skip integration install hooks (e.g., `npm install`, `uv sync`) for fast context-switch
        #[arg(long)]
        no_install: bool,
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
    /// Agent-oriented reflection: per-verb markdown bundle (purpose, invocation, output, JSON Schema)
    Explain {
        /// Verb to explain. Omit to list explainable verbs.
        command: Option<String>,
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
        /// Allow creation even when the source project directory has
        /// uncommitted changes. The dirty state is captured into the new
        /// workweave's project worktree. Without this flag, `create` refuses
        /// and names the dirty files so you can commit, stash, or opt in
        /// explicitly.
        #[arg(long)]
        capture_dirty: bool,
    },
    /// Delete a workweave
    Delete {
        /// Workweave name
        name: String,
        /// Delete even if the workweave has uncommitted changes (matches
        /// `git branch -D`). Without this flag, deletion refuses if the
        /// project worktree or any manifest-repo worktree is dirty.
        #[arg(long)]
        force: bool,
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
    // Early-dispatch did-you-mean hints for removed/relocated flags.
    // These run before clap's full parse so we can produce a friendly error
    // instead of clap's generic "unexpected argument" message.
    {
        let raw_args: Vec<String> = std::env::args().collect();
        // Detect: rwv sync --retire (--retire has moved to sync-to)
        if raw_args.get(1).map(|s| s.as_str()) == Some("sync")
            && raw_args.iter().any(|a| a == "--retire")
        {
            eprintln!(
                "error: `--retire` has moved to `rwv sync-to`; use `rwv sync-to --retire` instead"
            );
            std::process::exit(2);
        }
    }

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
                let project = repoweave::manifest::ProjectName::new(project);
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
                    Some(WorkweaveAction::Delete { name, force }) => {
                        repoweave::workweave::delete_workweave(
                            primary_root,
                            &project,
                            &WorkweaveName::new(name),
                            force,
                        )?;
                    }
                    Some(WorkweaveAction::Create {
                        name,
                        force,
                        from,
                        capture_dirty,
                    }) => {
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
                            capture_dirty,
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
            frozen,
            force,
            no_reference,
            roles,
            repos,
            jobs,
        }) => {
            let cwd = std::env::current_dir()?;
            repoweave::workspace::require_workspace_or_empty(&cwd, force)?;
            let mode = if frozen {
                fetch::FetchMode::Frozen
            } else {
                fetch::FetchMode::Default
            };
            let filter = repoweave::selector::RepoFilter::parse(&roles, &repos)?;
            let jobs = repoweave::parallel::resolve_jobs(jobs);
            fetch::run_fetch(&source, &cwd, mode, no_reference, &filter, jobs)?;
        }
        Some(Commands::Add {
            url,
            role,
            new,
            project,
        }) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            if new {
                add_remove::run_add_new(&url, &cwd, project_override)?;
            } else {
                add_remove::run_add(&url, role, &cwd, project_override)?;
            }
        }
        Some(Commands::Remove {
            path,
            delete,
            force,
            project,
        }) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            add_remove::run_remove(&path, delete, force, &cwd, project_override)?;
        }
        Some(Commands::Lock {
            dirty,
            commit,
            project,
        }) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            lock::lock(&cwd, dirty, commit, project_override)?;
        }
        Some(Commands::Update {
            dirty,
            commit,
            project,
            roles,
            repos,
            jobs,
        }) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let filter = repoweave::selector::RepoFilter::parse(&roles, &repos)?;
            let jobs = repoweave::parallel::resolve_jobs(jobs);
            update::run_update(&cwd, dirty, commit, project_override, &filter, jobs)?;
        }
        Some(Commands::Push {
            project,
            dry_run,
            force,
            roles,
            repos,
            jobs,
        }) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let filter = repoweave::selector::RepoFilter::parse(&roles, &repos)?;
            let jobs = repoweave::parallel::resolve_jobs(jobs);
            push::run_push(&cwd, project_override, dry_run, force, &filter, jobs)?;
        }
        Some(Commands::Doctor {
            locked,
            fix,
            json,
            all,
            project,
        }) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            if locked {
                let has_drift = check::run_check_locked(&cwd, project_override)?;
                if has_drift {
                    std::process::exit(1);
                }
            } else if json {
                let has_errors = check::run_check_json(&cwd, project_override, all)?;
                if has_errors {
                    std::process::exit(1);
                }
            } else {
                let has_errors = check::run_check(&cwd, fix, project_override, all)?;
                if has_errors {
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Status { json, project }) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            status::run_status(&cwd, json, project_override)?;
        }
        Some(Commands::Sync {
            source,
            strategy,
            force,
            json,
            jobs,
            project,
            do_continue,
        }) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            // sync's default is serial (jobs=1). This differs from fetch/update
            // (which auto-resolve to min(nproc, 8)) because sync's `--json`
            // contract pins envelope output under `-j 1` and NDJSON under
            // `-j > 1`; defaulting to auto would silently switch envelope ->
            // NDJSON on multi-core hosts. No `-j` or `-j 1` emits the
            // pretty envelope.
            let jobs = match jobs {
                Some(n) => repoweave::parallel::resolve_jobs(Some(n)),
                None => 1,
            };
            // When --continue is set, source is None (read from op-state).
            // When --continue is absent, source is Some (required by clap).
            let source_ref = source.as_ref();
            if json {
                sync::run_sync_json(
                    &cwd,
                    source_ref,
                    strategy,
                    force,
                    false,
                    project_override,
                    jobs,
                    do_continue,
                )?;
            } else {
                sync::run_sync(
                    &cwd,
                    source_ref,
                    strategy,
                    force,
                    false,
                    project_override,
                    jobs,
                    do_continue,
                )?;
            }
        }
        Some(Commands::SyncTo {
            target,
            strategy,
            force,
            retire,
            json,
            jobs,
            project,
            do_continue,
        }) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let jobs = match jobs {
                Some(n) => repoweave::parallel::resolve_jobs(Some(n)),
                None => 1,
            };
            // Resolve target: if None and not --continue, read .rwv-workweave marker's
            // parent field. If --continue, target is read from op-state inside
            // run_sync_to (target is always None when --continue due to clap conflict).
            let resolved_target = if do_continue {
                // --continue: target comes from op-state; pass None sentinel.
                None
            } else {
                Some(match target {
                    Some(t) => t,
                    None => {
                        // Bare `rwv sync-to` — must be inside a workweave.
                        let ctx = repoweave::workspace::WorkspaceContext::resolve(&cwd, None)?;
                        match &ctx.location {
                            repoweave::workspace::WorkspaceLocation::Workweave { dir, .. } => {
                                let marker = repoweave::workspace::WorkweaveMarker::read(dir)?
                                    .ok_or_else(|| {
                                        anyhow::anyhow!(
                                            "bare `rwv sync-to` requires a \
                                                 `.rwv-workweave` marker in the workweave; \
                                                 found none at {}",
                                            dir.display()
                                        )
                                    })?;
                                sync::SyncSource::Path(marker.parent)
                            }
                            repoweave::workspace::WorkspaceLocation::Weave { .. } => {
                                anyhow::bail!(
                                    "bare `rwv sync-to` targets the workweave's recorded \
                                     parent, but CWD ({}) is in the primary weave, not a \
                                     workweave. Provide a target explicitly.",
                                    cwd.display()
                                );
                            }
                        }
                    }
                })
            };
            if json {
                sync::run_sync_to_json(
                    &cwd,
                    resolved_target.as_ref(),
                    strategy,
                    force,
                    retire,
                    project_override,
                    jobs,
                    do_continue,
                )?;
            } else {
                sync::run_sync_to(
                    &cwd,
                    resolved_target.as_ref(),
                    strategy,
                    force,
                    retire,
                    project_override,
                    jobs,
                    do_continue,
                )?;
            }
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
        Some(Commands::Activate {
            project,
            no_install,
        }) => {
            let cwd = std::env::current_dir()?;
            activate::activate_with_options(
                &project,
                &cwd,
                activate::ActivateOptions { no_install },
            )?;
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
        Some(Commands::Explain { command }) => {
            explain::explain(command.as_deref())?;
        }
    }

    Ok(())
}
