# Verb vs. composition

When does an rwv verb earn its place in the CLI, and when does the same
operation belong as a one-liner composed from `rwv status --json`, `jq`,
and the existing per-repo tooling?

rwv's design pressure pushes verbs in two directions at once: more
verbs make common workflows shorter, but they also turn rwv into a
least-common-denominator wrapper around git, where every cross-repo
gesture is encoded twice (once in rwv and once in the shell people
already know). This joint states the principle the team uses to decide,
and walks it through every verb that exists and a few candidate verbs
that don't.

The sibling joint [verb-vs-vocabulary](./verb-vs-vocabulary.md) covers
what to *name* a verb once the gate is cleared.

## The principle

> A verb earns its keep when it encodes coordination the manifest
> knows and the VCS doesn't — per-repo policy from role, cross-repo
> dependency ordering, or rwv-owned state mutation. "Fewer characters
> for the same composition" doesn't count.

Three categories qualify:

1. **Per-repo policy from role.** The manifest assigns each repo a
   role (`primary`/`owned`, `fork`, `dependency`, `reference`). A
   verb's behavior depends on role-specific policy that the VCS
   doesn't know about — e.g. "exclude dependency and reference repos
   from the default push scope because they are not operator-writable."
2. **Cross-repo dependency ordering.** The verb must do work in repo A
   before doing it in repo B because A's state is a precondition for
   the eventual recorded state. The lock-as-precondition relationship
   between manifest repos and the project repo is the canonical
   example.
3. **rwv-owned state mutation.** The operation must read or write
   files rwv owns: `rwv.toml`, `rwv.lock`, `.rwv-active`,
   `.rwv-workweave`, ecosystem workspace files, etc. The VCS doesn't
   know about these files, so composition can't get them right.

The negative side of the principle is just as load-bearing: *if all a
proposed verb does is wrap a shell pipeline that already exists, it
fails the gate*. `gita`-style "run this command across all my repos" is
unix composition; it doesn't need to be a verb in rwv.

## Why this matters

Two failure modes the principle prevents:

- **rwv as a git wrapper.** If every verb that takes more keystrokes
  in git than in rwv gets added, rwv becomes a least-common-denominator
  wrapper. The cost is enormous and recurring: every git flag has to
  be re-exposed; every git surprise has to be re-documented; the
  verb's behavior drifts from git's as git evolves.
- **rwv as a runner.** If "run any command across repos" is a verb,
  rwv has volunteered to be a process manager. The shell already does
  this well; `jq` + `xargs` + `parallel` already exist. The cost of
  being a runner is the cost of competing with tools that have
  existed for forty years.

The principle is also asymmetric in a useful way: it favors *removing*
verbs that don't pass the gate. Verb removal is rare, but the
principle gives reviewers a clear test to apply when a verb's value
shrinks (e.g., when a composition recipe gets cleaner upstream).

## Walking the existing verbs

### `rwv fetch`

Reads `rwv.lock`, materializes constituent clones to the recorded
SHAs. Bootstrap when no lock exists.

- *Per-repo policy from role?* Yes — role determines which repos are
  fetched and to which recorded SHA; the VCS impl owns the remote name
  but the manifest knows which SHAs to target.
- *Cross-repo dependency ordering?* The project repo must come first
  to read the lock; manifest repos follow.
- *rwv-owned state?* The lock is read.

Pass. The "fetch all my repos" composition exists (`rwv status --json
| jq -r '.repos[].path' | xargs ...`) but has to spell the remote name
itself, and cannot read the lock-target SHA for each repo.

### `rwv update`

Advances each manifest repo to its branch HEAD and re-snapshots the
lock.

- *Per-repo policy from role?* No — the remote is the backend's to
  name, and it names one for every repo.
- *Cross-repo ordering?* The manifest repos must advance before the
  lock is regenerated; the lock generation is the verb's whole point.
- *rwv-owned state?* The lock is written.

Pass.

### `rwv lock`

Snapshots manifest tips into `rwv.lock`. No network.

- *rwv-owned state?* The lock *is* the entire output.

Pass. Composition cannot construct the lock without re-implementing
the snapshot logic and lock format.

### `rwv push`

