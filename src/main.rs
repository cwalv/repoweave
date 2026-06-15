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
    #[command(flatten, next_help_heading = "Workspace context")]
    WorkspaceContext(WorkspaceContextCmd),

    #[command(flatten, next_help_heading = "Setup & lifecycle")]
    SetupLifecycle(SetupLifecycleCmd),

    #[command(flatten, next_help_heading = "Workweaves")]
    Workweaves(WorkweavesCmd),

    #[command(flatten, next_help_heading = "Locking & verification")]
    LockingVerification(LockingVerificationCmd),

    #[command(flatten, next_help_heading = "Multi-workspace ops")]
    MultiWorkspaceOps(MultiWorkspaceOpsCmd),

    #[command(flatten, next_help_heading = "Network & publishing")]
    NetworkPublishing(NetworkPublishingCmd),

    #[command(flatten, next_help_heading = "Tooling")]
    Tooling(ToolingCmd),
}

// ── Workspace context ─────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum WorkspaceContextCmd {
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
    /// Print workspace root path
    Resolve,
}

// ── Setup & lifecycle ─────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum SetupLifecycleCmd {
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
        /// Emit per-repo outcomes as JSON. Under `-j 1` (or no `-j`), emits a
        /// `{ "$schema": ..., "outcomes": [...] }` envelope. Under `-j N` with
        /// `N > 1`, streams NDJSON (one self-describing record per repo as workers
        /// finish). See `rwv explain fetch`.
        #[arg(long)]
        json: bool,
    },
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
}

// ── Workweaves ────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum WorkweavesCmd {
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
}

// ── Locking & verification ────────────────────────────────────────────────────

#[derive(Subcommand)]
enum LockingVerificationCmd {
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
    /// Show per-repo state of the CWD workspace
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
}

// ── Multi-workspace ops ───────────────────────────────────────────────────────

#[derive(Subcommand)]
enum MultiWorkspaceOpsCmd {
    /// Restore CWD workspace to its pre-sync state using savepoint refs
    Abort,
    /// Bring another workspace's committed state into this one (pull/align; use `rwv sync-to` to land work upward)
    ///
    /// Absorbs `<source>`'s committed lock and advances each manifest repo to the locked SHA.
    /// Default strategy is `ff` (fast-forward only); pass `--strategy=rebase` when histories
    /// have diverged and ff would bail.
    ///
    /// Examples:
    ///
    ///   Bring primary's HEAD into the workweave you are iterating in:
    ///
    ///     rwv sync primary
    ///
    ///   Rebase instead of fast-forward when divergent:
    ///
    ///     rwv sync primary --strategy=rebase
    Sync {
        /// Source workspace: `primary`, a bare workweave name, or a path
        /// (absolute, or relative to the primary workspace). Required unless
        /// `--continue` is passed (source is then read from the in-progress
        /// op-state file). If you meant to land work upward, use `rwv sync-to`.
        #[arg(required_unless_present = "do_continue")]
        source: Option<SyncSource>,
        /// Sync strategy: ff (default) or rebase
        #[arg(long, default_value = "ff", value_enum, conflicts_with = "do_continue")]
        strategy: SyncStrategy,
        /// Consent: skip the lock-freshness precondition on both source and destination.
        /// Use when the lock is intentionally ahead of HEAD (e.g. you know the workspace
        /// was updated without a fresh `rwv lock` run). Usual fix without this flag:
        /// run `rwv lock` in the relevant workspace first.
        #[arg(long, conflicts_with = "do_continue")]
        allow_stale_lock: bool,
        /// Consent: discard CWD's project commits that are not reachable from source,
        /// hard-resetting the project repo to source's tip. Pre-sync state is preserved
        /// in refs/rwv/pre-op/<id> and recoverable via `rwv abort`. Refused when the
        /// project repo has uncommitted changes (unrecoverable loss).
        #[arg(long, conflicts_with = "do_continue")]
        discard_local_commits: bool,
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
        /// All parameters (source, strategy, overrides, etc.) are read from the in-progress
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
        /// Step 3 (FF-advance target) is always ff regardless of this flag.
        #[arg(
            long,
            default_value = "rebase",
            value_enum,
            conflicts_with = "do_continue"
        )]
        strategy: SyncStrategy,
        /// Consent: skip the lock-freshness precondition on both source and destination.
        /// Use when the lock is intentionally ahead of HEAD (e.g. you know the workspace
        /// was updated without a fresh `rwv lock` run). Usual fix without this flag:
        /// run `rwv lock` in the relevant workspace first.
        #[arg(long, conflicts_with = "do_continue")]
        allow_stale_lock: bool,
        /// Consent: discard CWD's project commits that are not reachable from target,
        /// hard-resetting the project repo to target's tip. Pre-sync state is preserved
        /// in refs/rwv/pre-op/<id> and recoverable via `rwv abort`. Refused when the
        /// project repo has uncommitted changes (unrecoverable loss).
        #[arg(long, conflicts_with = "do_continue")]
        discard_local_commits: bool,
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
        /// All parameters (target, strategy, retire, overrides, etc.) are read from the
        /// in-progress op-state file. No other flags may be passed alongside `--continue`
        /// (except `--project`). To change parameters mid-op, run `rwv abort` and re-invoke.
        #[arg(long = "continue")]
        do_continue: bool,
    },
}

