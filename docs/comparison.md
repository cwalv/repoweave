# Alternatives

repoweave is not the only way to coordinate work across multiple repositories. This page compares the main alternatives — including the cases where they win and the costs of adopting `rwv`.

For the full design rationale behind repoweave's choices, see the [monorepo lens](explanation/lenses/monorepo.md).

## Summary

| Alternative | Wins when | repoweave wins when |
|---|---|---|
| **Local toolchain path overrides (`[patch]`, `go.work`, `pnpm link`, `uv` sources)** | Two repos on one developer's machine need a quick local link for an afternoon | Multiple contributors, CI, or parallel agent worktrees need reproducible layout, committed workspace files, and a shared cross-repo lock |
| **git submodules** | You need VCS-native pinning in a single repo with no additional tooling | You want a writable workspace (not just pinning), workspace files, workweaves, or agent integration |
| **git subtree** | You want to vendor a dependency's files into one repo's history with zero external tooling | Repos have independent ownership/ACL boundaries or you frequently push changes back to multiple upstreams |
| **gita / meta / bulk-git tools** | All you need is bulk `git status` / `git pull` across loosely related repos | You need a reproducible lock, cross-repo workspace files, or isolated parallel work |
| **Monorepo migration** | Your repos share a release cadence, are all controlled by one team, and the storage/history cost is acceptable | Repos are independently owned, separately versioned, or too large to vendor into one tree |
| **Monorepo + exporter (Copybara / ShipIt / josh)** | You already have a monorepo and must publish vanilla-consumable slices of it | Your components already live in separate repos — the slice is a subset, not a translated copy |
| **repo (Android) / west — manifest meta-repos** | Single-vendor OS/firmware trees where the manifest is a checkout instruction and builds run outside git | You want a writable coordinated workspace: ecosystem workspace files, workweaves, a derived lock, sync verbs |
| **git worktree (per-repo)** | You're working in a single repo and want parallel branches | You need worktrees across N repos with shared ecosystem files and a transactional landing path |

## Local toolchain path overrides (`[patch]`, `go.work`, `pnpm link`, `uv` sources)

Before reaching for a multi-repo tool, most developers wire sibling checkouts together using their language toolchain's native local-override mechanism: Cargo `[patch.crates-io]` or `path = "../foo"`, uncommitted `go.work` files, `pnpm link` / `workspace:*`, or `[tool.uv.sources]`.

**Native toolchain overrides win when:** you are linking two repos on your own machine for a short-lived change, or you already have a way to guarantee where every contributor clones sibling repos. Zero extra binaries required.

**repoweave wins when:** the local link stops being a one-off hack. Hand-maintained relative paths (`path = "../protocol"`) assume every developer and CI runner clones the same repos at the same relative paths; committing `[patch]` or relative path overrides to a library repo breaks standalone consumers; leaving them uncommitted means `git clone` does not produce a working build; and spinning up an isolated parallel workspace requires manually cloning or adding worktrees for every repo and rewriting the relative paths. repoweave standardizes the canonical clone layout (`<registry>/<owner>/<repo>/`), authors the ecosystem workspace files (`Cargo.toml [workspace]`, `go.work`, `package.json`, `pyproject.toml`) inside `projects/<name>/` while leaving member repos untouched, pins all repo SHAs in `rwv.lock`, and provisions multi-repo worktrees (`rwv workweave`) with matching workspace wiring in one command.

## git submodules

Git submodules pin a commit SHA from one repo inside another. They solve the reproducibility problem ("what version of the dependency was checked out?") entirely within git, with no additional tooling.

**git submodules win when:** your use case is purely pinning — a firmware repo that vendors a hardware-abstraction library at a known commit, a docs repo that embeds a spec. No writable workspace needed, no parallel work, no agent integration.

**repoweave wins when:** you want to *work* across repos, not just pin them. Submodules check out detached HEADs by default, have no concept of generated ecosystem workspace files, no workweave isolation, no `sync-to --retire` landing path, and no structured agent surface. Updating submodules across several repos is manual and error-prone; `rwv update` advances every manifest repo in parallel and re-snapshots `rwv.lock` only after every advance succeeds.

