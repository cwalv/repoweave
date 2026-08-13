# Doctor findings

Every finding `rwv doctor` reports carries a stable kebab-case `kind` tag.
This page has one entry per `kind`, under that exact token, so a finding you
met in the terminal can be looked up without translation.

Each `rwv doctor` line is written to stand on its own: it names the rule it is
enforcing and the command that repairs it, and you should never need this page
to act on one. What this page adds is the part a one-line message cannot carry
— why the rule exists, why `--fix` declines to act where it does, and where to
read further.

## Finding your entry

The tag is not printed in the default text output. Ask for it:

```bash
rwv doctor --json | jq -r '.violations[] | .kind + (
    if .sub_kind == null then ""
    elif (.sub_kind | type) == "string" then " / " + .sub_kind
    else " / " + (.sub_kind | keys[0]) end)'
```

The two `sub_kind` shapes are why that expression looks the way it does: a
sub-kind with no fields of its own is a plain string, one that carries fields
is a single-key object whose key is the tag.

Findings an integration raised sit on a second array and take the same two
shapes in their `kind`:

```bash
rwv doctor --json | jq -r '.issues[] |
    (if (.kind | type) == "string" then .kind else (.kind | keys[0]) end)'
```

Eleven kinds carry a `sub_kind` — `branch-discipline`, `clone-topology`,
`dead-op-lease`, `index-drift`, `missing-replay-exclusion`,
`orphaned-savepoint`, `provenance`, `weave-root-identity-conflict`,
`working-tree-drift`, `workweave-drift`, `workweave-tree-integrity`. Where the
sub-kind decides the disposition the entry has a sub-heading per `sub_kind`;
where it only narrows what was found, one entry covers them all.

The full wire shape is in [Formats](./formats.md), and the committed JSON
Schema is `docs/reference/schemas/doctor.json`.

## Vocabulary used throughout

**Auto-fixable** — `rwv doctor --fix` repairs it without further input.

**Report-only** — `--fix` will not touch it, and that is a decision rather
than a gap. Either the repair needs a judgment rwv cannot make, or the state
is indistinguishable from work you meant to keep.

**Report-only by default** — `--fix` repairs it only when you also pass the
flag the entry names. The flag is named for the consequence it accepts, never
as a blanket override.

Every entry below opens with one of those three marks, and the marks are the
published statement of what `--fix` does. `rwv doctor --help` and
`rwv explain doctor` point here rather than restating the set, and a test
compares each mark against the code that repairs it — so an arm that is added,
removed or changes disposition without this page moving with it fails the
build.

**Safe class / live class** — a distinction applied to anything droppable. A
ref or savepoint whose tip is already reachable from a live branch carries no
unique commits, so removing it loses nothing; a tip carrying commits nothing
else reaches is live work and is never removed automatically. The reasoning is
in [Shared-refs drift](../explanation/joints/shared-refs-drift.md).

**Ownership receipt** — a persisted record, keyed to one exact ref in one
exact store, saying rwv created that ref. rwv destroys a ref only against a
receipt. A branch that merely *looks* like one of rwv's — a hand-made
`<project>--<workweave>` — is yours, is reported so you can see it, and is
never deleted. Ownership is by record, never by name shape. The normative rule
set is in the repo at `docs/internals/branch-model.md`; that file documents
the implementation and is not part of this published book.

---

## Manifest, lock, and repo registration

### `orphaned-clone`

**Error. Report-only.** A directory under a registry path that no project's
`rwv.toml` lists. rwv will not remove a directory it was never told about.

**What to do:** `rwv add <url>` to register it, or remove the directory
yourself. Note that without `--all`, orphan detection is skipped entirely — a
repo missing from the active project may belong to another one, and reporting
it would be a false positive.

### `dangling-reference`

**Error. Report-only.** An `rwv.toml` entry whose path is not on disk.

**What to do:** `rwv fetch` re-materializes it in place, or drop the entry
from the manifest if the repo is genuinely gone.

### `missing-role`

**Warning. Report-only.** An `rwv.toml` entry with no `role` field. Role is
how rwv knows a repo's change resistance and whether it belongs in generated
ecosystem workspace files.

**What to do:** add one. See [Roles](./roles.md).

### `unparseable-project`

**Error. Report-only.** A project directory exists but does not load: either
its `rwv.toml` or its `rwv.lock` failed to parse. It is reported at error
severity specifically so a project rwv cannot see into does not read as a
clean project with zero findings.

**What to do:** the message names which of the two files failed, and carries
that file's remedy — the manifest is yours to edit, at the line the parser
names; the lock is generated, so `rwv lock` rewrites it. Note that the
`manifest_path` field always names the manifest, because it locates the
project rather than the failure.

