# Alternatives

repoweave is not the only way to coordinate work across multiple repositories. This page compares the main alternatives honestly — including the cases where they win.

For the full design rationale behind repoweave's choices, see the [monorepo lens](explanation/lenses/monorepo.md).

## Summary

| Alternative | Wins when | repoweave wins when |
|---|---|---|
| **git submodules** | You need VCS-native pinning in a single repo with no additional tooling | You want a writable workspace (not just pinning), workspace files, workweaves, or agent integration |
| **gita / meta / bulk-git tools** | All you need is bulk `git status` / `git pull` across loosely related repos | You need a reproducible lock, cross-repo workspace files, or isolated parallel work |
| **Monorepo migration** | Your repos share a release cadence, are all controlled by one team, and the storage/history cost is acceptable | Repos are independently owned, separately versioned, or too large to vendor into one tree |
| **git worktree (per-repo)** | You're working in a single repo and want parallel branches | You need worktrees across N repos with shared ecosystem files and a transactional landing path |

## git submodules

Git submodules pin a commit SHA from one repo inside another. They solve the reproducibility problem ("what version of the dependency was checked out?") entirely within git, with no additional tooling.

**git submodules win when:** your use case is purely pinning — a firmware repo that vendors a hardware-abstraction library at a known commit, a docs repo that embeds a spec. No writable workspace needed, no parallel work, no agent integration.

**repoweave wins when:** you want to *work* across repos, not just pin them. Submodules have no concept of workspace files (no generated `Cargo.toml [workspace]`, `go.work`, etc.), no workweave isolation, no `sync-to --retire` landing path, and no structured agent surface. Updating a submodule to a new commit is manual and error-prone; `rwv update` advances all repos and re-snapshots the lock atomically.

## gita / meta / bulk-git tools

[gita](https://github.com/nosarthur/gita), [meta](https://github.com/mateodelnorte/meta), and similar tools let you run the same git command across a set of loosely related repos: `gita super status`, `meta git pull`. They are thin convenience wrappers — no lock file, no workspace files, no isolation.

**gita wins when:** all you need is bulk `git status` / `git pull` across a collection of repos you happen to work in together. Zero setup cost, no new mental model.

**repoweave wins when:** you need reproducibility (a committed lock that pins every repo), cross-repo workspace files (so `cargo test --workspace` spans all repos without a publish step), isolated parallel work (workweaves), or an agent sandbox with a transactional landing path. repoweave's `rwv status --json | jq ... | xargs git ...` covers the bulk-command case via Unix composition; see [adjacent tools](adjacent-tools.md) and [run a command across repos](how-to/run-a-command-across-repos.md). The gita integration is an opt-in alternative for users who prefer gita's summary sugar; see [reference/integrations/gita](reference/integrations/gita.md).

## Monorepo migration

Merging all your repos into a single monorepo eliminates the coordination problem entirely: one commit, one lock, one CI run, one `git blame`.

**Monorepo wins when:** your repos share a release cadence, a single team controls all of them, the history cost of merging is acceptable, and you want atomic cross-repo commits enforced at the VCS level. Large companies (Google, Meta, Microsoft) invest heavily in monorepo tooling for exactly these reasons.

**repoweave wins when:** repos are independently owned by different teams or open-source maintainers; repos publish externally on different cadences; storage or CI cost of a merged history is prohibitive; or you want to keep per-repo access control and code review workflows intact. Migrating to a monorepo is also a one-way door — recovering independent histories later is painful. See [the monorepo lens](explanation/lenses/monorepo.md) for the full cadence story and where the equivalence holds.

## git worktree (per-repo)

`git worktree add` gives you a parallel working tree of a single repository on a different branch — no stash dance, instant context switch.

**git worktree wins when:** your work lives in a single repo. It is a first-class git primitive with no extra tooling required.

**repoweave wins when:** your workspace spans N repos. A `rwv workweave` is `git worktree` extended across every repo in the manifest — plus per-workweave `node_modules`/`.venv`/`target`, ecosystem workspace files, and a `sync-to --retire` landing path that coordinates the cross-repo fast-forward in one command. repoweave uses `git worktree` internally; the two are complementary at different scopes.
