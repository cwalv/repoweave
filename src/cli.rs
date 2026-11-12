//! CLI type definitions for the `rwv` binary.
//!
//! The [`Cli`] struct and every [`clap::Subcommand`]-derived enum live here so
//! they are reachable from integration tests and other binaries (e.g.
//! `generate-explain`) via `repoweave::cli::Cli`.  The argument dispatch that
//! consumes them is [`dispatch`], also in this module tree — `main.rs` is a
//! shim over [`dispatch::run`] and contains no logic. See [`dispatch`]'s and
//! [`consent`]'s doc comments for why the split falls there.

pub mod consent;
pub mod dispatch;
pub mod operand;

use std::ffi::OsString;

use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;

use crate::manifest;
use crate::op_state::SyncStrategy;
use operand::{Operand, SourceArg};

#[derive(Parser)]
#[command(name = "rwv", bin_name = "rwv", version = crate::rwv_version_with_build(), about = "A cross-repo workspace manager")]
pub struct Cli {
    /// Resolve workspace as if invoked from <path>. Any path inside a
    /// checkout works; the normal containment walk (marker, root, $HOME
    /// ceiling) runs from there. Relative path arguments elsewhere on the
    /// command line resolve against this directory. Repeating this flag is
    /// an error. If you meant to address a workweave by name, use
    /// -w/--workweave instead.
    #[allow(rustdoc::invalid_html_tags)]
    #[arg(
        short = 'C',
        long = "cwd",
        value_name = "PATH",
        global = true,
        help_heading = "Global options"
    )]
    pub cwd_override: Option<Operand>,

    /// Address a workweave by identity (<project>--<name>). The workspace is
    /// found via -C or process cwd; the workweave is then selected from the
    /// registry for the named project. Container-location-independent: the name
    /// survives placement changes that would break a path-based address. Use
    /// -C <path> when outside the ecosystem entirely; compose with -w to select
    /// a specific workweave within the located workspace. Repeating this flag
    /// is an error. If you meant to address by path, use -C instead.
    #[allow(rustdoc::invalid_html_tags)]
    #[arg(
        short = 'w',
        long = "workweave",
        value_name = "PROJECT--NAME",
        global = true,
        help_heading = "Global options"
    )]
    pub workweave_flag: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Args)]
pub struct RepoSelectorArgs {
    /// Limit the operation to repos with this role. Repeat to union multiple roles. Combined as a union with --repo.
    #[arg(long = "role")]
    pub roles: Vec<String>,
    /// Limit the operation to repos matching this selector. Bare strings match exactly; `re:<pat>` matches as regex; `glob:<pat>` matches as glob. Repeat for union. Combined as a union with --role.
    #[arg(long = "repo")]
    pub repos: Vec<String>,
}

impl RepoSelectorArgs {
    pub fn parse_filter(
        &self,
    ) -> Result<crate::selector::RepoFilter, crate::selector::FilterError> {
        crate::selector::RepoFilter::parse(&self.roles, &self.repos)
    }
}

/// Every `<noun> <verb>` spelling a top-level command offers for `verb`: the
/// places a bare `rwv <verb>` was probably meant.
pub fn noun_spellings_of(verb: &str) -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .filter(|noun| noun.get_subcommands().any(|sub| sub.get_name() == verb))
        .map(|noun| format!("{} {verb}", noun.get_name()))
        .collect()
}

// Commands are listed in declaration order; that order is the grouping — no
// labeled help sections. (clap 4 `next_help_heading` applies to args/options,
// not to flattened Subcommand variants, so heading strings would be dead config.)
#[derive(Subcommand)]
pub enum Commands {
    // ── Workspace context ─────────────────────────────────────────────────────
    /// Activate a project (switch `.rwv-active`, surface weave-root symlinks, then run integration install hooks like `npm install` / `uv sync` / `cargo fetch`). Moves no checkout: a member the project shares with the active project that holds edits or is not aligned for it refuses the activation; `--update` aligns first
    Activate {
        /// Project name
        project: String,
        /// Align the project's members as `rwv update` does before activating, instead of refusing over a shared member that is not aligned for it. A shared member holding edits still refuses
        #[arg(long)]
        update: bool,
        /// Skip integration install hooks (e.g., `npm install`, `uv sync`) for fast context-switch; `rwv materialize` runs them later
        #[arg(long)]
        no_materialize: bool,
    },
    /// Settle drift, clean up disabled integrations, repair missing weave-root symlinks, and run integration install hooks for the project this checkout already presents, without selecting anything. Hooks materialize; they never move a pin
    Materialize {
        /// Consent to discarding the current content of drifted rwv-attested generated files and regenerating them from the current inputs
        #[arg(long, conflicts_with = "adopt_drifted")]
        regenerate_drifted: bool,
        /// Consent to recording drifted rwv-attested generated files' current content as the accepted generation
        #[arg(long)]
        adopt_drifted: bool,
        /// Consent to unlinking weave-root symlinks at names the active project no longer declares (the files they point at are untouched)
        #[arg(long)]
        remove_undeclared_links: bool,
    },
    /// Print structured workspace context for agent system prompts
    Prime {
        /// Always emit output, even when CWD is not inside a weave or workweave
        #[arg(long)]
        no_suppress: bool,
    },
    /// Print weave root path
    Resolve,

