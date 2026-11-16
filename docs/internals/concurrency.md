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
gives its index and refs: atomic publication (write-then-rename, or an
exclusive publish where exclusivity is the point), and serialized read-modify-write. A
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

**An exclusive publish has two modes and a refusal, chosen per call.** The
op record, every lease, every claim and every takeover guard is written
whole into a sibling temp, fsynced, and published by `durable_file::create_new`
in one step that fails on an occupied path: a reader sees nothing or the whole
file, and of two racing writers exactly one wins and the other reads the
winner's complete file. The first mode is a hard link of the temp. The second,
reached only when the link fails with an error that says this filesystem
makes no links, is a rename that refuses to replace: Linux
`renameat2(RENAME_NOREPLACE)`, macOS `renamex_np(RENAME_EXCL)`, Windows
`MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`. Both are atomic and
exclusive within one filesystem; the rename's exclusivity is the kernel's
check under the parent directory's lock on Linux, and the server's on SMB.
Where the rename is not made either, the publish refuses
`filesystem-without-exclusive-publish`, naming the filesystem as the OS
reports it (`statfs` on Linux and macOS, `GetVolumeInformationW` on Windows).
An exclusive create followed by a write is never a fallback: a writer killed
between the two leaves an empty file that every reader of an op record or a
claim refuses, and no verb clears it.

The refusal comes before an operation's first write wherever the directories
it will publish in are known then: `durable_file::refuse_without_exclusive_publish`
publishes a temp onto another temp-shaped name and removes both (a probe
killed midway leaves only names the sweep removes), and it runs for the owner
and lease workspaces in `acquire_op`, the project store in a landing's
`guard_and_mark` before the record, and the index store and the new
workweave's nearest existing directory in `workweave create`. A directory
known only later (a landing's target, a workspace moved in place, a takeover
guard) refuses at its own publish, and the caller's arm takes back what was
written before: each such arm is reached by a test in
`tests/process/exclusive_publish_test.rs` through `RWV_TEST_WITHHOLD_UNDER`,
which narrows the withheld modes to one path prefix. `rwv activate` run alone
is not probed: its ledger claim follows the generator's write, which is in the
workspace the claim is in and is regenerated on the next run.

Which errors select the rename is a list of raw codes, never an
`ErrorKind`, because `PermissionDenied` covers both a filesystem without links
and a permission truly denied:

| Call | Selects the next mode | Platform |
|---|---|---|
| `link(2)` | `EPERM` (vfat, exfat: no link operation), `EOPNOTSUPP`/`ENOTSUP`, `ENOSYS` (some FUSE and SMB mounts) | unix |
| `CreateHardLinkW` | `ERROR_INVALID_FUNCTION`, `ERROR_NOT_SUPPORTED` (FAT, exFAT) | Windows |
| `renameat2` / `renamex_np` | `EINVAL` (no `RENAME_NOREPLACE`: NFS, FUSE without rename2), `ENOSYS` (kernel before 3.15, or a platform with no such call), `EOPNOTSUPP`/`ENOTSUP` (no `RENAME_EXCL`) | unix |
| `MoveFileExW` | `ERROR_INVALID_FUNCTION`, `ERROR_NOT_SUPPORTED` | Windows |

`EPERM` from `link(2)` means no links here on vfat and exfat, and the rename
is reached on it. It has other causes: `fs.protected_hardlinks` on a temp a
mount reports as another user's (a forced `uid=`, a 9p or FUSE share with
mapped ownership), an immutable source, a seccomp policy denying `linkat`.
On those the rename is as exclusive and as atomic as the link, so nothing is
downgraded; where the rename is refused too, the refusal names the filesystem
when the cause was ownership or policy, a wrong diagnosis rather than a wrong
outcome. A denied directory write is `EACCES`, which fails at the temp before
any link. `EMLINK` (a filesystem whose links stop at one) is not a selector
and surfaces as itself. Every other error, `EACCES` and `ERROR_ACCESS_DENIED` included, is
reported as it is and never retried in another mode. A filesystem that makes
links never reaches the rename. On Windows `ERROR_ACCESS_DENIED` and
`ERROR_SHARING_VIOLATION` are first retried in the same mode for the claim
wait (`held_open`): Windows answers them while another handle has the file
open, and for every open of a name whose file was deleted until its last
handle closes, which a released claim's holder and waiters keep it for a
moment. One that outlasts the wait is reported as it is.

FUSE without rename2 that also has no links, and FAT on macOS, refuse. No
filesystem without hard links can be mounted in the gate without privilege,
so the suite stands one in by withholding a mode from a test build
(`RWV_TEST_WITHHOLD`, read only with the `test-pause` feature), and the error
it returns is the one such a filesystem returns.