// ── Network & publishing ──────────────────────────────────────────────────────

#[derive(Subcommand)]
enum NetworkPublishingCmd {
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
        /// Emit per-repo outcomes as JSON (array-of-records with stable per-variant `kind`). See `rwv explain push`.
        /// Under `-j > 1`, output switches to NDJSON (one record per line, streamed as each repo completes).
        #[arg(long)]
        json: bool,
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
        /// Emit per-repo outcomes as JSON (envelope under -j 1, NDJSON under -j > 1). See `rwv explain update`.
        #[arg(long)]
        json: bool,
    },
}

// ── Tooling ───────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum ToolingCmd {
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
    /// Generate workspace-level configuration files
    Setup {
        #[command(subcommand)]
        action: SetupAction,
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
        /// path). Use `--force` for explicit rebuild scenarios. Refuses
        /// if the existing workweave has uncommitted changes — destroy
        /// those explicitly with `workweave delete --force`.
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
        // Detect: rwv sync --force / rwv sync-to --force (--force has been
        // split into named overrides; emit a friendly migration message).
        let is_sync = raw_args.get(1).map(|s| s.as_str()) == Some("sync")
            || raw_args.get(1).map(|s| s.as_str()) == Some("sync-to");
        if is_sync && raw_args.iter().any(|a| a == "--force") {
            eprintln!(
                "error: `--force` has been removed from `rwv sync` and `rwv sync-to`.\n\
                 \n\
                 Replace it with the specific override(s) you need:\n\
                   --allow-stale-lock        skip the lock-freshness precondition\n\
                   --discard-local-commits   discard CWD project commits not in source \
                 (recoverable via `rwv abort`)"
            );
            std::process::exit(2);
        }
        // Detect: rwv workweave <PROJECT> <WORD> where WORD is a bare token that
        // is neither a known subcommand nor a flag. clap consumes <PROJECT> as
        // the `[PROJECT]` positional, then sees WORD as an *unexpected argument*
        // for the outer `workweave` command (it never reaches the subcommand
        // recognition path), so its generic message is "unexpected argument" and
        // it can't offer a "did you mean". Reframe it as a missing-subcommand
        // error with a create-shaped suggestion. See rwv-b2z / the CLI UX audit.
        //
        // Guards (audit §5 design note):
        //   - Only fire when a 4th token (argv[3]) is present and non-flag, so
        //     `rwv workweave foundations` (list default) and
        //     `rwv workweave foundations --help` (help) are untouched.
        //   - PROJECT (argv[2]) must itself be non-flag, so `rwv workweave
        //     --claude-hook` (no [PROJECT]) is untouched.
        //   - Skip when WORD is a known WorkweaveAction so the real subcommand
        //     path (create/delete/list, plus clap's `help`) keeps working.
        if raw_args.get(1).map(|s| s.as_str()) == Some("workweave") {
            let project = raw_args.get(2).map(|s| s.as_str());
            let word = raw_args.get(3).map(|s| s.as_str());
            let is_flag = |s: &str| s.starts_with('-');
            if let (Some(project), Some(word)) = (project, word) {
                const KNOWN_SUBCOMMANDS: &[&str] = &["create", "delete", "list", "help"];
                if !is_flag(project) && !is_flag(word) && !KNOWN_SUBCOMMANDS.contains(&word) {
                    eprintln!(
                        "error: '{word}' is not a valid subcommand for 'rwv workweave {project}'\n\
                         Did you mean:  rwv workweave {project} create {word}\n\
                         Available subcommands: create, delete, list"
                    );
                    std::process::exit(2);
                }
            }
        }
    }

    // Suppress the "For more information, try '--help'" footer on clap errors
    // when `--help`/`-h` is already present in the invocation: re-advising the
    // flag the user just typed is noise. We can't hook this cleanly in clap 4
    // (no stable on-error footer override in derive mode), so detect `--help`
    // in the raw args and, on a clap error, re-render it with that one footer
    // line filtered out. Non-help invocations keep clap's default error path.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let help_requested = std::env::args().any(|a| a == "--help" || a == "-h");
            if help_requested && err.use_stderr() {
                let rendered = err.render().to_string();
                let stripped: String = rendered
                    .lines()
                    .filter(|line| !line.contains("For more information, try"))
                    .collect::<Vec<_>>()
                    .join("\n");
                eprintln!("{stripped}");
                std::process::exit(err.exit_code());
            }
            err.exit();
        }
    };

    match cli.command {
        None => {
            let cwd = std::env::current_dir()?;
            let ctx = WorkspaceContext::resolve(&cwd, None)?;
            println!("{}", ctx.display());
        }
        Some(Commands::Workweaves(WorkweavesCmd::Workweave {
            project,
            hook_mode,
            claude_hook,
            action,
        })) => {
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
        Some(Commands::SetupLifecycle(SetupLifecycleCmd::Fetch {
            source,
            frozen,
            force,
            no_reference,
            roles,
            repos,
            jobs,
            json,
        })) => {
            let cwd = std::env::current_dir()?;
            repoweave::workspace::require_workspace_or_empty(&cwd, force)?;
            let mode = if frozen {
                fetch::FetchMode::Frozen
            } else {
                fetch::FetchMode::Default
            };
            let filter = repoweave::selector::RepoFilter::parse(&roles, &repos)?;
            // fetch's default is auto-resolve (min(nproc, 8)), unlike sync which
            // defaults to serial to preserve envelope vs NDJSON contract. fetch's
            // JSON contract follows the same shape: envelope when -j 1 or no -j
            // with default resolution, NDJSON when -j > 1. Because the default
            // can resolve to > 1 on multi-core hosts, agents should pass `-j 1`
            // explicitly when they require the envelope shape.
            let jobs = repoweave::parallel::resolve_jobs(jobs);
            fetch::run_fetch(&source, &cwd, mode, no_reference, &filter, jobs, json)?;
        }
        Some(Commands::SetupLifecycle(SetupLifecycleCmd::Add {
            url,
            role,
            new,
            project,
        })) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            if new {
                add_remove::run_add_new(&url, &cwd, project_override)?;
            } else {
                add_remove::run_add(&url, role, &cwd, project_override)?;
            }
        }
        Some(Commands::SetupLifecycle(SetupLifecycleCmd::Remove {
            path,
            delete,
            force,
            project,
        })) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            add_remove::run_remove(&path, delete, force, &cwd, project_override)?;
        }
        Some(Commands::LockingVerification(LockingVerificationCmd::Lock {
            dirty,
            commit,
            project,
        })) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            lock::lock(&cwd, dirty, commit, project_override)?;
        }
        Some(Commands::NetworkPublishing(NetworkPublishingCmd::Update {
            dirty,
            commit,
            project,
            roles,
            repos,
            jobs,
            json,
        })) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let filter = repoweave::selector::RepoFilter::parse(&roles, &repos)?;
            // Update's default is auto-parallel (min(nproc, 8)). The envelope/NDJSON
            // split mirrors sync: -j 1 (or unspecified with --json) emits the
            // envelope; -j > 1 streams NDJSON. Note: unlike sync, update defaults
            // to auto-parallel even without --json, so --json + no -j will default
            // to multi-worker NDJSON on multi-core machines. Callers that want the
            // envelope must pass `-j 1` explicitly alongside --json.
            let jobs = repoweave::parallel::resolve_jobs(jobs);
            update::run_update(&cwd, dirty, commit, json, project_override, &filter, jobs)?;
        }
        Some(Commands::NetworkPublishing(NetworkPublishingCmd::Push {
            project,
            dry_run,
            force,
            roles,
            repos,
            jobs,
            json,
        })) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            let filter = repoweave::selector::RepoFilter::parse(&roles, &repos)?;
            // push's default is serial (jobs=1). This differs from fetch/update
            // (which auto-resolve to min(nproc, 8)) because push's `--json`
            // contract pins envelope output under `-j 1` and NDJSON under
            // `-j > 1`; defaulting to auto would silently switch envelope ->
            // NDJSON on multi-core hosts.
            let jobs = match jobs {
                Some(n) => repoweave::parallel::resolve_jobs(Some(n)),
                None => 1,
            };
            push::run_push(&cwd, project_override, dry_run, force, &filter, jobs, json)?;
        }
        Some(Commands::LockingVerification(LockingVerificationCmd::Doctor {
            locked,
            fix,
            json,
            all,
            project,
        })) => {
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
        Some(Commands::LockingVerification(LockingVerificationCmd::Status { json, project })) => {
            let cwd = std::env::current_dir()?;
            let project_override = project.map(repoweave::manifest::ProjectName::new);
            status::run_status(&cwd, json, project_override)?;
        }
        Some(Commands::MultiWorkspaceOps(MultiWorkspaceOpsCmd::Sync {
            source,
            strategy,
            allow_stale_lock,
            discard_local_commits,
            json,
            jobs,
            project,
            do_continue,
        })) => {
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
            let request = sync::SyncRequest {
                source,
                strategy,
                allow_stale_lock,
                discard_local_commits,
                retire: false,
                project_override,
                jobs,
                do_continue,
            };
            if json {
                sync::run_sync_json(&cwd, request)?;
            } else {
                sync::run_sync(&cwd, request)?;
            }
        }
        Some(Commands::MultiWorkspaceOps(MultiWorkspaceOpsCmd::SyncTo {
            target,
            strategy,
            allow_stale_lock,
            discard_local_commits,
            retire,
            json,
            jobs,
            project,
            do_continue,
        })) => {
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
            let request = sync::SyncRequest {
                source: resolved_target,
                strategy,
                allow_stale_lock,
                discard_local_commits,
                retire,
                project_override,
                jobs,
                do_continue,
            };
            if json {
                sync::run_sync_to_json(&cwd, request)?;
            } else {
                sync::run_sync_to(&cwd, request)?;
            }
        }
        Some(Commands::MultiWorkspaceOps(MultiWorkspaceOpsCmd::Abort)) => {
            let cwd = std::env::current_dir()?;
            sync::run_abort(&cwd)?;
        }
        Some(Commands::WorkspaceContext(WorkspaceContextCmd::Resolve)) => {
            let cwd = std::env::current_dir()?;
            let ctx = WorkspaceContext::resolve(&cwd, None)?;
            println!("{}", ctx.active_path().display());
        }
        Some(Commands::SetupLifecycle(SetupLifecycleCmd::Init {
            project,
            provider,
            adopt,
        })) => {
            let cwd = std::env::current_dir()?;
            if adopt {
                init::init_adopt(&project, &cwd)?;
            } else {
                init::init(&project, provider.as_deref(), &cwd)?;
            }
        }
        Some(Commands::WorkspaceContext(WorkspaceContextCmd::Activate {
            project,
            no_install,
        })) => {
            let cwd = std::env::current_dir()?;
            activate::activate_with_options(
                &project,
                &cwd,
                activate::ActivateOptions { no_install },
            )?;
        }
        Some(Commands::WorkspaceContext(WorkspaceContextCmd::Prime { no_suppress })) => {
            let cwd = std::env::current_dir()?;
            prime::prime(&cwd, no_suppress)?;
        }
        Some(Commands::Tooling(ToolingCmd::Setup { action })) => {
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
        Some(Commands::Tooling(ToolingCmd::Completions { shell })) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "rwv", &mut std::io::stdout());
        }
        Some(Commands::Tooling(ToolingCmd::Explain { command })) => {
            explain::explain(command.as_deref())?;
        }
    }

    Ok(())
}
