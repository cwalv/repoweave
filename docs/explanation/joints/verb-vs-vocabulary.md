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
state the two-vocabulary choice explicitly.

## Related joints

- [verb-vs-composition](./verb-vs-composition.md) — whether a verb
  earns its keep at all.
- [vcs-as-seam](./vcs-as-seam.md) — the inner-boundary counterpart;
  what belongs in the Vcs trait vs. in rwv core.
