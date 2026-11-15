# Verb vocabulary: VCS surface vs. package-manager surface

A sibling joint to [verb-vs-composition](./verb-vs-composition.md). That
joint decides *whether* a new rwv verb earns its keep. This one decides
*what to name it* once the gate is cleared.

rwv's CLI surface draws from two different vocabularies, and that
asymmetry is principled, not accidental.

## The two vocabularies

**VCS surface** (`git`, `hg`, `sl`, `jj`): `clone`, `fetch`, `pull`,
`push`. Verbs that move commits between local and remote without
re-deciding what "current" means. The lock is read, never mutated, by
these operations.

**Package-manager surface** (`cargo`, `npm`, `uv`, `pnpm`): `update`,
`lock`. Verbs whose principal artifact is the lockfile. They settle the
local state against the recorded state of the world, or re-snapshot it;
the network fetch (if any) is a side effect of deciding what to record.

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
suggests that `push` should sit next to `publish`, or that
everything that talks to a remote should share a vocabulary. Neither is
the right gate. The audience's mental model on encountering the verb
name is what matters, and that model is set by the closest single-tool
analog.

## Worked examples

### `rwv push` (not `publish`)

Pure transmit. Pushes commits in member repos within the default
scope (owned + fork), then the project repo last; refuses on lock
mismatch. Dependency and reference repos are excluded from the default
scope (not operator-writable). The lock is **read** as a precondition
(HEAD must match the recorded SHA) but never mutated. The audience
expectation is "behaves like `git push` plus manifest-knowledge
ordering" — exactly what it does. `publish` would suggest a
package-manager release operation (registry upload, version bump, side
effects beyond transmit) that doesn't match the verb's behavior. VCS
verb.

### `rwv update --remote` (not `pull`)

Advances each member repo to its branch HEAD and re-snapshots
`rwv.lock`. The principal artifact is the new lock; the network fetch
exists in service of the snapshot. The audience expectation maps
cleanly to `cargo update` / `npm update`: "go get the latest versions
and record them." Calling it `pull` would suggest a VCS merge into the
working tree (no lock mutation implied), which understates the central
effect, and `rwv pull` already names a different effect (below).
Package-manager verb.

### `rwv update`

Clones the members missing on disk and aligns each present member to the
commit `rwv.lock` pins for it. The lock is read, never written. It shares
the verb with `update --remote` because both settle the weave against the
lock: one makes the weave match it, the other makes it match the remotes.
The `--remote` flag marks the form that fetches the remotes' tips and
rewrites the lock. Package-manager verb.

### `rwv clone`

Clones the project repo from a source, then every member at the commit
`rwv.lock` pins, and activates the project when none is active. The lock is
read-only, and a member the lock cannot place is listed and left for the
operator rather than recorded. Audience expectation: `git clone` of several
repos, with the manifest deciding which and where. VCS verb.

### `rwv fetch`

Pruning fetch of every remote of the project repo and of each member on
disk. It moves no local branch, checkout or lock. Audience expectation:
`git fetch --prune` against several repos, with the manifest deciding which
and where. VCS verb.

### `rwv pull`

Fetches, fast-forwards the project branch to its upstream, then runs
`rwv update`, so the members follow the lock the project branch brought in.
Audience expectation: `git pull --ff-only` of the project repo, with the
members following. The lock arrives with the project branch; `pull` never
writes one. VCS verb.

### `rwv lock`

Checkpoints local state into `rwv.lock`. Pure lockfile mutation; no
network. The lockfile is the entire artifact. Package-manager verb.

### `rwv sync` / `rwv land` (not `pull` / `push`)

`rwv sync` takes new work into the checked-out weave branch: a feature
takes in the branch it lands into, and `--from <branch>` merges another
weave branch in. `rwv land` pushes CWD's committed state into
another workspace. Together they are a direction-explicit pair for
integrating work locally. The question is why these are *their own* vocabulary
rather than `pull`/`push` variants.

**The `push` and `pull` arguments fail first.** `rwv push` is already
taken: it moves commits from local repos to their VCS remotes,
manifest-aware and ordered. `rwv pull` is taken too: it takes in the
project repo's upstream and has the members follow the lock it brings.
Reusing either name for a local-to-local operation would be a vocabulary
collision, not a symmetry.

**The deeper reason: how the work is integrated.** No VCS `push` command accepts a
`--rebase` option. The reason is structural: the remote is not yours to
rewrite. `git push --force` exists but it replaces the remote's tip
wholesale; there is no "rebase my local commits onto the remote and then
fast-forward it" because the remote is a shared, authoritative ref that
you cannot mutate mid-operation.

`rwv sync` and `rwv land` have no such constraint. Both sides are
local — each workspace is owned by the operator running the command.
That ownership is what lets the target's `history` setting choose a
rebase: under `linear`, in both verbs, CWD's commits are replayed onto what
they will land on, rewriting CWD's history — which is safe only because CWD
is yours to rewrite; under `merge`, a merge commit is composed in CWD
instead, and the target still only fast-forwards. A `push`/`pull` vocabulary
would imply the VCS-remote contract (remote is authoritative; local rewrites
are dangerous) and obscure the local-ownership property that makes either
choice a local one. The target's setting decides how work is integrated; work moving between published branches moves by merge or cherry-pick, whatever the setting; and a rebased published branch is republished only by a named, leased `push --overwrite <repo>`.

The single-repo analog is `sl rebase --dest <bookmark>` or `git rebase
<branch> && git merge --ff-only`: strategy is available, just without a
dedicated cross-workspace verb. rwv's contribution is not a new concept —
single-repo VCSes can do local-to-local alignment too — it is the
multi-repo bundling: loop over N member repos, compose each as the target's history
setting says, record `rwv.lock` once from the composed tips, and handle workspace lifecycle
(`--retire`). The sync vocabulary is doing real work, not inventing a new
direction concept.

The argument convention mirrors `git pull` versus `git push <target>`: `sync`
takes in what the branch's record names, as `git pull` takes in a branch's
configured upstream, and `--from <branch>` names another branch as
`git merge <branch>` does; `land` lands into what the record names, and
`--target <T>` names another destination for one run. See [sync-semantics](./sync-semantics.md)
for the direction-explicit pair contract.

## Implication for future verbs

When proposing a new verb, after it has cleared the
[verb-vs-composition](./verb-vs-composition.md) gate, ask:

1. What is the *principal effect* of this verb — moving commits, or
   re-snapshotting the lock?
2. Which single-tool analog (git/hg/sl/jj vs. cargo/npm/uv) will the
   audience reach for first?

Pick the name from that vocabulary. Do not reach for surface symmetry
within rwv — VCS verbs (`clone`, `fetch`, `pull`, `push`) next to
package-manager verbs (`update`, `lock`) is the *correct* shape, because the
two groups do different kinds of work.

The internal counterpart to this principle —
[vcs-as-seam](./vcs-as-seam.md) — applies to *implementation*: anywhere
the verb's code is about to use a VCS-specific name or mechanism, the
abstraction lives in the Vcs trait.

## Related joints

- [verb-vs-composition](./verb-vs-composition.md) — whether a verb
  earns its keep at all.
- [vcs-as-seam](./vcs-as-seam.md) — the inner-boundary counterpart;
  what belongs in the Vcs trait vs. in rwv core.
