# rwv role

## Purpose

Change a repo's `role` in this workspace's `rwv.toml`, regenerate the
integration-managed files from the edited manifest, and settle the repo's
files for its new role.

`role` is an **intent verb**, in the family of `add` and `remove`: it mutates
the manifest and regenerates the integrations' files so the result can be
committed alongside the `rwv.toml` change. The regeneration matters because a
`reference` repo is excluded from every ecosystem's build graph, so making a
repo a reference removes it from a generated Cargo workspace, npm workspace
or `go.work`, and giving it back another role puts it back.

Settling is what makes the role true on disk:

- **To `reference`:** every tracked regular file of the repo loses its write
  bits. Directories keep theirs, so untracked files can still be created and
  the VCS can still replace files on an update. Tracked symlinks are never
  chmod'ed.
- **From `reference`:** the owner's write bit comes back. A file that shares
  its storage with another workspace's copy is first replaced by a private
  copy, because write bits belong to the file rather than to the name.

The change is local to this workspace until it lands. `rwv sync-to` carries
it to the target, which settles the repo on landing, and every other
workspace settles it on its next `rwv sync`. A hand edit of `role` in
`rwv.toml` converges the same way, and `rwv doctor --fix` settles it in
place (`rwv explain member-permissions`).

## Invocation

```
rwv role <repo> <role> [--project <name>] [--no-materialize]
```

- `<repo>` — the repo's manifest-relative path, as `rwv.toml` lists it (e.g.
  `github/myorg/myrepo`).
- `<role>` — `owned`, `fork`, `dependency` or `reference`. Any other value is
  rejected before anything is read.
- `--project <name>` — operate on this project rather than the active project.
  Does not change `.rwv-active`.
- `--no-materialize` — skip the integration install hooks (`npm install`,
  `uv sync`, `cargo fetch`, …) of the regeneration this runs at primary;
  `rwv materialize` runs them later. Inside a workweave the hooks are skipped
  either way.

Run `rwv --help role` for the full clap surface.

## Output

Progress messages go to stderr: the integration activation output, then the
change made. When the repo already has the role, `role` leaves `rwv.toml`
alone, regenerates from it, settles the member's files for that role (so it
finishes a hand edit of the role), and reports how many files it settled.

A side effect the verb skipped is said on stderr in one line naming the
command that performs it; nothing is said when it skipped none:

- with `--no-materialize`: `[skipped] core: the install hooks did not run
  (--no-materialize); `rwv materialize` runs them`;
- inside a workweave, where an authoring verb runs no hooks: `[skipped] core:
  the install hooks did not run (an authoring verb runs none inside a
  workweave); `rwv materialize` runs them`;
- for a project the weave root does not present (`--project`), whose files it
  regenerates and stops there: `[skipped] core: the weave-root links were not
  refreshed (`<project>` is not the project this weave root presents); `rwv
  activate <project>` refreshes them`, and the same line for the install hooks,
  ending `runs them`;
- for a hook whose tool is not on `PATH`: its `tool-missing` warning.

## Exit codes

- `0` — the role changed (or already was the one asked for).
- non-zero — the path is not in the manifest; the change is to `reference`
  while the repo has uncommitted changes to tracked files (commit, stash or
  discard them first); or the regeneration failed.

The manifest edit and the regeneration land together or not at all: when
activation refuses before the install hooks run, `rwv.toml` and every
regenerated file are put back as they were, and the repo's files keep the
permissions of the role they had. The install hooks run after that point and
have no undo, so when one fails the error says `rwv.toml` was updated, and the
repo is settled for its new role regardless.

## Examples

Keep a repo around as read-only study material:

```
rwv role github/myorg/vendored-lib reference
```

Start working on it again:

```
rwv role github/myorg/vendored-lib owned
```
