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

Every entry for a finding on the `violations` array opens with one of those
three marks, and the marks are the published statement of what `--fix` does.
`rwv doctor --help` and `rwv explain doctor` point here rather than restating
the set, and a test compares each mark against the code that repairs it — so an
arm that is added, removed or changes disposition without this page moving with
it fails the build.

Entries under [The integration channel](#the-integration-channel) carry no
mark, and that is a decision rather than an omission: on that channel `--fix`
authority is decided per finding and travels with it as `safe_to_fix`, so a
mark keyed to the kind would be false for one of the findings the kind covers.
The section says so where it starts.

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
set is implementation detail and is not part of this published book.

---

## Portability of recorded names

### `confusable-siblings`

**Warning. Report-only.** Two recorded sibling identities differ only by ASCII
case: two projects under one parent directory, or two repo-path segments under
one parent within a project.

rwv holds the two as distinct identities and keeps resolving them byte-exactly
— this finding changes nothing about that, and stores no folded key. What it
reports is a portability hazard: a filesystem that folds case cannot hold both,
so a clone or fetch of this weave onto macOS or Windows collides. That is why
it fires on case-sensitive hosts too, where the pair is perfectly legal and
nothing else would notice until someone else's machine failed. `rwv init` and
`rwv fetch` print the same warning at mint, so a pair is usually reported when
it is created rather than found later.

**What to do:** rename one of the two if this weave is meant to travel.
Never auto-fixed — which of two recorded identities should change is the
operator's call, and on the host that raised the warning nothing is broken yet.

**Residue:** the fold is ASCII, so it does not see non-ASCII confusables
(`ß`/`SS`, precomposed against decomposed). Those are the same class one size
down, and only a filesystem that actually folds them will report them.

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

**Also a sync failure kind.** One condition, one name, so `rwv sync --json` and
`rwv sync-to --json` report a repo whose HEAD they could not read under this
same kind, as a per-repo `failure.kind` rather than a `rwv doctor` violation.
The text lands in `message` there instead of `error`, and sync raises it for
one case a scan cannot see: the source lock pinning a version the source
workspace itself could not resolve, which leaves sync with no HEAD to compare
against for a reason that is nothing to do with the repo in front of it.

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

### `projectless-dir`

**Warning. Report-only.** A directory under `projects/` with no `rwv.toml`
anywhere below it. A project is a directory under `projects/` that holds an
`rwv.toml`, named by its path relative to `projects/`, so this directory is
not one and contains none — nothing enumerates it, and no verb can act on it.

The finding exists because the silence is the problem. A directory made by
hand ahead of its manifest, or left behind by a clone that failed before one
was written, sits in the tree looking like a project and is absent from every
listing rwv prints. A directory that only *holds* projects — `projects/acme/`
above `projects/acme/web-app/` — is not reported: it has a manifest below it,
which is what makes it a namespace rather than a stray.

**What to do:** write an `rwv.toml` in it — a bare `[repositories]` table is
enough — or remove the directory. `rwv init` is not the repair: it mints the
directory, and refuses one that is already there.

### `unnameable-project`

**Warning. Report-only.** A directory under `projects/` that holds an
`rwv.toml`, whose path relative to `projects/` is not a name rwv accepts. The
`derived` field carries that path and `error` says which rule refused it —
`--` or a leading/trailing `-` (ambiguous against the `--` that joins project
to workweave in a branch or workweave directory name), a `+` (what rwv writes
in place of `/` when a project name has to be one path segment), or a git
ref-name rule.

The project is on disk and no verb can address it: every one of them takes the
name through the validator first, so `rwv activate`, `rwv workweave` and
`--project` all refuse it. Reported whatever `--project` narrows the run to,
since a name the validator refuses can never equal the scope.

The character policy behind the refusal — and the alternative that was
measured and left closed on purpose — is documented in
[formats.md — Names, and the characters they exclude](formats.md#names-and-the-characters-they-exclude).

**What to do:** rename the directory to a name that validates. Nothing inside
it needs to change — the name is the weave's, not the project repo's.

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
`legacy-alongside-current` — both lines are present, which is not the
current one plus harmless residue: two `merge=` assignments on one path
resolve by reading order, and the legacy name stays bound to whatever
`merge.ours.driver` the operator's global config defines.

**What to do:** `rwv doctor --fix` appends the line — or migrates the legacy
spelling in place, dropping it where the current line is already there — and
commits it when the repo has no other staged changes. The committed form is
what sync's invariant reads, so the commit is the part that makes it take
effect.

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

### `unreadable-owned-state`

**Warning. Report-only.** A `.rwv-owned-digests` that exists and does not
parse. It is rwv's record of the generations it accepted for the project, and
two checks decide from it: `managed-file-drift`, which compares a generated
file against the content rwv last accepted, and `derived-state-stale`, which
compares a generation's recorded inputs against the checkout.

Both read an unreadable record as "nothing is attested" and then report
nothing, so without this finding the project is indistinguishable from a clean
one — including for files that had already drifted before the record became
unreadable. This finding is what says the two checks did not run, rather than
that they ran and found nothing.

An **absent** record is not this finding and is not a problem: a weave that has
never run a generator has nothing to attest, and a file it has never stamped
has no entry. Both stay silent, which is what makes a fresh or upgraded weave
quiet rather than noisy.

**What to do:** run `rwv materialize` to re-derive the project's generated
files and record them afresh. `rwv materialize` takes no project argument — it
acts on whichever project the checkout presents — so this only reaches the
finding for the active project. For any other project, run `rwv activate
<project>` first; the repair runs there once it is the active one.

Where the project generates none — no cargo workspace, and no other
integration that owns a whole file — there is nothing to re-derive, and the
run leaves an empty record instead, saying on its way past which file it did
that to. That is a repair and not a shrug: every check reading an unreadable
record already answers "nothing is attested", so an empty one states what they
were all computing, and states it somewhere this finding can see the fault has
gone.

`--fix` does not do it for you: rebuilding the record attests whatever is on
disk at that moment as accepted, and what was accepted before is exactly what
has been lost, so nothing can check the two against each other first. If the
current content matters, inspect it before you re-derive.

### `unreadable-workweave-index`

**Error. Report-only.** A `.rwv-workweave-index` that exists and does not
parse. Everything derived from it is unevaluated: the recorded placements, the
ownership receipts, and whether the file needs the `legacy-workweave-index`
migration.

Reported at error severity rather than warning because the alternative is
worse than silence. Without this finding every marker-bearing workweave in the
project surfaces as `unregistered-workweave` — "run `rwv doctor --fix` to
adopt it" — and that repair reads the same file and dies on the same parse
error. While the index does not parse, `unregistered-workweave` and
`stale-registry-entry` are not reported for the project at all; this finding
replaces them, because neither is a fact about a registry rwv cannot read.

**What to do:** repair the file by hand, or delete it and re-run `rwv doctor
--fix`, which re-adopts the workweaves on disk into a fresh index. `--fix`
never rewrites it for you: a corrupt index gives no way to tell which entries
it meant to hold, and the ownership receipts in it are the only record of
which refs are rwv's to delete.

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

**Scope.** The scan enumerates the containers each project records, so this
finding covers a directory sitting in one of them. A workweave placed outside
them all — `rwv workweave <project> create <name> --dir <path>` — is not among
the candidates: an unrecorded one there produces no finding and `--fix` has
nothing to adopt. Retire the directory and create the workweave again instead,
which does not depend on where it sits.

With no registry entry the name comes from the basename, read against the
marker's own project — so a directory in a container whose basename that
project does not render carries no name to adopt, and reports
[`misnamed-dir`](#misnamed-dir) instead of this.

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

#### `misnamed-dir`

**Warning. Report-only.** A marker-bearing workweave directory whose basename
disagrees with its records: it does not spell `{marker project}--{name}`,
where the name is the one the project's registry records for this path. Only a
hand-rename produces this — `rwv workweave create` derives the directory name
from the same pair it writes into the marker and the registry.

Identity is by record, so the scans keep working from the records — a renamed
directory keeps its recorded branch expectation and its project scope. What
this finding reports is that the directory's name now lies about those
records, which misleads operators and collides with any future workweave
whose records genuinely mint this basename. Where no registry entry names the
path there is no recorded name to disagree with, and the question narrows to
whether the basename is one the marker's project could have rendered at all:
when it is not, identity is unrecoverable, the scans skip the directory
entirely, and this finding is the only signal.

**What to do:** rename the directory to the name the finding reports (the
checkouts inside were registered under the recorded name, so restoring it
also restores their worktree back-pointers). When no record pins the intended
name, rename it to the `<project>--<name>` you intend, or remove the
directory. Never auto-fixed: a directory rename under live checkouts is not
rwv's to perform, and in the unrecoverable case the target is not derivable.

#### `nested-workweave-dir`

**Warning. Report-only.** A recorded workweave whose directory sits *below* a
workweave container instead of directly in it. A multi-segment project name
used to render its `/` straight through into the directory name, so
`chatly/web-app` plus `wtest` placed the workweave at
`<container>/chatly/web-app--wtest` and left `chatly` behind as a directory the
container scan reads as a stray. rwv now writes that `/` as `+`, so a workweave
created today is `<container>/chatly+web-app--wtest` and this finding names the
single-segment directory the records spell.

The container scan reads a container's immediate children, so nothing else in
`workweave-tree-integrity` sees such a directory at all. This finding is
emitted from the registry, which records the path.

**What to do:** retire this workweave and create it again. Never auto-fixed,
and the repair is not a rename: the move crosses a directory boundary, which
invalidates the worktree back-pointers of every checkout inside it and the
recorded path that found it. Workweaves are ephemeral by design — rwv does not
migrate a live one in place.

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

#### `placement-disagreement`

**Warning. Report-only.** The manifest key disagrees with `placement(url)` —
the path the entry's own URL derives to under the canonical
`{registry}/{owner}/{repo}` layout. `--fix` has no arm here for the same
reason `origin-url-mismatch` has none: the repair is either moving the
checkout or re-keying the manifest entry, and which one is right is the
operator's call. This is a different comparison from `origin-url-mismatch` —
that one compares the manifest URL against the clone's `origin` URL; this one
compares the manifest *path* against `placement` of the manifest URL — so an
entry can trip either, both, or neither independently.

`reference`-role entries are exempt: the arrangement `origin-url-mismatch`
already documents — a mirror URL under a path named for what it mirrors —
would otherwise report both findings for the same intentional divergence.
`fork`-role entries are exempt too, per [Roles](./roles.md) §`fork`: the
manifest key may name either the fork or the upstream it forked, so a
disagreement between the two is not a defect.

**What to do:** the message names both paths — move the checkout (and the
manifest key) to the derived path to converge, or re-key the manifest entry
at the derived path if that is where it belongs. If the divergence is
deliberate — a fork keyed on its upstream's coordinates, a mirror under the
path it mirrors — the entry's existing `role` is what the scan already reads
to exempt it; nothing further to do.

---

## Branches and ownership receipts

### `branch-discipline`

`branch-discipline` enforces the invariant that every workweave repo checkout
sits on its own `<project>--<workweave>` ephemeral branch, every canonical
clone sits on a non-ephemeral branch, and stale ephemeral branches left over
from deleted workweaves are surfaced. It catches manual operations no other
scan can see — a `git switch main` inside a workweave, or a `branch -D` that
left an orphan behind in the canonical.

Every deletion `--fix` performs is gated twice: on an ownership receipt for
that exact ref in that exact store, and on a warrant proving the loss is safe.
Neither gate is inferable from a branch name.

Its findings are grouped below by what each one is about — the workweave
checkout, the canonical store, or a leftover ephemeral branch — because the
disposition differs across them and no statement about the kind as a whole
would be true. Every finding names a `sub_kind`, which `rwv doctor --json`
prints and which `rwv explain <sub-kind>` serves directly; that is the entry
to read, and this one is only the route to it.

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

Report-only is deliberate, not a missing arm. The state is operator-made — a
`git switch` run by hand — unlike the fetch-written detachments whose repairs
are native consented arms, and the printed command is exact and
registry-aware (it names an existing recorded branch whenever a receipt
exists), which is what keeps hand-running it safe. If measured recurrence in
owned weaves reopens the question, the native form is a targeted sub-verb,
not a bulk consent flag.

#### `foreign-ephemeral`

**Warning. Report-only.** The checkout is on an ephemeral ref rwv recorded for
a *different* workweave. Keyed on the receipt, not on the name: a hand-made
look-alike lands in `shared-branch` instead. Both are report-only, so the
distinction costs nothing but accuracy.

**What to do:** switch to this workweave's own ref. Report-only is deliberate
for the same reason as `shared-branch`'s: the state is operator-made and the
printed switch is exact.

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

#### `blocked-ephemeral-namespace`

**Warning. Report-only.** Two or more refs share this workweave's namespace in
one store. git holds `refs/heads/p--w` and `refs/heads/p--w/x` as a file and a
directory of the same name, so the flat ref cannot be created and no migration
arm can run.

Reported *in place of* `unmigrated-ephemeral-branch`, not beside it: the
migration pass skips the pair before any arm runs, so the rename that finding
promises cannot happen. The skip is deliberate — every arm records its
ownership receipt before it writes the ref, and a receipt for a pre-flat name
resolves to no workweave on disk, which reads as stale and deletable.

**What to do:** decide which ref is this workweave's branch and move or delete
the others, leaving at most one ref under the namespace; then re-run `rwv
doctor --fix` to migrate it. Never auto-fixed: which ref is which is not
derivable from the refs.

#### `blocked-detached-namespace`

**Warning. Report-only.** The workweave checkout is in detached-HEAD state AND
two or more refs share its namespace in one store. The same guard that skips
`fix_branch_model_migration` when the namespace is blocked also prevents the
`--adopt-detached-checkouts` arm from running — the guard fires before any arm
is reached.

Reported *in place of* `detached`, not beside it: the `detached` finding
promises `--adopt-detached-checkouts`, and that flag's arm cannot run while the
namespace is blocked. The principle is consent-tier-independent: a consented
remedy that cannot run misleads the operator exactly as an auto remedy does —
consent changes who acts, not whether the named action works.

**What to do:** decide which ref is this workweave's branch and move or delete
the others, leaving at most one ref under the namespace; then re-run `rwv
doctor` to get the ordinary `detached` finding with a remedy that will actually
run. Never auto-fixed: which ref is which is not derivable from the refs.

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

"No longer on disk" is asked of three sources: the container walk, the
workweave index, and git's own worktree table. The third is what keeps a
workweave placed outside every container (`rwv workweave <project> create
<name> --dir <path>`) whose index entry has been lost out of all three
classes — a branch a live checkout is sitting on is never reported here.

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

That field is why the entries below open with prose instead of one of the three
marks. On this channel what `--fix` may touch is decided per finding and
travels with it, where a mark is a statement about a whole kind — and one kind
here covers both dispositions at once. `surfacing` reports a link a fresh pass
repairs outright and a link standing over content you wrote; no single mark is
true of both. So read `safe_to_fix` on the finding you have, and take the
entry's **What to do** as the account of when each answer comes up.

### `tool-missing`

The ecosystem CLI the integration drives is not on `PATH`. The integration
names which tool in its message, and reports nothing further about the files
that tool owns, because it has nothing to ask.

**What to do:** install the named tool, or turn the integration off for this
project — `enabled = false` under its `[integrations.<name>]` block in
`rwv.toml` — if this weave does not use that ecosystem. Turning one off leaves
whatever it already authored on disk; see `disabled-integration-artifact`.

### `managed-file-missing`

A file the integration owns is not in the project directory. rwv authors that
file, so its absence is a state a fresh generation reaches rather than a
decision you made.

**What to do:** `rwv doctor --fix`, which re-runs the generation that writes
it. `rwv activate` and `rwv materialize` run the same step.

### `managed-file-drift`

Owned content on disk differs from what `rwv activate` would write, or the
owning tool can no longer read it.

Two shapes with two answers, and `safe_to_fix` is what tells them apart. Drift
in a file rwv regenerates from present inputs is repaired by regenerating it.
Drift in a *generated* file — one whose accepted content rwv recorded a digest
for — is not, because the content on disk may be something you meant: rwv
cannot tell an edit you made from an edit you want discarded, and the finding
carries `safe_to_fix: false` rather than guess.

**What to do:** `rwv doctor --fix` for the first. For the second the message
names the three exits and they are all `rwv materialize`'s:
`--adopt-drifted` records the current content as the accepted generation,
`--regenerate-drifted` discards it and regenerates from present inputs, or
restore the file to the recorded content yourself.

### `managed-file-user-held`

The owned key or region is present without rwv's ownership marker. You hold
the pen on it.

Always `safe_to_fix: false`, and this is the one kind where that is a property
of the kind rather than of the finding: taking over an unmarked region means
overwriting content rwv never wrote and has no record of. `--fix` reports it
and moves on.

**What to do:** nothing, if the content is what you want — the state is stable
and rwv keeps reporting rather than acting. To hand the region over, do what
the message says: cut it over by hand, or write the ownership marker it names.
The spelling differs by file format, which is why the finding names it rather
than this page.

### `surfacing`

A weave-root symlink out of step with what `rwv activate`'s
surfacing step would put there — the finding and `--fix` read the same
predicate, so they agree on what "in step" means. Most of what it reports
`--fix` (or `rwv activate` / `rwv materialize`, which run the same step)
resolves outright, because the divergence is exactly what a fresh pass already
corrects.

Real content occupying a link's path stays report-only, and so does a link
surfaced at a name the active project no longer declares — for the same
reason in each case: on disk, either is indistinguishable from something you
made yourself at that path, so `--fix` will not guess and overwrite it.

**What to do:** `rwv doctor --fix` resolves anything it reports as fixable.
For a link at a name the project no longer declares, run `rwv materialize
--remove-undeclared-links`, which unlinks exactly the names the finding
named — the files they pointed at are untouched either way.

### `config-rejected`

`rwv.toml` asks for something the workspace cannot satisfy — a name two
sections claim, a declared file that is not there, a member topology the
ecosystem tool rejects.

The config parsed and rwv understood the request; what it names is not
available. That is the boundary this kind draws: a value rwv could not read at
all never reached a predicate, so nothing was asked and nothing here applies.

**What to do:** edit `rwv.toml` so it asks for something that exists — the
message names which of the two sides to move. Never auto-fixed: rwv cannot
tell whether the declaration or the workspace is the part you meant.

### `malformed-settings`

An `[integrations.<name>]` block in `rwv.toml` does not read as the settings
that integration declares — a value of the wrong type, most often. The finding
carries the deserializer's own account of it, which names the field and the
type it expected.

Separate from `config-rejected`, and the distinction is the one worth knowing:
there, rwv understood the request and the workspace could not meet it; here no
value was recovered at all, so nothing was asked and no predicate ran. The two
have different remedies — one moves the workspace, the other fixes a typo —
which is why they are two tokens.

Always `safe_to_fix: false`. The repair is an edit to a file you hold the pen
on, and the only repair `--fix` has for this project regenerates from the very
settings that did not read — so it is withheld rather than attempted and
reported as failed.

Everything that integration would otherwise have reported for this project is
missing from the run alongside it. A hook that cannot read its own
configuration has nothing to say, and this finding is rwv saying so rather
than reporting a healthy project.

**What to do:** correct the field the message names, or delete it to take the
default. Then re-run `rwv doctor` — the findings that were suppressed while
the block was unreadable will appear.

### `member-incompatibility`

The one kind that carries fields rather than only
a tag, because the four facts its predicate established are what the remedy
turns on: `path` (the managed file holding the value), `key`, `on_disk`,
`required`, and `required_by` (the member file carrying the requirement).
Doctor is the standing observation arm for it, and `rwv update` reports the
same finding at the moment it creates one. Neither gates: nothing refuses on
it, and `--fix` cannot repair it — rwv seeded the key once and never
overwrites it, so this is not drift.

**What to do:** raise the value at `key` in `path` to what `required` names,
or change the member at `required_by` so it stops requiring it. Which of the
two is right is a policy question about your weave, which is why rwv reports
the observation and stops.

### `derived-state-stale`

The standing form of the note `rwv sync` prints once.
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

### `disabled-integration-artifact`

An integration is disabled for this project, but content it authored is still
on disk.

This is the one kind `--fix` has no arm for at all,
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

### `integration-failed`

An integration's hook returned an error; the runner captured it so the
remaining integrations could still run.

The kind of last resort. A hook that returns `Err` carries no kind of its own,
so this is the only tag the runner can put on what it caught, and the message
is the whole of what the operator gets. A condition that has a remedy worth
naming is reported by *returning* it as a finding under its own kind instead —
so seeing this one means either an environment failure rwv has nothing to say
about, or a condition that has not been given its kind yet.

**What to do:** read the message; the hook that failed is named in
`integration`. There is no generic repair, and `--fix` re-runs the same hook.

### `core-finding`

Raised by doctor itself while driving the integrations. On the wire this
appears only under `--fix`, which `--json` has no form of — see the
disjointness rule above.

Two shapes reach it. Under `--fix`, a repair rwv attempted and could not
complete. In the default text output, the roll-up lines that stand in for a
class of core findings whose per-item detail is behind `rwv doctor --json`.

**What to do:** whatever the finding it stands for calls for — the entry is
above, on the `violations` half of this page. For a roll-up, take the per-item
detail from `rwv doctor --json` first.