`--fix` deliberately has no arm for either. Rewriting a manifest rwv could not
parse would be guesswork, and regenerating a lock re-pins the workspace to
whatever the working tips happen to be — a decision about state, not a repair.

### `legacy-manifest-format`

**Error. Report-only.** A project directory holds an `rwv.yaml`, the name the
manifest had before it became TOML, and no `rwv.toml`. It is reported at error
severity because nothing in the project loads: without this finding the
directory would read as having no manifest at all and be passed over in
silence.

**What to do:** rewrite it as `rwv.toml` by hand and delete the `rwv.yaml`.
`--fix` deliberately has no arm here, for the same reason as
`unparseable-project` above and one more: the manifest is hand-authored, and
the comments and key order you wrote have no mechanical translation between
the two formats. A conversion that guessed at them would be worse than the
refusal. See [Formats](./formats.md) for the shape to write.

### `stale-lock`

**Error. Report-only.** A repo's HEAD does not match the SHA `rwv.lock` pins.

**What to do:** `rwv lock` to re-pin from the working tips, or `rwv sync` to
bring the tree to the locked revisions. Which you want depends on whether the
tree or the lock is the state you meant. The lock is a derived artifact, not a
source of truth — see [Lock-as-derived](../explanation/joints/lock-as-derived.md).

### `incomplete-lock`

**Error. Report-only.** A manifest entry with no `rwv.lock` entry at all — a
coverage gap rather than a freshness one. Only raised for a project that has a
lock file; a project with no lock yet is a separate, unlocked state.

**What to do:** `rwv lock` to write the missing entry.

### `unresolvable-lock-entry`

**Error. Report-only.** An `rwv.lock` entry names a revision this clone cannot
resolve — a lock written against history that was never fetched here. The
entry is kept as a finding rather than dropped: dropping it would leave the
repo with no locked revision to compare against, which reads as healthy.

**What to do:** fetch the missing history, or `rwv lock` to re-pin from the
working tips if the lock is the stale side.

### `head-unreadable`

**Error. Report-only.** A repo present under a registry directory whose HEAD
could not be read. Every freshness comparison for that repo — stale lock,
drift, provenance — is unevaluated, and reporting the read failure is what
keeps the repo from looking clean.

**What to do:** the `error` field carries what git said. A directory that is
not a git repo, a corrupt `.git`, and a permissions problem all land here.

### `projects-dir-unreadable`

**Error. Report-only.** The `projects/` directory exists but could not be
listed — a permissions problem, most plausibly. Every project under it is
invisible to this scan, and without this finding that reads exactly like a
workspace that genuinely has none: a broken registry and an empty one would
otherwise print the same "clean" result. A `projects/` that does not exist
yet (before the first `rwv add`) is a different, unremarkable state and does
not raise this.

**What to do:** the `error` field carries what the filesystem said. Fix the
permissions (or whatever is blocking the listing) and re-run `rwv doctor`.

### `missing-replay-exclusion`

**Warning. Auto-fixable.** A project repo's `.gitattributes` lacks the
`rwv.lock merge=rwv-ours` entry. Without it, `rwv sync`'s rebase carries your
lock edits through the merge inputs instead of letting the lock be
regenerated from the result.

The `sub_kind` says which repair applies. `absent` — no entry for `rwv.lock`
at all. `legacy-spelling` — the entry is there under the pre-rename
`merge=ours`, which reads as satisfied to a human and satisfies nothing:
sync's check matches the current name, and `ours` collides with a driver a
global git config may define for something else entirely.

**What to do:** `rwv doctor --fix` appends the line — or migrates the legacy
spelling in place — and commits it when the repo has no other staged changes.
The committed form is what sync's invariant reads, so the commit is the part
that makes it take effect.

### `replay-exclusion-unreadable`

**Warning. Report-only.** Reading the project repo's `.gitattributes` failed,
so the replay exclusion is neither confirmed present nor confirmed missing.
Reported rather than swallowed: an unevaluated invariant that stays silent
reads exactly like one that holds.

**What to do:** the `error` field carries what git said. Once the file is
readable, re-run `rwv doctor` to get the real answer.

### `missing-merge-driver-config`

**Warning. Auto-fixable.** The project repo does not define the `rwv-ours`
merge driver in its own git config. `rwv sync` passes the definition per
invocation, so its own rebase is unaffected — but a bare `git rebase
--continue` you run afterwards is not, and git treats an undefined driver as
`merge=binary`: conflict markers in `rwv.lock` where the lock was supposed to
be regenerated.

**What to do:** `rwv doctor --fix` plants the config. `config_key` names the
key it writes.

### `merge-driver-config-unreadable`

