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

**URL is your writable fork.** The manifest `url` field must point at your personal or team fork — the repo you have push access to. `rwv` clones that URL to `origin` and treats `fork` identically to `owned` for clone, push, and fetch.

**The path may name either.** The manifest key is a placement, not a claim about ownership. `rwv add https://github.com/me/my-fork.git --role fork` places at `github/me/my-fork`; an entry keyed on the upstream's coordinates against a fork URL — the arrangement the [formats](./formats.md) example shows — is equally valid. `rwv doctor` does not report the difference for a `fork`-role entry.

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
- **Read-only.** Every tracked regular file of a `reference` repo has no write bits, in the weave and in every workweave. Directories stay writable, so untracked files (build output, editor swap files) can still be created and the VCS can still replace files when the repo moves. `rwv doctor` reports a `reference` repo with writable tracked files as [`member-permissions`](./doctor-findings.md#member-permissions). In the weave, one clone serves every project that lists it, so it is read-only only when all of them declare it `reference`; where they disagree it stays writable, and `rwv doctor` reports [`role-disagreement`](./doctor-findings.md#role-disagreement).

`rwv doctor` reports untracked clones in registry directories as **orphans**. Tracking a clone as `reference` is how you tell `rwv` "I cloned this on purpose, leave it alone."

#### In a workweave

A workweave holds a `reference` repo the way it holds every other repo: as a
worktree of the canonical weave-root clone, on the workweave's own ephemeral
branch. Where the filesystem supports copy-on-write cloning, a new
workweave's files are cloned from another workspace's copy, so it costs disk
only where its files differ. On Linux, a `reference` repo's read-only files
can instead be hard links to another workspace's read-only copy of the same
file. Neither changes what the repo holds: its files have the same bytes and
the same permissions however they were placed.

Changing a repo's role — with `rwv role` (`rwv explain role`) or by editing
`rwv.toml` — changes only its files' permissions; the checkout stays as it is.

## Per-project, not per-repo

The same repo can have different roles in different projects. `engine.io` might be a `fork` in `web-app` (patched for reconnection logic) and a `dependency` in another project (used unmodified).

```toml
# projects/web-app/rwv.toml

[repositories."github/socketio/engine.io"]
type = "git"
url = "https://github.com/chatly/engine.io.git"
version = "main"
role = "fork"
```

```toml
# projects/another-app/rwv.toml

[repositories."github/socketio/engine.io"]
type = "git"
url = "https://github.com/chatly/engine.io.git"
version = "main"
role = "dependency"
```

The active project's `rwv.toml` determines the current role.

## Default for new entries

`rwv add` defaults to `role = "owned"` when `--role` is not specified. Override:

```bash
rwv add https://github.com/example/lib.git --role dependency
rwv add https://github.com/me/my-fork.git --role fork
rwv add https://github.com/other/code.git --role reference
```

## Roles in `rwv push`

`rwv push` walks the manifest and applies per-role policy:

| Role | `rwv push` behavior |
|---|---|
| `owned` | Push (with lock-precondition check) |
| `fork` | Push (same as `owned`) |
| `dependency` | Skip — you don't push upstream code |
| `reference` | Skip — read-only |

See [push a cross-repo feature](../how-to/push-cross-repo-feature.md).

## Roles in `rwv status --json`

Roles surface in `rwv status --json` output so agent harnesses and shell scripts can filter on them. `rwv each --role <role>` runs a command in the repos with that role; see [run a command across repos](../how-to/run-a-command-across-repos.md).

## Migrating from `role: primary`

The role is spelled `owned`, not `primary`: "primary" names the *workspace* (the non-workweave weave root, as in `rwv sync primary`), and one word for two concepts is what the spelling avoids.

**The parser does not accept `primary` as a role, and nothing rewrites it for you.** A manifest carrying `role = "primary"` fails to load, and the error names the spelling to use at the line holding it:

```
$ rwv doctor
[error: unparseable-project] core: alpha: failed to parse rwv.toml at projects/alpha/rwv.toml: rwv.toml is yours to edit; rwv will not rewrite a file it could not parse: TOML parse error at line 5, column 8
  |
5 | role = "primary"
  |        ^^^^^^^^^
the `primary` role spelling is no longer accepted; the role is spelled `owned`
```

Change the value to `owned`. Editing the manifest is yours because the manifest is yours — see [formats](./formats.md).

The `--role` CLI flag (`rwv add --role`, `rwv push --role`, etc.) reads its value through the same parser, so it refuses `primary` with the same sentence (`unknown-role`).

`rwv status --json` and other machine-readable outputs emit the role as `"owned"`.

## Related

- [workspace lens — Roles](../explanation/lenses/workspace.md#roles-change-resistance-made-explicit) — the conceptual frame
- [reference/cli — Selector grammar](./cli.md#selector-grammar) — `--role` filtering on action verbs
- [reference/formats](./formats.md) — `rwv.toml` shape
