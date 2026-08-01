# Roles

Roles classify a repo's relationship to a project — its *change resistance*. They are per-project (the same repo can have different roles in different projects) and a first-class field on every `rwv.toml` entry.

## Roles

| Role | Change resistance | Meaning |
|---|---|---|
| `owned` | None | Your code. Change freely if it's an improvement. |
| `fork` | Low | Forked upstream. Ideally changes accepted upstream, but expediency is fine. |
| `dependency` | Medium | Code you build against. Changes need upstream acceptance, or convert to a fork. |
| `reference` | High | Cloned for reading/study during design. No local changes. Could be removed when done. |

### `owned`

The work surface. Repos you author or own outright. Change freely; no upstream coordination needed.

In a typical project, repos under your organization (e.g., `github/chatly/*` for a team building under `chatly`) are `owned`.

### `fork`

Role label for a fork relationship. Changes ideally flow back upstream via PR, but local patches are acceptable.

**URL is your writable fork.** The manifest `url:` field must point at your personal or team fork — the repo you have push access to. `rwv` clones that URL to `origin` and treats `fork` identically to `owned` for clone, push, and fetch.

If you also want to track the upstream-of-record, add it yourself:

```bash
cd github/chatly/engine.io
git remote add upstream https://github.com/socketio/engine.io.git
```

**Push policy.** `rwv push` pushes `fork` repos the same as `owned` repos (to `origin`).

### `dependency`

Code you build against but don't own. Changes need upstream acceptance, or convert to a `fork` for local patches.

`dependency` repos are part of the build graph — they appear in generated ecosystem workspace files (`go.work`, `package.json` workspaces, Cargo `[workspace]` members) so cross-package imports resolve locally.

Use `dependency` over `reference` when the repo is actually a build input. Use `reference` when you're reading the code for study or design and don't want it in the build graph.

### `reference`

Cloned for reading or study, not as a build input. Examples: a sibling project's repo you're consulting during a design phase; an upstream library whose internals you want to grep.

`reference` repos are:

- **Visible** in the workspace (so editors and grep see them).
- **Excluded** from generated ecosystem workspace files (`go.work`, `package.json` workspaces, Cargo workspace members) — they don't appear as build-graph nodes.
- **Read-only** by convention. `rwv doctor` will eventually warn on local changes; for now the convention is operator discipline.

`rwv doctor` reports untracked clones in registry directories as **orphans**. Tracking a clone as `reference` is how you tell `rwv` "I cloned this on purpose, leave it alone."

#### Workweave materialization: symlink, not worktree

When `rwv workweave create` materializes a workweave, `reference` repos
are created as a **symlink** at `<workweave>/<repo_path>` pointing to
the single canonical weave-root clone `<weave>/<repo_path>` — not as a
per-workweave `git worktree`. Every workweave therefore shares the same
on-disk files for reference repos; `git fetch` on the canonical clone is
immediately visible in all workweaves without updating N worktrees.

This is why reference repos are safe to share: they are read-only by
definition, so no per-workweave branch divergence needs to be isolated.
The symlink satisfies clone-topology invariant I1 (single canonical
store) by identity, and I2/I3's worktree and ephemeral-branch
requirements do not apply to symlinked references — see
[clone-topology](../explanation/joints/clone-topology.md) for the
precise carve-out.

**Escape hatch.** Pass **`--worktree-references`** to `rwv workweave
create` to restore the old behavior and cut a proper `git worktree` for
reference repos in that workweave. The resulting checkout is treated
identically to any other worktree (it has its own ephemeral branch, is
eligible for sync, and flows through all normal paths); only the default
materialization changes. This flag records nothing — the on-disk
`is_symlink()` test is the sole authority for downstream commands.

## Per-project, not per-repo

The same repo can have different roles in different projects. `engine.io` might be a `fork` in `web-app` (patched for reconnection logic) and a `dependency` in another project (used unmodified).

```toml
# projects/web-app/rwv.toml

[repositories."github/socketio/engine.io"]
url = "https://github.com/chatly/engine.io.git"
role = "fork"
```

```toml
# projects/another-app/rwv.toml

[repositories."github/socketio/engine.io"]
url = "https://github.com/chatly/engine.io.git"
role = "dependency"
```

The active project's `rwv.toml` determines the current role.

## Default for new entries

`rwv add` defaults to `role: owned` when `--role` is not specified. Override:

```bash
rwv add https://github.com/example/lib.git --role dependency
rwv add https://github.com/me/my-fork.git --role fork
rwv add https://github.com/other/code.git --role reference
```

## Heuristic, not rule

A common pattern: `github/{your-org}/*` is likely `owned`; `github/{other-org}/*` is likely `dependency` or `reference`. This is a default expectation, not enforced. The active project's `rwv.toml` always wins.

## Roles in `rwv push`

`rwv push` walks the manifest and applies per-role policy:

| Role | `rwv push` behavior |
|---|---|
| `owned` | Push (with lock-precondition check) |
| `fork` | Push (same as Owned) |
| `dependency` | Skip — you don't push upstream code |
| `reference` | Skip — read-only |

See [push a cross-repo feature](../how-to/push-cross-repo-feature.md).

## Roles in `rwv status --json`

Roles surface in `rwv status --json` output so agent harnesses and shell scripts can filter on them. See [run a command across repos](../how-to/run-a-command-across-repos.md) for filtering recipes (`jq '.repos[] | select(.role == "owned")'`).

## Migrating from `role: primary`

The `owned` role was previously spelled `primary`. The rename to `owned` resolved an overload — "primary" is also the name for the *workspace* (the non-workweave weave root, as in `rwv sync primary`). Using one word for two distinct concepts caused enough confusion to justify the rename.

**The parser does not accept the legacy spelling, and nothing rewrites it for you.** A manifest carrying `role = "primary"` fails to load, and the error names the spelling that replaced it at the line holding it:

```
$ rwv doctor
error: alpha: manifest at projects/alpha/rwv.toml cannot be parsed: TOML parse error at line 5, column 8
  |
5 | role = "primary"
  |        ^^^^^^^^^
the `primary` role spelling is no longer accepted; the role is spelled `owned`
```

Change the value to `owned`. Editing the manifest is yours because the manifest is yours — see [formats](./formats.md).

The `--role` CLI flag (`rwv add --role`, `rwv push --role`, etc.) rejects the legacy spelling through the same parser, so it answers with the same sentence.

`rwv status --json` and other machine-readable outputs always emit the canonical `"owned"` spelling; that contract was unchanged by this revision.

## Related

- [workspace lens — Roles](../explanation/lenses/workspace.md#roles-change-resistance-made-explicit) — the conceptual frame
- [reference/cli — Selector grammar](./cli.md#selector-grammar) — `--role` filtering on action verbs
- [reference/formats](./formats.md) — `rwv.toml` shape
