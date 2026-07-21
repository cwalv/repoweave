//! Snapshot test for `rwv --help` output.
//!
//! Pins the exact stdout from `rwv --help` so that accidental reordering,
//! addition, or removal of subcommands is caught by CI.  The expected string
//! was captured from the compiled binary after the fo-wbbqof.6 refactor
//! (flattened Commands enum, declaration-order grouping, no labeled sections).
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

mod common;

// `tempfile` is a dev-dependency; no explicit `use` needed since we call it
// fully qualified in the test bodies.

#[test]
fn rwv_help_matches_snapshot_no_plugins() {
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
  -C, --cwd <PATH>                 Resolve workspace as if invoked from <path>. Any path inside a checkout works; the normal containment walk (marker, root, $HOME ceiling) runs from there. Relative path arguments elsewhere on the command line resolve against this directory. Repeating this flag is an error. If you meant to address a workweave by name, use -w/--workweave instead
  -w, --workweave <PROJECT--NAME>  Address a workweave by identity (<project>--<name>). The workspace is found via -C or process cwd; the workweave is then selected from the registry for the named project. Container-location-independent: the name survives placement changes that would break a path-based address. Use -C <path> when outside the ecosystem entirely; compose with -w to select a specific workweave within the located workspace. Repeating this flag is an error. If you meant to address by path, use -C instead
";

    // Pin PATH to an empty directory so no `rwv-*` plugins are discovered
    // and the "External commands" section is absent from the output.
    let empty_dir = tempfile::tempdir().expect("tempdir");
    common::rwv()
        .arg("--help")
        .env("PATH", empty_dir.path())
        .assert()
        .success()
        .stdout(expected);
}

/// When `rwv-*` executables are present on PATH the "External commands"
/// section appears after the core usage block.
#[test]
fn rwv_help_external_commands_section_with_fixture_plugin() {
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let plugin_dir = tempfile::tempdir().expect("tempdir");
    let script_path = plugin_dir.path().join("rwv-myplugin");
    {
        let mut f = fs::File::create(&script_path).unwrap();
        write!(f, "#!/bin/sh\necho hi\n").unwrap();
    }
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

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
        text.contains("myplugin"),
        "expected fixture plugin name 'myplugin' in --help section: {text}"
    );
}

/// With an empty PATH (no plugins) the "External commands" section is absent.
#[test]
fn rwv_help_no_external_commands_section_with_empty_path() {
    let empty_dir = tempfile::tempdir().expect("tempdir");
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