    // ── Setup & lifecycle ─────────────────────────────────────────────────────
    /// Clone a project and each of its members at the commit `rwv.lock` pins. A member the lock does not pin, or pins at a commit no ref reaches, is cloned at its declared branch's tip and listed; the exit status is then 3.
    Clone {
        /// Project to clone: a URL (`file://` for a local remote) or `owner/repo` shorthand
        source: String,
        /// Clone into a non-empty directory that is not a workspace
        #[arg(long)]
        allow_non_empty_dir: bool,
        /// Skip cloning repositories with role: reference
        #[arg(long)]
        no_reference: bool,
        /// Number of parallel per-repo workers. Default: min(nproc, 8). `-j 1` is explicit serial.
        #[arg(short = 'j', long = "jobs")]
        jobs: Option<usize>,
        /// Emit per-repo outcomes, and the members left without a pin, as one JSON envelope once every repo is done, whatever `-j` is. See `rwv explain clone`.
        #[arg(long)]
        json: bool,
        /// Skip the integration install hooks (e.g., `npm install`, `uv sync`) of the activation a clone performs when no project is active; `rwv materialize` runs them later
        #[arg(long)]
        no_materialize: bool,
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
        /// With `--adopt`, skip integration install hooks (e.g., `npm install`, `uv sync`); `rwv materialize` runs them later
        #[arg(long, requires = "adopt")]
        no_materialize: bool,
    },
    /// Add a repo to the active project, remove one, pin one at a commit, or change a member's role or submodule setting
    Member {
        #[command(subcommand)]
        action: MemberAction,
    },

    /// Record how a weave branch is used, list the weave branches with their records, or delete one
    #[command(arg_required_else_help = true)]
    Branch {
        #[command(subcommand)]
        action: BranchAction,
    },

    // ── Workweaves ────────────────────────────────────────────────────────────
    /// Create, delete, or list workweaves
    #[command(arg_required_else_help = true)]
    Workweave {
        /// Hook mode: print only the workweave path to stdout (for Claude Code WorktreeCreate hook). An install hook that fails in the created workweave is a warning here, not a failure
        #[arg(long)]
        hook_mode: bool,
        /// Claude Code hook mode: read JSON from stdin, handle create/remove automatically. An install hook that fails in a created workweave is a warning, not a failure
        #[arg(long, conflicts_with = "hook_mode")]
        claude_hook: bool,
        #[command(subcommand)]
        action: Option<WorkweaveAction>,
    },