**Residuals of the exclusive publish.** These are known and not guarded:

- A userspace filesystem that accepts `RENAME_NOREPLACE` and ignores it. The
  kernel's check under the parent directory's lock covers racers through one
  mount, so the rename is exclusive within one mount, or wherever the server
  enforces the flag. Two mounts, or two hosts, of such a server can each
  rename onto the name and each be told it won.
- An NFS `link` the server made and answered, retransmitted, with `EEXIST`.
  The writer then reads its own file as the winner's and refuses on itself.
- A directory fsync that fails after the publish. The name exists and the
  caller is told the publish failed.

**A killed writer's temp is removed by the next publish of its name.** A temp
is never at a published name, so one left behind blocks nothing. Every
publish, `create_new` and `replace` alike, first removes each temp beside its
target whose name begins with the target's and that no writer holds. A temp's
name carries the pid, a key drawn once per process and a per-process serial,
so a name is never reused. On unix a writer holds an advisory `flock` on its
temp from its creation until the publish ends, which the kernel drops when the
writer dies, and the sweep removes only a temp whose lock it takes. On Windows
a writer holds its temp open without sharing deletion until just before it
publishes, so the sweep's delete fails while it is held. The lock decides
nothing but that a temp is garbage: no record's or claim's standing reads it.
A writer whose temp a sweep met in the instant before it held it, or after it
let go on Windows, finds it gone at the publish and stages it again. Where
the filesystem takes no lock, or honors it only on one host (some NFS and
SMB mounts), the sweep takes no temp it cannot lock, or removes one a writer
on another host still holds, which that writer then stages again: temps may
be left, and nothing else changes.

**One liveness primitive tells a live holder from a dead one.**
`src/liveness.rs` is the only place that asks whether a process still holds a
file. A holder takes an advisory lock on the file and keeps it while it runs;
the kernel drops it when the process dies. A Windows lock is mandatory, so
there the byte locked lies past the file's content and a reader of the
holder's pid and host is never refused. A temp's sweep, a short claim's
waiters and a scratch owner's sweep all ask `try_hold`, which answers `Taken`
(the file has no holder), `Busy` (a live holder) or `Unavailable` (the
filesystem has no advisory locks, tested under `RWV_TEST_WITHHOLD=advisory-lock`).
`Unavailable` never refuses the call that asked: the file's body names the
holder's pid and host, and a process on the same host reads the pid
(`holder_is_gone`); a holder on another host is never shown gone from here.
What is then left, and only then, is a temp or scratch file whose owner
cannot be shown dead, and for a short claim the refusal that names its holder
and says to rerun from that host, where the pid check takes the claim over, or
to run `rwv doctor --fix --release-unreachable-claims` where that host is gone
(the `claim-holder-unreachable` finding, found only where the lock is
`Unavailable`).
A short claim is a file at a published name (`.rwv-fetch.lock`,
`rwv-workweave-index.lock`, `.rwv-owned-digests.lock`) created with
`create_new`, then locked; its holder removes it before it lets go of the
lock, and a waiter that locked the removed file checks the path still names
the file it locked (`is_current`: device and inode on unix, volume and file id
on Windows) before it counts itself the holder. A file
left by a killed holder is locked by the next claimant, with no wait.

**Scratch files a call makes in a git dir go through one owner.** Every
private index and detached checkout a call makes (`rwv-scratch-<pid>-<name>`:
the drift refresh's two indexes, the commit, records, backup and replay
indexes, the merge checkout) is named for the process beside an owner file
`rwv-scratch-<pid>` that the process holds locked (`Scratch`,
`src/git/scratch.rs`). Each call first removes every other owner's files whose
lock it can take, runs `git worktree prune` when one was a checkout, and
removes its own when it drops. A call is never refused for want of the lock.

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
the verb refuses as `state-claim-held`, naming the holder. The claim orders rwv's own fetches; a `git fetch` typed by hand into
the same store takes no `.rwv-fetch.lock`.

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

