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
use operand::Operand;

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
        /// Project to clone: a URL (`file://` for a local remote) or `<registry>/<owner>/<repo>` shorthand
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
        /// Project name (or URL / `<registry>/<owner>/<repo>` shorthand when --adopt is used)
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

    /// Create a weave branch, record how one is used, list the weave branches with their records, rename one, or delete one
    #[command(arg_required_else_help = true)]
    Branch {
        #[command(subcommand)]
        action: BranchAction,
    },

    /// Put the project repo on a weave branch and align the members to it, or detach the workspace at a revision. A recorded branch reopens bare; an unrecorded one is refused until `rwv branch set-mode` records it; one checked out in another workspace is refused. Aligning for the checked-out branch is `rwv update`
    ///
    /// The project repo moves as the VCS moves it, carrying edits that the move
    /// does not overwrite, and every member it would move is judged first:
    /// nothing moves while any is refused. An uncommitted `rwv.toml` or
    /// `rwv.lock` is refused, and a checkout mid-operation. From a detached
    /// project repo, what it holds that no branch reaches is kept as a backup
    /// first. `--detach` makes the workspace a detached one: every repo at the
    /// pins of the revision, and no record is written. A branch whose `rwv.toml`
    /// an earlier release wrote is switched onto and stops before the members
    /// (`rwv doctor --fix` there).
    #[command(arg_required_else_help = true)]
    Switch {
        /// The branch to switch to, local or on a remote as last fetched
        #[arg(value_name = "W", conflicts_with = "detach")]
        branch: Option<String>,
        /// Detach the workspace at this revision instead: every repo sits at the pins of its lock, and no record is written
        #[arg(long, value_name = "REV", conflicts_with = "branch")]
        detach: Option<String>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
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
        /// Repair every finding marked Auto-fixable in docs/reference/doctor-findings.md, which carries that mark on each finding it documents. Never touches live staged content or live edits. Idempotent. Active-project scoped; use --all to widen.
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
    /// Record where each member's checkout is as its pin, moving a pin forward only: a member with no pin keeps none, and one behind or diverged from its pin is refused, `--repo` included; `rwv member pin` moves a pin backward by name. No integration hooks fire; run `rwv materialize` after lock changes the workspace membership to refresh node_modules / .venv / etc.
    Lock {
        /// Record each member's committed HEAD although it has uncommitted changes to tracked files, leaving those edits out of the lock
        #[arg(long)]
        dirty: bool,
        /// Record only the repos matching this selector; it narrows only, and moves no pin backward. Bare strings match exactly; `re:<pat>` matches as regex; `glob:<pat>` matches as glob. Repeat for union.
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
        /// Plumbing: merge three rwv.lock files as the lock merge driver. This is the command `merge.rwv-lock.driver` names, which git runs from the project repo with the base, ours and theirs files it wrote: pins merge three-way, the result is written over OURS and the exit is 0; a merge it cannot settle writes both locks between conflict markers over OURS and exits 1, leaving rwv.lock conflicted
        #[arg(
            long = "merge-driver",
            num_args = 3,
            value_names = ["BASE", "OURS", "THEIRS"],
            conflicts_with_all = ["dirty", "repos", "commit", "message", "project"]
        )]
        merge_driver: Option<Vec<std::path::PathBuf>>,
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
    /// Roll an in-flight sync or landing back to its savepoints in every workspace it touched, run from any of them
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
    /// Take the target's new work into this feature, or merge another weave branch in
    ///
    /// In a feature, each member takes in the branch it lands on, then the project branch
    /// takes in the target's tip, with the relock. A member with no new work follows forward;
    /// work built on another lineage is refused before anything moves. `--from <branch>`
    /// merges that branch's history instead, in any weave branch.
    ///
    /// Examples:
    ///
    ///   rwv sync
    ///
    ///   rwv sync --strategy rebase
    ///
    ///   rwv sync --from stable
    ///
    ///   rwv sync --reset-to-target github/o/app
    Sync {
        /// Merge this weave branch's history in for one run, as a merge commit, whatever the setting
        #[arg(long, value_name = "BRANCH", conflicts_with_all = ["do_continue", "target"])]
        from: Option<String>,
        /// Take in this branch instead of the record's target, for one run
        #[arg(long, value_name = "T", conflicts_with = "do_continue")]
        target: Option<String>,
        /// Move this repo's branch to the branch it lands on, keeping a backup and listing the new work it drops; repeat per repo (`projects/<p>` for the project branch, to the target's tip with its lock)
        #[arg(
            long = "reset-to-target",
            value_name = "REPO",
            conflicts_with = "do_continue"
        )]
        reset_to_target: Vec<crate::manifest::RepoOperand>,
        /// How a branch holding new work takes in: ff (fast-forward only, the default) or rebase
        #[arg(long, default_value = "ff", value_enum, conflicts_with = "do_continue")]
        strategy: SyncStrategy,
        /// Emit the refusal envelope as JSON. See `rwv explain sync`.
        #[arg(long, conflicts_with = "do_continue")]
        json: bool,
        /// Run up to N per-repo fetches in parallel. Default: 1 (serial).
        #[arg(short = 'j', long = "jobs", conflicts_with = "do_continue")]
        jobs: Option<usize>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Resume a sync a conflict stopped, after resolving it; every parameter comes from the op record
        #[arg(long = "continue")]
        do_continue: bool,
    },
    /// Land this feature into the branch its record lands into
    ///
    /// The work is composed in this workspace first: each member that holds work the
    /// branch it lands on lacks is rebased onto that branch's tip, the project branch onto
    /// the target's tip, then the lock records the result. Then each receiving branch and
    /// the target fast-forward where the target is checked out, that workspace aligns, and
    /// this workspace's members follow the target's new pins. Every refusal is reported
    /// before anything moves.
    ///
    /// Examples:
    ///
    ///   rwv land
    ///
    ///   rwv land --retire
    ///
    ///   rwv land --target stable
    Land {
        /// Land into this branch instead of the record's target, for one run
        #[arg(long, value_name = "T", conflicts_with = "do_continue")]
        target: Option<String>,
        /// How a member holding work the branch it lands on lacks is composed: rebase (the
        /// default) replays it onto that branch's tip; ff refuses unless every repo
        /// fast-forwards
        #[arg(
            long,
            default_value = "rebase",
            value_enum,
            conflicts_with = "do_continue"
        )]
        strategy: SyncStrategy,
        /// Once the landing is done, remove the workweave and the feature's branches by
        /// the deletion rule: a branch holding anything not landed is kept and listed
        #[arg(long, conflicts_with = "do_continue")]
        retire: bool,
        /// Emit per-repo outcomes as JSON. With `--continue`, the envelope carries a
        /// `resumed` field naming the phase the stopped landing had reached. See `rwv
        /// explain land`.
        #[arg(long)]
        json: bool,
        /// Run up to N per-repo steps in parallel. Default: 1 (serial).
        #[arg(short = 'j', long = "jobs", conflicts_with = "do_continue")]
        jobs: Option<usize>,
        /// Operate on this project instead of the active project (does not change `.rwv-active`)
        #[arg(long)]
        project: Option<String>,
        /// Resume a landing a conflict stopped, after resolving it; every parameter comes
        /// from the op record. `--project` asserts the recorded project and refuses another
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
    /// Fetch, fast-forward the project branch to its upstream, then update the members to the lock it holds. A diverged branch is refused unless `--merge-upstream` merges it or `--reset-to-upstream` moves it to its upstream.
    Pull {
        /// Merge a diverged project branch with its upstream, first parent the upstream, merging each member whose pin both sides changed first, and recording the merged pins in the commit. Runs under an operation record: a conflict stops it, `--continue` resumes it, and `rwv abort` puts everything back
        #[arg(long, conflicts_with = "do_continue")]
        merge_upstream: bool,
        /// Move the project branch to its upstream, keeping the tip it leaves in a backup. The repo is `projects/<name>`; refused where the upstream is gone, a record lands into the branch, or the checkout has edits
        #[arg(long, value_name = "REPO", conflicts_with = "do_continue")]
        reset_to_upstream: Vec<crate::manifest::RepoOperand>,
        /// Resume the pull a conflict stopped, from the workspace it runs in
        #[arg(long = "continue")]
        do_continue: bool,
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
        /// With --remote, merge each acted-on member branch that has diverged from its upstream, first parent the upstream. A branch no checkout has attached merges as refs and touches no working tree. Runs under an operation record: a conflict stops it, `--continue` resumes it, and `rwv abort` puts everything back
        #[arg(long, requires = "remote", conflicts_with = "do_continue")]
        merge_upstream: bool,
        /// With --remote, move this member's branch to its upstream, keeping the tip it leaves in a backup. Repeat the flag per repo; refused where the upstream is gone, a record lands into the branch, or the checkout has edits
        #[arg(
            long,
            value_name = "REPO",
            requires = "remote",
            conflicts_with = "do_continue"
        )]
        reset_to_upstream: Vec<crate::manifest::RepoOperand>,
        /// With --remote, resume the update a conflict stopped, from the workspace it runs in
        #[arg(long = "continue", requires = "remote")]
        do_continue: bool,
        /// Record each member's committed HEAD although it has uncommitted changes to tracked files, leaving those edits out of the lock
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
    /// Drop the named backups of one repo, or with `--all` every backup of it, printing each tip. Refused where it would leave a commit some project's live lock pins reached by no branch, remote-tracking branch or backup
    #[command(
        group(clap::ArgGroup::new("which").required(true).args(["names", "all"])),
        override_usage = "rwv backup drop <REPO> <NAMES>...\n       rwv backup drop <REPO> --all"
    )]
    Drop {
        /// The repo: its workspace-relative path, or `projects/<name>` for the project repo
        repo: crate::manifest::RepoOperand,
        /// Backups to drop, as `rwv backup list` names them
        names: Vec<String>,
        /// Drop every backup of the repo instead of naming them
        #[arg(long)]
        all: bool,
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
    /// Record how the checked-out weave branch accepts landings: its top-level `history`, or with `--repo` one member's, which overrides the top-level one for that member. Commits only this change
    ///
    /// `linear` fast-forwards, else rebases onto the receiving branch;
    /// `merge` fast-forwards, else makes a merge commit; `ff-only` lands only
    /// by fast-forward. No value is the default. `--unset` removes the key: a
    /// member's then follows the top-level setting. Other edits to `rwv.toml`
    /// stay uncommitted where they are.
    #[command(override_usage = "rwv branch set-history [OPTIONS] <linear|merge|ff-only|--unset>")]
    SetHistory {
        /// The setting to record
        #[arg(
            value_enum,
            required_unless_present = "unset",
            conflicts_with = "unset"
        )]
        history: Option<manifest::History>,
        /// Remove the setting instead
        #[arg(long)]
        unset: bool,
        /// The member whose own setting to write, as its workspace-relative path; `projects/<name>` names the top-level setting, as no `--repo` does
        #[arg(long, value_name = "REPO")]
        repo: Option<crate::manifest::RepoOperand>,
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
    /// Create a weave branch, record it, and switch the workspace to it: bare, a feature onto the checked-out branch; `--target <T>`, a feature onto `<T>`; `--declared`, a branch whose members follow its own entries. The branch starts at `--at`, else at the checked-out commit (a bare create or `--target`: at the tip of the branch it lands into)
    ///
    /// The workspace switches as the VCS's own branch creation does, carrying
    /// uncommitted edits, an uncommitted `rwv.toml` included, and an
    /// uncommitted `rwv.lock` too where the branch starts at the checked-out
    /// commit. From a detached project repo a spelling is required and the
    /// branch starts at its HEAD. A feature's `owned` and `fork` members get
    /// branches of the same name at their pins. A name any repo of the
    /// workspace already has is refused, nothing created.
    Create {
        /// The new branch
        branch: String,
        /// Record it as a feature that lands into `<T>`, which must itself have a record
        #[arg(long, value_name = "T", conflicts_with = "declared")]
        target: Option<String>,
        /// Record it as `declared`: its members follow the branches its own `rwv.toml` entries declare
        #[arg(long)]
        declared: bool,
        /// Declare an existing branch for a member, in the branch's first commit: `<member>=<branch>`, the whole remainder being the branch name; repeat per member
        #[arg(long, value_name = "M=B")]
        set_branch: Vec<String>,
        /// Create a branch and declare it for a member, in the branch's first commit: `<member>=<branch>`, the whole remainder being the branch name; repeat per member
        #[arg(long, value_name = "M=B")]
        set_new_branch: Vec<String>,
        /// Start the branch a `--set-branch` or `--set-new-branch` names for the same member at this revision: `<member>=<rev>`; repeat per member
        #[arg(long, value_name = "M=REV")]
        pin: Vec<String>,
        /// Start the branch at this revision
        #[arg(long, value_name = "REV")]
        at: Option<String>,
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
        /// Role for the repo (`owned` when not given)
        #[arg(long, value_enum)]
        role: Option<manifest::Role>,
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
#[allow(clippy::large_enum_variant)]
pub enum WorkweaveAction {
    /// Create a workweave, or open or detach one, from the workspace the verb runs in. That workspace is provenance only: it does not switch, and its uncommitted files stay where they are, reported
    ///
    /// With no branch flag the workweave holds a feature under the minted
    /// name `<project>--<ww>`, onto the branch the workspace has checked out,
    /// never published. `--new-branch <W>` creates `W` as `branch create`
    /// does; `--branch <W>` opens `W` by its record, a branch another
    /// workspace has checked out refused; `--detach <rev>` makes a detached
    /// workspace at any revision and writes no record. Re-running an
    /// identical spelling on a clean workweave returns it, reported; any
    /// difference is refused and changes nothing.
    Create {
        /// Workweave name
        name: String,
        /// Create this weave branch for the workweave, as `branch create` creates one
        #[arg(
            long,
            value_name = "W",
            conflicts_with_all = ["branch", "detach"]
        )]
        new_branch: Option<String>,
        /// Open this weave branch by its record. A branch with no record is refused until `rwv branch set-mode` records it; a branch checked out in another workspace is refused
        #[arg(long, value_name = "W", conflicts_with = "detach")]
        branch: Option<String>,
        /// Make a detached workspace at this revision: every repo sits at the pins of its lock, a branch another workspace has checked out included, and no record is written
        #[arg(
            long,
            value_name = "REV",
            conflicts_with_all = ["target", "declared", "at", "set_branch", "set_new_branch", "pin"]
        )]
        detach: Option<String>,
        /// Record the created branch as a feature that lands into `<T>`, which must itself have a record
        #[arg(
            long,
            value_name = "T",
            conflicts_with_all = ["declared", "branch"]
        )]
        target: Option<String>,
        /// Record the created branch as `declared`: its members follow the branches its own `rwv.toml` entries declare
        #[arg(long, conflicts_with = "branch")]
        declared: bool,
        /// Declare an existing branch for a member, in the branch's first commit: `<member>=<branch>`, the whole remainder being the branch name; repeat per member
        #[arg(
            long,
            value_name = "M=B",
            conflicts_with_all = ["branch", "detach"]
        )]
        set_branch: Vec<String>,
        /// Create a branch and declare it for a member, in the branch's first commit: `<member>=<branch>`, the whole remainder being the branch name; repeat per member
        #[arg(
            long,
            value_name = "M=B",
            conflicts_with_all = ["branch", "detach"]
        )]
        set_new_branch: Vec<String>,
        /// Start the branch a `--set-branch` or `--set-new-branch` names for the same member at this revision: `<member>=<rev>`; repeat per member
        #[arg(
            long,
            value_name = "M=REV",
            conflicts_with_all = ["branch", "detach"]
        )]
        pin: Vec<String>,
        /// Start the branch at this revision
        #[arg(long, value_name = "REV", conflicts_with_all = ["branch", "detach"])]
        at: Option<String>,
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
