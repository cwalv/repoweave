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
line-granular (`append_ignore_line` in `src/workweave_index.rs`): the two
writers that share one surface under different claims — the workweave index's
and the owned-digest ledger's, both records of `projects/<p>/` — cannot drop
each other's line, because an append publishes only the missing line and never
a stale image of the whole file. An external whole-file rewrite remains
detect-don't-prevent: the next publish of a record re-appends its lost line,
and doctor's tracked-index finding is the net behind a copy that reaches the
index. Driven and pinned by `tests/ignore_surface_concurrency_test.rs`.

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