**Warning. Report-only.** Reading the project repo's git config for the
merge-driver definition failed, so the definition is neither confirmed present
nor confirmed missing.

**What to do:** the `error` field carries what git said.

### `phantom-merge-driver`

**Warning. Report-only.** A `.gitattributes` line assigns an `rwv-`-prefixed
merge driver that rwv does not define. git resolves `merge=<name>` through
`merge.<name>.driver` config and falls back to an ordinary textual three-way
merge, silently, when nothing defines the name. The line reads like a working
declaration and does nothing.

The `rwv-` prefix is what makes this diagnosable rather than presumptuous:
that namespace is rwv's, so a name inside it that rwv does not define is one
nothing will ever define.

**What to do:** correct the driver name, or define the driver in git config.

---

## Weave identity and the workweave registry

### `dangling-active-project`

**Error. Auto-fixable.** `.rwv-active` names a project whose
`projects/<name>/` directory is not on disk. Every verb that reads the active
project would fail with a confusing downstream error.

**What to do:** `rwv doctor --fix` clears the pointer.

### `weave-root-identity-conflict`

**Error.** A weave root carries both `.rwv-active` and `.rwv-workweave`. The
two files name the same fact — which project this tree belongs to — and are
mutually exclusive by design: a primary root carries the pointer, a workweave
root carries the marker. A tree holding both holds two copies of its own
identity with nothing keeping them in agreement.

Fixability turns on evidence held *outside* the tree, because the two files
are themselves the ambiguity. Primary-ness has no independent signature: a
primary root and a workweave root both hold `projects/` and registry
directories. The workweave registry at
`<primary>/projects/<project>/.rwv-workweave-index` is the outside witness —
it is written only by `rwv workweave create` and records the absolute path of
every workweave it made, so a tree the registry names is a workweave on the
authority of a file that tree does not contain and could not have forged by
being copied.

#### `registered-workweave`

**Auto-fixable.** The marker names this workspace's primary, and that
primary's registry records this exact directory. The tree is a workweave, so
`.rwv-active` is the redundant copy.

**What to do:** `rwv doctor --fix` deletes the pointer and leaves the marker.

#### `marker-unverifiable`

**Report-only.** The marker itself cannot witness what it claims: it is
unreadable, a legacy marker (YAML, or missing the required `parent:` field),
or names a `primary:` that verifies as no workspace at all. A marker that
cannot prove its own claim cannot prove which of the two files is the stray
either.

**What to do:** repair the marker first — `rwv doctor --fix` migrates a legacy
one; an unreadable or dangling one needs a hand edit — then re-run `rwv
doctor` to classify the pointer against the repaired marker. Never
auto-fixed: `--fix` does not touch `.rwv-active` here.

#### `unwitnessed`

**Report-only.** The marker parses and verifies, but nothing outside the tree
settles which file is the stray: it names a different primary, or names this
primary but no registry entry points back here. The most likely cause of the
last shape is a workweave copied out of band with `cp -r` — the copy carries
both files, and the registry still names only the original.

**What to do:** decide which file is the stray and delete it yourself.
Deleting either one automatically would be a guess, and the wrong guess
destroys operator state: the marker carries `primary` and `parent` values that
exist nowhere else.

### `legacy-workweave-marker`

**Warning. Auto-fixable.** A `.rwv-workweave` marker this build cannot use as
written: YAML (markers are JSON now), possibly also missing the `parent:`
field required before the format changed.

**What to do:** `rwv doctor --fix` rewrites the marker as JSON, backfilling
`parent: <primary value>` where it is absent. Run it from the primary weave —
this finding is only ever reported from there, because from inside the
workweave the marker refuses before any verb dispatches.

### `legacy-workweave-index`

**Warning. Auto-fixable.** A `.rwv-workweave-index` written before ownership
receipts existed, with no `receipts` field — the index-side twin of
`legacy-workweave-marker`.

Adding the field is the precondition for every other arm of the migration:
recording a new receipt refuses against a legacy index rather than erasing the
only signal that the migration has not run. This one is reported rather than
refused at read, for two reasons — the migration has to be able to read the
index it is about to migrate, and an unmigrated index already fails closed on
its own, because it holds no receipts and so nothing in it is destroyable.

**What to do:** `rwv doctor --fix` adds the field.

### `workweave-tree-integrity`

Anomalies in the `.rwv-workweave` marker tree and the registry that records
it. The registry is advisory rather than authoritative, and this finding is
what keeps it honest.

#### `dangling-parent`

**Warning. Auto-fixable.** The marker's `parent:` path no longer exists — the
parent was retired or deleted out of band while this child remained. The
normal retire and delete paths adopt children before the parent is destroyed,
so this only arises off the happy path.