    // ── Locking & verification ────────────────────────────────────────────────
    /// Convention enforcement and lock-freshness checking
    Doctor {
        /// Zero exit iff every repo's tip matches its rwv.lock entry (scriptable precondition for rwv sync)
        #[arg(long)]
        locked: bool,
        /// Repair every finding marked Auto-fixable in docs/reference/doctor-findings.md, which carries that mark on each finding it documents. Never touches live staged content or live edits, and never a ref rwv holds no ownership receipt for. Idempotent. Active-project scoped; use --all to widen.
        #[arg(long, conflicts_with = "locked")]
        fix: bool,
        /// Emit violations as JSON (array-of-records with stable per-variant `kind`). See `rwv explain doctor`.
        #[arg(long, conflicts_with_all = ["locked", "fix"])]
        json: bool,
        /// Scan all projects and run weave-wide checks (orphan detection, cross-project stale locks, etc.).
        /// By default only the active project is checked.
        #[arg(long)]
        all: bool,
        /// With `--fix`, relink a workweave checkout whose link dangles and
        /// whose pinned revision and branch are in no clone onto the
        /// workweave's branch created at the canonical clone's HEAD, keeping
        /// every file: the pinned commits are given up, and their changes
        /// stand as edits. Without this flag, `--fix` reports the checkout.
        #[arg(long)]
        abandon_lost_pins: bool,
        /// Filter the report to the named finding kind (repeatable). Kind names are the wire spellings `--json` emits for violations and integration issues (e.g. `alignment`, `orphaned-savepoint`, `tool-missing`); an unknown name refuses, listing the valid set. Kinds whose classes normally collapse to a per-class count line render itemized under the filter — this is the drill-down. The exit code reflects the filtered view; with `--json`, any finding in it exits non-zero, warnings included.
        #[arg(long = "kind", value_name = "KIND", conflicts_with_all = ["locked", "fix"])]
        kind: Vec<String>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Record where each member's checkout is as its pin, moving a pin forward only: a member with no pin keeps none, and one behind or diverged from its pin is refused unless `--repo` names it. No integration hooks fire; run `rwv materialize` after lock changes the workspace membership to refresh node_modules / .venv / etc.
    Lock {
        /// Allow locking repos with uncommitted changes
        #[arg(long)]
        dirty: bool,
        /// Record only the repos matching this selector, and let one of them move its pin backward. Bare strings match exactly; `re:<pat>` matches as regex; `glob:<pat>` matches as glob. Repeat for union.
        #[arg(long = "repo", value_name = "REPO")]
        repos: Vec<String>,
        /// Commit rwv.lock after writing it, together with rwv.toml when it has uncommitted changes
        #[arg(long)]
        commit: bool,
        /// Commit message for `--commit`, in place of the generated summary of which repos changed
        #[arg(short = 'm', long = "message", value_name = "MSG", requires = "commit")]
        message: Option<String>,
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
        #[command(flatten)]
        selector: RepoSelectorArgs,
    },
    /// Show the VCS's log in every repo of the workspace, or with `--against` the commits beyond what a weave branch pins
    ///
    /// A bare `rwv log` is the VCS's log of HEAD in each repo. A RANGE is read
    /// in the VCS's own syntax in every repo, each endpoint resolving there as
    /// the VCS would resolve it: a repo where an endpoint names nothing prints
    /// `name no-such-branch` in its section, the others are still listed, and
    /// the exit is non-zero.
    ///
    /// `--against W` reads a weave branch's pins instead: per member, the
    /// commits HEAD reaches that W's pin for it does not, and in the project
    /// repo those W lacks. Run it from a feature to list what it has not
    /// landed, or address the workweave with `-w`.
    Log {
        /// A range in the VCS's own syntax (`feat-x..`, `main..feat-x`, `v1.2...`); with none, HEAD
        #[arg(
            value_parser = clap::builder::NonEmptyStringValueParser::new(),
            conflicts_with = "against"
        )]
        range: Option<String>,
        /// List the commits HEAD reaches that this weave branch's pins do not, per member, and in the project repo those the branch lacks
        #[arg(
            long,
            value_name = "W",
            value_parser = clap::builder::NonEmptyStringValueParser::new()
        )]
        against: Option<String>,
        /// Show each repo's diff from the common ancestor of its pin and HEAD instead of the commit listing, so a pin that moved on after HEAD forked is not shown as reversals
        #[arg(long, requires = "against")]
        diff: bool,
        /// Emit one `{ "$schema": ..., "repos": [...] }` JSON envelope. See `rwv explain log`.
        #[arg(long)]
        json: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },

    // ── Multi-workspace ops ───────────────────────────────────────────────────
    /// Roll an in-flight sync or sync-to back to its savepoints in every workspace it touched, run from any of them
    Abort {
        /// Consent to restoring THIS repo over a foreign tip — commits the operation did not create, which abort otherwise refuses to move the branch off. Name the repo as abort's own output does (its workspace-relative path, or `projects/<name>` for the project repo); repeat the flag per repo. There is deliberately no all-repos form. The abandoned commits are first kept as an `abandoned` backup, which `rwv backup list` shows and `rwv backup drop` drops.
        #[arg(long = "abandon-foreign-tip", value_name = "REPO")]
        abandon_foreign_tip: Vec<crate::manifest::RepoOperand>,
    },
    /// List, drop or prune the backups rwv keeps before any step that would leave a commit or an edit unpreserved
    #[command(arg_required_else_help = true)]
    Backup {
        #[command(subcommand)]
        action: BackupAction,
    },
    /// Bring another workspace's committed state into this one (pull/align; use `rwv sync-to` to land work upward)
    ///
    /// Absorbs `<source>`'s committed lock and advances each member repo to the locked SHA.
    /// Default strategy is `ff` (fast-forward only); pass `--strategy=rebase` when histories
    /// have diverged and ff would bail.
    ///
    /// Examples:
    ///
    ///   Bring primary's HEAD into the workweave you are iterating in:
    ///
    ///   rwv sync primary
    ///
    ///   Rebase instead of fast-forward when divergent:
    ///
    ///   rwv sync primary --strategy=rebase
    Sync {
        /// Source workspace: `primary`, a bare workweave name, or a path
        /// (absolute, or relative to the invocation directory). Required unless
        /// `--continue` is passed (source is then read from the in-progress
        /// op-state file). If you meant to land work upward, use `rwv sync-to`.
        #[arg(required_unless_present = "do_continue")]
        source: Option<SourceArg>,
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
        #[allow(rustdoc::invalid_html_tags)]
        #[arg(long, conflicts_with = "do_continue")]
        discard_local_commits: bool,
        /// Emit per-repo outcomes as JSON (array-of-records with stable per-variant `kind`). See `rwv explain sync`.
        #[arg(long, conflicts_with = "do_continue")]
        json: bool,
        /// Run up to N per-repo manifest syncs in parallel. Default: 1 (serial).
        #[arg(short = 'j', long = "jobs", conflicts_with = "do_continue")]
        jobs: Option<usize>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Resume a sync that was interrupted mid-op (e.g. after resolving a conflict).
        /// All parameters (source, strategy, project, overrides, etc.) are read from the
        /// in-progress op-state file. No other flags may be passed alongside `--continue`
        /// (except `--project`, which asserts rather than rebinds: it must name the project
        /// the op was started for, and a value contradicting the record is refused — the
        /// phases already run are bound to it). To change parameters mid-op, run
        /// `rwv abort` and re-invoke.
        #[arg(long = "continue")]
        do_continue: bool,
    },
    /// Advance target workspace to CWD's tip (3-step orchestration: rebase CWD against target,
    /// auto-relock, then fast-forward target to CWD's converged tip). CWD's unique commits land
    /// on top of target's prior history; target absorbs CWD's state with CWD as the newest
    /// contribution.
    SyncTo {
        /// Target workspace: `primary`, a bare workweave name, or a path
        /// (absolute, or relative to the invocation directory). Omit to target
        /// the recorded parent from `.rwv-workweave`; errors if not in a workweave.
        /// Must not be passed alongside `--continue` (target is then read from op-state).
        #[arg(conflicts_with = "do_continue")]
        target: Option<SourceArg>,
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
        #[allow(rustdoc::invalid_html_tags)]
        #[arg(long, conflicts_with = "do_continue")]
        discard_local_commits: bool,
        /// Land work then delete the workweave on success (requires clean worktree and
        /// member repos converged with target after sync-to completes).
        #[arg(long, conflicts_with = "do_continue")]
        retire: bool,
        /// Emit per-repo outcomes as JSON (array-of-records with stable per-variant `kind`).
        /// May be combined with `--continue`: the resumed op's envelope reports the
        /// machine's own coordinates and carries a `resumed` field naming the phase the
        /// interrupted op had reached. See `rwv explain sync-to`.
        #[arg(long)]
        json: bool,
        /// Run up to N per-repo manifest syncs in parallel. Default: 1 (serial).
        #[arg(short = 'j', long = "jobs", conflicts_with = "do_continue")]
        jobs: Option<usize>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Resume a sync-to that was interrupted mid-op (e.g. after resolving a conflict).
        /// All parameters (target, strategy, retire, project, overrides, etc.) are read from
        /// the in-progress op-state file. No other flags may be passed alongside `--continue`
        /// (except `--json`, and `--project`, which asserts rather than rebinds: it must name
        /// the project the op was started for, and a value contradicting the record is
        /// refused — the phases already run are bound to it). To change parameters mid-op,
        /// run `rwv abort` and re-invoke.
        #[arg(long = "continue")]
        do_continue: bool,
    },

    // ── Network & publishing ──────────────────────────────────────────────────
    /// Bring every remote's branches and tags into each repo's object store, dropping remote branches that are gone. Moves no branch, checkout or lock, and reports what moved.
    Fetch {
        /// Skip repositories with role: reference
        #[arg(long)]
        no_reference: bool,
        #[command(flatten)]
        selector: RepoSelectorArgs,
        /// Number of parallel per-repo workers. Default: min(nproc, 8). `-j 1` is explicit serial.
        #[arg(short = 'j', long = "jobs")]
        jobs: Option<usize>,
        /// Emit per-repo results as one JSON envelope once every repo is done, whatever `-j` is. See `rwv explain fetch`.
        #[arg(long)]
        json: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Fetch, fast-forward the project branch to its upstream, then update the members to the lock it holds.
    Pull {
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        selector: RepoSelectorArgs,
        /// Number of parallel per-repo workers. Default: min(nproc, 8). `-j 1` is explicit serial.
        #[arg(short = 'j', long = "jobs")]
        jobs: Option<usize>,
        /// Emit one JSON envelope once every step is done, whatever `-j` is. See `rwv explain pull`.
        #[arg(long)]
        json: bool,
        /// Skip the integration install hooks (e.g., `npm install`, `uv sync`) of the activation an update performs when it selects the weave's only project; `rwv materialize` runs them later
        #[arg(long)]
        no_materialize: bool,
    },
    /// Clone the members missing on disk and align the rest to rwv.lock, without writing it. With --remote, advance each repo to its branch HEAD on the remote and record the advanced tips in the lock (network bump; analogous to `cargo update` / `npm update`).
    Update {
        /// Advance each repo to its branch HEAD on the remote and record the advanced tips in the lock
        #[arg(long)]
        remote: bool,
        /// Allow update with uncommitted changes in repos when relocking
        #[arg(long, requires = "remote")]
        dirty: bool,
        /// Commit rwv.lock together with the integration files regenerated against the new tips, and with rwv.toml when it has uncommitted changes
        #[arg(long, requires = "remote")]
        commit: bool,
        /// Commit message for `--commit`, in place of the generated summary of which repos changed
        #[arg(short = 'm', long = "message", value_name = "MSG", requires = "commit")]
        message: Option<String>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        selector: RepoSelectorArgs,
        /// Number of parallel per-repo workers. Default: min(nproc, 8). `-j 1` is explicit serial.
        #[arg(short = 'j', long = "jobs")]
        jobs: Option<usize>,
        /// Emit per-repo outcomes as one JSON envelope once every repo is done, whatever `-j` is. See `rwv explain update`.
        #[arg(long)]
        json: bool,
        /// Skip integration install hooks (e.g., `npm install`, `uv sync`); `rwv materialize` runs them later
        #[arg(long)]
        no_materialize: bool,
    },

    /// Publish a weave branch: its members' branches, then its record and the drops made here, then the project branch. Runs from any workspace on a recorded, named branch; never writes the lock. Pushes `owned` and `fork` members by default and a `dependency` or `reference` member only when named.
    Push {
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Print the push plan without executing
        #[arg(long)]
        dry_run: bool,
        /// Push this weave branch instead of the checked-out one, judging each member by the branch's record and lock as the refs hold them
        #[arg(long, value_name = "W")]
        branch: Option<String>,
        /// Replace this repo's diverged push branch, keeping the remote tip as a backup and leasing the replacement to it. The repo is its workspace-relative path, or `projects/<name>` for the project repo; repeat the flag per repo. Never rewrites a branch some weave branch declares
        #[arg(long, value_name = "REPO")]
        overwrite: Vec<crate::manifest::RepoOperand>,
        /// Limit the push to repos with this role. Repeat to union multiple roles. Combined as a union with --repo. Naming a `dependency` or `reference` role pushes those members. With a selector the project branch and its record are pushed only when `projects/<name>` is named by --repo.
        #[arg(long = "role")]
        roles: Vec<String>,
        /// Limit the push to repos matching this selector. Bare strings match exactly; `re:<pat>` matches as regex; `glob:<pat>` matches as glob. Repeat for union. Combined as a union with --role. `projects/<name>` names the project repo.
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// Number of parallel per-repo workers for the fetches and member pushes. Default: 1 (serial): every push authenticates and runs the repo's pre-push hook, so concurrent pushes would share one terminal's credential prompts and run those hooks side by side, and the default push set is already narrowed to writable roles. Project-repo push always runs serially as the last step.
        #[arg(short = 'j', long = "jobs")]
        jobs: Option<usize>,
        /// Emit per-repo outcomes as JSON (array-of-records with stable per-variant `kind`). See `rwv explain push`.
        #[arg(long)]
        json: bool,
    },
    // ── Tooling ───────────────────────────────────────────────────────────────
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
    /// Register Claude Code integration (hooks and settings) for this user
    Setup {
        #[command(subcommand)]
        action: SetupAction,
    },

    // External subcommand fallthrough — see plugins.rs. clap routes any
    // subcommand this enum does NOT match into `External`, so core verbs
    // above always win (the "builtin first" invariant): the plugin path is
    // unreachable for a name clap already knows. The captured vec holds the
    // verb (element 0) and its remaining args in order, preserving `--`,
    // repeated flags, and anything else the plugin needs verbatim.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

#[derive(Subcommand)]
pub enum BackupAction {
    /// List each repo's backups, marking redundant, `rebased` and `abandoned` ones, each with the commits only it holds
    List {
        /// Emit one `{ "$schema": ..., "repos": [...] }` JSON envelope. See `rwv explain backup`.
        #[arg(long)]
        json: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Drop the named backups of one repo, or all of that repo's, printing each tip. Refused where it would leave a commit some project's live lock pins reached by no branch, remote-tracking branch or backup
    Drop {
        /// The repo: its workspace-relative path, or `projects/<name>` for the project repo
        repo: crate::manifest::RepoOperand,
        /// Backups to drop, as `rwv backup list` names them; none drops every backup of the repo
        names: Vec<String>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Fetch every remote of every repo with pruning, then drop each backup a branch or remote-tracking branch reaches in full. Refused where it would leave a live pin unreachable
    Prune {
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum BranchAction {
    /// List the weave branches with their records, marking a conflicted or unreadable record, a target that is gone and a record no remote holds yet
    List {
        /// Emit one `{ "$schema": ..., "branches": [...] }` JSON envelope. See `rwv explain branch`.
        #[arg(long)]
        json: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Record how a weave branch is used: `feature` work onto a target branch, or `declared`. Writes the record only; it moves no branch and no checkout.
    ///
    /// Without a branch name the project repo's checked-out branch is recorded.
    /// `feature` takes `--target`, the branch the feature lands into, which
    /// must have a record of its own: the target's mode is stored with the
    /// feature's record. `declared` takes no `--target`.
    #[command(override_usage = "rwv branch set-mode [OPTIONS] [W] <feature|declared>")]
    SetMode {
        /// `[W] <feature|declared>`: the branch to record (the checked-out one when omitted), then its mode
        #[arg(num_args = 1..=2, value_names = ["W", "MODE"], required = true)]
        words: Vec<String>,
        /// With `feature`: the branch the feature lands into
        #[arg(long, value_name = "T")]
        target: Option<String>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Rename the project repo's checked-out branch, and in a feature its `owned` and `fork` members' branches of the same name; the record, its stored entries, every record that lands into it and the workspace's aligned branch follow. A declared-mode project branch can be renamed; a member branch some live weave branch declares cannot
    ///
    /// Each renamed branch stops taking in from the remote branch it did, and
    /// each old remote branch is reported as left, with the command that
    /// deletes it once landed. A `<new>` that exists locally, or on a remote as
    /// last fetched, is refused unless `--adopt` names the remote's.
    Rename {
        /// The new name of the checked-out project branch
        new: String,
        /// Take up a branch a remote already has under `<new>` (a rename made on the forge, or on another machine): each renamed branch takes in from the remote's `<new>`. Refused where the two have diverged
        #[arg(long)]
        adopt: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Delete a weave branch and the branches that belong to it, by the deletion rule: each is deleted when everything on it has landed on the branch it lands into and no live lock pins a commit only it reaches; the rest survive, listed with why, and the exit status is 3. A declared branch never belongs
    ///
    /// The branch it lands into is the record's target, or `--target`, which
    /// a branch that is not a feature needs. A branch checked out anywhere is
    /// refused, and so is one other branches' records land into.
    Delete {
        /// The branch to delete; it may exist only on a remote, as last fetched
        branch: String,
        /// The branch whose commits count as having landed; the record's target when omitted
        #[arg(long, value_name = "T")]
        target: Option<String>,
        /// Also delete a branch that has not landed, after keeping its tip as a backup. Confirms the work merged elsewhere or abandoned; lifts only that clause of the rule
        #[arg(long)]
        discard_unlanded: bool,
        /// Also delete each push branch of what belongs that has landed on the target's remote-tracking branch
        #[arg(long)]
        delete_remote: bool,
        /// Also delete this repo's push branch, landed or not, after keeping its tip as a backup. The repo is its workspace-relative path, or `projects/<name>` for the project repo; repeat the flag per repo. Needs `--discard-unlanded`
        #[arg(long, value_name = "REPO", requires = "discard_unlanded")]
        delete_remote_unlanded: Vec<crate::manifest::RepoOperand>,
        /// Emit one `{ "$schema": ..., "deleted": [...], "survivors": [...] }` JSON envelope. See `rwv explain branch`.
        #[arg(long)]
        json: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum MemberAction {
    /// Add a repo to the active project
    Add {
        /// Repository URL (`file://` for a local remote) or registry
        /// shorthand such as `github/owner/repo`; with --adopt, the path of an
        /// existing clone; with --new, a creation address: a bare registry
        /// name, or `registry/owner/repo`
        url: String,
        /// Role for the repo
        #[arg(long, default_value_t = manifest::Role::Owned, value_enum)]
        role: manifest::Role,
        /// Create a new repo instead of cloning: the argument is a creation
        /// address, not a URL or path
        #[arg(long, conflicts_with = "adopt")]
        new: bool,
        /// Register an existing clone instead of cloning: the argument is its
        /// path, relative paths resolving against the invocation directory, and
        /// its position under the weave root is the path recorded. The URL is
        /// read from its origin remote
        #[arg(long)]
        adopt: bool,
        /// Record `recurse-submodules = false` for the member: new checkouts of
        /// it leave its submodules uninitialized
        #[arg(long)]
        no_recurse_submodules: bool,
        /// A creation parameter as NAME=VALUE (repeatable). See `rwv explain member`
        /// for what each registry declares. Only meaningful with --new.
        #[arg(long = "param", value_name = "NAME=VALUE", requires = "new")]
        params: Vec<String>,
        /// Creation parameters as one JSON object of string values, e.g.
        /// '{"root": "/srv/repos"}'. A name given both here and via --param refuses.
        /// Only meaningful with --new.
        #[arg(long = "params-json", value_name = "JSON", requires = "new")]
        params_json: Option<String>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Skip integration install hooks (e.g., `npm install`, `uv sync`); `rwv materialize` runs them later
        #[arg(long)]
        no_materialize: bool,
    },
    /// Remove a repo from the active project
    Remove {
        /// Path of the repo to remove, as `rwv.toml` lists it
        path: String,
        /// Delete the clone directory
        #[arg(long)]
        delete: bool,
        /// With `--delete`, remove the clone even if other projects still reference it
        #[arg(long, requires = "delete")]
        delete_shared_clone: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Skip integration install hooks (e.g., `npm install`, `uv sync`); `rwv materialize` runs them later
        #[arg(long)]
        no_materialize: bool,
    },
    /// Change a repo's role in the active project, and settle its files for it
    SetRole {
        /// Path of the repo, as `rwv.toml` lists it
        repo: String,
        /// The role to give it
        #[arg(value_enum)]
        role: manifest::Role,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Skip integration install hooks (e.g., `npm install`, `uv sync`); `rwv materialize` runs them later
        #[arg(long)]
        no_materialize: bool,
    },
    /// Pin a member at a commit of your choosing, forward or backward, and align it to the pin
    Pin {
        /// Path of the repo, as `rwv.toml` lists it
        repo: String,
        /// The commit to pin: a commit id, or a tag or branch name that resolves to one here. Where the member follows a declared branch, the branch or its upstream must reach it
        rev: String,
        /// Commit `rwv.lock` after writing it, together with `rwv.toml` when it has uncommitted changes
        #[arg(long)]
        commit: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Declare the branch a member follows, and pin the member on it
    SetBranch {
        /// Path of the repo, as `rwv.toml` lists it
        repo: String,
        /// The branch to declare. It must exist, locally or on a remote as last
        /// fetched, unless `--new` creates it
        branch: String,
        /// Create the branch, which must exist nowhere: at the member's pin, or
        /// at `--at`
        #[arg(long)]
        new: bool,
        /// Pin the member at this commit of the branch, or of its upstream, instead
        /// of at the branch's tip; with `--new`, create the branch there
        #[arg(long, value_name = "REV")]
        at: Option<String>,
        /// Set the member's url as `member set-url` does, in the same commit
        #[arg(long)]
        url: Option<String>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Skip integration install hooks (e.g., `npm install`, `uv sync`); `rwv materialize` runs them later
        #[arg(long)]
        no_materialize: bool,
    },
    /// Change a member's url, and relink the branch the member sits on to the new url's remote
    SetUrl {
        /// Path of the repo, as `rwv.toml` lists it
        repo: String,
        /// The new url
        url: String,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Skip integration install hooks (e.g., `npm install`, `uv sync`); `rwv materialize` runs them later
        #[arg(long)]
        no_materialize: bool,
    },
    /// Say whether new checkouts of a repo populate its submodules
    SetRecurseSubmodules {
        /// Path of the repo, as `rwv.toml` lists it
        repo: String,
        /// `true` populates the submodules a checkout records; `false` leaves
        /// them uninitialized
        #[arg(action = clap::ArgAction::Set)]
        recurse: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Skip integration install hooks (e.g., `npm install`, `uv sync`); `rwv materialize` runs them later
        #[arg(long)]
        no_materialize: bool,
    },
}

#[derive(Subcommand)]
pub enum WorkweaveAction {
    /// Create a new workweave
    Create {
        /// Workweave name
        name: String,
        /// Destroy an existing workweave at this path before recreating.
        ///
        /// Without this flag, re-invoking `create` against an existing
        /// workweave preserves non-git state in place (the idempotent
        /// path). Use `--replace-existing` for explicit rebuild scenarios.
        /// Refuses if the existing workweave has uncommitted or unmerged
        /// work — destroy that explicitly with `workweave delete
        /// --discard-uncommitted --discard-unlanded`.
        #[arg(long)]
        replace_existing: bool,
        /// Workspace to fork the new workweave from: `primary`, a bare
        /// workweave name of this project, or a path (absolute, or relative to
        /// the invocation directory), read as `rwv sync <SOURCE>` reads it. A
        /// directory with a bare name is written `./<dir>`. Omit to fork from
        /// CWD's active workspace (the workweave when invoked from inside one,
        /// otherwise primary).
        ///
        /// Forking from an existing workweave is how you DUPLICATE one — for
        /// a scratch variant, an experiment, or a throwaway baseline. Do not
        /// copy a workweave with `cp`: the result aliases the original's git
        /// state rather than duplicating it.
        #[arg(long)]
        from: Option<SourceArg>,
        /// Allow creation even when the source project directory has
        /// uncommitted changes. Uncommitted edits to `rwv.toml` and `rwv.lock`
        /// are moved into the new workweave's project worktree: copied there,
        /// then discarded from the source once the workweave exists. Other
        /// uncommitted files stay in the source. Without this flag, `create`
        /// refuses and names the dirty files so you can commit, stash, or opt
        /// in explicitly.
        #[arg(long)]
        capture_dirty: bool,
        /// The exact path to place the workweave at.
        ///
        /// Recorded in the registry and used as given; there is no default.
        /// Absolute paths are used as-is; relative paths resolve against the
        /// invocation directory. Useful for big-disk workweaves, tmpfs
        /// experiments, or one-off placements. Refused together with
        /// `--basedir`.
        #[arg(long, conflicts_with = "basedir")]
        dir: Option<Operand>,
        /// The directory to place the workweave under for this invocation,
        /// in place of the recorded default (`rwv workweave
        /// set-default-create-basedir`). The workweave lands at
        /// `<basedir>/<project>--<name>`. Refused together with `--dir`.
        #[arg(long)]
        basedir: Option<Operand>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Skip integration install hooks (e.g., `npm install`, `uv sync`) in the new workweave; `rwv materialize` inside it runs them later
        #[arg(long)]
        no_materialize: bool,
    },
    /// Delete a workweave, and the branches that belong to it by the deletion rule. A branch that has not landed survives, listed with why, and the exit status is 3
    Delete {
        /// Workweave name
        name: String,
        /// Delete even if a worktree has uncommitted changes, keeping them as
        /// a backup first. Without this flag, deletion refuses and names the
        /// dirty paths.
        #[arg(long)]
        discard_uncommitted: bool,
        /// Delete even if a detached HEAD holds commits no branch reaches, and
        /// delete a branch of the workweave that has not landed, keeping each
        /// as a backup first. Without this flag, deletion refuses the
        /// detached commits and the rule keeps the branch.
        #[arg(long)]
        discard_unlanded: bool,
        /// Also delete each push branch of what belongs to the workweave that
        /// has landed on the target's remote-tracking branch
        #[arg(long)]
        delete_remote: bool,
        /// Also delete this repo's push branch, landed or not, after keeping
        /// its tip as a backup. The repo is its workspace-relative path, or
        /// `projects/<name>` for the project repo; repeat the flag per repo.
        /// Needs `--discard-unlanded`
        #[arg(long, value_name = "REPO", requires = "discard_unlanded")]
        delete_remote_unlanded: Vec<crate::manifest::RepoOperand>,
        /// Emit one `{ "$schema": ..., "discarded": {...}, "deleted": [...], "survivors": [...] }` JSON envelope. See `rwv explain workweave`.
        #[arg(long)]
        json: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// List existing workweaves
    List {
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Record where `workweave create` places new workweaves by default.
    ///
    /// Sets the `basedir` of the project's workweave index — the directory
    /// `workweave create` places new workweaves under by default. Per-workweave
    /// `--dir` and `--basedir` on `create` are unaffected. Until it is set, the
    /// default is `<parent-of-primary>/.workweaves`. The index is kept with the
    /// project clone, outside its working tree.
    SetDefaultCreateBasedir {
        /// Path to record. Relative paths resolve against the invocation
        /// directory.
        path: Operand,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum SetupAction {
    /// Register Claude Code hooks: `rwv prime` for SessionStart and PreCompact, `rwv workweave --claude-hook` for WorktreeCreate and WorktreeRemove
    Claude {
        /// Remove all rwv hooks from Claude Code settings
        #[arg(long)]
        uninstall: bool,
    },
}
