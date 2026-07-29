//! CLI type definitions for the `rwv` binary.
//!
//! The [`Cli`] struct and every [`clap::Subcommand`]-derived enum live here so
//! they are reachable from integration tests and other binaries (e.g.
//! `generate-explain`) via `repoweave::cli::Cli`.  The argument dispatch that
//! consumes them is [`dispatch`], also in this module tree — `main.rs` is a
//! shim over [`dispatch::run`] and contains no logic. See [`dispatch`]'s and
//! [`consent`]'s doc comments for why the split falls there.

pub mod dispatch;

use std::ffi::OsString;

use clap::{Parser, Subcommand};
use clap_complete::Shell;

use crate::manifest;
use crate::op_state::SyncStrategy;
use crate::sync::SyncSource;

#[derive(Parser)]
#[command(name = "rwv", version = crate::rwv_version(), about = "A cross-repo workspace manager")]
pub struct Cli {
    /// Resolve workspace as if invoked from <path>. Any path inside a
    /// checkout works; the normal containment walk (marker, root, $HOME
    /// ceiling) runs from there. Relative path arguments elsewhere on the
    /// command line resolve against this directory. Repeating this flag is
    /// an error. If you meant to address a workweave by name, use
    /// -w/--workweave instead.
    #[arg(
        short = 'C',
        long = "cwd",
        value_name = "PATH",
        global = true,
        help_heading = "Global options"
    )]
    pub cwd_override: Option<String>,

    /// Address a workweave by identity (<project>--<name>). The workspace is
    /// found via -C or process cwd; the workweave is then selected from the
    /// registry for the named project. Container-location-independent: the name
    /// survives placement changes that would break a path-based address. Use
    /// -C <path> when outside the ecosystem entirely; compose with -w to select
    /// a specific workweave within the located workspace. Repeating this flag
    /// is an error. If you meant to address by path, use -C instead.
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

// Commands are listed in declaration order; that order is the grouping — no
// labeled help sections. (clap 4 `next_help_heading` applies to args/options,
// not to flattened Subcommand variants, so heading strings would be dead config.)
#[derive(Subcommand)]
pub enum Commands {
    // ── Workspace context ─────────────────────────────────────────────────────
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

    // ── Setup & lifecycle ─────────────────────────────────────────────────────
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
    /// Clone a project and align repos to rwv.lock (no network bump). Use `rwv update` to advance to branch HEAD. With no SOURCE, re-materialize missing manifest members of the active project (repair verb for dangling references).
    Fetch {
        /// Source to fetch from. Omit to re-materialize missing manifest members of the active project in the current workspace.
        source: Option<String>,
        /// Error if the lock file is missing or incomplete (CI mode)
        #[arg(long)]
        frozen: bool,
        /// Bootstrap into a non-empty directory that is not a workspace
        #[arg(long)]
        allow_non_empty_dir: bool,
        /// Skip cloning/fetching repositories with role: reference
        #[arg(long)]
        no_reference: bool,
        /// Align a present repo even where that changes what HEAD is attached to: materialize the pin on a detached HEAD instead of refusing
        #[arg(long)]
        detach_checkouts: bool,
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
        /// With `--delete`, remove the clone even if other projects still reference it
        #[arg(long)]
        delete_shared_clone: bool,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
    },

    // ── Workweaves ────────────────────────────────────────────────────────────
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
        /// With `--fix`, reattach a canonical store's detached HEAD to its
        /// tracking counterpart when that counterpart exists and its tip
        /// equals HEAD. Without this flag, `--fix` only reports a detached
        /// canonical, naming the `git switch` that would reattach it.
        #[arg(long)]
        reattach_checkouts: bool,
        /// With `--fix`, let the branch-model migration mint a workweave's
        /// ephemeral branch at a detached checkout's HEAD (the lock SHA).
        /// When a pre-flat `<project>--<workweave>/<segment>` branch holds
        /// that name, it is given up to make room — and the migration warns
        /// when doing so strands commits HEAD does not carry. Without this
        /// flag, `--fix` reports both tips and leaves the checkout alone.
        #[arg(long)]
        adopt_detached_checkouts: bool,
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

