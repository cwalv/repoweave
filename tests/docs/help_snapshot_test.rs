//! Snapshot test for `rwv --help` output.
//!
//! Pins the exact stdout from `rwv --help` so that accidental reordering,
//! addition, or removal of subcommands is caught by CI.  The expected string
//! was captured from the compiled binary after the Commands enum was
//! flattened (declaration-order grouping, no labeled sections).
//!
//! The "External commands" section appears in `--help` only when `rwv-*`
//! executables are found on PATH. This test pins the PATH to an empty
//! directory so the section is absent — preserving snapshot stability
//! independent of what the host has installed. A separate test (below)
//! verifies the section appears and contains the right content when a
//! fixture plugin is present.
//!
//! If the core help text legitimately changes (new command, updated
//! description), rebuild the binary, run `rwv --help` with an empty PATH,
//! and update the expected string here.

use crate::common;

// `tempfile` is a dev-dependency; no explicit `use` needed since we call it
// fully qualified in the test bodies.

#[test]
fn rwv_help_matches_snapshot_no_plugins() {
    let expected = "\
A cross-repo workspace manager

Usage: rwv [OPTIONS] [COMMAND]

Commands:
  activate     Activate a project (switch `.rwv-active`, surface weave-root symlinks, then run integration install hooks like `npm install` / `uv sync` / `cargo fetch`). Moves no checkout: a member the project shares with the active project that holds edits or is not aligned for it refuses the activation; `--update` aligns first
  materialize  Settle drift, clean up disabled integrations, repair missing weave-root symlinks, and run integration install hooks for the project this checkout already presents, without selecting anything. Hooks materialize; they never move a pin
  prime        Print structured workspace context for agent system prompts
  resolve      Print weave root path
  clone        Clone a project and each of its members at the commit `rwv.lock` pins. A member the lock does not pin, or pins at a commit no ref reaches, is cloned at its declared branch's tip and listed; the exit status is then 3
  init         Initialize a new project
  member       Add a repo to the active project, remove one, pin one at a commit, or change a member's role or submodule setting
  branch       Create a weave branch, record how one is used, list the weave branches with their records, rename one, or delete one
  switch       Put the project repo on a weave branch and align the members to it, or detach the workspace at a revision. A recorded branch reopens bare; an unrecorded one is refused until `rwv branch set-mode` records it; one checked out in another workspace is refused. Aligning for the checked-out branch is `rwv update`
  workweave    Create, delete, or list workweaves
  doctor       Convention enforcement and lock-freshness checking
  lock         Record where each member's checkout is as its pin, moving a pin forward only: a member with no pin keeps none, and one behind or diverged from its pin is refused, `--repo` included; `rwv member pin` moves a pin backward by name. No integration hooks fire; run `rwv materialize` after lock changes the workspace membership to refresh node_modules / .venv / etc
  status       Show per-repo state of the CWD workspace
  log          Show the VCS's log in every repo of the workspace, or with `--against` the commits beyond what a weave branch pins
  abort        Roll the operation in progress (a sync, landing, pull or update --remote that merges or resets, or cherry-pick) back to its savepoints in every workspace it touched, run from any of them
  backup       List, drop or prune the backups rwv keeps before any step that would leave a commit or an edit unpreserved
  sync         Take the target's new work into this feature, or merge another weave branch in
  land         Land this feature into the branch its record lands into
  cherry-pick  Port one project commit onto this feature: copy each member's change, carry each dependency's pin, and commit the project change with the relock
  fetch        Bring every remote's branches and tags into each repo's object store, dropping remote branches that are gone. Moves no branch, checkout or lock, and reports what moved
  pull         Fetch, fast-forward the project branch to its upstream, then update the members to the lock it holds. A diverged branch is refused unless `--merge-upstream` merges it or `--reset-to-upstream` moves it to its upstream
  update       Clone the members missing on disk and align the rest to rwv.lock, without writing it. With --remote, advance each repo to its branch HEAD on the remote and record the advanced tips in the lock (network bump; analogous to `cargo update` / `npm update`)
  push         Publish a weave branch: its members' branches, then its record and the drops made here, then the project branch. Runs from any workspace on a recorded, named branch; never writes the lock. Pushes `owned` and `fork` members by default and a `dependency` or `reference` member only when named
  completions  Generate shell completions
  explain      Agent-oriented reflection: per-verb markdown bundle (purpose, invocation, output, JSON Schema)
  setup        Register Claude Code integration (hooks and settings) for this user
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version

Global options:
  -C, --cwd <PATH>                 Resolve workspace as if invoked from <path>. Any path inside a checkout works; the normal containment walk (marker, root, $HOME ceiling) runs from there. Relative path arguments elsewhere on the command line resolve against this directory. Repeating this flag is an error. If you meant to address a workweave by name, use -w/--workweave instead
  -w, --workweave <PROJECT--NAME>  Address a workweave by identity (<project>--<name>). The workspace is found via -C or process cwd; the workweave is then selected from the registry for the named project. Container-location-independent: the name survives placement changes that would break a path-based address. Use -C <path> when outside the ecosystem entirely; compose with -w to select a specific workweave within the located workspace. Repeating this flag is an error. If you meant to address by path, use -C instead
";

    // Pin PATH to an empty directory so no `rwv-*` plugins are discovered
    // and the "External commands" section is absent from the output.
    let empty_dir = common::tempdir().expect("tempdir");
    common::rwv()
        .arg("--help")
        .env("PATH", empty_dir.path())
        .assert()
        .success()
        .stdout(expected);
}

/// When `rwv-*` executables are present on PATH the "External commands"
/// section appears after the core usage block, listing the verb as typed.
#[test]
fn rwv_help_external_commands_section_with_fixture_plugin() {
    let plugin_dir = common::tempdir().expect("tempdir");
    common::write_plugin(plugin_dir.path(), "rwv-myplugin", "echo hi\n", "echo hi\n");

    let out = common::rwv()
        .arg("--help")
        .env("PATH", plugin_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("External commands"),
        "expected 'External commands' section in --help with fixture plugin: {text}"
    );
    assert!(
        text.lines().any(|l| l.trim() == "myplugin"),
        "expected the verb `myplugin` on a line of its own in --help: {text}"
    );
}

/// With an empty PATH (no plugins) the "External commands" section is absent.
#[test]
fn rwv_help_no_external_commands_section_with_empty_path() {
    let empty_dir = common::tempdir().expect("tempdir");
    let out = common::rwv()
        .arg("--help")
        .env("PATH", empty_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        !text.contains("External commands"),
        "unexpected 'External commands' section with empty PATH: {text}"
    );
}