**What to do:** `rwv doctor --fix` re-points `parent` at primary, which always
exists. Branch names are left untouched.

#### `parent-chain-anomaly`

**Warning. Report-only.** A cycle, a parent equal to self, or a parent marker
whose project differs from this workweave's. Cannot arise from
`rwv workweave create`; can arise from hand-edited markers or directory
copies.

**What to do:** correct the marker by hand.

#### `unregistered-dir`

**Warning. Report-only.** A directory under `.workweaves/` with no
`.rwv-workweave` marker at all — an orphan of a failed create, a manually
placed directory, or the remnant of a deleted workweave.

**What to do:** inspect and remove it, or add a marker if it should be a
workweave.

#### `unregistered-workweave`

**Warning. Auto-fixable.** A marker-bearing directory whose `(project, name)`
is not recorded in that project's registry. The workweave exists on disk but
primary does not know about it.

**What to do:** `rwv doctor --fix` adopts the entry. Read paths (`list`,
`delete`) deliberately do not adopt on the fly — adoption is an operator
decision, so it happens where you asked for it.

#### `stale-registry-entry`

**Warning. Auto-fixable.** A registered workweave whose recorded path is not a
valid workweave: missing directory, missing marker, or marker validation
fails. This covers both "deleted out of band with the registry left behind"
and "an index committed to version control carries paths that are wrong on
this machine".

**What to do:** `rwv doctor --fix` prunes the entry.

#### `foreign-primary`

**Warning. Report-only.** The marker's `primary:` path neither matches the
workspace this scan started from nor resolves to any workspace — for example
a workweave copied to another machine whose marker still holds the origin
machine's absolute path.

**What to do:** correct `primary:` by hand, or remove the copy.

#### `foreign-primary-other-workspace`

**Warning. Report-only.** The marker's `primary:` resolves to a different but
perfectly valid workspace root — the normal shape when several weaves share
one workweave container. It is not a defect in this workweave, so it is the
one finding excluded from the text report; every sibling weave's doctor would
otherwise repeat this about every other sibling. It is still enumerated under
`--json`.

**What to do:** nothing.

#### `tracked-index`

**Warning. Report-only.** The `.rwv-workweave-index` is tracked by the project
repo. The index is machine-local state; a checked-in copy propagates absolute
paths to every clone and every workweave checkout.

**What to do:** `git rm --cached projects/<project>/.rwv-workweave-index` and
add it to `.gitignore`. `--fix` cannot untrack a file without touching commit
history, so this stays yours.

#### `unreadable-marker`

**Warning. Report-only.** A `.rwv-workweave` marker that parses as neither
current JSON nor a legacy shape `rwv doctor --fix` can migrate — most often
YAML with no `primary:` field for the migration to backfill from. Every
marker rwv has ever written carries all three required fields, so this is
hand-corruption or a truncated write, not a shape upgrading produces.

**What to do:** the finding message names what to write by hand. There is
nothing here to guess a repair from, so `--fix` leaves the file untouched.

### `workweave-drift`

**Warning. Report-only.** A worktree the manifest lists is missing from a
workweave (`missing`), or a worktree exists that the manifest does not list
(`extra`).

**What to do:** `rwv sync` materializes a missing worktree. An extra one is
either a repo to `rwv add` or a directory to remove.

### `stale-worktree-registration`

**Warning. Auto-fixable.** A git worktree registration pointing at a directory
that no longer exists.

**What to do:** `rwv doctor --fix` runs `git worktree prune`. The repair is
information-preserving by construction: the only state dropped is a pointer to
a directory that already is not there.

---

## Clone topology

### `clone-topology`

**Error. Report-only.** One of the manifest's repos is on disk in a shape that
breaks the bottom tier of the stability stack: the slot at
`<weave>/<repo_path>` must be a canonical store — a full clone — and every
workweave checkout `<workweave>/<repo_path>` must be a linked workspace whose
object store resolves to that canonical store.

All four sub-kinds are invisible to every higher-tier check, which operate on
revisions and content rather than on physical object-store topology — so this
finding is the only thing that reports them. All four are report-only: the
repair is an object-store migration, which is out of `--fix` scope.

The invariants and the reasoning are in
[Clone topology](../explanation/joints/clone-topology.md).

#### `standalone-in-workweave`

A full clone is hosted under `.workweaves/` instead of at the manifest's
canonical slot — the inverted-primary case, where the canonical store has
migrated into one workweave and other workweaves link into *it*.

A symlinked `reference` checkout is not this: it is the single canonical store
viewed through a symlink, which upholds the single-store invariant by
identity, and the scan excludes it. A real standalone store inside a workweave
is a real directory.

