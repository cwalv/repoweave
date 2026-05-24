# Roles

Roles classify a repo's relationship to a project — its *change resistance*. They are per-project (the same repo can have different roles in different projects) and a first-class field on every `rwv.yaml` entry.

> **Naming note.** The role for "code you author/own" is being renamed from `primary` to `owned`. During the transition, both spellings parse; new docs use `owned`. The on-disk YAML examples may still show `role: primary` in places that haven't been swept yet.

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

A fork of an upstream repo. Changes ideally flow back upstream (PRs to the source-of-record), but local patches are acceptable.

**Remote convention.** When `rwv fetch` or `rwv add` clones a repo with `role: fork`, it names the remote `upstream` instead of the default `origin`. The source URL is the upstream-of-record, not your push target, so leaving `origin` unset prevents a stray `git push` from hitting the upstream and getting 403'd.

You're responsible for adding your own fork as `origin`:

```bash
cd github/socketio/engine.io
git remote add origin git@github.com:chatly/engine.io.git
```

Existing clones are never modified. `rwv` prints a one-line notice when a `role: fork` repo's `origin` already points at the source-of-record so you can decide whether to rename the remote.

**Push policy.** `rwv push` skips `fork` repos by default — they need an explicit per-repo push to your fork's remote.

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

## Per-project, not per-repo

The same repo can have different roles in different projects. `engine.io` might be a `fork` in `web-app` (patched for reconnection logic) and a `dependency` in another project (used unmodified).

```yaml
# projects/web-app/rwv.yaml
repositories:
  github/socketio/engine.io:
    url: https://github.com/chatly/engine.io.git
    role: fork
```

```yaml
# projects/another-app/rwv.yaml
repositories:
  github/socketio/engine.io:
    url: https://github.com/chatly/engine.io.git
    role: dependency
```

The active project's `rwv.yaml` determines the current role.

## Default for new entries

`rwv add` defaults to `role: owned` when `--role` is not specified. Override:

```bash
rwv add https://github.com/example/lib.git --role dependency
rwv add https://github.com/upstream/thing.git --role fork
rwv add https://github.com/other/code.git --role reference
```

## Heuristic, not rule

A common pattern: `github/{your-org}/*` is likely `owned`; `github/{other-org}/*` is likely `dependency` or `reference`. This is a default expectation, not enforced. The active project's `rwv.yaml` always wins.

## Roles in `rwv push`

`rwv push` walks the manifest and applies per-role policy:

| Role | `rwv push` behavior |
|---|---|
| `owned` | Push (with lock-precondition check) |
| `fork` | Skip — push manually to your fork's remote |
| `dependency` | Skip — you don't push upstream code |
| `reference` | Skip — read-only |

See [push a cross-repo feature](../how-to/push-cross-repo-feature.md).

## Roles in `rwv status --json`

Roles surface in `rwv status --json` output so agent harnesses and shell scripts can filter on them. See [run a command across repos](../how-to/run-a-command-across-repos.md) for filtering recipes (`jq '.repos[] | select(.role == "owned")'`).

## Related

- [workspace lens — Roles](../explanation/lenses/workspace.md#roles-change-resistance-made-explicit) — the conceptual frame
- [reference/cli — Selector grammar](./cli.md#selector-grammar) — `--role` filtering on action verbs
- [reference/formats](./formats.md) — `rwv.yaml` shape