    // ── Multi-workspace ops ───────────────────────────────────────────────────
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
    ///   rwv sync primary
    ///
    ///   Rebase instead of fast-forward when divergent:
    ///
    ///   rwv sync primary --strategy=rebase
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

    // ── Network & publishing ──────────────────────────────────────────────────
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
        /// Commit rwv.lock together with the integration files regenerated against the new tips
        #[arg(long)]
        commit: bool,
        /// Align a repo even when that detaches a branch attached to
        /// something other than its tracking counterpart, or moves the
        /// counterpart in a way that is not a fast-forward
        #[arg(long)]
        detach_checkouts: bool,
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
    /// Generate workspace-level configuration files
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
        /// --discard-uncommitted --discard-unmerged-commits`.
        #[arg(long)]
        replace_existing: bool,
        /// Workspace to fork the new workweave from. Accepts `primary`, an
        /// absolute or relative path, or is omitted to fork from CWD's
        /// active workspace (the workweave when invoked from inside one,
        /// otherwise primary). Relative paths resolve against primary, so
        /// peer workweaves can be referenced by directory name (e.g.
        /// `--from .workweaves/myproj--hotfix`).
        ///
        /// Forking from an existing workweave is how you DUPLICATE one — for
        /// a scratch variant, an experiment, or a throwaway baseline. Do not
        /// copy a workweave with `cp`: the result aliases the original's git
        /// state rather than duplicating it.
        #[arg(long)]
        from: Option<String>,
        /// Allow creation even when the source project directory has
        /// uncommitted changes. The dirty state is captured into the new
        /// workweave's project worktree. Without this flag, `create` refuses
        /// and names the dirty files so you can commit, stash, or opt in
        /// explicitly.
        #[arg(long)]
        capture_dirty: bool,
        /// Cut a real `git worktree` for `role: reference` repos instead of
        /// the default symlink to the canonical weave-root clone. Restores
        /// the legacy behavior (per-workweave reference refs) at the cost of
        /// duplicating each reference repo's full working tree into this
        /// workweave. By default, reference repos are symlinked — zero
        /// working-tree duplication, byte-identical across workweaves.
        #[arg(long)]
        worktree_references: bool,
        /// Explicit per-workweave placement override.
        ///
        /// Places the workweave at exactly this path (recorded in the
        /// registry). Overrides the default container for this
        /// invocation only. Useful for big-disk workweaves, tmpfs
        /// experiments, or one-off placements. Absolute paths are used
        /// as-is; relative paths resolve against the primary root.
        #[arg(long)]
        dir: Option<String>,
    },
    /// Delete a workweave
    Delete {
        /// Workweave name
        name: String,
        /// Delete even if a worktree has uncommitted changes. Without this
        /// flag, deletion refuses and names the dirty paths.
        #[arg(long)]
        discard_uncommitted: bool,
        /// Delete even if a worktree holds commits not merged into the
        /// parent weave. Without this flag, deletion refuses and names the
        /// diverged repos. Together with `--discard-uncommitted` this is the
        /// `git branch -D` contract, which discards both.
        #[arg(long)]
        discard_unmerged_commits: bool,
    },
    /// List existing workweaves
    List,
    /// Record the workweave container for this project.
    ///
    /// Sets the `container` field in `projects/<project>/.rwv-workweave-index`
    /// — the directory `workweave create` places new workweaves under by
    /// default. Per-workweave `--dir` overrides on `create` are unaffected.
    /// The recorded container is an explicit act, visible in the checked-in
    /// tree via `.gitignore` and audit-visible via the file itself.
    SetContainer {
        /// Absolute path to record. Relative paths resolve against the
        /// primary workspace root.
        path: String,
    },
    /// Show this workweave's UNIQUE commits vs its recorded parent, per repo.
    ///
    /// Parent identity comes from the `.rwv-workweave` marker (not the branch
    /// name). Unique commits are those in the workweave's history but not the
    /// parent's — correct even when the parent advanced since the fork. Must
    /// be run from inside a workweave.
    Log {
        /// Show the workweave's unique diff vs its parent instead of the
        /// commit listing. Anchored at the common ancestor of the workweave
        /// tip and the parent tip, so commits the parent gained after the
        /// fork are not shown as reversals.
        #[arg(long)]
        diff: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum SetupAction {
    /// Generate AGENTS.md at the workspace root
    AgentsMd,
    /// Register rwv prime as a Claude Code hook (SessionStart + PreCompact)
    Claude {
        /// Remove all rwv hooks from Claude Code settings
        #[arg(long)]
        uninstall: bool,
    },
}

// ---------------------------------------------------------------------------
// Consent tokens
// ---------------------------------------------------------------------------
//
// This is the CLI layer's flag module, and it is the *only* place that can
// construct `DetachConsent`, `ReattachConsent`, `DiscardUnmergedConsent` and
// `AdoptDetachedConsent`. Not by convention — there is no route a reviewer
// has to watch for. Two compiler-checked seals, one per construction route a
// token has:
//
//  1. The tuple literal. Each struct's field is unnamed and unmarked
//     (private), and Rust resolves field privacy against the *declaring
//     module*, not the call site — so `DetachConsent(())` cannot be written
//     from any other module, in this crate or any other. Pinned per token by
//     `tests/branch_model_compile_fail_test.rs`, by error code.
//
//  2. The minting function. `from_flag` is `pub(in crate::cli)`: visible to
//     this module tree and nowhere else. The only other member of the tree
//     is `cli::dispatch`, which is where a parsed flag exists to mint from.
//     `vcs.rs` — which must only ever *receive* a token, never mint one —
//     cannot call it: `DetachConsent::from_flag(true)` there is E0624,
//     `associated function is private`.
//
// Seal 2 is why dispatch lives in `cli::dispatch` rather than in `main.rs`.
// A `[[bin]]` target is a *separate crate* from this `[lib]`, so a minting
// caller out there can only reach a `pub` constructor — and a `pub fn`
// returning the token is a second construction route reachable from every
// module of this crate — exactly the reach these seals exist to deny. The
// narrowest visibility that admits an out-of-crate caller is `pub`; the
// narrowest that admits `cli::dispatch` is `pub(in crate::cli)`.
//
// `granted()` — the unconditional mint, which checks nothing — is
// `#[cfg(test)]`, and exists only on the tokens some in-crate fixture
// actually needs one for. It is absent from the build of the library that
// the binary and the integration tests link against, so in-crate fixtures
// can still build a token while product code has no unconditional mint to
// reach for at all.
//
// House rule: escape hatches are named for the precondition they waive,
// never a bare `--force`. `--detach-checkouts` and `--reattach-checkouts`
// name two categorically different consequences — losing the name your
// commits hang off, versus moving which name they hang off — so they mint
// two tokens, not one `ChangeAttachmentConsent`.
pub mod consent {
    /// Proof that the operator consented to leaving a checkout on no
    /// branch. Minted from `--detach-checkouts`.
    ///
    /// `Copy`: a zero-sized proof token, not a capability that guards a
    /// resource — duplicating "the operator consented" is harmless, and
    /// per-repo callers (parallel fetch/update workers) each need their
    /// own value from the one token the CLI dispatch minted.
    #[derive(Debug, Clone, Copy)]
    pub struct DetachConsent(());

    impl DetachConsent {
        /// Mint unconditionally, for in-crate test fixtures that need a
        /// token without exercising CLI parsing (e.g. `git.rs`'s `Vcs` impl
        /// tests). `#[cfg(test)]`: absent from the library that the binary
        /// and the integration tests link against, so no product code — in
        /// this module or any other — has an unconditional mint to reach for.
        #[cfg(test)]
        pub(crate) fn granted() -> Self {
            Self(())
        }

        /// Mint from the parsed `--detach-checkouts` value: `Some` iff the
        /// operator passed it. Every verb's dispatch mints through here,
        /// so the flag-to-token mapping lives in exactly one place.
        ///
        /// `pub(in crate::cli)`: a parsed flag exists only in
        /// [`crate::cli::dispatch`], and confining the mint to this module
        /// tree is what turns "only the flag module can construct one" into a
        /// compile error everywhere else.
        pub(in crate::cli) fn from_flag(detach_checkouts: bool) -> Option<Self> {
            detach_checkouts.then_some(Self(()))
        }
    }

    /// Proof that the operator consented to moving a checkout from one
    /// branch to another. Minted from `--reattach-checkouts`.
    ///
    /// `Copy`: see [`DetachConsent`]'s doc comment.
    #[derive(Debug, Clone, Copy)]
    pub struct ReattachConsent(());

    impl ReattachConsent {
        /// Mint unconditionally. `#[cfg(test)]`: see
        /// [`DetachConsent::granted`]'s doc comment.
        #[cfg(test)]
        pub(crate) fn granted() -> Self {
            Self(())
        }

        /// Mint from the parsed `--reattach-checkouts` value: `Some` iff
        /// the operator passed it. `pub(in crate::cli)`: see
        /// [`DetachConsent::from_flag`]'s doc comment.
        pub(in crate::cli) fn from_flag(reattach_checkouts: bool) -> Option<Self> {
            reattach_checkouts.then_some(Self(()))
        }
    }

    /// Proof that the operator consented to discarding commits that are
    /// not merged into the baseline. Minted from
    /// `--discard-unmerged-commits`.
    ///
    /// `Copy`: see [`DetachConsent`]'s doc comment.
    #[derive(Debug, Clone, Copy)]
    pub struct DiscardUnmergedConsent(());

    impl DiscardUnmergedConsent {
        /// Mint unconditionally. `#[cfg(test)]`: see
        /// [`DetachConsent::granted`]'s doc comment.
        #[cfg(test)]
        pub(crate) fn granted() -> Self {
            Self(())
        }

        /// Mint from the parsed `--discard-unmerged-commits` value: `Some`
        /// iff the operator passed it. `pub(in crate::cli)`: see
        /// [`DetachConsent::from_flag`]'s doc comment. Integration tests
        /// that need the post-waiver behaviour of `workweave delete` enter
        /// through [`crate::cli::dispatch::workweave_delete`], which is this
        /// mint's only caller.
        pub(in crate::cli) fn from_flag(discard_unmerged_commits: bool) -> Option<Self> {
            discard_unmerged_commits.then_some(Self(()))
        }
    }

    /// Proof that the operator consented to two things a migration of a
    /// detached checkout does: minting a workweave's flat ephemeral ref **at
    /// a detached HEAD**, and — when a pre-flat branch holds the name —
    /// giving that branch's name up so the flat one can exist in its place.
    /// Minted from `--adopt-detached-checkouts`.
    ///
    /// A third token rather than a reuse of [`ReattachConsent`]: reattaching
    /// moves a checkout onto a branch that already exists and loses nothing,
    /// while this births a branch at the lock SHA and can strand a legacy
    /// branch's tip. Different consequence, different flag, different token —
    /// the house rule stated at the top of this module.
    ///
    /// `Copy`: see [`DetachConsent`]'s doc comment.
    #[derive(Debug, Clone, Copy)]
    pub struct AdoptDetachedConsent(());

    impl AdoptDetachedConsent {
        // No `granted()`: no in-crate fixture needs one yet, and an unused
        // unconditional mint is dead code the linter would reject anyway.

        /// Mint from the parsed `--adopt-detached-checkouts` value: `Some`
        /// iff the operator passed it. `pub(in crate::cli)`: see
        /// [`DetachConsent::from_flag`]'s doc comment.
        pub(in crate::cli) fn from_flag(adopt_detached_checkouts: bool) -> Option<Self> {
            adopt_detached_checkouts.then_some(Self(()))
        }
    }
}