#### `disconnected-weave-clone`

The workspace at `<weave>/<repo_path>` is a full clone, but one or more of
this weave's workweave checkouts of the same repo resolve to a *different*
canonical store. The weave-path clone publishes a separate object graph
nobody syncs to; push and pull become asymmetric and silent.

#### `wrong-parent-worktree`

A linked worktree under `.workweaves/<workweave>/<repo_path>` whose canonical
store is not the weave canonical. Commits made there land in a different
object store than the canonical, and merged-checks across the two answer "no"
silently.

#### `weave-clone-is-worktree`

The weave path itself is a linked worktree of some other clone — full
inversion. There is no canonical store at the manifest slot at all.

### `missing-canonical-clone`

**Warning. Report-only.** A workweave worktree whose canonical clone — the
primary-weave clone it was linked from — is gone from disk. git commands in
the dependent worktree then fail opaquely; this finding names the true root
cause instead of letting the failure be misread as live local edits.

**What to do:** `rwv fetch` re-materializes the canonical in place, then
re-run `rwv doctor`. There is no auto-fix because doctor never clones —
network access stays behind explicit verbs.

### `uninitialized-submodule`

**Warning. Report-only.** A worktree has a `.gitmodules` file but one or more
of its listed submodule paths are empty. `git submodule update --init` has
never run there. Detection needs no network — the scan only stats the paths
`.gitmodules` lists.

**What to do:** run the git command the finding names.

### `provenance`

#### `origin-url-mismatch`

**Warning. Report-only.** The clone's `origin` URL differs from the
manifest's. Until it is reconciled, a push may publish to the wrong remote.

**What to do:** decide which is right and change the other. `--fix` has no arm
here because that decision is yours: a `reference`-role repo may point at a
different remote on purpose — a local mirror, say — and the message says so
when the role is `reference`.

#### `lock-sha-unreachable`

**Error. Report-only.** The SHA `rwv.lock` pins is absent from the clone's
object store.

**What to do:** `rwv fetch`. A sync will not recover it — the object has to
come from the remote.

---

## Branches and ownership receipts

`branch-discipline` enforces the invariant that every workweave repo checkout
sits on its own `<project>--<workweave>` ephemeral branch, every canonical
clone sits on a non-ephemeral branch, and stale ephemeral branches left over
from deleted workweaves are surfaced. It catches manual operations no other
scan can see — a `git switch main` inside a workweave, or a `branch -D` that
left an orphan behind in the canonical.

Every deletion `--fix` performs is gated twice: on an ownership receipt for
that exact ref in that exact store, and on a warrant proving the loss is safe.
Neither gate is inferable from a branch name.

### `branch-discipline` — a workweave checkout on the wrong branch

#### `shared-branch`

**Warning. Report-only.** The workweave checkout is on a non-ephemeral branch
such as `main` — from a `git switch` inside the workweave, or a clone that was
never moved onto an ephemeral branch.

A symlinked `reference` checkout legitimately shares the canonical store's
branch and has no per-workweave ephemeral branch by design; the scan skips
those. A `reference` repo created with `--worktree-references` is a real
worktree on its own ephemeral branch and is checked normally.

**What to do:** the message names the `git switch` that repairs it, and picks
between `git switch <name>` and `git switch -c <name>` based on whether the
ref already exists.

#### `foreign-ephemeral`

**Warning. Report-only.** The checkout is on an ephemeral ref rwv recorded for
a *different* workweave. Keyed on the receipt, not on the name: a hand-made
look-alike lands in `shared-branch` instead. Both are report-only, so the
distinction costs nothing but accuracy.

**What to do:** switch to this workweave's own ref.

#### `detached`

**Warning. Report-only by default.** HEAD points directly at a commit instead
of a named branch, which breaks the merged-check and ref-namespace
invariants.

**What to do:** `rwv doctor --fix --adopt-detached-checkouts` mints the
workweave's flat ref *at HEAD*. When a pre-flat branch of this workweave's
namespace also exists, both tips are reported side by side, because those two
commits are what you are choosing between — adopting at HEAD can strand the
other one.

#### `unmigrated-ephemeral-branch`

**Warning. Auto-fixable.** The checkout is on a pre-flat
`<project>--<workweave>/<segment>` ref of its own namespace — the shape rwv
minted before ephemeral names were flattened.

**What to do:** `rwv doctor --fix` records a receipt at the ref's current tip
and renames it to the flat name. A rename preserves the tip, so no commit
moves. Namespace membership is decided against the name this workweave
*mints*, never by taking the observed name apart.

#### `unrecorded-ephemeral-branch`

