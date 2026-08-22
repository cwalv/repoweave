# Refusals

When `rwv` declines to act on purpose, it prints the reason and then one more
line:

```
rwv explain <token>
```

That token is a stable kebab-case name for the *condition* — not for the
message, which may be reworded, and not for the site, which may move. This
page has one entry per token, under that exact name, so a refusal you met in
the terminal can be looked up without translation.

Every refusal's message is written to stand on its own: it names what stopped
and what unblocks. You should not need this page to act on one. What an entry
adds is the part a message cannot carry — why the rule exists, which exit
applies under which circumstances, and where the rule sits in the machine.

## Reading an entry

Run the command the refusal printed:

```bash
rwv explain push-from-workweave
```

`rwv explain` serves the entry below verbatim. A token whose condition `rwv
doctor` also reports keeps its single entry on
[Doctor findings](./doctor-findings.md) — one condition has one entry
wherever that entry lives, and `rwv explain` finds it either way.

## What does and does not carry a token

A token means **rwv could have acted and declined**: a precondition it will
not cross, or an input it will not accept. Absence is informative. An error
with no token is a passthrough (a VCS or filesystem failure), an internal
invariant, or an argument the parser rejected before any verb ran — those
print `error:` and exit 2, where a refusal prints `Error:` and exits 1.

---

### `ambiguous-workweave-address`

**Condition.** Two recorded `(project, workweave)` pairs render the same flat
`<project>--<name>` address, so `-w` names more than one checkout.

**Why.** The flat address is meant to be a bijection: one spelling, one
workweave. When it is not, acting on "the" match would pick one of two live
checkouts by accident, and the wrong one is destructive on a delete.

**Exits.** Address the workweave by path with `-C` instead, which is
unambiguous. To remove the collision permanently, retire one of the two and
recreate it under a name that does not collide.

**Where this sits.** The flat rendering and the characters it reserves are in
[Formats](./formats.md); the addressing model is
[Workweave hierarchy](../explanation/joints/workweave-hierarchy.md).

### `backslash-in-repo-path`

