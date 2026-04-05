# Concepts

This page explains the ideas behind repoweave — why it exists, how it relates to existing tools, and the design trade-offs it makes.

### Workspaces

Many major package ecosystems have converged on the concept of a workspace that groups multiple packages under one root for cross-package imports, shared dependency resolution, and coordinated development (`go.work`, Cargo `[workspace]`, pnpm workspaces, uv workspaces, etc.). They deal with packages, not repositories, but packages and repositories are often 1:1.

### Monorepos

Full monorepos eliminate the version dance and solve other problems that workspaces help, but require vendoring or forking everything. Revision logs are polluted, provenance is obscured, and collaboration on or distribution of logical subsets of the monorepo is much more painful than it is with repositories that are scoped appropriately.

### The weave metaphor

The goal is to weave independent **threads** (your repositories) into a single, coherent **fabric** — a unified workspace. The threads keep their identity and history; they simply work better together.

A `weave` is a workspace in the same sense as a `go.work` workspace or a Cargo `[workspace]`, but with superpowers. Often, the workspace configurations can be generated from the repoweave manifest alone. In addition to simple cross-package imports and shared dependency resolution that workspace management tools bring, you get monorepo ergonomics.

repoweave provides a `lock` mechanism analogous to package manager locks, with a similar feel to the atomic commit you get from working in a monorepo — no more edit → bump version → publish → update dependents → reinstall dance for repos in the same weave. The `lock` makes it easy to reproduce a weave on another machine or in CI. It also makes it easy to create ephemeral workweaves for isolated work or review, like a multi-repo `git worktree`. All your code lives in one directory tree, so every tool that touches the filesystem — editors, grep, agents, debuggers, build tools — works across all of it, just like a monorepo.

## Core idea

The weave has three layers:

1. **The directory tree** — repos under one root. Every tool benefits: search, navigation, agents, editors. This is the convention alone — no tooling required.
2. **Ecosystem wiring** — the weave directory (or workweave directory) is the **workspace surface**: the directory that ecosystem tools see. Integrations generate workspace files (`package.json`, `go.work`, `Cargo.toml`, `pnpm-workspace.yaml`) at this surface so cross-package imports resolve locally. Ecosystem tools don't know repos exist — they see a workspace directory with packages. `import { thing } from '@myorg/shared'` just works.
3. **Reproducibility** — a committed `rwv.yaml` file and its `rwv.lock` pin each repo to an exact revision, making the project state reproducible from a single project repo.

Committing a project gets a coherent cross-repo "revision", similar to a monorepo commit. You commit in individual repos first, then regenerate and commit the lock file. By default, it's not atomic as a monorepo is, but it could be scripted to be so. It's two-phase commit — the lock update is detectable and reversible:

```bash
# 1. Commit in individual repos (already done)
# 2. Update and commit the lock file
rwv lock
cd projects/web-app
git add rwv.lock && git commit -m "lock: add payment endpoint"
```

To reproduce a project from scratch:

```bash
mkdir my-workspace && cd my-workspace
rwv fetch chatly/web-app
```

`sha256sum rwv.lock` gives a single fingerprint for the project state — the multi-repo equivalent of `git rev-parse HEAD` on a monorepo.

## Locking and the version dance

In a traditional multi-repo setup, changing `protocol` before `server` can use it requires: bump protocol's version, publish, update server's dependency, install, test — and repeat for every iteration. With repoweave, the ecosystem workspace wiring means `server` already imports from the local `protocol` checkout during development. You edit across repos, test, iterate — no bumps, no publishing. The version bump dance is deferred to release time, where it happens once rather than on every change.

This is the same way a monorepo works. The lock file captures your exact state whether or not you've done a formal version bump.

Most ecosystem tools also enforce version constraints within the workspace: if you bump `protocol` to 2.0.0 and `server` depends on `^1.0`, Cargo, Go, and npm catch the incompatibility immediately. You discover constraint issues during development, not after publishing. (Python/uv is an exception — workspace members override constraints silently.)

See [Releasing](./releasing.md) for the release-time workflow when repos publish to external registries.

## Why not git submodules?

Git submodules aim to solve a similar problem — coordinating code across repos — but take a different approach. The feature mapping is close:

