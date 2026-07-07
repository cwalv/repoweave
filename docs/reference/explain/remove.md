# rwv remove

## Purpose

Remove a repo from the active project's `rwv.yaml` and re-run integration
activation so ecosystem files (Cargo workspace, npm workspace, etc.) are
updated to no longer include the removed member.

`remove` is an **intent verb**: it mutates the manifest and regenerates
integration-managed content so the resulting files can be committed alongside
the `rwv.yaml` change.

Removing from the manifest does **not** delete the clone from disk by default.
The clone remains at its canonical path under the primary weave root (shared
infrastructure — other projects may reference it). Use `--delete` to also
remove the on-disk directory.

### `--delete` refusal rules

`--delete` removes the clone directory after unregistering it from the
manifest. It will refuse (with an error) if another project's `rwv.yaml`
references the same path — deleting shared infrastructure would break that
project's fetch/sync operations.

Use `--force` alongside `--delete` to bypass the cross-project safety check
and remove the clone directory regardless. Use this only when you have verified
that no other project uses the repo, or when you intend to break those
references.

### Clone placement

Clones live at the canonical path under the primary weave root, not inside
workweaves (which use `git worktree`). `remove --delete` therefore removes the
canonical clone, which also invalidates any live worktrees pointing at it.
(See `rwv explain workweave` for the workweave topology model.)

## Invocation

```
rwv remove <path> [--delete] [--force] [--project <name>]
```

- `<path>` — manifest-relative path of the repo to remove (as listed in
  `rwv.yaml`, e.g. `github/myorg/myrepo`).
- `--delete` — also remove the clone directory from disk. Errors if another
  project references the same path; bypass with `--force`.
- `--force` — skip the cross-project safety check when `--delete` is set.
  Has no effect without `--delete`.
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
  failed (without `--force`), directory removal failed, or activation
  returned an error.

## Examples

Remove from manifest (keep clone on disk):

```
rwv remove github/myorg/old-repo
```

Remove from manifest and delete the clone:

```
rwv remove github/myorg/old-repo --delete
```

Remove and force-delete even if another project references the clone:

```
rwv remove github/myorg/old-repo --delete --force
```

## Common errors

- *path not in manifest* — the given path does not appear in `rwv.yaml`. Check
  the exact manifest path with `rwv status`.
- *another project references this path; refusing --delete without --force* —
  the clone is shared. Verify the other project and use `--force` only when
  safe.
- *manifest parse failure* — `rwv.yaml` could not be loaded; verify the file
  is valid YAML.
