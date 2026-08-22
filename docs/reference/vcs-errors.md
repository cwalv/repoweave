# VCS errors

When a VCS call underneath `rwv` fails, rwv does not decline — it was stopped.
The failure still carries a stable kebab-case name for the *condition*, and
that name is what a machine consumer is meant to branch on rather than parsing
the message text. This page has one entry per name, so a kind read off a
machine surface can be looked up without translation.

Sync's own per-repo failure kinds are here too. They say why a repo did not
reach the target, and they are on this page because they are in the same JSON
object as the names above: sync's kind is the outer one and the VCS kind, when
there is one, is the `cause.kind` beneath it. A reader holding a failed repo
outcome needs both, and splitting them across two pages would publish how rwv
is built rather than anything about the failure.

## How this page differs from Refusals

[Refusals](./refusals.md) names conditions rwv *could have acted on and
declined*. The names here are the other half: a git call rwv was in the middle
of came back with a failure it could recognise. rwv did not decline — it was
stopped. VCS and filesystem passthroughs are outside the refusal class on
purpose: the absence of a refusal token is meant to be informative, so a
passthrough must not borrow one.

The practical consequence is that **the exit is usually git-side**. A refusal's
exit is something to do to rwv; most exits below are something to do to the
repository, after which the rwv command re-runs unchanged.

A condition that two registers name keeps its single entry wherever that entry
already lives — `mid-operation` and `untracked-collision` are shared with the
refusal register and are documented on [Refusals](./refusals.md);
`head-unreadable` is shared with `rwv doctor` and is documented on [Doctor
findings](./doctor-findings.md). `rwv explain` serves an entry without regard
to which page holds it, so the split costs a reader nothing.

## Where these names appear

- `rwv sync --json` / `rwv sync-to --json`, under a failed repo outcome, as
  `failure.kind` — always present — and as `failure.cause.kind` when the
  failure carries a typed cause at all. The `cause` field is omitted rather
  than null when it does not, so read it as optional.
- The published schemas under `docs/reference/schemas/`, where the same set
  appears as an `"enum"`.

`rwv explain <kind>` serves the entry below verbatim.

---

### `command-failed`

**Condition.** A VCS command failed for a reason rwv has no more specific name
for. Carries the arguments rwv passed and the VCS's own stderr.

**What it means.** This is the fallback, and it is deliberately a wide one:
implementations map to the most specific kind they can detect and land here for
everything else. Its presence is not itself a diagnosis — the stderr it carries
is.

**Exits.** Read the `stderr` field; it is the VCS's own account, unedited. Run
the same command by hand (`args` is exactly what rwv passed) to see it in
context.

**Note for consumers.** Do not branch on this kind expecting a stable
condition. A failure that gains a specific kind in a later release stops
arriving as `command-failed`, and that is the intended direction of change:
this set grows at `command-failed`'s expense.

### `ff-impossible`

**Condition.** A sync failure kind. `--strategy ff` could not advance a repo to
the target, because the target is not ahead of where that repo's HEAD already
is.

**What it means.** `ff` is the strategy that will not rewrite history: it moves
a branch pointer forward or it stops. A repo that has diverged — local commits
the target does not contain — has nothing to fast-forward to, so the strategy
has run out of moves rather than hit an error. This kind never carries a
`cause`: nothing underneath failed.

**Exits.** Re-run with `--strategy rebase` to replay the local commits on top of
the target. That is the deliberate choice `ff` exists to make you make, because
rebase rewrites the repo's history and fast-forward does not. If the local
commits are not wanted, move the branch yourself and re-run unchanged.

### `hook-rejected`

**Condition.** A `git worktree add` registered its destination and then failed,
which is the shape a `post-checkout` hook rejection takes.

**What it means.** The registration is written before the hook runs, so the
destination is a registered worktree even though the command failed. The
`stderr` is the hook author's text, not git's — git's own output names no hook,
which is why this cannot be recognised by reading it.

**The evidence is weaker than the name.** What rwv observes is that the add got
past writing the admin entry and then died. A refusing hook does that; so would
a checkout that failed of its own accord at the same point. Nothing has
reproduced the second case, but it is an unmeasured gap rather than an excluded
one — read `stderr` before concluding a hook is involved.

**Exits.** Read `stderr` for the hook's own reason and satisfy it. The
destination is registered, so clear it with `git -C <repo> worktree remove
<path>` (or `git -C <repo> worktree prune` if the directory is already gone)
before re-running.