| repoweave | Git submodules |
|---|---|
| project `rwv.yaml` | `.gitmodules` |
| project `rwv.lock` | revision stored in parent tree (inherent) |
| `rwv fetch` | `git submodule update --init --recursive` |
| `rwv lock` | `git add <submodule>` (records current SHA) |

Submodules are better at one thing: **atomic locking**. The SHA is part of the parent's git tree — there's no two-phase commit. When you commit the parent, the lock updates atomically. repoweave's explicit lock file is the price of not using submodules.

But submodules take ownership in ways that conflict with multi-repo development:

- **Detached HEAD** — submodules check out a SHA, not a branch. You `cd` into one and you're in detached HEAD. You have to `git checkout main` before working. Every repo, every time.
- **Can't adopt existing clones** — submodules want to own the clone. You can't take 16 repos already on disk and retroactively make them submodules.
- **Parent owns the relationship** — updating a submodule means: commit in child, `cd` to parent, `git add child`, commit parent. The parent is always in the loop. For reference repos you don't control, this is backwards.
- **No partial fetch** — submodules are all-or-nothing per parent. No "fetch only the web-app project's repos." No project-scoped views.
- **No roles** — submodules are a flat list. No way to distinguish primary from reference, no per-project role assignments.
- **Flat nesting only** — if a dependency uses submodules, you get recursive submodule hell. repoweave's flat `{registry}/{owner}/{repo}` layout avoids nesting.

The design trade-off: submodules get atomic locking for free by taking ownership. repoweave gives up atomic locking to preserve sovereignty — repos stay on normal branches, you work in them normally, and the lock file is an explicit (two-step) operation.

## Prior art: multi-repo coordination

There is no universal standard for multi-repo development. "Polyrepo" names the strategy (the counterpart to "monorepo") but prescribes no conventions. Each ecosystem that needed multi-repo coordination invented its own:

| Tool | Ecosystem | Manifest format | Lock/pin mechanism |
|------|-----------|----------------|--------------------|
| Google `repo` | Android/embedded | XML (`default.xml`) | `repo manifest -r` (revision-locked manifest) |
| West | Zephyr RTOS | YAML | `west manifest --freeze` |
| vcstool | ROS | YAML (`.repos`) | `vcs export --exact` |
| git submodules | General | `.gitmodules` | SHA in parent tree (inherent) |

These tools are well-established within their ecosystems but none crossed over to become a general standard. Each makes trade-offs specific to its community — `repo` assumes Gerrit, West requires a manifest repo, submodules take ownership of clones.

Meanwhile, ecosystem workspace tools have converged on a shared *pattern* — a manifest listing directories for cross-package resolution — without coordinating on format:

| Tool | Manifest | Purpose |
|------|----------|---------|
| npm/pnpm | `package.json` workspaces / `pnpm-workspace.yaml` | Cross-package imports, shared deps |
| Go | `go.work` | Cross-module resolution |
| Cargo | `Cargo.toml` `[workspace]` | Shared lock, shared target |
| uv | `pyproject.toml` `[tool.uv.workspace]` | Cross-package deps |

These don't care about repo boundaries — they list directories. They handle dependency resolution but not repo lifecycle (cloning, pinning, reproducing).

repoweave sits at the intersection: it provides the repo lifecycle layer (like `repo`/West/vcstool) and generates the ecosystem workspace configs (like `go.work`/pnpm). The manifest format is YAML, based on vcstool's `.repos` format — the most portable of the existing formats.

## Design decisions

1. **Shared tool state across project switches** — acceptable cost. Ecosystem install after `rwv activate` is incremental. Workweaves are the escalation when switching is too slow.

2. **`rwv add` takes a URL** — the manifest needs URLs for `rwv fetch` on other machines. If the repo is already on disk, the clone step is a no-op.

3. **No `lock-all`** — each project needs `activate` first. Lock explicitly per project.

4. **`rwv init` auto-activates** — like `git init`, a one-time setup. Optional `--provider` sets up the remote.

5. **`rwv fetch` updates the lock** — fetches at branch HEAD and updates `rwv.lock` with actual revisions. `--locked` checks out exact revisions. `--frozen` errors if lock is stale (CI).

6. **Workweave location** — `.workweaves/{name}` under the weave directory. The `.rwv-workweave` marker records the weave path and project.
