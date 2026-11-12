# Concurrency posture

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

The calibration for judging any concurrency or interference concern in rwv:
which races are defects to fix, which are detection gaps, and which are out of
scope by design. The space of things that *could* go wrong — a second rwv, an
editor, a build, an agent, a crash at any instruction — is too large to guard
against exhaustively; this document is the budget that decides where the effort
goes.

## Git is the reference budget — goal and ceiling

rwv orchestrates git repositories and inherits its correctness economics from
git. The effort git spends to stay correct is the target for the analogous rwv
surface, and effort git declines to spend marks the boundary of what rwv guards
against. Both directions bind: a surface where rwv is sloppier than git's
analog is a defect, and a proposal requiring more caution than git applies to
the analogous surface is out of scope by default — the case for it must argue
the analogy fails, not that a failure is imaginable.

## Two tiers of state

**rwv's own metadata: sound.** The records only rwv writes — op-state records,
ownership ledgers, workspace markers, lock snapshots — get the treatment git
gives its index and refs: atomic publication (write-then-rename, or a `link(2)`
claim where exclusivity is the point), and serialized read-modify-write. A
crash must not tear them, and a concurrent rwv must not silently drop another
invocation's write. A missing property here is a defect, not a hardening
opportunity. The lease machinery in [op-state.md](op-state.md) is this tier's
exclusion primitive.

The workweave index is one of these records, and its claim sits beside it in the
project clone's shared directory, not in the project's working tree. The primary
and every workweave of a project write the one index from different checkouts,
so the claim has to be the one file they all reach, which that directory is.

Readers of these records take no claim, as git's readers take no
`index.lock`: the claim serializes writers, and write-then-rename is all a
reader needs. Windows will not rename onto a file while any handle to it is
open — POSIX rename semantics lift that only on a filesystem that has them,
and only for holders that share deletion — so there a reader, or a scanner
that opened what rwv just wrote, fails a publish for as long as it holds the
file. The publish retries the replacement for the claim budget
(`rename_over` in `src/durable_file.rs`), as git for Windows retries its own
rename, rather than make every reader take the claim git's readers never
take.

**A fetch into an object store is claimed: one at a time per store.** Two
fetches into one store race on the refs they update, and the loser dies on a
ref lock. A canonical clone's store is shared by its workweaves' worktrees, so
seats working in parallel meet there. Every pruning fetch — `rwv fetch`, and
the fetch `rwv pull` and `rwv update --remote` run before they act — therefore
runs inside `FetchClaim::hold` (`src/fetch_claim.rs`), which holds the claim
file `.rwv-fetch.lock` in the store directory, and `Vcs::fetch_pruning` takes
the held claim as a witness, so a fetch without one is not expressible. It is
the same state-claim mechanism as the workweave index's claim
(`StateFile::FetchClaim` beside `StateFile::IndexClaim` in
`src/state_file.rs`): a peer is waited for within the shared claim budget, then
the verb refuses as `state-claim-held`, naming the file to delete when no rwv is
running. The claim orders rwv's own fetches; a `git fetch` typed by hand into
the same store takes no `.rwv-fetch.lock`.

The claim also covers the one record a fetch keeps: `FetchClaim::fetch_pruning`
writes, beside the claim, when each remote the fetch completed did
(`StateFile::FetchRecord`, `.rwv-fetched`), so the record is per store and per
remote, as the remote-tracking refs it dates are. It is rwv's record of its own
fetches, so a transfer from a local path or a hand `git fetch` does not move it.