**Warning. Auto-fixable.** The workweave's flat ref exists in the canonical
store but rwv holds no receipt for it — what a build that minted flat names
before receipts existed leaves behind. Until it is adopted the ref is nobody's,
so `rwv workweave delete` will leave it behind.

**What to do:** `rwv doctor --fix` adopts it at its observed tip.

#### `unborn-checkout`

**Warning. Report-only.** The checkout is on a branch with no commits. This is
report-only not because a fix is missing but because there is no revision to
record a receipt against, so there is nothing the migration could own.

**What to do:** make an initial commit, then re-run `rwv doctor --fix`.

### `branch-discipline` — what the canonical store is attached to

#### `canonical-holds-live-workweave-ref`

**Warning. Report-only.** The canonical store is attached to a ref rwv
recorded for a workweave that is still on disk. git forbids one branch being
checked out in two worktrees of the same store, so reaching this state means a
directory was moved or copied.

**What to do:** decide which of the two checkouts is the real one and switch
the other off the ref. Nothing here can tell them apart, which is exactly why
`--fix` has no arm.

#### `canonical-holds-leaked-ref`

**Warning. Report-only.** The canonical store is attached to a ref recorded
for a workweave that is gone — a leak. The reclaim cannot run while the
store's own HEAD is on it, because git refuses to delete a branch a worktree
uses, so there is no arm here even though the ref is one rwv owns.

**What to do:** `git switch <tracking-branch>`, then re-run
`rwv doctor --fix`. Once the store is off the ref it becomes an ordinary
stale-branch finding and `--fix` reclaims it under a warrant.

#### `canonical-detached`

**Warning. Report-only by default.** The canonical store — or the project
repo, which is an instance of the same branch model rather than an exception
to it — is in detached-HEAD state.

**What to do:** `rwv doctor --fix --reattach-checkouts` reattaches, but only
when the tracking declaration's local counterpart exists *and* its tip equals
HEAD. That condition is deliberately false for the ordinary post-fetch state
(a stale counterpart with HEAD at the lock SHA), so the flag repairs the
minority it can prove safe rather than reattaching a whole weave. Without it,
the finding names the `git switch` that would repair it and nothing moves.

### `branch-discipline` — stale ephemeral branches

A `<project>--<workweave>` branch in a canonical clone whose workweave is no
longer on disk. Three classes, and only one is removable.

#### `stale-ephemeral-branch-safe`

**Warning. Auto-fixable.** rwv holds a receipt for the ref and its tip is
reachable from the store's tip, so it carries no unique commits.

**What to do:** `rwv doctor --fix` deletes it under a merged warrant, with no
information loss.

#### `stale-ephemeral-branch-live`

**Warning. Report-only.** rwv holds a receipt, but the tip carries commits not
reachable from the primary tracking branch — unique work. No merged warrant
can be established, so `--fix` never touches it. The tip SHA is reported so
you can inspect the commits before deciding.

**What to do:** land the commits, archive the branch, or delete it by hand.

#### `stale-ephemeral-branch-unowned`

**Warning. Report-only.** A branch shaped like one rwv minted before names
were flattened, which no workweave on disk claims and which rwv holds no
receipt for. It is not rwv's: name shape is not ownership. Deleting this class
is precisely how a hand-made branch could disappear under `--fix`, so `--fix`
never touches it — and this is the one entry where that is permanent rather
than pending an arm nobody has written.

This is the one class discovered by name shape rather than by asking the
registry — every other arm has a receipt or a live workweave's minted name to
ask, and this one has neither. The alternative to a shape heuristic here is
not a better signal; it is silence, and the refs you most need to see are
exactly the ones the migration cannot reach. What keeps it sound is that the
heuristic yields a yes-or-no and nothing else: no name is taken apart, no
workweave is named, and the only route to a deletion runs through a receipt.
A false positive costs one line of output and can cost nothing more.

**What to do:** remove it by hand if it is yours to remove.

### `dangling-ref-receipt`

**Warning. Auto-fixable.** An ownership receipt naming a ref that is not in
the store it names — the benign residue of a crash between the receipt write
and the ref creation.

Receipts are written *before* the refs they describe, precisely so a crash
leaves this state rather than an unreceipted ref, which nothing could ever
destroy. A dangling receipt authorizes nothing: no deletion warrant can be
built against a ref that is not there.

**What to do:** `rwv doctor --fix` retracts it. Only raised when the store is
present and readable; a receipt whose *store* is gone is left alone here.

### `pre-flat-ref-receipt`

**Warning. Auto-fixable.** An ownership receipt whose ref name carries a `/`
segment — a name no live workweave of that project mints, so a record claiming
a ref rwv cannot have created. It is written on purpose, mid-flight, by the
pre-flat migration (adopt, then rename, then retract) and survives whenever
that rename does not complete. Unlike `dangling-ref-receipt`, the ref itself
usually *does* exist; the residue is in the registry, not in the store.