## git subtree

`git subtree` copies another repository's files into a subdirectory of your repository and optionally squashes or merges history back and forth using plain `git`.

**git subtree wins when:** consumers should be able to clone a single repository with plain `git clone` and no extra tools, and upstream round-tripping happens rarely.

**repoweave wins when:** each component is a peer repository with its own CI, issue tracker, access control, and release cycle. `git subtree push` scans commit history to split commits back out, which grows slower and more brittle as repo count and commit volume increase, and every subtree update mixes vendor commits into the host repo's log. repoweave keeps each repository's history and remote cleanly separated while still pinning their exact cross-repo state in `projects/<name>/rwv.lock`.

## gita / meta / bulk-git tools

[gita](https://github.com/nosarthur/gita), [meta](https://github.com/mateodelnorte/meta), and similar tools let you run the same git command across a set of loosely related repos: `gita super status`, `meta git pull`. They are thin convenience wrappers — no lock file, no workspace files, no isolation.

**gita wins when:** all you need is bulk `git status` / `git pull` across a collection of repos you happen to work in together. Zero setup cost, no new mental model.

**repoweave wins when:** you need reproducibility (a committed lock that pins every repo), cross-repo workspace files (so `cargo test --workspace` spans all repos without a publish step), isolated parallel work (workweaves), or an agent sandbox with a transactional landing path. repoweave's `rwv status --json | jq ... | xargs git ...` covers the bulk-command case via Unix composition; see [adjacent tools](adjacent-tools.md) and [run a command across repos](how-to/run-a-command-across-repos.md). The gita integration is an opt-in alternative for users who prefer gita's summary sugar; see [reference/integrations/gita](reference/integrations/gita.md).

## Monorepo migration

Merging all your repos into a single monorepo eliminates the coordination problem entirely: one commit, one lock, one CI run, one `git blame`.

**Monorepo wins when:** your repos share a release cadence, a single team controls all of them, the history cost of merging is acceptable, and you want atomic cross-repo commits enforced at the VCS level. Large companies (Google, Meta, Microsoft) invest heavily in monorepo tooling for exactly these reasons.

**repoweave wins when:** repos are independently owned by different teams or open-source maintainers; repos publish externally on different cadences; storage or CI cost of a merged history is prohibitive; or you want to keep per-repo access control and code review workflows intact. Migrating to a monorepo is also a one-way door — recovering independent histories later is painful. See [the monorepo lens](explanation/lenses/monorepo.md) for the full cadence story and where the equivalence holds.

## Monorepo + exporter (Copybara / ShipIt / josh)

The large-monorepo shops don't actually choose between monorepo and polyrepo — they run a monorepo internally and pay an **exporter** to publish slices of it: [Copybara](https://github.com/google/copybara) (Google), ShipIt (Meta), [josh](https://github.com/josh-project/josh). The exporter computes a dependency closure, generates a standalone root manifest, vendors third-party code, and re-syncs on a schedule. Every "synced periodically from our monorepo" repo on GitHub is this pattern.

**The exporter wins when:** the monorepo already exists and its internal benefits (atomic cross-cutting commits, one-version policy enforced by construction) outweigh the boundary cost. The exported artifact is genuinely excellent for its consumer: self-contained, coherent by construction, buildable with vanilla git + toolchain, zero additional tooling.

**repoweave wins when:** the components already live (or belong) in separate repos. Then the slice is a *subset*, not a translated copy: publishing = per-repo ACLs plus a visible project manifest. Nothing the exporter must reconstruct or erase — provenance, upstream identity, the contribution backchannel — was ever lost, because the unit of composition and the unit of sharing coincide. See [the exporter tax](explanation/lenses/monorepo.md#the-exporter-tax-sharing-a-slice) in the monorepo lens.

## repo (Android) / west — manifest meta-repos

Android's [repo](https://gerrit.googlesource.com/git-repo) and Zephyr's [west](https://docs.zephyrproject.org/latest/develop/west/index.html) drive N repos from a manifest, like repoweave. The differences are in what happens after checkout: they stop at "the right repos at the right revisions," with no generated ecosystem workspace files, no worktree-based isolation primitive, and (for `repo`) a wrapped-git workflow that fights the substrate.

**repo/west win when:** a single vendor controls the tree, the build system consumes source paths directly (Soong, CMake/Zephyr), and the manifest is primarily a checkout instruction for CI and release engineering. These are mature, proven at enormous scale, and west's `manifest --freeze` covers the pinning story.

**repoweave wins when:** you want a writable coordinated *workspace* rather than a coordinated *checkout* — generated `[workspace]`/`go.work`/workspaces files so ecosystem tools see one project, workweaves for isolated parallel work, a lock that is derived rather than hand-advanced, and sync verbs with a transactional landing path.

## git worktree (per-repo)

`git worktree add` gives you a parallel working tree of a single repository on a different branch — no stash dance, instant context switch.

**git worktree wins when:** your work lives in a single repo. It is a first-class git primitive with no extra tooling required.

**repoweave wins when:** your workspace spans N repos. A `rwv workweave` is `git worktree` extended across every repo in the manifest — plus per-workweave `node_modules`/`.venv`/`target`, ecosystem workspace files, and a `sync-to --retire` landing path that coordinates the cross-repo fast-forward in one command. repoweave uses `git worktree` internally; the two are complementary at different scopes.

## The meta-repo failure mode

Any multi-repo tool separates the **record** (`rwv.toml`, `rwv.lock`, generated workspace files) from the **reality** (N git repositories that can be mutated on their own). A monorepo cannot express divergence between its manifest and its tree because they are the same commit; a meta-repo tool can. When record and reality are separate, **reconciliation is the product**.

Earlier tools split into two dataflows:

1. **No record (`gita`, `meta`):** fan out commands across directories, but pin nothing and generate no workspace wiring. Nothing can drift, and nothing is reproducible.
2. **Authoritative record (`git submodules`, `repo`, `west`):** treat the composition record like a source file that checkouts must conform to. Detached HEADs and forgotten pointer-bump commits are direct consequences of that dataflow.

repoweave uses a third dataflow: **the lock is derived on mutation and authoritative only at materialization (`rwv fetch` / `rwv sync`)**:

- **Reconciliation is built into the verbs:** intent verbs (`add`, `remove`, `update`, `role`) author ecosystem workspace files and `activate` surfaces them at the weave root; `sync` and `sync-to` recompute `rwv.lock` in Phase 3 ([lock-as-derived](explanation/joints/lock-as-derived.md)), making lock merge conflicts impossible.
- **Drift is detectable and repairable:** out-of-band git operations are still possible, so `rwv doctor` (and per-integration `verify()`) detects divergence and `rwv abort` rolls back mid-operation sync state via `refs/rwv/pre-op/<id>/` savepoints.
- **Ordinary git stays first-class:** there is no `rwv commit` or wrapped git porcelain ([verb-vs-composition](explanation/joints/verb-vs-composition.md)).

## Costs and when not to use rwv

Adopting `rwv` carries real trade-offs:

1. **An extra binary on developer and CI machines.** Contributors and CI jobs that build across the weave install `rwv` (though individual member repos remain buildable standalone with plain `git`).
2. **An $(N+1)$th repository to maintain.** The project repo (`projects/<name>/`) holds `rwv.toml`, `rwv.lock`, and generated workspace files; cross-repo features include a lock update commit in the project repo (`rwv lock --commit` + `rwv push`).
3. **Branch discipline in canonical clones.** Canonical clones in the primary weave track the branch declared in `rwv.toml` (`version = "main"`). Isolated feature work routes through workweaves (`rwv workweave <project> create <name>`) rather than ad-hoc `git checkout -b` inside canonical clones (see [Branch model](explanation/joints/branches.md)).
4. **Overkill for a single repo or rarely coupled repos.** If all your code lives in one repository, use `git worktree` directly. If two repositories only interact through a stable published API once a quarter, publish to a package registry and skip workspace coordination altogether.
