# rwv remove

## Purpose

Remove a repo from the active project's `rwv.toml` and re-run integration
activation so ecosystem files (Cargo workspace, npm workspace, etc.) are
updated to no longer include the removed member.

`remove` is an **authoring verb**: it mutates the manifest and regenerates
integration-managed content so the resulting files can be committed alongside
the `rwv.toml` change.

Removing from the manifest does **not** delete the checkout from disk by
default. Use `--delete` to also remove it.

### Finishing a hand edit

An entry already removed from `rwv.toml` by hand is finished by the same
command. When the path is no longer in the manifest but this workspace still
holds a checkout there that is this project's — this project's `rwv.lock`
still names it, or no other project lists it — `remove` regenerates the
integration files without it and says it was already out of the manifest.
`--delete` then deletes that checkout exactly as it would have deleted a
listed one. A path with no such checkout refuses (`repo-not-in-manifest`).

### What `--delete` deletes

`--delete` removes the checkout in the workspace the command runs in:

- **In the primary weave**, the canonical clone at its path under the weave
  root.
- **In a workweave**, that workweave's worktree of the repo, and the branch
  the workweave was given in the canonical clone, so nothing of the
  workweave's still claims that clone. A branch holding commits that neither
  the workweave's parent nor the primary weave holds stays, and a warning
  names it. The canonical clone in the primary is never touched from a
  workweave; it is removed when the change lands and the primary drops the
  repo, or by `rwv remove <path> --delete` in the primary weave.

Every guard runs before the manifest is written, so a refused delete leaves
`rwv.toml` and the checkout as they were:

- *uncommitted changes* in the checkout refuse (`dirty-checkout`);
- *commits no other clone reaches* refuse
  (`dropped-repo-has-unique-commits`): in the primary, any branch with no
  remote counterpart or with commits ahead of it; in a workweave, a worktree
  tip the canonical clone does not contain;
- in the primary, a clone **another project's `rwv.toml` lists** refuses
  (`shared-clone-referenced`) unless `--delete-shared-clone` is given, and a
  clone that **live workweave worktrees or standing receipts** still claim
  refuses (`store-still-claimed`).

(See `rwv explain workweave` for the workweave topology model.)

## Invocation

```
rwv remove <path> [--delete] [--delete-shared-clone] [--project <name>] [--no-materialize]
```

- `<path>` — manifest-relative path of the repo to remove (as listed in
  `rwv.toml`, e.g. `github/myorg/myrepo`).
- `--delete` — also remove this workspace's checkout from disk (see "What
  `--delete` deletes" above).
- `--delete-shared-clone` — delete the clone even when another project still
  references it. Requires `--delete`.
- `--project <name>` — operate on this project rather than the active project.
  Does not change `.rwv-active`.
- `--no-materialize` — skip the integration install hooks (`npm install`,
  `uv sync`, `cargo fetch`, …) of the regeneration this runs at primary;
  `rwv materialize` runs them later. Inside a workweave the hooks are skipped
  either way.

Run `rwv remove --help` for the full clap surface.

## Output

Progress messages to stderr:

- `Removed '<path>' from manifest`, or `'<path>' was already out of the
  manifest; regenerated without it` for an entry removed by hand.
- Integration activation output (symlinks created/removed).
- `Deleted '<checkout>'` when `--delete` removed one.

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

- `0` — repo removed from manifest (or already out of it), activation
  succeeded.
- non-zero — no entry and no leftover checkout at the path, a `--delete`
  guard refused, directory removal failed, or activation returned an error.

The manifest edit and the regeneration it drives land together or not at all.
When activation refuses before the install hooks run, `rwv.toml` and every
regenerated file are put back as they were, the error says so, and `--delete`
deletes nothing. The install hooks run after that point and have no undo, so
when one fails the error says `rwv.toml` was updated.

## Examples

Remove from manifest (keep clone on disk):

```
rwv remove github/myorg/old-repo
```

Remove from manifest and delete this workspace's checkout:

```
rwv remove github/myorg/old-repo --delete
```

Remove and delete the clone even if another project references it:

```
rwv remove github/myorg/old-repo --delete --delete-shared-clone
```

## Common errors

- *path not in manifest* — the given path does not appear in `rwv.toml` and no
  leftover checkout of this project's is there. Check the exact manifest path
  with `rwv status`.
- *uncommitted changes* or *unique commits* — `--delete` would lose work.
  Commit and push it, or discard it, then re-run.
- *another project references this path; refusing --delete without
  --delete-shared-clone* — the clone is shared. Verify the other project and
  use `--delete-shared-clone` only when safe.
- *manifest parse failure* — `rwv.toml` could not be loaded; verify the file
  is valid TOML.