Left in place it is worse than no receipt at all. The canonical-store scan
asks which live workweave mints the recorded name, a segmented name is minted
by none, so the ref reads as a leak — and holding a receipt is exactly what
lifts a ref out of the untouchable unowned class into the ones `--fix` deletes
from. Where the ref is also checked out, every `--fix` re-attempts a deletion
git refuses, and doctor never converges.

**What to do:** `rwv doctor --fix` retracts the receipt. Retraction disowns;
it does not touch the ref. Afterwards the branch is unowned, which `--fix`
never deletes.

---

## Working state and in-flight operations

### `index-drift`

A repo's index does not match its HEAD tree — the silent stale-index left by a
shared-ref advance in a sibling worktree.

#### `safe-to-fix`

**Warning. Auto-fixable.** The index tree matches the tree of a recent
ancestor commit, so the displaced tree is permanently in the commit graph.

**What to do:** `rwv doctor --fix` resets the index.

#### `live-staged`

**Warning. Report-only.** The index tree is in no recent ancestor. You have
live staged content, and `--fix` must not touch it.

**What to do:** commit or stash it, then re-run.

### `working-tree-drift`

A repo's working-tree files do not match its HEAD tree, after a shared-ref
advance in a sibling worktree.

#### `safe-to-fix`

**Warning. Auto-fixable.** Every modified file's on-disk content matches a
blob reachable from HEAD, so nothing is lost by restoring.

**What to do:** `rwv doctor --fix` restores the files.

#### `live-edits`

**Warning. Report-only.** At least one modified file's content is in no recent
ancestor's tree. Those are your edits.

**What to do:** commit or stash them, then re-run.

### `orphaned-savepoint`

A `refs/rwv/pre-op/<op-id>` savepoint whose op id matches no `.rwv-op` file in
this workspace tree.

#### `redundant`

**Warning. Auto-fixable.** The savepoint tip is reachable from the current
branch tip, so the commits are still anchored by a live branch.

**What to do:** `rwv doctor --fix` drops the ref. No objects are lost.

#### `live`

**Warning. Report-only.** The savepoint tip is *not* reachable. The ref is the
last pointer to commits that would otherwise become unreachable — the same
reason the reflog is never cut.

**What to do:** recover what you want from it, then delete it by hand.

### `stale-op-state`

**Warning. Report-only.** A `.rwv-op` file is present at a workspace root.
The finding reports the file's age, its path, and the `verb` that started the
op (`sync` or `sync-to`).

`--fix` has no arm here and will not grow one: another terminal may be
mid-conflict-resolution, and rwv has no daemon that could know which workspace
the op state legitimately belongs to.

**What to do:** inspect it, then either resume with the `--continue` the
finding names — an op is only resumable under the verb that started it — or
`rwv abort` to roll back.

### `dead-op-lease`

**Warning. Auto-fixable.** A `.rwv-op-lease` whose recorded owner has no
matching `.rwv-op`. Unlike `stale-op-state`, this one *is* auto-fixable,
because the classification is structural rather than temporal: the lease
pointer resolves to no paired owner record. No wall clock, no timeout, no
liveness guess. Dropping a lease whose owner is provably absent cannot clobber
an in-flight operation, because there is no such operation.

The lease's age is reported as context — never as a decision input. The two
sub-kinds share that disposition and differ only in what they name:
`owner-record-absent` (the owner workspace has no `.rwv-op` at all — the
classical crash between acquire and mark) and `owner-op-id-mismatch` (the
owner has an `.rwv-op`, but for a different operation, so this lease points at
one that has already finished).

**What to do:** `rwv doctor --fix` removes the lease.

---

## Cargo observatory

Both findings are warnings and both are report-only. rwv observes across
sovereign repos; it does not mandate to them.

### `cargo-version-skew`

**Warning. Report-only.** The same crate is required at different version
strings by two or more cargo workspace members, after `workspace = true`
indirection is resolved.

**What to do:** reconcile the requirements if you want them reconciled. rwv
reports the skew and stops there.

### `cargo-patch-shadowing`

**Warning. Report-only.** A member's `.cargo/config.toml` declares a
`[patch.<registry>].<crate>` key that silently defeats a weave-level entry for
the same key — cargo resolves patches closest-config-wins, per key.

This doubles as the mandatory precheck for derived-patch generation: when a
patch silently does not apply, cargo's own mismatch diagnostic actively
misleads by blaming the registry, so surfacing the shadow at scan time is what
preserves your ability to diagnose the real cause.