Pushes each manifest repo in the default scope (owned + fork), then
the project repo last; refuses on lock mismatch.

- *Per-repo policy from role?* Yes — `Role::Dependency` and
  `Role::Reference` are excluded from the default push scope (anchored
  by `tests/doc_claims_push_test.rs`). Dependency and reference repos
  return 403 against upstreams the operator doesn't own. Fork repos
  push just like owned repos; both use `origin`.
- *Cross-repo ordering?* Yes — project repo last. The committed lock
  must never reference unpushed manifest SHAs (otherwise
  collaborators' `rwv fetch` hits "object missing" against
  pinned-but-unpublished commits).
- *rwv-owned state?* The lock is read as a precondition (HEAD must
  match recorded SHA, else refuse).

Strong pass. All three criteria fire. See
[verb-vs-vocabulary](./verb-vs-vocabulary.md) for why this is `push`
and not `publish`.

### `rwv sync` / `rwv sync-to`

A direction-explicit pair. `rwv sync <source>` aligns CWD with another
workspace's committed state; `rwv sync-to <target>` pushes CWD's
committed state into another workspace. Both run Phase 2 (manifest
repos), Phase 1' (project repo, lock-excluded), Phase 3 (re-lock) on
the destination workspace.
See [sync-semantics](./sync-semantics.md).