**Condition.** A repo path spells `\`.

**Why.** A manifest repo path is a POSIX-shaped key that becomes a directory
path on every platform. Accepting `\` would make one manifest mean two
different trees depending on the OS reading it.

**Exits.** Spell the path with `/`. This is a validation refusal — nothing was
written, so there is nothing to undo.

**Where this sits.** [Formats](./formats.md) states the path grammar.

### `claude-settings-missing`

**Condition.** `rwv setup claude` was asked to edit a settings file that does
not exist.

**Why.** The verb edits an existing settings document rather than creating
one, because creating it would mean inventing defaults for a tool rwv does not
own.

**Exits.** Run the tool once so it writes its own settings file, then re-run
`rwv setup claude`.

### `dangling-primary`

**Condition.** A workweave marker records a `primary:` path that no longer
exists.

**Why.** The marker is how a workweave knows which weave it belongs to. With
the primary gone, every path the marker implies is unresolvable, and rwv
cannot tell a moved primary from a deleted one.

**Exits.** If the primary moved, re-point the marker at its new location. If
it was deleted, this workweave is orphaned — retire the directory. `rwv doctor
--fix` does not guess between those two.

**Where this sits.** [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md).

### `derivation-inputs-moved`

**Condition.** An attested input changed while a generated file was being
written from it.

**Why.** rwv records what a generated file was derived from so a later read
can tell "this is current" from "this looks plausible". If an input moves
mid-write, the record would attest a derivation that never happened as a whole.

**Exits.** Re-run the verb. The write was abandoned rather than half-recorded,
so the previous state is intact.

**Where this sits.** [File ownership](../explanation/joints/file-ownership.md).

### `dirty-checkout`

**Condition.** A checkout the verb must move, record or delete carries
uncommitted tracked changes.

**Why.** Sync rebases or fast-forwards these repos; a lock records their tips.
Running over tracked dirt would either leave a half-rebased tree needing `rwv
abort`, or record a tip that does not describe what is on disk.

**Exits.** Commit or stash the named changes, then re-run — the refusal lists
the exact files. Untracked files are not this condition and never refuse: they
survive rebase and fast-forward untouched.

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md);
the lock's derived status is [Lock as derived](../explanation/joints/lock-as-derived.md).

### `dirty-project-dir`

**Condition.** `rwv workweave create` found uncommitted changes in the source
project directory and no `--capture-dirty`.

**Why.** A workweave forks the project repo. Uncommitted work is not on any
branch, so a plain fork silently leaves it behind in the source while the
operator believes it came along.

**Exits.** Commit the changes and create; or pass `--capture-dirty`, which
carries them into the new workweave — the flag is named for the consequence it
accepts.

### `dropped-repo-has-unique-commits`

**Condition.** A checkout being dropped from the lock carries commits no other
clone reaches.

**Why.** Dropping a repo removes its checkout. If it holds the only copy of
some commits, removal destroys them, and no `rwv abort` recovers work that was
never anywhere else.

**Exits.** Push the commits to their remote, or merge them somewhere that
survives, then re-run. If the commits are genuinely unwanted, remove the
checkout by hand first so the loss is a deliberate act.

**Where this sits.** [Destructive operations](../explanation/destructive-operations.md).

### `empty-selector-pattern`

**Condition.** `--repo re:` or `--repo glob:` was given with nothing after the
prefix.

**Why.** An empty pattern matches everything or nothing depending on the
engine, and a repo subset that silently means "all" is the shape that runs a
destructive verb over the whole weave.

**Exits.** Supply a pattern, or drop `--repo` entirely to mean all repos
explicitly.

### `foreign-tip`

**Condition.** `rwv abort` found a ref at a tip the recorded operation does not
account for.

**Why.** Abort restores each repo to the savepoint the op took. A tip the op
never wrote means something else moved that ref, and restoring would discard a
change the operation is not responsible for.

**Exits.** Inspect the named repos and decide: if the foreign commits are
wanted, move them somewhere safe first; if not, reset the ref yourself and
re-run `rwv abort`.

**Where this sits.** [Shared refs and drift](../explanation/joints/shared-refs-drift.md).

### `foreign-workweave-in-target-dir`

**Condition.** The target directory is already another workweave's root.

**Why.** Two workweaves at one path would share a marker and an index entry
while disagreeing about which is which, and every later address would resolve
to whichever was recorded last.

**Exits.** Choose a different directory, or retire the workweave that is there
first. Reusing the directory is deliberately not offered here — see
`target-dir-occupied` for the occupant rwv can adopt.

### `head-not-on-branch`

**Condition.** HEAD is detached, unborn, or unreadable in a repo the verb must
land on or move.

**Why.** Landing work means advancing a branch. A detached HEAD names no
branch to advance, and an unborn one has no commit to advance from, so the
verb has nowhere to put the result.

**Exits.** Check out the branch you meant. For an unborn branch, make an
initial commit first. For push specifically, the canonical branch is the one
publishing requires — see `project-repo-off-canonical-branch`.

**Where this sits.** [Branch model](../explanation/joints/sync-semantics.md).

### `health-floor-too-low`

**Condition.** The weave records a health floor below the version this binary
requires.

**Why.** Repair arms for older states are removed at each release rather than
carried forever. A binary that cannot repair the state it finds must say so
instead of acting on assumptions that no longer hold.

**Exits.** Step through the bridge version the message names: install it, run
`rwv doctor --fix --all` until `rwv doctor --all` reports clean — which records
the floor — then upgrade again.

**Where this sits.** [Pyramid of stability](../explanation/joints/pyramid-of-stability.md).

### `inapplicable-flag`

**Condition.** A flag was passed in a mode where it has no meaning.

**Why.** Accepting it silently would let an operator believe a constraint was
applied that nothing read. The refusal is deliberately generic: a per-flag
token would name the remedy's argument rather than a state.

**Exits.** Drop the flag, or switch to the mode that reads it — the message
names which.

### `invalid-ref-name`

**Condition.** A name is not usable as a ref-name component.

**Why.** rwv mints refs from names an operator supplies. A name git will not
accept as a ref becomes a failure deep inside a VCS call, far from the input
that caused it, so it is rejected at the boundary instead.

**Exits.** Choose another name. The rules are git's rather than rwv's, and the
message names which one was broken.

**Where this sits.** [Formats](./formats.md) lists the characters rwv's own
naming rules exclude on top of git's.

### `lock-state-mismatch`

**Condition.** Manifest repos disagree with the committed lock, at push time.

**Why.** The project repo carries the lock that pins manifest SHAs. Publishing
it while the clones say something else would hand collaborators a lock
describing a tree that does not exist anywhere.

**Exits.** `rwv lock` to re-pin from the working tips, or check out the locked
revisions in each repo the message names. Which one is right depends on
whether the tree or the lock is the state you meant.

**Where this sits.** [Lock as derived](../explanation/joints/lock-as-derived.md).

### `malformed-provider`

**Condition.** `--provider` is not `registry/owner`.

**Why.** The provider is two fields, and a single token could be either one.
Guessing would silently create a project under the wrong owner.

**Exits.** Spell it `registry/owner`.

### `malformed-repo-path`

**Condition.** A repo path is not `registry/owner/repo`.

**Why.** The three segments are what makes a path resolvable to a clone URL
without a lookup table. A shorter path names no registry, so no URL can be
inferred.

**Exits.** Supply all three segments, or pass a full URL and let `rwv add`
derive the path.

**Where this sits.** [Formats](./formats.md).

### `malformed-workweave-address`

**Condition.** `-w` is not `<project>--<name>` with both halves non-empty.

**Why.** The flat address is the one spelling that identifies a workweave
without a path. Half an address cannot select one.

**Exits.** Give both halves, or address by path with `-C`.

### `marker-binding-disagreement`

**Condition.** The project this operation is bound to and the one the marker
records are not the same.

**Why.** A workweave is created for one project and holds that project's
repos. Acting under a different binding would write one project's state into
another's checkout.

**Exits.** Run the verb from the workspace whose marker matches, or pass the
project the marker records. Do not edit the marker to agree — that hides the
disagreement rather than resolving it.

### `mid-operation`

**Condition.** A repo is mid-rebase, mid-merge, or otherwise mid-operation.

**Why.** These states hold a partially-applied result that only the tool that
started them can finish. Running over one would compound two incomplete
operations.

**Exits.** Finish or abort the in-flight VCS operation in the named repo, then
re-run.

### `missing-creation-param`

**Condition.** An `rwv add --new` creation address's registry declares a
required parameter the supplied map has no entry for.

**Why.** `plan_creation` cannot mint a URL or an upstream from an incomplete
parameter set, and guessing a value would place or create something the
operator did not ask for.

**Exits.** Add `--param <name>=<value>` for each parameter the refusal names
— the same declared surface `rwv explain add`'s creation-parameter table
renders, so the two cannot disagree.

### `missing-lock`

**Condition.** `--frozen` was passed and there is no lock file.

**Why.** `--frozen` means "use exactly what the lock pins". With no lock there
is nothing to be faithful to, and proceeding would resolve versions afresh —
the opposite of what the flag asks for.

**Exits.** Drop `--frozen` to resolve and write a lock, or fetch a project that
carries one.

### `nested-project-name`

**Condition.** The name would place a project inside another project.

**Why.** Projects are siblings under `projects/`. A nested one would be found
by the containment walk as part of its parent, and every path derived from it
would be ambiguous.

**Exits.** Choose a name that does not traverse into an existing project.

### `no-active-project`

**Condition.** No project was supplied and none is selected here.

**Why.** Project-scoped verbs act on one project's repos. With no selection
rwv would have to pick, and picking wrongly acts on the wrong tree.

**Exits.** `rwv activate <name>` to select one, or pass `--project <name>` for
a single invocation.

**Where this sits.** [Weave root](../explanation/joints/weave-root.md).

### `no-explain-entry`

**Condition.** `rwv explain` has no page for the name given.

**Why.** `rwv explain` is reflection over core's committed, CI-checked
surfaces. It never consults `$PATH`, so a plugin's name is not an entry here
even when the plugin is installed and dispatchable.

**Exits.** `rwv explain` with no argument lists everything served. For a
plugin, its own `--help` is the documentation.

**Where this sits.** [Plugin boundary](../explanation/joints/plugin-boundary.md).

### `no-matching-registry`

**Condition.** No registry recognises the argument, in either direction — a
URL that maps to no local path, or a path that infers no URL.

**Why.** The registry set is what makes paths and URLs interconvertible. An
argument outside it cannot be placed on disk or fetched.

**Exits.** Use a URL from a supported registry, or add the repo under an
explicit three-segment path so no inference is needed.

### `no-op-recorded`

**Condition.** `--continue` or `abort` was invoked where no operation is
recorded.

**Why.** Both act on op-state. With none present there is nothing to resume or
roll back, and inventing a starting point would replay from a state no
operation ever established.

**Exits.** Omit `--continue` to start a new operation. If you expected one to
be in flight, it completed or was already aborted.

### `no-remote-default-branch`

**Condition.** The remote publishes no default branch.

**Why.** rwv records what to track rather than guessing `main`. A remote with
no published default gives nothing honest to record, and a fabricated guess
would be wrong silently.

**Exits.** The message names the repair for the VCS in question — set the
remote's default branch, then re-run.

### `no-remote-url`

**Condition.** A local clone has no conventional remote to read a URL from.

**Why.** The manifest records where a repo comes from. A clone with no remote
has no origin to record, and an invented one would not fetch.

**Exits.** Add the remote, or supply the URL explicitly.

### `no-such-workweave`

**Condition.** The address names no recorded workweave.

**Why.** Addressing is by record, not by directory scan: a directory that
looks like a workweave but is not recorded is not addressable, deliberately.

**Exits.** `rwv workweave <project> list` shows what is recorded. If the
directory exists but is unrecorded, `rwv doctor --fix` adopts it when it sits
in the recorded container under the expected name.

**Where this sits.** [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md).

### `no-weave-root`

**Condition.** The containment walk found no weave above the working
directory.

**Why.** Every path rwv resolves is relative to a weave root. Without one
there is no frame to interpret the command in.

**Exits.** `cd` into a weave, or `rwv init` to create one here.

**Where this sits.** [Weave root](../explanation/joints/weave-root.md).

### `occupied-placement`

**Condition.** An `rwv add --new` creation address places at a path the
manifest already maps to a different URL.

**Why.** Placement is a function of the identity a registry mints, not of a
creation parameter like `--param root=<dir>`. Two different roots naming the
same owner and repo would both place at the same path if this proceeded
silently, and the second creation would overwrite the first's meaning in the
manifest without saying so.

**Exits.** Use a different owner or repo so the two do not collide, or remove
the existing entry first if replacing it is what's intended.

### `not-fast-forwardable`

**Condition.** The tips are not in the fast-forward relation `--strategy=ff`
requires.

**Why.** `ff` is the strategy that promises to add nothing and rewrite
nothing. Where a fast-forward is impossible, honouring that promise means
stopping rather than quietly doing something else.

**Exits.** Re-run with `--strategy rebase` to replay the divergent commits, or
reconcile the repos by hand first. Past replay the exit is different — see
`target-diverged-mid-op`.

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md).

### `occupied-bootstrap-dir`

**Condition.** The bootstrap target is neither a workspace nor empty, and no
consent flag was given.

**Why.** Bootstrapping writes weave structure into a directory. Doing that
over unrelated content mixes two trees with no record of which files were
whose.

**Exits.** Choose an empty directory, or pass `--allow-non-empty-dir` to
accept bootstrapping over what is there.

### `op-acquisition-raced`

**Condition.** The atomic op-state create lost to a peer whose record then
vanished.

**Why.** Two processes tried to start an operation at once. One won and then
finished or aborted, so by the time this process looked, the record it lost to
was gone — a race, not a stuck state.

**Exits.** Retry. This is distinct from `op-in-progress`, where a record is
present and the exits are resume-or-abort rather than retry.

### `op-in-progress`

**Condition.** A sync or sync-to operation already holds op-state covering
this workspace.

**Why.** An operation in flight is rewriting the very repos another verb would
read or move. Op-state is held deliberately across a crash — there is no
auto-expiry, because a stale record and a live one are indistinguishable from
outside.

**Exits.** Two, and the message names both: resume with `--continue` from the
owning workspace once the cause is resolved, or `rwv abort` to roll the whole
operation back. Waiting is only an exit if the operation is genuinely still
running.

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md).

### `op-parked`

**Condition.** A phase stopped and op-state is held at that phase.

**Why.** The refusal that stopped the phase changed nothing, and the operation
is still recorded. Both ways out of it remain open, which is what a fresh
refusal cannot tell you.

**Exits.** Fix the cause the message names, then `--continue`; or `rwv abort`
to roll the whole operation back. The route line for a parked operation names
the *gate* that fired where that gate has its own token, so you may be sent to
a more specific entry than this one.

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md).

### `owned-branch-moved`

**Condition.** A receipted ref is not at the tip its receipt records.

**Why.** rwv retracts only refs it authored and still recognises. A ref that
moved since the receipt was written may carry someone else's work, and
retracting it would destroy commits rwv never accounted for.

**Exits.** Inspect the ref. If the new commits are wanted, move them somewhere
that survives; if they are not, reset the ref to the recorded tip and re-run.

**Where this sits.** [Shared refs and drift](../explanation/joints/shared-refs-drift.md).

### `partial-run-aborted`

**Condition.** Some repos failed, and an artifact that would otherwise be
written was deliberately withheld.

**Why.** This is not a bare failure tally. A lock or success record written
over a partial run would describe a state that never existed, so the run
declines to write it — the withholding is the decision this token names.

**Exits.** Fix the failures the run listed, then re-run. Nothing partial was
recorded, so the re-run starts from the same place this one did.

### `project-dir-missing`

**Condition.** The named project has no `projects/<name>/` in this workspace.

**Why.** A project is its directory. rwv will not create one implicitly to
satisfy an address, because a typo would then produce an empty project rather
than an error.

**Exits.** Check the spelling against `rwv status`, or `rwv fetch` the project
into this workspace if it belongs here.

### `project-dir-occupied`

**Condition.** The project directory name is already taken.

**Why.** Two projects at one directory name would share every derived path.
rwv will not merge into an occupied slot, because the occupant's files are not
rwv's to reinterpret.

**Exits.** Fetch under a scoped path — the refusal prints the scoped form —
or choose a fresh directory.

### `project-push-withheld`

**Condition.** Manifest repos failed to push, so the project repo was
deliberately not pushed.

**Why.** The project repo carries the lock pinning manifest SHAs. Publishing
it after a partial manifest push would advertise revisions collaborators
cannot fetch. Withholding it is the decision, not an accident of ordering.

**Exits.** Fix the manifest-repo failures the run listed and re-run `rwv push`.
Manifest-side state may already be published; the lock carrier is not, so no
collaborator has seen an inconsistent pair.

### `project-repo-off-canonical-branch`

**Condition.** The project repo is attached to a non-canonical branch at push
time.

**Why.** Publishing is defined from the canonical branch. Pushing from a topic
branch would publish a lock nobody's canonical history reaches.

**Exits.** Check out the canonical branch the message names and re-run. If the
work belongs on it, land it there first.

### `provider-cannot-mint-url`

**Condition.** `rwv init --provider <registry>/<owner>` names a registry that
cannot construct a clone URL from an owner and a project name alone.

**Why.** `init --provider` mints a URL and adds it as the project repo's
remote; it carries no creation surface of its own — no `--root`, no
parameters — so a registry whose `clone_url` needs more than that, `local`
being the case that exists, has nothing to mint from.

**Exits.** Create the project without `--provider` (`rwv init <name>`), then
set the remote once the repository exists elsewhere.

### `push-from-workweave`

**Condition.** `rwv push` was invoked from a workweave.

**Why.** Workweave branches are ephemeral and named for a seat, not for
publication. Pushing them would leak per-seat branch names to a shared remote
where nothing retracts them. Unlike `wrong-checkout-kind`, push *is* defined
at a workweave — it is refused on branch-hygiene policy, which is why it has
its own entry.

**Exits.** `rwv sync-to` from the workweave to land the work on the parent,
then push from there.

**Where this sits.** [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md).

### `receipt-revision-uncanonical`

**Condition.** A receipt's recorded revision is not a commit id.

**Why.** Receipts prove which tip rwv authored a ref at. A tag or branch name
there would resolve differently later, so the proof would not be one.

**Exits.** This is a corrupt record rather than an operator mistake. `rwv
doctor` reports it; the repair is to retract and re-create the receipt through
the verb that authored it.

### `registry-path-disagreement`

**Condition.** Two paths both claim to be this workweave.

**Why.** The index maps a name to one path. Two claimants mean a delete could
remove either, and the one it removes is not the one the operator addressed.

**Exits.** Inspect both directories and retire the stale one, then re-run.

### `repair-target-changed`

**Condition.** The object `--fix` re-observed before acting is gone or moved.

**Why.** `--fix` observes, decides, then re-observes immediately before
writing. A target that changed in that window is one another process is
touching, and repairing it would act on a state nobody surveyed.

**Exits.** Re-run `rwv doctor --fix`. This is a race rather than a fault:
whether it vanished or merely moved is timing, which is why one token covers
both.

### `replace-target-holds-work`

**Condition.** The reuse or replace target holds work that no flag available
here consents to losing.

**Why.** Consent flags are named for the specific consequence they accept. This
state mixes predicates — uncommitted changes and unlanded commits together —
and no single flag names that combined loss, so none is offered.

**Exits.** Deal with the work directly: commit or discard the changes, land or
abandon the commits, then re-run. The refusal lists what it found.

**Where this sits.** [Destructive operations](../explanation/destructive-operations.md).

### `repo-not-in-manifest`

**Condition.** `rwv remove` names a path the manifest does not carry.

**Why.** Removing something absent is either a typo or a stale script. Silently
succeeding would let both go unnoticed.

**Exits.** Check the spelling against `rwv status`. The clone may still exist
on disk without a manifest entry — that is `rwv doctor`'s orphaned-clone
finding rather than this.

### `repo-without-commits`

**Condition.** A repo the create would fork has no resolvable HEAD.

**Why.** A workweave forks each repo at a commit. A repo with no commits has
no point to fork from, and creating an empty checkout would produce a
workweave whose repos are not comparable to the source's.

**Exits.** Make an initial commit in the named repo, or remove it from the
manifest if it does not belong.

### `resume-contradicts-record`

**Condition.** `--continue` arguments disagree with the recorded operation.

**Why.** The record is what the operation is; the arguments are what the caller
believes it is. Proceeding under the caller's belief would resume a different
operation than the one that stopped.

**Exits.** Re-invoke `--continue` with arguments matching the record — the
message names both sides — or `rwv abort` if the record is the thing that is
wrong.

### `retire-not-converged`

**Condition.** `--retire`'s merged-check found divergence from the target.

**Why.** Retiring deletes the workweave. That is only safe once everything in
it is reachable from the target; divergence means some of it is not, and the
delete would take it.

**Exits.** Land the divergent commits — re-run `sync-to` without `--retire`
first — then retire. Or inspect and discard them deliberately.

**Where this sits.** [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md).

### `shared-clone-referenced`

**Condition.** `--delete`'s clone is referenced by other projects, and
`--delete-shared-clone` was not given.

**Why.** One clone can serve several projects. Deleting it because one project
dropped the repo would break the others without their asking.

**Exits.** Drop the repo from the manifest without `--delete` to leave the
clone in place, or pass `--delete-shared-clone` to accept removing it for
every project that references it.

### `shared-name-contested`

**Condition.** Two projects claim one weave-root surfacing name.

**Why.** Surfacing links live at the weave root, which is a single namespace.
Two claimants mean whichever activates last wins, and the loser's link
silently points at the other's file.

**Exits.** Rename the surfaced file in one of the projects, or stop surfacing
it there.

**Where this sits.** [Symlinks as structure](../explanation/joints/symlinks-as-structure.md).

### `state-claim-held`

**Condition.** A peer process holds the state-file claim past the wait budget.

**Why.** State files are written under a claim so two processes cannot
interleave writes. The budget exists so a hung peer surfaces as a refusal
rather than an indefinite hang.

**Exits.** Wait for the other process and re-run. If nothing is running, the
claim is stale — `rwv doctor` reports and repairs that case.

### `store-claims-unreadable`

**Condition.** The records that would prove a store unclaimed could not be
read.

**Why.** The delete is gated on nothing claiming the store. When the claim
records cannot be read, rwv cannot show the gate is satisfied, so it fails
closed — an unreadable claim is treated as a claim.

**Exits.** Fix what made the records unreadable — permissions, a partially
removed worktree registration — then re-run. Do not delete the store by hand to
get past this: the claims it could not read may be real.

**Where this sits.** [Destructive operations](../explanation/destructive-operations.md).

### `store-still-claimed`

**Condition.** Live worktrees or standing receipts claim the store a delete
would destroy.

**Why.** Deleting a store takes every ref and object with it at once, for every
checkout derived from it. A claim means at least one such checkout is live.

**Exits.** Delete the workweaves that hold the claims first — that removes
their worktrees and retracts their receipts — then re-run. The refusal lists
the claimants.

**Where this sits.** [Clone topology](../explanation/joints/clone-topology.md).

### `surfacing-path-occupied`

**Condition.** Something rwv did not create sits where a surfacing link
belongs.

**Why.** rwv replaces links it authored, never files it did not. An occupant
with no provenance may be an operator's real file, and overwriting it would be
silent data loss.

**Exits.** Move or delete the occupant yourself, then re-run the verb that
surfaces it.

**Where this sits.** [Symlinks as structure](../explanation/joints/symlinks-as-structure.md).

### `target-dir-occupied`

**Condition.** The directory slot is taken by an occupant rwv cannot address.

**Why.** Distinct from `foreign-workweave-in-target-dir`: that occupant is a
workweave rwv could name and retire, this one is not addressable, so there is
no verb that can clear it.

**Exits.** Clear the directory yourself, or choose another with `--dir`.

### `target-diverged-mid-op`

**Condition.** At advance time the target carries commits the current tip does
not.

**Why.** Replay already happened against the target as it was. A target that
moved since means the plan this operation computed no longer describes the
landing, and re-choosing a strategy now would replay against a base the earlier
phases did not use.

**Exits.** `rwv abort` and start again against the target's current state. This
is deliberately not `not-fast-forwardable`'s remedy — past replay, a different
strategy is not the fix.

**Where this sits.** [Sync semantics](../explanation/joints/sync-semantics.md).

### `target-lock-behind`

**Condition.** A sync-to target's committed lock is behind its own HEAD.

**Why.** sync-to replays against the target's committed lock. A lock behind
HEAD means those commits would be missing from the replayed tip, and the final
fast-forward could not land.

**Exits.** Run `rwv lock --commit` in the target workspace, then re-run. There
is deliberately no `--allow-stale-lock` here: skipping the check does not make
the missing commits appear, which is why this is not `stale-lock`.

**Where this sits.** [Lock as derived](../explanation/joints/lock-as-derived.md).

### `unaccepted-generated-content`

**Condition.** Attested generated files hold content rwv never accepted, and no
consent was given.

**Why.** rwv records a digest when it accepts a generation. Content that does
not match was written by something else, and regenerating over it would
discard an edit nobody recorded.

**Exits.** Inspect the difference. Keep the edit by moving it somewhere rwv
does not own, or accept the regeneration with the consent flag the message
names.

**Where this sits.** [File ownership](../explanation/joints/file-ownership.md).

### `uncommitted-work`

**Condition.** A workweave being deleted carries uncommitted changes, and
`--discard-uncommitted` was not given.

**Why.** Deleting takes the working tree with it. Uncommitted work exists
nowhere else, so the flag is named for exactly the loss it accepts.

**Exits.** Commit or land the work, or pass `--discard-uncommitted` to accept
losing it. Unlanded *commits* are a separate consent — see `unmerged-commits`
— because one flag cannot name two different losses.

**Where this sits.** [Destructive operations](../explanation/destructive-operations.md).

### `uncompilable-selector`

**Condition.** A `--repo` pattern does not compile.

**Why.** An uncompilable pattern selects nothing, and a repo subset that
silently selects nothing looks like a successful no-op run.

**Exits.** Fix the pattern. The engine's own error is quoted in the message;
that text belongs to the pattern library rather than to rwv.

### `unknown-finding-kind`

**Condition.** `--kind` names nothing in the doctor wire vocabulary.

**Why.** Filtering on an unknown kind would report zero findings, which is
indistinguishable from a clean weave — the most dangerous silent success in the
tool.

**Exits.** The refusal lists the valid kinds, derived from the vocabulary
itself. [Doctor findings](./doctor-findings.md) has an entry for each.

### `unknown-registry`

**Condition.** `--provider` names a registry rwv does not have.

**Why.** The registry is what turns a path into a clone URL. An unknown one
cannot do that, and defaulting would fetch from somewhere the operator did not
name.

**Exits.** Use one of the built-in registries, or give a full URL so no
registry lookup is needed.

### `unknown-role`

**Condition.** A role value names no known role, including the legacy
spellings.

**Why.** Roles decide what sync and push will do to a repo. An unrecognised
role has no defined behaviour, and defaulting one would apply a policy the
manifest did not ask for.

**Exits.** Use one of the values in [Roles](./roles.md). Legacy spellings are
recognised well enough to be rejected by name rather than silently accepted.

### `unknown-verb`

**Condition.** The name is neither a core verb nor an `rwv-<verb>` executable
on `$PATH`.

**Why.** rwv dispatches unknown verbs to plugins by convention. A name that
matches neither is a typo or a missing install, and guessing between them
would run the wrong thing.

**Exits.** `rwv --help` lists core verbs. For a plugin, check it is installed
and named `rwv-<verb>`. This is distinct from `no-explain-entry`, which
refuses names that *do* dispatch.

**Where this sits.** [Plugin boundary](../explanation/joints/plugin-boundary.md).

### `unmerged-commits`

**Condition.** A workweave being deleted has commits not reachable from the
target, and `--discard-unmerged-commits` was not given.

**Why.** The commits exist only on the ephemeral branch the delete retracts.
The flag names that specific loss — separately from uncommitted changes, which
is a different loss and has its own flag.

**Exits.** `rwv sync-to` to land the commits, then delete; or pass
`--discard-unmerged-commits` to accept losing them.

**Where this sits.** [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md).

### `unowned-branch-in-namespace`

**Condition.** The ephemeral name is held by a ref with no receipt.

**Why.** rwv only moves refs it authored and can prove it authored. A ref in
the ephemeral namespace with no receipt was created by something else, and rwv
does not claim a branch it did not write.

**Exits.** Delete the ref yourself if it is stale, or create the workweave
under a different name. Whether rwv noticed at creation or at delete time is
timing rather than a different state, which is why one token covers both.

**Where this sits.** [Shared refs and drift](../explanation/joints/shared-refs-drift.md).

### `unpushable-repo-branch`

**Condition.** Repos in the push plan are not on a pushable branch.

**Why.** Push publishes each repo from its checked-out branch. A detached or
otherwise unpushable checkout has nothing to publish, and the run stops before
the network rather than half-publishing.

**Exits.** Check out a branch in each repo the run listed, then re-run. Nothing
was pushed, so there is no partial publication to reconcile.

### `unreadable-status`

**Condition.** A dirty-state read failed, so the operation failed closed.

**Why.** The gate asks whether a checkout is clean. When that cannot be
answered, treating it as clean would rebase over unknown state — so an
unreadable repo is refused rather than assumed.

**Exits.** Inspect the named repos directly (`git -C <repo> status`) and fix
what makes them unreadable, then re-run. No commit-or-stash advice is offered
here on purpose: rwv cannot enumerate changes it could not read.

### `unrenderable-name`

**Condition.** A name spells a character the flat rendering reserves.

**Why.** Project and workweave names are rendered into a single flat address.
The excluded characters are the ones that would make two distinct pairs render
the same string, so the address would stop identifying one checkout.

**Exits.** Choose another name. [Formats](./formats.md) lists the excluded
characters and the rule each one protects.

### `unresolvable-repo-source`

**Condition.** The source parses as neither a URL nor a known shorthand.

**Why.** rwv accepts both spellings and converts between them. An argument that
is neither cannot be placed on disk or fetched, and guessing which was meant
would clone from somewhere unintended.

**Exits.** Give a full URL, or `owner/repo` shorthand for a supported registry.

### `untracked-collision`

**Condition.** Untracked files stand where the operation must write.

**Why.** Untracked files are normally harmless — they survive rebase and
fast-forward untouched, which is why dirt checks ignore them. This is the
exception: these specific paths are ones the operation would overwrite, and
they are not in any commit to recover from.

**Exits.** Move or delete the named files, then re-run.

### `unusable-creation-param`

**Condition.** An `rwv add --new` creation parameter's value cannot be used
as given: a name the registry does not declare, a `--params-json` entry that
is not a JSON string, a `root` directory that does not exist, a `root` inside
the weave it would create a member of, or a name supplied through more than
one spelling (the address's own three-segment shorthand, `--param`, and
`--params-json` all write into the same map, and a name given twice is a
disagreement about intent rather than a precedence question a default could
answer silently).

**Why.** Each of these would silently create something other than what the
operator asked for — a directory materialised from a typo, a parameter no
registry reads, an upstream the weave itself would walk, delete, and report
on as one of its own members, or a value picked by unstated precedence
between two the operator gave.

**Exits.** Correct the named parameter and re-run. For a `root` that does not
exist, create it first; for one inside the weave, choose a directory outside
it; for a name given twice, supply it in only one spelling.

### `version-is-a-pin`

**Condition.** `version:` names a revision where it must name a branch.

**Why.** `version:` declares what to *track*; the lock records where you *are*.
A SHA or tag there collapses the two, leaving nothing to track and a lock that
can never move.

**Exits.** Put a branch name in `version:`. To hold a repo at a fixed revision,
commit the lock rather than overloading the tracking field.

**Where this sits.** [Lock as derived](../explanation/joints/lock-as-derived.md).

### `workweave-name-taken`

**Condition.** The index already records this name at another path.

**Why.** A name maps to one workweave. Recording a second would make every
later address ambiguous, and the delete that followed would remove whichever
was found first.

**Exits.** Choose another name, or retire the recorded workweave first. If the
recorded path no longer exists, `rwv doctor` reports the stale entry and
`--fix` clears it.

### `wrong-address-flag`

**Condition.** An addressing flag was given the other flag's argument shape.

**Why.** `-C` takes a path and `-w` takes a flat address. Each is unambiguous
alone; swapped, one would resolve to nothing and the other to the wrong
checkout. One token covers both directions because it is one mistake seen from
two ends.

**Exits.** `-C <path>` for a directory, `-w <project>--<name>` for a recorded
workweave.

### `wrong-checkout-kind`

**Condition.** The verb is defined for the other kind of checkout.

**Why.** Some verbs act on the primary weave and some on a workweave, because
the state they change lives at one and not the other. Running one in the wrong
place would write state that its own reader never looks for.

**Exits.** `cd` to the checkout the verb is defined at — the message names
which and where it is. `push` is not this condition: it *is* defined at a
workweave and refused on policy, under `push-from-workweave`.

**Where this sits.** [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md).