**What to do:** remove the member-level key, or accept that the weave-level
one is inert.

---

## The integration channel

Everything above is a finding one of rwv's own scans made, and lands on
`--json`'s `violations` array. A finding an *integration* raised lands on the
`issues` array instead, under its own tag set. The two arrays are disjoint: an
`issues` entry never carries `kind: "core-finding"`.

An `issues` entry carries `integration` (which integration raised it),
`severity`, the operator-facing `message`, and `safe_to_fix` — `false` marking
a file region you hold the pen on, which `--fix` reports and never overwrites.

| `kind` | What it is |
| --- | --- |
| `tool-missing` | The ecosystem CLI the integration drives is not on `PATH`. |
| `managed-file-missing` | A file the integration owns is not in the project directory. |
| `managed-file-drift` | Owned content on disk differs from what `rwv activate` would write, or the owning tool can no longer read it. |
| `managed-file-user-held` | The owned key or region is present without rwv's ownership marker. You hold the pen; `--fix` will not touch it. |
| `surfacing` | A weave-root symlink onto an owned file is absent, occupied by real content, or resolves into a different project. |
| `config-rejected` | `rwv.toml` asks for something the workspace cannot satisfy — a name two sections claim, a declared file that is not there, a member topology the ecosystem tool rejects. |
| `member-incompatibility` | A value you hold is incompatible with what the members require. Carries the observation as fields, below. |
| `derived-state-stale` | Generated ecosystem state no longer follows from the inputs it was derived from. Reported only — see below. |
| `disabled-integration-artifact` | An integration is disabled for this project, but content it authored is still on disk. Reported only — see below. |
| `integration-failed` | An integration's hook returned an error; the runner captured it so the remaining integrations could still run. |
| `core-finding` | Raised by doctor itself while driving the integrations. On the wire this appears only under `--fix`, which `--json` has no form of — see the disjointness rule above. |

`derived-state-stale` is the standing form of the note `rwv sync` prints once.
rwv records the digests of the inputs each generation read — the project
manifest and `rwv.lock` — beside the digest of what it produced, so whether a
lock file still follows from this checkout is answerable at any later moment
from present state alone: nothing is regenerated to compare against, no other
workspace is consulted, and no history is kept. A workweave answers for itself.

An entry written before inputs were attested reads as stale. That is the honest
answer rather than a lenient one — rwv accepted those bytes without recording
what produced them, so it cannot claim they still follow from anything — and it
heals itself, because the next generation rewrites the entry in the attested
shape. Expect to see it once per project after upgrading.

On `--json` this finding also appears in the `advisories` array, in the same
`{kind, remedy, inputs}` shape `rwv sync --json` emits, so an agent branches on
one vocabulary rather than two. `inputs` names the paths that moved, and is
empty exactly when the entry records no inputs to compare.

**What to do:** run `rwv materialize`. If the generated file also holds content
rwv never accepted, materialize refuses first and names the two consents — see
`rwv explain materialize`; taking either one clears both conditions, because the
generation that follows attests its own inputs.

`disabled-integration-artifact` is the one kind `--fix` has no arm for at all,
and the omission is the design rather than a gap. Disabling an integration
withdraws the justification for what it authored but not the content: the marked
region in your hybrid file, the lock file it generated, and the weave-root
symlinks that surfaced both are all still there. The state disablement implies
is their absence, and reaching it means deleting — so the finding names
`rwv materialize`, which removes each artifact by its own cleanup shape (a
marked region is stripped out of the file and your content stays; a file rwv
wrote whole is removed; the surfacing symlinks go with them). `--fix` stays out
because disabling an integration is a one-character edit to `rwv.toml`, and a
typo must not put deletion one flag away.

Nothing you authored is ever named here. An artifact is attributed to an
integration by rwv's own ownership evidence on disk — the `managed by rwv`
marker for a region, and for a lock file the marker on the workspace file that
made rwv its author — so a hand-authored `Cargo.toml`, and the lock beside it,
are yours and stay yours. `static-files` never appears: it surfaces files you
committed and authors none.

**What to do:** re-enable the integration if the disablement was a mistake, or
run `rwv materialize` to remove what it left. Doing nothing is also stable —
the finding is a warning and does not change doctor's exit status.

`member-incompatibility` is the one kind that carries fields rather than only
a tag, because the four facts its predicate established are what the remedy
turns on: `path` (the managed file holding the value), `key`, `on_disk`,
`required`, and `required_by` (the member file carrying the requirement).
Doctor is the standing observation arm for it, and `rwv update` reports the
same finding at the moment it creates one. Neither gates: nothing refuses on
it, and `--fix` cannot repair it — rwv seeded the key once and never
overwrites it, so this is not drift.