Other rwv verbs respect a live claim (`refuse_moving_claimed` in
`src/landing_claim.rs`): `rwv switch` (the branch it leaves and the one it
opens, and each member branch its alignment would switch a member onto:
`Aligner::refuse_claimed` in `src/alignment.rs`, which a verb judging a move
it has not begun calls; an op's own alignment under its claim does not), `rwv branch delete` and `rwv workweave delete` (through
`deletion::Rule::open`), `rwv branch rename` (both names), `rwv branch
set-mode`, `rwv member set-branch` (the project branch it commits to and the
member branch it re-points), `rwv push` (each branch it publishes) and the
refs `rwv update --remote` moves or settles each check, before they write,
whether any branch they would move, delete, rename, re-point or publish is
the target of a claim another op holds or a branch that op's record lands
into or carries (`OwnerRecord::claimed_branches`), and refuse
`state-claim-held`, naming the holder and its two exits. `rwv sync`, `rwv
pull`, `rwv cherry-pick` and `rwv land` move only branches checked out in
their own workspace, or under their own claim: a landing leases the workspace
holding any branch it moves in a checkout, and `rwv switch` and `rwv member
set-branch` refuse to open a claimed project or member branch, so they meet
the lease. Raw git is left to each
move's compare-and-swap.

The claim names its op and the workspace holding the op's record (JSON). The
record is written before the claim is taken and cleared only after it is
released, so a claim whose workspace is here (it reads as a workspace root)
and records no op with its id, or no record at all, is dead, and the next
`take` removes it. Only that is evidence: a workspace that is not here to read
(deleted, on another mount, outside a sandbox) is never judged dead, since
from here a deleted workweave and one this process cannot see look alike. A
landing waits on such a claim and refuses `claim-owner-unreachable`;
`rwv doctor` reports it and `--fix` releases it (`unreachable_claims`,
`release_unreachable`). One residual: the guard `rwv doctor --fix` removes
an unreachable claim under names the workspace it runs in, which records no
op, so a second `rwv doctor --fix` racing the first reads that guard as dead
and goes ahead too; if a landing creates its claim between the first's
removal and the second's, the second's re-read finds the new bytes and leaves
them, but a second whose re-read came before that create removes the new
claim. Only two concurrent fixes of the same claim reach it. Removal is
serialized by a
takeover lock beside the claim, `<claim>.takeover.<dead op>`, created
exclusively and naming its holder as the claim does: only that holder
removes the claim, and only after re-reading the bytes it judged dead. While
the claim reads those bytes nothing can replace it (no create succeeds on an
occupied path, and its dead op releases nothing), so the re-read and the
removal are one step to every other taker; a taker that judged an older
claim dead finds the live one and leaves it. A lock is removed the same way
(`remove_dead`): a lock whose holder is dead is removed under its own lock,
`<lock>.takeover.<its holder>`, re-read there, so no live lock is removed
either. A lock whose holder is a standing op (a landing killed in its
takeover, its record kept) is waited on as a claim is, and the refusal names
that op, whose `rwv abort` removes the locks it holds. A landing that has just
taken a claim removes every lock beside it whose holder is dead
(`sweep_dead_takeovers`), so a lock left by a taker whose op ended without
removing it does not stay; it never blocked anything, since it guards only
the removal of a claim reading the dead op's bytes, and that op never claims
again. The claim path is never emptied while it holds a live claim
(`tests/process/landing_claim_contention_test.rs` drives this with takers in
separate processes). No process id, host or clock is consulted, so a
claim whose op is recorded is never taken from it. A claim and a takeover guard carry
a `format` marker (`CLAIM_FORMAT`, written by `Holder`, read only as it); one
that names another format is a claim this rwv does not read. A claim whose
bytes this rwv does not read, another release's included, or a takeover guard beside it that this rwv does not read,
is never judged dead either: a landing waits on it and refuses
`claim-unreadable`, and `rwv doctor` reports it and releases it only under
`--fix --release-unreadable-claims`, or on one target only under
`--fix --release-unreadable-claims-on <target>`, which the refusal prints
(`unreadable_claims`, `release_unreadable`, `is_on_target`), which removes a file only while it holds the bytes the
same run reported. The removal is a compare and then an unlink, not one step:
a landing of another release that releases its claim, and a taker that claims
the target, both inside that instant, would lose the new claim. The flag is
the operator's word that no such landing stands. Taking a claim is an
exclusive publish of a fully written temp (`durable_file::create_new`, below),
which fails on an occupied path, as the op record's is. The compare-and-swap of each move stays the
guard against every writer that takes no claim.

**A landing waits on the claim holding only its record.** The record is
written before the wait, with the branch the landing goes into and the
consent it was given (`TakeInRecord::sync_from`, `allow_member_change`), and
no plan. While it waits it prints whom it waits for and what ends the wait.
A landing killed while it waits leaves that record: `rwv abort` clears it,
and `rwv land --continue` takes it up as a landing that did nothing yet
(`resume_before_the_plan`): it starts again under its own op id, waiting on
the claim again, and a refusal there clears the record as a fresh landing's
does. A landing killed after it took the claim and before it recorded its
plan is taken up the same way, finding the claim its own.

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