### `io`

**Condition.** An I/O operation underneath a VCS call failed: rwv could not
spawn git, could not read what git wrote, or could not read or write a file the
operation needed.

**What it means.** This is the most-raised kind in the register, and the
broadest — it covers spawning the subprocess, decoding its output, and the
handful of files rwv reads or writes around a git call (`.gitattributes`, a
destination directory). `ctx` says which of those was being attempted and on
what path; `message` is the operating system's own account, carried across the
wire boundary as text because an `io::Error` does not serialize.

**Read `ctx` and `message` together.** Neither is sufficient alone: `ctx` names
rwv's intent, `message` names the failure. A `ctx` of `failed to spawn git …`
with a not-found `message` means git is not on `PATH`; the same `ctx` with a
permission `message` means it is, and is not executable by you.

**Exits.** Depends entirely on `message`, and the usual causes are outside rwv:
git missing from `PATH`, a permission denied on the repo or its parent, a full
or read-only filesystem, or a path that another process removed while the
operation was running. Fix the underlying condition and re-run — rwv wrote
nothing on this path, so there is nothing to undo.

### `not-a-repo`

**Condition.** A path rwv needed to be a repository is not one, or is not there.

**What it means.** rwv reached a path the manifest or lock names and found
nothing it can operate on. The common causes are a clone that never completed,
a directory removed by hand, and a manifest entry whose path no longer matches
the tree.

**Exits.** `rwv doctor` reports the structural version of this across the whole
workspace and names the repair. For a single missing checkout, `rwv fetch`
re-clones it from the manifest. If the path is wrong rather than the tree, fix
the manifest entry and re-lock.

**Where this sits.** [Doctor findings](./doctor-findings.md) covers the
workspace-scale form.

### `rebase-conflict`

**Condition.** An in-flight replay stopped on a conflict that needs a person.
The `op` field names which replay it was. Its type admits `rebase`, `merge` and
`cherry-pick`; every raise today is a rebase, so `rebase` is the only value a
consumer will actually receive.

**What it means.** The repository is left in the VCS-native in-flight state, on
purpose: conflict markers in the working tree and the replay's own state
directory. Nothing was rolled back, because rolling back would discard the part
of the replay that already applied.

**Exits.** Resolve the conflicted files and stage them, then resume with the rwv
verb that started the replay — `rwv sync --continue` or `rwv sync-to
--continue`. Resume through rwv rather than through git directly: rwv has
remaining phases after the replay, and a replay finished behind its back leaves
them undone. To abandon instead, `rwv abort` rolls the op back to the state it
started from.

### `rebase-failed`

**Condition.** A sync failure kind. `--strategy rebase` did not land a repo on
the target. Says that the replay did not finish, not why.

**What it means.** The why is in `cause`, and reading it is the whole point of
the split: a conflict needing a person and a repo git could not read are the
same kind here and different `cause.kind`s underneath. A conflict arrives as
`rebase-conflict` and leaves the repo mid-replay; anything else is a git
failure that changed nothing.

**Exits.** Branch on `cause.kind` and follow that entry — `rwv explain` serves
it the same way it served this one. With no `cause`, the `message` field is all
there is; that combination means the replay failed through a path that had no
typed error to carry, and the message is the VCS's own account.

### `revision-not-found`

**Condition.** A named revision — SHA, tag, or branch — did not resolve in the
repository.

**What it means.** Most often the lock pins a revision the local clone has never
fetched, or one that was rewritten or garbage-collected upstream after the lock
was written.

**Exits.** `rwv fetch` first: an unfetched-but-existing revision is the common
case and fetching resolves it. If it still does not resolve, the revision is
gone rather than absent, and the lock has to move — `rwv lock` re-snapshots
from the tips the manifest tracks.

**Where this sits.** [Lock as derived](../explanation/joints/lock-as-derived.md)
covers why the lock is re-snapshotted rather than repaired.

### `stale-ref-witness`

**Condition.** rwv observed where a repo's HEAD was, and by the time it acted on
that observation the repo had moved. Carries what was expected and what was
found.

**What it means.** rwv re-observes before every ref write rather than trusting
the earlier read, and refuses when the two disagree. Something moved the repo in
between: another rwv invocation, a git command in another terminal, an editor's
VCS integration.

**Exits.** Re-run. The second run observes the repo where it now is and proceeds
from there. If it repeats, something is moving the repo concurrently — find it
before re-running, because the refusal is the only thing standing between that
process and an interleaved write.
