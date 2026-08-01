# rwv remove

## Purpose

Remove a repo from the active project's `rwv.toml` and re-run integration
activation so ecosystem files (Cargo workspace, npm workspace, etc.) are
updated to no longer include the removed member.

`remove` is an **intent verb**: it mutates the manifest and regenerates
integration-managed content so the resulting files can be committed alongside
the `rwv.toml` change.

Removing from the manifest does **not** delete the clone from disk by default.
The clone remains at its canonical path under the primary weave root (shared
infrastructure — other projects may reference it). Use `--delete` to also
remove the on-disk directory.

### `--delete` refusal rules

`--delete` removes the clone directory after unregistering it from the
manifest. It will refuse (with an error) if another project's `rwv.toml`
references the same path — deleting shared infrastructure would break that
project's fetch/sync operations.

Use `--delete-shared-clone` alongside `--delete` to remove the clone directory
regardless. Use this only when you have verified that no other project uses the
repo, or when you intend to break those references.

### Clone placement

Clones live at the canonical path under the primary weave root, not inside
workweaves (which use `git worktree`). `remove --delete` therefore removes the
canonical clone, which also invalidates any live worktrees pointing at it.
(See `rwv explain workweave` for the workweave topology model.)

## Invocation

```
rwv remove <path> [--delete] [--delete-shared-clone] [--project <name>]
```

- `<path>` — manifest-relative path of the repo to remove (as listed in
  `rwv.toml`, e.g. `github/myorg/myrepo`).
- `--delete` — also remove the clone directory from disk. Errors if another
  project references the same path; waive with `--delete-shared-clone`.
- `--delete-shared-clone` — delete the clone even when another project still
  references it. Has no effect without `--delete`.
- `--project <name>` — operate on this project rather than the active project.
  Does not change `.rwv-active`.

Run `rwv --help remove` for the full clap surface.

## Output

Progress messages to stderr:

- Manifest rewrite confirmation.
- Integration activation output (symlinks created/removed).
- When `--delete` removes the directory: confirmation or error.

## Exit codes

- `0` — repo removed from manifest, activation succeeded.
- non-zero — path not found in manifest, `--delete` cross-project check
  failed (without `--delete-shared-clone`), directory removal failed, or
  activation returned an error.

## Examples

Remove from manifest (keep clone on disk):

```
rwv remove github/myorg/old-repo
```

Remove from manifest and delete the clone:

```
rwv remove github/myorg/old-repo --delete
```

Remove and delete the clone even if another project references it:

```
rwv remove github/myorg/old-repo --delete --delete-shared-clone
```

## Common errors

- *path not in manifest* — the given path does not appear in `rwv.toml`. Check
  the exact manifest path with `rwv status`.
- *another project references this path; refusing --delete without
  --delete-shared-clone* — the clone is shared. Verify the other project and
  use `--delete-shared-clone` only when safe.
- *manifest parse failure* — `rwv.toml` could not be loaded; verify the file
  is valid YAML.
