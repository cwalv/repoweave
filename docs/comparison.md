# Alternatives

repoweave is not the only way to coordinate work across multiple repositories. This page compares the main alternatives honestly — including the cases where they win.

For the full design rationale behind repoweave's choices, see the [monorepo lens](explanation/lenses/monorepo.md).

## Summary

| Alternative | Wins when | repoweave wins when |
|---|---|---|
| **git submodules** | You need VCS-native pinning in a single repo with no additional tooling | You want a writable workspace (not just pinning), workspace files, workweaves, or agent integration |
| **gita / meta / bulk-git tools** | All you need is bulk `git status` / `git pull` across loosely related repos | You need a reproducible lock, cross-repo workspace files, or isolated parallel work |
| **Monorepo migration** | Your repos share a release cadence, are all controlled by one team, and the storage/history cost is acceptable | Repos are independently owned, separately versioned, or too large to vendor into one tree |
| **Monorepo + exporter (Copybara / ShipIt / josh)** | You already have a monorepo and must publish vanilla-consumable slices of it | Your components already live in separate repos — the slice is a subset, not a translated copy |
| **repo (Android) / west — manifest meta-repos** | Single-vendor OS/firmware trees where the manifest is a checkout instruction and builds run outside git | You want a writable coordinated workspace: ecosystem workspace files, workweaves, a derived lock, sync verbs |
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

## Monorepo + exporter (Copybara / ShipIt / josh)

The large-monorepo shops don't actually choose between monorepo and polyrepo — they run a monorepo internally and pay an **exporter** to publish slices of it: [Copybara](https://github.com/google/copybara) (Google), ShipIt (Meta), [josh](https://github.com/josh-project/josh). The exporter computes a dependency closure, generates a standalone root manifest, vendors third-party code, and re-syncs on a schedule. Every "synced periodically from our monorepo" repo on GitHub is this pattern.

**The exporter wins when:** the monorepo already exists and its internal benefits (atomic cross-cutting commits, one-version policy enforced by construction) outweigh the boundary cost. The exported artifact is genuinely excellent for its consumer: self-contained, coherent by construction, buildable with vanilla git + toolchain, zero additional tooling.

**repoweave wins when:** the components already live (or belong) in separate repos. Then the slice is a *subset*, not a translated copy: publishing = per-repo ACLs plus a visible project manifest. Nothing the exporter must reconstruct or erase — provenance, upstream identity, the contribution backchannel — was ever lost, because the unit of composition and the unit of sharing coincide. See [the exporter tax](explanation/lenses/monorepo.md#the-exporter-tax-sharing-a-slice) in the monorepo lens.

## repo (Android) / west / gitslave — manifest meta-repos

Android's [repo](https://gerrit.googlesource.com/git-repo), Zephyr's [west](https://docs.zephyrproject.org/latest/develop/west/index.html), and the older [gitslave](https://gitslave.sourceforge.net/) drive N repos from a manifest, like repoweave. The differences are in what happens after checkout: they stop at "the right repos at the right revisions," with no generated ecosystem workspace files, no isolation primitive, and (for repo/gitslave) a wrapped-git workflow that fights the substrate.

**repo/west win when:** a single vendor controls the tree, the build system consumes source paths directly (Soong, CMake/Zephyr), and the manifest is primarily a checkout instruction for CI and release engineering. These are mature, proven at enormous scale, and west's `manifest --freeze` covers the pinning story.

**repoweave wins when:** you want a writable coordinated *workspace* rather than a coordinated *checkout* — generated `[workspace]`/`go.work`/workspaces files so ecosystem tools see one project, workweaves for isolated parallel work, a lock that is derived rather than hand-advanced, and sync verbs with a transactional landing path.

## git worktree (per-repo)

`git worktree add` gives you a parallel working tree of a single repository on a different branch — no stash dance, instant context switch.

**git worktree wins when:** your work lives in a single repo. It is a first-class git primitive with no extra tooling required.

**repoweave wins when:** your workspace spans N repos. A `rwv workweave` is `git worktree` extended across every repo in the manifest — plus per-workweave `node_modules`/`.venv`/`target`, ecosystem workspace files, and a `sync-to --retire` landing path that coordinates the cross-repo fast-forward in one command. repoweave uses `git worktree` internally; the two are complementary at different scopes.

## The meta-repo failure mode — an honest self-assessment

Tools in this family have a reputation: "worst of both worlds." It was earned by real corpses — submodules' detached-HEAD confusion and forgotten-pointer-bump PRs, gitslave's abandonment, repo-tool's wrapped-git friction. Before trusting repoweave, it's fair to ask why it wouldn't join them.

The shared architecture — and repoweave has it too — is that the **record** (a manifest, a lock, pointers) is a separate thing from the **reality** (N repos that move). A monorepo cannot express divergence between its manifest and its tree: they are the same frozen bytes in one commit. Any meta-repo tool *can*: mutate outside the tool's verbs — a stray `git clone` into the weave, a hand-edited generated file, a repo pulled without re-locking — and record and reality disagree. That class of state is not rare in this architecture; it is *expressible*, and what's expressible eventually occurs.

The corpses' shared mistake was taking on the record/reality split and then not servicing it: a pointer in the tree (submodules) or a thin command fan-out (gitslave, meta) with no reconciliation machinery. **When record and reality are separate things, the reconciliation machinery is the product.** Shipping the split without the machinery delivers polyrepo's coordination costs with monorepo's expectations — the worst-of-both reputation, accurately assigned.

Where repoweave shares the failure mode, honestly:

- Divergence is expressible, so it will happen. `rwv doctor` exists *because* the architecture permits drift — detection is an admission, not a boast.
- Coherence is conditional on mutations flowing through the verbs. Out-of-band mutation breaks the invariant silently until doctor runs.
- Materialization depends on N remotes serving the pinned SHAs — a lock's referents can dangle (mitigated by mirrors/providers, but that's configuration, not construction).

Why it may escape the corpses' fate — the machinery, specifically:

- **Reconciliation is folded into the verbs**, not left as chores: `add`/`remove`/`activate` regenerate the ecosystem views transactionally; `sync` recomputes the lock every time ([lock-as-derived](explanation/joints/lock-as-derived.md)), so the record follows reality by construction on every supported path and lock conflicts are structurally impossible.
- **Generated views are coarse** — repo paths, membership — so their contact surface with member-repo internals is small; most upstream churn cannot stale them.
- **Drift is detectable and repairable**, not latent: `doctor` checks, per-integration `verify()`, savepoint-based `abort`.
- **Ordinary git stays first-class.** No `rwv commit`, no wrapped porcelain — the tool doesn't fight the substrate it records ([verb-vs-composition](explanation/joints/verb-vs-composition.md)). This was repo-tool's and gitslave's most user-hostile sin.

The honest conclusion: the architecture is the same one submodules had. Whether that lands as best-of-both or worst-of-both is decided by whether the reconciliation machinery holds up under real use — the bet is on execution quality, not on a structural trick the corpses lacked.