- *Per-repo policy from role?* Yes — manifest repos and project repo
  follow different strategies for the same `--strategy` choice (the
  project repo excludes the lock from the merge; manifest repos
  don't).
- *Cross-repo ordering?* Yes — Phase 2 must complete before Phase 3
  can capture the resulting manifest tips into the new lock.
- *rwv-owned state?* The lock is the principal output of Phase 3.

Strong pass. See [verb-vs-vocabulary](./verb-vs-vocabulary.md) for why
these are `sync`/`sync-to` and not `pull`/`push`.

### `rwv activate`

Switches the active project in a workspace; regenerates ecosystem
workspace files; runs integration install hooks.

- *rwv-owned state?* `.rwv-active`, ecosystem workspace files,
  symlinks.

Pass.

## Walking candidate verbs

### `rwv publish`

Proposed periodically. Intent: "release this project — bump versions,
push tags, upload to registries, update dependents."

- *Per-repo policy from role?* Possibly — different roles might
  trigger different release behaviors.
- *Cross-repo ordering?* Yes — dependencies before dependents in the
  cross-repo dependency graph.
- *rwv-owned state?* Some — version bumps in manifests, possibly the
  lock.

The first two criteria would pass. The third is shaky: rwv doesn't
own the version-bump or registry-upload mechanics; those are
ecosystem-specific (cargo, npm, uv, ...) and already have well-developed
release tooling per ecosystem.

The likely right shape is *not* a single `rwv publish` verb but a
how-to that composes existing ecosystem release commands with
manifest-aware ordering. The exception that earns the verb would be
the cross-repo coordination piece — but that piece is small enough to
live as a how-to recipe rather than a verb.

**Verdict:** does not pass the gate as proposed. Document the
composition; revisit the verb if the composition turns out to share
non-trivial coordination logic.

### `rwv pull`

Proposed as a symmetry-with-push counterpart: "fetch and merge in all
manifest repos."

- *Per-repo policy from role?* No — the same backend-named remote
  `rwv fetch` uses (`origin` in the git impl).
- *Cross-repo ordering?* No — pull is per-repo independent.
- *rwv-owned state?* The lock would have to be updated (pulling
  changes the manifest tips); but at that point the verb is just
  `rwv update`.

This is the trap case. "Symmetry with push" is not coordination the
manifest knows and the VCS doesn't — it's surface symmetry within
rwv, which is exactly what the principle excludes. The work `rwv
pull` would do collapses into either:

- `rwv update` (fetch and update the lock) if the user wants the new
  state recorded, or
- shell composition (`rwv status --json | jq ... | xargs git pull`)
  if the user doesn't.

**Verdict:** does not pass. The asymmetry between `rwv push` and "no
rwv pull" is principled, not an inconsistency:
`rwv push` enforces a lock-state precondition that `git pull` would
have to *re-derive* (since `git pull` itself moves the manifest tips
and would have to decide what the new lock should record). `rwv
update` already owns that decision. See
[verb-vs-vocabulary](./verb-vs-vocabulary.md) for the naming side.

### "Run this command across repos"

Recurring proposal in some form (`rwv each`, `rwv run`, `rwv exec`).
Intent: take a shell command, run it in every constituent repo,
collect output.

- *Per-repo policy from role?* No — the command is opaque.
- *Cross-repo ordering?* No — typically per-repo independent.
- *rwv-owned state?* No.

Three nos. This is exactly the failure mode the principle is built
to prevent. The composition shape (`rwv status --json | jq -r
'.repos[].absolute_path' | xargs -I{} bash -c '...'`) is well-developed
elsewhere; `gita super primary <cmd>` is the showcase integration for
users who want the summary sugar. None of it needs to live in rwv
core.

**Verdict:** firmly fails the gate. Document in the
"run-a-command-across-repos" how-to and call out `gita` as the
opt-in summary sugar.

**The third answer.** The gate has always offered two outcomes: core verb, or
shell composition. The plugin space is now a third answer for this class of
operation. A `rwv-each` executable on `$PATH` is invoked as `rwv each <cmd>` and
receives the resolved workspace context through the environment envelope — so the
invocation is exactly as ergonomic as a core verb, without the maintenance cost of
living in core. This is where packaged compositions have a home. The verb-vs-
composition gate is unchanged; "run a command across repos" still firmly fails it;
what is new is that the refusal can point somewhere: the plugin space, and the
how-to at [run-a-command-across-repos](../../how-to/run-a-command-across-repos.md).

## Composition is not a downgrade

It is tempting to read "this should be composition, not a verb" as a
silver-medal verdict. It isn't. The compositions rwv leans on —
`rwv status --json | jq ... | xargs/parallel` — are first-class
elements of the design. They:

- Read the same metadata `rwv` itself reads. A composition can't
  drift from rwv's notion of "which repos are in this project" because
  it asks rwv directly.
- Scale to any per-repo operation the user can express in shell. Far
  beyond what a finite verb set could cover.
- Stay fluent. `xargs -P 8` and GNU `parallel` already handle
  parallelism; rwv inherits that for free.

The status JSON envelope shape (`{"$schema": "...", "repos": [...]}`)
is intentionally designed for composition — every record carries
enough metadata to identify the repo, and the schema is committed at
`docs/reference/schemas/status.json` and embedded inside the
`rwv explain status` bundle. This is rwv as a *data source*,
not as a runner.

## Anchoring

- `rwv push` default scope (owned + fork; dependency/reference
  excluded) is anchored by `tests/doc_claims_push_test.rs`.
- `rwv push` project-repo-last ordering is exercised by
  `tests/push_test.rs`.
- `rwv update` lock-snapshot behavior is anchored by
  `tests/doc_claims_update_test.rs`.
- The composition recipes that replace candidate verbs live in
  [how-to/run-a-command-across-repos.md](../../how-to/run-a-command-across-repos.md).

## Reference for proposers

When a new verb is proposed, this is the test:

1. Does it encode **per-repo policy from role**?
2. Does it encode **cross-repo dependency ordering**?
3. Does it read or write **rwv-owned state**?

A new verb should pass *at least one*. "Shorter than the shell
equivalent" alone is not enough; "symmetry with an existing verb"
alone is not enough. If none of (1), (2), (3) fires, the right
artifact is a how-to with a composition recipe.

If the verb passes the gate, see
[verb-vs-vocabulary](./verb-vs-vocabulary.md) for the naming half.
The implementation side has its own discipline: see
[vcs-as-seam](./vcs-as-seam.md) — anywhere the new verb's code is
about to use a VCS-specific command, name, or file convention, the
abstraction belongs in the Vcs trait.

## Related joints

- [verb-vs-vocabulary](./verb-vs-vocabulary.md) — naming side of the
  same gate.
- [vcs-as-seam](./vcs-as-seam.md) — the inner-boundary counterpart;
  this joint is the outer boundary.
- [sync-semantics](./sync-semantics.md) — worked example of all three
  gate criteria firing simultaneously.
