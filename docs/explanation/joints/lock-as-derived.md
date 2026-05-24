# Lock as derived state

`rwv.lock` is derived state. Its contents are fully determined by
running `rwv lock` against the current manifest-repo tips on disk. That
single property has outsized consequences for the rest of the design,
and this joint pulls them together.

## What "derived" means here

A piece of state is derived when there is a function from other state
to it and the function is the source of truth. The lock entries' values
are not opinions; they are reads of `git rev-parse HEAD` (one per
manifest repo) baked into a file. Re-running the function on the same
inputs always yields the same lock.

Useful frame for what this implies:

| Question | Single-repo analogue | repoweave analogue |
|---|---|---|
| Where does new state come from? | `git commit` on a working tree | `git commit` in each manifest repo |
| What captures cross-repo state? | n/a — single repo | `rwv lock` snapshots the manifest repo tips |
| Is the snapshot meaningful before commit? | No — uncommitted edits don't have SHAs | No — uncommitted edits in manifest repos don't have SHAs |
| Can two operators produce identical snapshots? | Yes, given the same commits | Yes, given the same commit set per repo |

The lock is to a project what `git commit` is to a working tree: a
cheap, content-addressed checkpoint of state that exists elsewhere on
disk. It is not a coordinator, not a config, not a constraint.

## `rwv lock` is a pure git snapshot

`rwv lock` reads each manifest repo's current HEAD and writes the
resulting set of SHAs (plus a small amount of bookkeeping) into
`rwv.lock`. It does not:

- Fetch anything from the network.
- Run integration installs.
- Mutate any manifest repo's working tree.
- Read the previous lock.

The last point matters: `rwv lock` does not respect or honor or
"update" the prior lock. It overwrites with whatever HEAD says. The
previous file is replaced by the current snapshot.

Install behavior used to be wired into `rwv lock` via integration
hooks. That coupling was removed; installs now belong to `rwv activate`,
which is the natural "make this workspace ready" verb. `rwv lock` is
pure write.

The other "advancement" verbs — `rwv update` and `rwv sync` — emit
fresh locks internally as part of their own work, but the locks they
emit are still derived: each one is the output of running the same
snapshot operation against the manifest tips at the moment of capture.

## `rwv.lock` is never a merge input

The consequence of "fully determined" is that the lock is never
something to merge. Two workspaces whose project repos both have lock
edits cannot have a meaningful merge conflict on the lock — there is
exactly one correct post-merge lock, and it is the snapshot of the
post-merge manifest tips. Asking an operator to resolve a lock conflict
by hand is asking them to compute a function whose inputs are already
known.

[`rwv sync`](./sync-semantics.md) encodes this directly. The lock is
excluded from the project-repo merge phase (Phase 1') and regenerated
in Phase 3 from the now-aligned manifest tips. Any lock-only commit on
the source side becomes an empty patch on the target side and is
dropped automatically. The phase model and the `merge=ours`
replay-exclusion mechanism are described in
[sync-semantics](./sync-semantics.md).

The exclusion is enforced by the VCS layer
(`Vcs::set_replay_exclusion`); the principle that owns this is
documented in [vcs-as-seam](./vcs-as-seam.md).

## Implications for everyday workflow

The "lock as derived" framing collapses a class of questions:

- *What happens if I hand-edit `rwv.lock`?* The next sync regenerates
  from manifest tips. Your edit either matches reality (no effect) or
  doesn't (gets overwritten). The file is not a place to author state.
- *What happens if I commit lock changes from two workweaves
  independently?* Each commit's lock-content is determined by that
  workweave's manifest tips at the time. When the second workweave
  syncs, the lock is recomputed; no conflict arises.
- *Why isn't there a "lock-only" diff to review?* The diff between two
  locks is fully implied by the diff between the manifest tips they
  pin. Reviewing the lock is reviewing a hash of the input set.
- *How do I roll the lock back to a prior state?* By checking out a
  prior project-repo commit. The lock at that commit is the lock that
  was derived from those manifest tips.

The lock is committed alongside the project-repo state for one purpose:
reproducibility. A collaborator who checks out the project repo at any
commit and runs `rwv fetch` gets exactly the manifest-tip set the lock
records. The lock is the *artifact*; manifest commits are the *source*.

## What the lock contains

The persisted shape is small. Each entry pins one manifest repo to one
revision (canonical SHA, with optional display form when a tag pointed
at HEAD). The file also carries top-level provenance fields such as
which workweave produced it. Detailed schema lives in
[reference/formats](../../reference/formats.md); the conceptual point
here is that *every* field in the file is a function of state recorded
elsewhere.

Hand-edits don't compound; they vanish on the next snapshot.

## Anchoring claims

The behaviors above are tool behaviors, anchored in source and tests:

- `rwv lock` does not run integrations:
  `src/lock.rs` — no integration-runner call in the lock codepath.
  (Pre-pass bead `fo-r982a` planned to land a `doc_claims_lock_test.rs`
  covering this; cite it once it lands.)
- `rwv.lock` exclusion during sync's project-repo replay:
  `tests/e2e_two_workweaves_test.rs` covers the n-way landing contract
  (lock-only changes, doc changes, genuine conflict).
- Replay-exclusion mechanism: see
  [vcs-as-seam](./vcs-as-seam.md) for the `Vcs::set_replay_exclusion`
  worked example.

## Related joints

- [sync-semantics](./sync-semantics.md) — the phase model that depends
  on "lock is never a merge input."
- [pyramid-of-stability](./pyramid-of-stability.md) — why the project
  needs a canonical-tip artifact in the first place.
- [shared-refs-drift](./shared-refs-drift.md) — the lock is one
  ingredient; on-disk drift between worktrees is another, and they
  interact.
- [verb-vs-composition](./verb-vs-composition.md) — why "rwv
  resolve-lock-conflicts" isn't a verb (the function has a closed-form
  answer).
