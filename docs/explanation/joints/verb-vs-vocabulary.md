# Verb vocabulary: VCS surface vs. package-manager surface

A sibling joint to [verb-vs-composition](./verb-vs-composition.md). That
joint decides *whether* a new rwv verb earns its keep. This one decides
*what to name it* once the gate is cleared.

rwv's CLI surface draws from two different vocabularies, and that
asymmetry is principled, not accidental.

## The two vocabularies

**VCS surface** (`git`, `hg`, `sl`, `jj`): `fetch`, `push`. Verbs that
move commits between local and remote without re-deciding what "current"
means. The lock is read, never mutated, by these operations.

**Package-manager surface** (`cargo`, `npm`, `uv`, `pnpm`): `update`,
`lock`. Verbs whose principal artifact is the lockfile. They re-snapshot
the recorded state of the world; the network fetch (if any) is a side
effect of deciding what to record.

Each rwv verb belongs to whichever surface most accurately describes
what the verb *does*, not which surface gives the cleanest internal
symmetry.

## The selection rule

> Name a verb after its *effect*, not after its *direction* (publish /
> consume) and not after the surface symmetry it would create *within
> rwv*.

Two corollaries:

- If the lock is the principal output, the verb belongs to the
  package-manager vocabulary.
- If commits move and the lock is read-only, the verb belongs to the
  VCS vocabulary.

"Surface symmetry within rwv" is the tempting wrong answer: it
suggests `fetch`/`push` should sit next to `pull`/`publish`, or that
everything that talks to a remote should share a vocabulary. Neither is
the right gate. The audience's mental model on encountering the verb
name is what matters, and that model is set by the closest single-tool
analogue.

## Worked examples

### `rwv push` (not `publish`)

Pure transmit. Pushes commits in every manifest repo, then the project
repo last; refuses on lock mismatch; skips `Role::Fork`. The lock is
**read** as a precondition (HEAD must match the recorded SHA) but never
mutated. The audience expectation is "behaves like `git push` plus
manifest-knowledge ordering" — exactly what it does. `publish` would
suggest a package-manager release operation (registry upload, version
bump, side effects beyond transmit) that doesn't match the verb's
behavior. VCS verb.

### `rwv update` (not `pull`)

Advances each manifest repo to its branch HEAD and re-snapshots
`rwv.lock`. The principal artifact is the new lock; the network fetch
exists in service of the snapshot. The audience expectation maps
cleanly to `cargo update` / `npm update`: "go get the latest versions
and record them." Calling it `pull` would suggest a VCS merge into the
working tree (no lock mutation implied), which understates the central
effect. Package-manager verb.

### `rwv fetch`

Reads `rwv.lock`, materializes clones, aligns each repo to the
lock-recorded SHA. Lock is read-only. Bootstrap behavior (no lock
present) is a degenerate case of the same operation. Audience
expectation: `git fetch` against several repos, with the manifest
deciding which and where. VCS verb.

### `rwv lock`

Checkpoints local state into `rwv.lock`. Pure lockfile mutation; no
network. The lockfile is the entire artifact. Package-manager verb.

### `rwv sync` / `rwv sync-to` (not `pull` / `push`)

`rwv sync <source>` absorbs another workspace's committed state into
CWD. `rwv sync-to <target>` pushes CWD's committed state into another
workspace. Together they are a direction-explicit pair for local-to-local
workspace alignment. The question is why these are *their own* vocabulary
rather than `pull`/`push` variants.

**The `push` argument fails first.** `rwv push` is already taken: it
moves commits from local repos to their VCS remotes, manifest-aware and
ordered. Reusing the name for a local-to-local operation would be a
vocabulary collision, not a symmetry.

**The `pull` argument fails on composition grounds.** `rwv pull` was
explicitly considered and rejected in
[verb-vs-composition](./verb-vs-composition.md): the work it would do
collapses into either `rwv update` (fetch + re-snapshot the lock) or
shell composition. The verb doesn't earn its keep.

**The deeper reason: strategy choice.** No VCS `push` command accepts a
`--rebase` option. The reason is structural: the remote is not yours to
rewrite. `git push --force` exists but it replaces the remote's tip
wholesale; there is no "rebase my local commits onto the remote and then
fast-forward it" because the remote is a shared, authoritative ref that
you cannot mutate mid-operation.

`rwv sync` and `rwv sync-to` have no such constraint. Both sides are
local workweaves — each is owned by the operator running the command.
That ownership is what makes `--strategy rebase` meaningful: the
destination's commits can be replayed onto the source's tip because the
destination is yours to rewrite. A `push`/`pull` vocabulary would imply
the VCS-remote contract (remote is authoritative; local rewrites are
dangerous) and obscure the local-ownership property that makes
`--strategy` sensible.

The single-repo analog is `sl rebase --dest <bookmark>` or `git rebase
<branch> && git merge --ff-only`: strategy is available, just without a
dedicated cross-workspace verb. rwv's contribution is not a new concept —
single-repo VCSes can do local-to-local alignment too — it is the
multi-repo bundling: loop over N manifest repos with lock-excluded replay,
regenerate `rwv.lock` in Phase 3, and handle workspace lifecycle
(`--retire`). The sync vocabulary is doing real work, not inventing a new
direction concept.

The naming convention is the `cp`/`rsync` source-first vs. dest-first
pattern: `sync <source>` identifies what CWD will absorb (argument is
source); `sync-to <target>` identifies where CWD's state will land
(argument is destination). That argument-position convention is the right
mental model for the pair. See [sync-semantics](./sync-semantics.md) for
the direction-explicit pair contract.

## Implication for future verbs

When proposing a new verb, after it has cleared the
[verb-vs-composition](./verb-vs-composition.md) gate, ask:

1. What is the *principal effect* of this verb — moving commits, or
   re-snapshotting the lock?
2. Which single-tool analogue (git/hg/sl/jj vs. cargo/npm/uv) will the
   audience reach for first?

Pick the name from that vocabulary. Do not reach for surface symmetry
within rwv — a `fetch`/`push` pair next to an `update`/`lock` pair is
the *correct* shape, because the two pairs do different kinds of work.

The internal counterpart to this principle —
[vcs-as-seam](./vcs-as-seam.md) — applies to *implementation*: anywhere
the verb's code is about to use a VCS-specific name or mechanism, the
abstraction lives in the Vcs trait.

## Origin

Surfaced during `rwv push` design review. The `publish` → `push` rename
made the asymmetry with `update` visible. The resolution was not to
chase consistency (renaming `update` → `pull` would have lost the
cargo/npm analogue that makes the verb's behavior obvious); it was to
state the two-vocabulary choice explicitly. The `sync`/`sync-to`
analysis was added when the direction-explicit pair was introduced; its
third worked example consolidates the prior reasoning from the `rwv pull`
rejection in [verb-vs-composition](./verb-vs-composition.md) and the
`sync --retire` direction fix.

## Related joints

- [verb-vs-composition](./verb-vs-composition.md) — whether a verb
  earns its keep at all.
- [vcs-as-seam](./vcs-as-seam.md) — the inner-boundary counterpart;
  what belongs in the Vcs trait vs. in rwv core.