**A landing claims its target: one landing at a time per target.** `rwv land`
takes the claim on its target branch (`LandingClaim::take` in
`src/landing_claim.rs`, the file `.rwv-landing.lock` under `rwv-landings/` in
the project repo's store, one directory per project and branch, named by a
digest) after it writes its op record and before it reads the target's tips
or leases the target's workspace, so a second landing into the same target
waits and then reads the tips the first one left, rather than losing a
compare-and-swap or meeting the first one's lease. The wait lasts while the
holder's op record keeps changing and the shared claim budget past its last
change. The claim is held for as long as the op stands, across every stop:
dropping the value releases nothing, so a stopped landing as refs, which
leases no workspace, still keeps others off what it moved. It is given up when
the record moves to `retire` and when an op with no retire ends
(`release_target`), the lease on the target's workspace first, and by `rwv
abort` by op id, whatever the record holds. A release removes the claim only
while it names its own op. A `rwv land --continue` or `rwv abort` that took
the claim, or a lease, its op did not hold when the command began puts back
what it took when it is refused (`LandingClaim::put_back`, `Retaken`), so a
landing that had released its target never blocks it again by being refused;
one that held the claim before keeps it.

A carry takes the same claim on every dependent it moves
(`Carrier::carry` in `src/stack.rs`), before it composes any of them, so a
landing into a dependent waits on a stopped carry rather than landing on
tips the carry is about to move. Those claims are held until the op that
carries ends (`feature_sync::act`, the landing's `cleanup`) or is aborted, and
the refusal names the holder's own continue command, read from its record.

The claim names its op and the workspace holding the op's record (JSON). The
record is written before the claim is taken and cleared only after it is
released, so a claim whose workspace records no op with its id, or no record
at all, is dead, and the next `take` removes it. Removal is serialized by a
takeover lock beside the claim, `<claim>.takeover.<dead op>`, created
exclusively and naming its holder as the claim does: only that holder
removes the claim, and only after re-reading the bytes it judged dead. While
the claim reads those bytes nothing can replace it (no create succeeds on an
occupied path, and its dead op releases nothing), so the re-read and the
removal are one step to every other taker; a taker that judged an older
claim dead finds the live one and leaves it. A lock whose holder is dead is
passed over for one named after that holder as well, so no lock is removed
but by its holder, and `rwv abort` of an op removes the locks it holds.
The claim path is never emptied while it holds a live claim. No process id, host or clock is consulted, so a
claim whose op is recorded is never taken from it. A claim whose bytes this
rwv does not read is never judged dead. Taking it is a hard-link publish of a
fully written temp (`durable_file::create_new`), which fails on an occupied
path, as the op record's is. The compare-and-swap of each move stays the
guard against every writer that takes no claim.

**The workspace state takes no claim.** `rwv-workspace-state` lives in one
checkout's private directory and only a verb acting on that workspace writes it,
published whole. Two mutating verbs racing in one workspace are what git leaves
unguarded in one worktree, so the file is atomic and not serialized. One that
does not parse reads as absent and is reported by the verb that reads it; losing
it loses no record.

**The index is the record of a workweave's parent.** The worktree relation says
nothing of `parent`, so of the three records a marker is restored from, two
speak to it, and reconcile takes the index's. A change of parent therefore
writes the index first and the marker second (`workspace::reparent`): a stop
between them leaves the index ahead, and the next verb in that workweave
restores the marker forward to it. The other order would have that verb undo
the change.

**The working tree: detect, don't prevent.** Member checkouts, manifests, and
everything else that editors, builds, and other processes legitimately touch is
shared, unlocked space. rwv never locks it and never assumes exclusive access —
exactly as git cannot stop an editor writing a file during `git add`.
Interference with the working tree is at most a detection problem, never an
exclusion problem.

**Ignore surfaces are the working tree, written from the sound tier.** The
`.gitignore` / `.git/info/exclude` line that keeps a machine-local record out
of VCS is git's file, not rwv's: operators and other tools legitimately write
it, and git takes no lock over it. So it earns no exclusion of its own — a
claim here would bind only rwv's processes while installing the wedged-lock
failure mode on a file rwv does not own. Instead the hygiene write is
line-granular (`append_ignore_line` in `src/workweave_index.rs`): writers that
share one surface cannot drop each other's line, because an append publishes
only the missing line and never a stale image of the whole file. The owned-digest
ledger, a file of `projects/<p>/`, is such a writer; the workweave index is not,
since it lives in the project clone's shared directory and needs no ignore line.
An external whole-file rewrite remains detect-don't-prevent: the next publish of
a record re-appends its lost line. Driven and pinned by
`tests/process/ignore_surface_concurrency_test.rs`.

## The attestation invariant

One rule binds both tiers: **rwv must never record a claim it did not
observe.** A verb that derives state from working-tree inputs and records
provenance for the result must re-read those inputs at the moment of recording,
and refuse loudly — with rerunning the verb as the stated remedy — if they no
longer match what the derivation consumed. This is git's racy-index
discipline: where git cannot be certain a file was unchanged while being
recorded, it re-checks rather than trusting the record. Detection converts a
silent false attestation into a loud retry. Prevention is neither required nor
attempted: the edit itself may be entirely legitimate; only the stale record of
it is a defect.

## Advisory refusals

Where an in-flight-operation marker exists, a verb that would interleave with
the operation refuses to start — the analog of git refusing to act mid-rebase.
These checks are best-effort reads: the window between check and start is
accepted, because the attestation invariant is the sound backstop behind them.
An advisory refusal earns its place by closing the common case cheaply; it must
never be cited as an exclusion guarantee. Checking the marker and acquiring the
lease are different acts — advice and exclusion — and code and docs must not
describe one as the other.

## Out of scope, deliberately

- **Universal per-verb mutual exclusion.** Git does not serialize its commands
  against each other, and it accepts real cost even for its few narrow locks —
  a stale lock file requires a human to remove. Extending exclusive leases
  across the whole verb surface buys little the two rules above do not already
  provide, and it installs the wedged-workspace failure mode everywhere.
- **External processes touching the filesystem mid-operation.** Not
  preventable, and not rwv's job. The attestation invariant bounds the damage
  to a loud retry.
- **Wrong content, honestly recorded.** rwv guards the honesty of its own
  records, not the wisdom of the edits they describe.

Per [testing.md](testing.md), a deliberate silence is a prohibition: an
out-of-scope declaration a reader could reasonably expect covered carries a
pinning test naming the boundary.

## Judging a new concern

In order: (1) whose state is touched — rwv's own metadata, where a gap is a
defect, or the working tree, where the question is at most detection? (2) does
any record end up claiming something unobserved? Then the attestation invariant
applies. (3) would the proposed guard outspend git on the analogous surface?
Then it is out of scope by default.
