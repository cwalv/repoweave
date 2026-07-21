//! Snapshot test for `rwv --help` output.
//!
//! Pins the exact stdout from `rwv --help` so that accidental reordering,
//! addition, or removal of subcommands is caught by CI.  The expected string
//! was captured from the compiled binary after the fo-wbbqof.6 refactor
//! (flattened Commands enum, declaration-order grouping, no labeled sections).
//!
//! If the help text legitimately changes (new command, updated description),
//! rebuild the binary, run `rwv --help`, and update the expected string here.

mod common;

#[test]
fn rwv_help_matches_snapshot() {
    let expected = "\
A cross-repo workspace manager

Usage: rwv [OPTIONS] [COMMAND]

Commands:
  activate     Activate a project (generate ecosystem files, create symlinks, then run integration install hooks like `npm install` / `uv sync` / `cargo generate-lockfile`)
  prime        Print structured workspace context for agent system prompts
  resolve      Print workspace root path
  add          Add a repo to the active project
  fetch        Clone a project and align repos to rwv.lock (no network bump). Use `rwv update` to advance to branch HEAD. With no SOURCE, re-materialize missing manifest members of the active project (repair verb for dangling references)
  init         Initialize a new project
  remove       Remove a repo from the active project
  workweave    Create, delete, or list workweaves
  doctor       Convention enforcement and lock-freshness checking
  lock         Snapshot repo versions (pure git SHA snapshot — no integration hooks fire). Run `rwv activate` after lock changes the workspace membership to refresh node_modules / .venv / etc
  status       Show per-repo state of the CWD workspace
  abort        Restore CWD workspace to its pre-sync state using savepoint refs
  sync         Bring another workspace's committed state into this one (pull/align; use `rwv sync-to` to land work upward)
  sync-to      Advance target workspace to CWD's tip (3-step orchestration: rebase CWD against target, auto-relock, then fast-forward target to CWD's converged tip). CWD's unique commits land on top of target's prior history; target absorbs CWD's state with CWD as the newest contribution
  push         Push manifest repos and then the project repo, in that order. Refuses from a workweave. Manifest pushes are attempt-all-and-collect; project repo is gated on every manifest repo succeeding
  update       Advance each repo to its branch HEAD and re-snapshot the lock (network bump; analogous to `cargo update` / `npm update`). Use `rwv fetch` for the read-only counterpart that aligns clones to the existing lock
  completions  Generate shell completions
  explain      Agent-oriented reflection: per-verb markdown bundle (purpose, invocation, output, JSON Schema)
  setup        Generate workspace-level configuration files
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version

Global options:
  -C, --cwd <PATH>  Resolve workspace as if invoked from <path>. Any path inside a checkout works; the normal containment walk (marker, root, $HOME ceiling) runs from there. Relative path arguments elsewhere on the command line resolve against this directory. Repeating this flag is an error. If you meant to address a workweave by name, use -w/--workweave instead
";

    common::rwv()
        .arg("--help")
        .assert()
        .success()
        .stdout(expected);
}
