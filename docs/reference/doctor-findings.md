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

Each line of the default text output opens with the finding's severity and
its tag, in the spelling `rwv explain` takes: `[error: dangling-reference]`, or
`[warning: working-tree-drift/safe-to-fix]` for a finding with a sub-kind.
`--json` carries the same tag as `kind` and `sub_kind`; this prints it in that
spelling:

```bash
rwv doctor --json | jq -r '.violations[] | .kind + (
    if .sub_kind == null then ""
    elif (.sub_kind | type) == "string" then "/" + .sub_kind
    else "/" + (.sub_kind | keys[0]) end)'
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

Seventeen kinds carry a `sub_kind` — `alignment`, `clone-topology`,
`dead-op-lease`, `record-drift`, `member-permissions`,
`missing-lock-merge-driver`, `orphaned-savepoint`, `pin`, `provenance`,
`stale-worktree-registration`, `unparseable-project`, `uninitialized-submodule`,
`weave-root-identity-conflict`,
`working-tree-drift`, `workweave-drift`, `workweave-tree-integrity`. Where the
sub-kind decides the disposition the entry has a sub-heading per `sub_kind`;
where it only narrows what was found, one entry covers them all.

`rwv explain <kind>` serves a kind's entry. A kind that a refused verb also
routes to has, ahead of its sub-kinds, the states the refusal prints and the
choices each state offers. A finding with a sub-kind is
looked up as `rwv explain <kind>/<sub-kind>`, for example
`rwv explain working-tree-drift/safe-to-fix`: that serves the sub-kind's own
entry, or the kind's entry where one covers them all. A sub-kind only one kind
carries resolves on its own as well. One that several kinds carry, as
`safe-to-fix` is, refuses with `ambiguous-explain-token` and lists the
spelling for each kind.

The full wire shape is in [Formats](./formats.md), and the committed JSON
Schema is `docs/reference/schemas/doctor.json`.

## Summary index

| `kind` (and `sub_kind` where applicable) | Severity | `--fix` disposition |
|---|---|---|
| [`confusable-siblings`](#confusable-siblings) | Warning | Report-only |
| [`orphaned-clone`](#orphaned-clone) | Error | Report-only |
| [`unknown-segment-clone`](#unknown-segment-clone) | Error | Report-only |
| [`dangling-reference`](#dangling-reference) | Error | Report-only |
| [`unparseable-project`](#unparseable-project) / `pre-schema` | Error | Auto-fixable |
| [`unparseable-project`](#unparseable-project) / `malformed` | Error | Report-only |
| [`legacy-manifest-format`](#legacy-manifest-format) | Error | Report-only |
| [`pin`](#pin) / `none`, `unreachable`, `behind`, `unpinned`, `diverged` | Error | Report-only |
| [`pin`](#pin) / `ahead` | Warning | Report-only |
| [`lock-pinned-by-name`](#lock-pinned-by-name) | Warning | Report-only |
| [`head-unreadable`](#head-unreadable) | Error | Report-only |
| [`projects-dir-unreadable`](#projects-dir-unreadable) | Error | Report-only |
| [`projectless-dir`](#projectless-dir) | Warning | Report-only |
| [`unnameable-project`](#unnameable-project) | Warning | Report-only |
| [`missing-lock-merge-driver`](#missing-lock-merge-driver) (`attribute-absent`, `attribute-legacy`, `config-absent`, `config-legacy`) | Warning | Auto-fixable |
| [`lock-merge-driver-unreadable`](#lock-merge-driver-unreadable) | Warning | Report-only |
| [`phantom-merge-driver`](#phantom-merge-driver) | Warning | Report-only |
| [`dangling-active-project`](#dangling-active-project) | Error | Auto-fixable |
| [`unreadable-active-project`](#unreadable-active-project) | Error | Auto-fixable |
| [`weave-root-identity-conflict`](#weave-root-identity-conflict) / `registered-workweave` | Error | Auto-fixable |
| [`weave-root-identity-conflict`](#weave-root-identity-conflict) / `marker-unverifiable`, `unwitnessed` | Error | Report-only |
| [`legacy-workweave-marker`](#legacy-workweave-marker) | Warning | Auto-fixable |
| [`unreadable-owned-state`](#unreadable-owned-state) | Warning | Report-only |
| [`unreadable-workweave-index`](#unreadable-workweave-index) | Error | Auto-fixable |
| [`workweave-tree-integrity`](#workweave-tree-integrity) / `dangling-parent`, `unregistered-workweave`, `stale-registry-entry` | Warning | Auto-fixable |
| [`workweave-tree-integrity`](#workweave-tree-integrity) / `parent-chain-anomaly`, `unregistered-dir`, `foreign-primary`, `foreign-primary-other-workspace`, `unreadable-marker`, `misnamed-dir`, `nested-workweave-dir`, `duplicate-name` | Warning | Report-only |
| [`workweave-drift`](#workweave-drift) / `missing`, `symlink`, `dangling-link` | Error | Auto-fixable |
| [`workweave-drift`](#workweave-drift) / `missing-unfetched`, `symlink-unfetched` | Error | Report-only |
| [`workweave-drift`](#workweave-drift) / `dangling-link-unfetched` | Error | Report-only by default (`--abandon-lost-pins`) |
| [`workweave-drift`](#workweave-drift) / `extra` | Warning | Report-only |
| [`member-permissions`](#member-permissions) (`writable-reference`, `read-only-member`) | Warning | Auto-fixable |
| [`role-disagreement`](#role-disagreement) | Warning | Report-only |
| [`history-disagreement`](#history-disagreement) | Warning | Report-only |
| [`stale-worktree-registration`](#stale-worktree-registration) / `directory-gone`, `directory-moved` | Warning | Auto-fixable |
| [`stale-worktree-registration`](#stale-worktree-registration) / `directory-stands`, `workweave-directory-gone` | Warning | Report-only |
| [`clone-topology`](#clone-topology) / `standalone-in-workweave` | Error | Auto-fixable |
| [`clone-topology`](#clone-topology) / `disconnected-weave-clone`, `wrong-parent-worktree`, `weave-clone-is-worktree` | Error | Report-only |
| [`missing-canonical-clone`](#missing-canonical-clone) | Warning | Report-only |
| [`uninitialized-submodule`](#uninitialized-submodule) / `fetched` | Warning | Auto-fixable |
| [`uninitialized-submodule`](#uninitialized-submodule) / `unfetched` | Warning | Report-only |
| [`provenance`](#provenance) / `origin-url-mismatch`, `relative-remote`, `placement-disagreement` | Warning | Report-only |
| [`provenance`](#provenance) / `unresolvable-url` | Error | Report-only |
| [`alignment`](#alignment) / `on-branch`, `on-branch-with-stray-commits`, `detached`, `detached-with-unpushed-commits`, `unaligned`, `project-detached` | Warning | Report-only |
| [`record-drift`](#record-drift) / `safe-to-fix` | Warning | Auto-fixable |
| [`record-drift`](#record-drift) / `live-recorded` | Warning | Report-only |
| [`working-tree-drift`](#working-tree-drift) / `safe-to-fix` | Warning | Auto-fixable |
| [`working-tree-drift`](#working-tree-drift) / `live-edits` | Warning | Report-only |
| [`orphaned-savepoint`](#orphaned-savepoint) / `redundant` | Warning | Auto-fixable |
| [`orphaned-savepoint`](#orphaned-savepoint) / `live` | Warning | Auto-fixable |
| [`unrecognized-savepoint-ref`](#unrecognized-savepoint-ref) | Warning | Report-only |
| [`stale-op-state`](#stale-op-state) | Warning | Report-only |
| [`unreadable-op-record`](#unreadable-op-record) | Warning | Report-only |
| [`dead-op-lease`](#dead-op-lease) (`owner-record-absent`, `owner-op-id-mismatch`) | Warning | Auto-fixable |
| [`claim-owner-unreachable`](#claim-owner-unreachable) | Warning | Auto-fixable |
| [`claim-unreadable`](#claim-unreadable) | Warning | Report-only by default (`--release-unreadable-claims`) |
| [`claim-holder-unreachable`](#claim-holder-unreachable) | Warning | Report-only by default (`--release-unreachable-claims`) |
| [`claim-unopenable`](#claim-unopenable) | Warning | Report-only |
| [`cargo-version-skew`](#cargo-version-skew) | Warning | Report-only |
| [`cargo-patch-shadowing`](#cargo-patch-shadowing) | Warning | Report-only |

The kinds on the `issues` array carry their severity and their `--fix`
authority on each finding (`severity`, `safe_to_fix`) rather than per kind, as
[The integration channel](#the-integration-channel) explains. Where the entry
states one answer for the whole kind, the table gives it:

| `kind` | `--fix` |
|---|---|
| [`tool-missing`](#tool-missing) | Never |
| [`managed-file-missing`](#managed-file-missing) | Per finding |
| [`managed-file-drift`](#managed-file-drift) | Per finding |
| [`managed-file-user-held`](#managed-file-user-held) | Never |
| [`surfacing`](#surfacing) | Per finding |
| [`config-rejected`](#config-rejected) | Never |
| [`malformed-settings`](#malformed-settings) | Never |
| [`member-incompatibility`](#member-incompatibility) | Never |
| [`derived-state-stale`](#derived-state-stale) | Never |
| [`disabled-integration-artifact`](#disabled-integration-artifact) | Never |
| [`integration-failed`](#integration-failed) | Per finding |
| [`core-finding`](#core-finding) | Per finding |

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
nothing else would notice until someone else's machine failed. `rwv init`
prints the same warning at mint, so a pair it creates is reported when it is
created rather than found later.

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

**What to do:** to register it, move it to the path its origin URL places it
at under the primary weave root (the canonical path
[`rwv member add`](./cli.md#rwv-member-add-url-) describes), unless it already sits there,
and run `rwv member add --adopt <path>` with that path. Adopted at any other path, it
is registered there, and an `owned` or `dependency` entry then reports
[`placement-disagreement`](#placement-disagreement). `rwv member add <url>` registers
it only once it sits at that path; anywhere else it makes a second clone and
leaves this one standing. Or remove it with `rwv member remove <path> --delete`,
which refuses while it holds what no remote has or its remote does not answer;
`rwv member remove <path> --delete --discard-unpublished` removes it with what no
remote is confirmed to hold, listing it first. Note that without
`--all`, orphan detection is skipped entirely — a repo missing from the active
project may belong to another one, and reporting it would be a false positive.

### `unknown-segment-clone`

**Error. Report-only.** A repo under a top-level directory that is neither a
built-in registry (`github/`, `gitlab/`, `bitbucket/`, `local/`) nor named by
any project's `rwv.toml` — so unlike `orphaned-clone`, nothing ever walked
that directory to find it. Always checked, with or without `--all`: which
top-level directories count as registry segments is already a fact about
every project's manifest, not about which ones this invocation loaded.

**What to do:** to register it, move it to the path its origin URL places it
at under the primary weave root (the canonical path
[`rwv member add`](./cli.md#rwv-member-add-url-) describes), unless it already sits there,
and run `rwv member add --adopt <path>` with that path. Adopted at any other path, it
is registered there, and an `owned` or `dependency` entry then reports
[`placement-disagreement`](#placement-disagreement). `rwv member add <url>` registers
it only once it sits at that path; anywhere else it makes a second clone and
leaves this one standing. Or remove the directory yourself.

### `dangling-reference`

**Error. Report-only.** An `rwv.toml` entry whose path is not on disk. An
entry whose `url` names no remote is reported as
[`unresolvable-url`](#unresolvable-url) instead, because `rwv update` cannot
clone it until the `url` is rewritten.

**What to do:** `rwv update` re-materializes it in place, or drop the entry
from the manifest if the repo is genuinely gone.

When a clone of the entry's URL stands at the path that URL places it at, and
no entry of the project names that path, the checkout was moved there ahead of
the re-key [`placement-disagreement`](#placement-disagreement) asks for. The
record's `clone_at_placement` names that path, and the message names the
repair: re-key the entry in `rwv.toml` to it, run `rwv lock`, and pin it with `rwv member pin <path> <rev> --commit`. That clone is
not reported as an [`orphaned-clone`](#orphaned-clone). `rwv update` there would
clone the URL a second time at the recorded path.

### `unparseable-project`

A project directory exists but does not load: its `rwv.toml` or its `rwv.lock` failed to parse.

**Why.** A project rwv cannot see into must not read as a clean project with zero findings, so `rwv doctor` reports it at error severity. Its `message` names which of the two files failed and carries that file's remedy, and `manifest_path` always names the manifest, because it locates the project rather than the failure. A verb that reads the manifest at the checked-out tip refuses when it names member-entry keys an earlier release wrote: no verb reads them, and the repair is mechanical.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: unparseable-project <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `here` | the project's `rwv.toml` at the checked-out tip names member-entry keys an earlier release wrote, each listed with the key that replaces it | every verb that reads the project's `rwv.toml`; `rwv doctor` reports it instead, and `rwv status` and `rwv prime` skip the project with a warning |
| `malformed` | the project's `rwv.toml` or `rwv.lock` does not parse, listed with the file that failed and the parser's error | `rwv sync` and `rwv land`, when the project's `rwv.lock` does not load after the project repo is replayed; `rwv doctor` reports it for every project |
| `at` | the `rwv.toml` at the tip of a live branch the verb acts on, not the checked-out one, names member-entry keys an earlier release wrote or does not parse, with the parser's error. `rwv doctor --fix` is offered for the keys an earlier release wrote; any other cause is fixed by a commit on that branch | the verbs that act on that branch |
| `mid-operation` | the `rwv.toml` the operation's merge or replay of the project repo produced does not parse, listed with the parser's error: the two sides merged as text without a conflict into a file that is not a manifest, or a conflict in it was resolved into one. Under merge history the merge is in progress and the merged file is in the working tree, marked resolved; continuing commits it in the merge. Under linear history the replay finished and the replayed tip holds the file; continuing makes the resolved file part of that commit, which the replay wrote and nothing has published. A resolved file that parses under a working file that differs and does not parse stops here too, before anything is committed. Under linear history the replayed tip whose `rwv.toml` parses stops here too where it differs from the merge of the values of the merge base, the commit replayed onto and the branch's tip before the replay, each entry that differs listed; an entry both sides changed differently keeps the tip's value | `rwv sync`, `rwv land` and `rwv pull --merge-upstream` when they first read what the merge or replay produced, and their `--continue` while the resolved `rwv.toml` does not parse; `rwv abort` runs |
| `commit` | the `rwv.toml` of a commit the verb reads by name does not parse, listed with the commit and the parser's error: one it copies from or compares with, or one it would check out or start a workweave at. The verb cannot tell which members the commit declares, so it moves and makes nothing; a commit is never rewritten, so nothing makes its `rwv.toml` parse | `rwv cherry-pick`, `rwv switch` to a commit, `rwv workweave create --at` |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `here` | rewrite the keys an earlier release wrote, committing only that | `rwv doctor --fix` | here | commits only the rewrite; other edits stay uncommitted |
| `at` | rewrite the keys an earlier release wrote, committing only that | `rwv doctor --fix` | where `<b>` is checked out | commits only the rewrite; other edits stay uncommitted |
| `mid-operation` | keep the text merge as the VCS made it | the stopped operation's `--continue --keep text-merge` | here | writes nothing; the text merge is not judged again in this operation |
| `mid-operation` | edit rwv.toml into what you mean and mark it resolved, then continue | VCS action (git: `git -C <checkout> add -- <paths>`); the stopped operation's `--continue` | the checkout | the resolved file is taken whole and not judged again |
| `mid-operation` | edit rwv.toml until it parses, then mark it resolved | VCS action (git: `git -C <checkout> add -- <paths>`) | the checkout | marks the working file resolved as it stands; the operation is still in progress |
| `mid-operation` | let the operation go on with the resolved rwv.toml | the stopped operation's `--continue` | here | a merge commits it in the merge; a replay makes it part of the replayed tip; it stops again while the resolved file does not parse |
| `mid-operation` | abandon the operation that stopped | `rwv abort`, against the stopped operation's workspace | here | `rwv abort` moves only what the operation moved, back to where it found it |

**Where this sits.** [Formats](./formats.md) states the member-entry keys; [Upgrade rwv](../how-to/upgrade-rwv.md) walks through the rewrite.

#### `pre-schema`

**Error. Auto-fixable.** The project's `rwv.toml` names member-entry keys an earlier release wrote: `version` where `branch` is now expected, or `submodules` where `recurse-submodules` is. Every verb that reads the manifest at the checked-out tip refuses with this kind, and `rwv explain unparseable-project` is its route line.

**What to do:** `rwv doctor --fix`, run in the project's checkout. It renames the keys in place — `version` to `branch`, `submodules = "skip"` to `recurse-submodules = false`, and drops `submodules = "local"`, which is what an absent `recurse-submodules` means — keeping the file's comments and key order. It writes the rename into the working file and into the project's HEAD commit and commits only that, in one commit: any other edit to `rwv.toml`, and any edit to another file, stays uncommitted where it was. It declines, changing nothing, while another file is marked for the next commit or a rebase, merge or cherry-pick is in progress in the project repo, and says so; run it again once that is dealt with. A value of `submodules` other than `"local"` or `"skip"`, or an entry that names both a key and its replacement, is not guessed at: the repair names it and leaves the file.

A commit other than the checked-out one that names the old keys is not reported and is not refused. It declares nothing to the verbs that read it, and its `rwv.lock` is read as any lock is.

#### `malformed`

**Error. Report-only.** Any other reason the project did not load. The message names which file failed and carries that file's remedy — the manifest is yours to edit, at the line the parser names; the lock is restored from version control, or written afresh by `rwv lock`, which then pins nothing until `rwv member pin` or `rwv update --remote` gives each member its pin. A repository entry with no `role`, an unknown top-level key, an unknown `[integrations.<name>]` table, an unknown key in one, and an `enabled` that is not `true` or `false` are all parse failures of this kind, each naming the line and what was expected. `rwv sync` and `rwv land` refuse with it when the project's `rwv.lock` does not load after the project repo is replayed, and `rwv abort` rolls that operation back; an `rwv.toml` the replay or merge produced that does not parse stops as `mid-operation` instead.

`--fix` has no arm for it. Rewriting a manifest rwv could not parse would be guesswork, and choosing the commits a lock pins is a decision about state, not a repair.

### `legacy-manifest-format`

**Error. Report-only.** A project directory holds an `rwv.yaml`, the name the
manifest had before it became TOML, and no `rwv.toml`. It is reported at error
severity because nothing in the project loads: without this finding the
directory would read as having no manifest at all and be passed over in
silence.

**What to do:** rewrite it as `rwv.toml` by hand and delete the `rwv.yaml`.
`--fix` has no arm here, for the same reason as
`unparseable-project` / `malformed` above and one more: the manifest is hand-authored, and
the comments and key order you wrote have no mechanical translation between
the two formats. A conversion that guessed at them would be worse than the
refusal. See [Formats](./formats.md) for the shape to write.

### `pin`

A commit a lock pins is not there to be at: the lock has no entry for a member, or pins a commit that no ref in the clone reaches; or a verb would delete, move or overwrite the last ref that reaches a commit some project's live lock pins.

**Why.** A lock pins each member at a commit so that a fresh clone can build the project. With no entry there is nothing to reproduce, and a commit no ref reaches is one the remotes this clone has fetched no longer hold; the member is checked out on its declared branch at the tip of the remote's branch instead, so the weave is usable, and the lock is left as it was for you to settle. Removing the last ref that reaches a pinned commit leaves the pin unreachable: no clone can fetch it, and the lock describes a state nobody can recreate. A pin is live in every project of the weave, not only the one the verb runs in, because a member's clone is shared.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: pin <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `behind` | the checkout's commit is an ancestor of the commit the lock pins: the lock records commits the checkout lacks | `rwv push`; `rwv status` reports it |
| `ahead` | the checkout holds commits beyond the commit the lock pins, and each is pushed or pinned by another live lock, which the facts name; the commits are published with `rwv push` only where none is pushed yet | `rwv status` reports it; `rwv push` reports it and goes on; `rwv cherry-pick` refuses it for a member it would copy onto |
| `unpinned` | the checkout holds the commits listed beyond the commit the lock pins, which no remote-tracking branch and no live lock reaches | `rwv push`; `rwv cherry-pick`, for a member it would copy onto; `rwv land`, in the target's workspace, or for a branch the target declares and nothing has checked out; `rwv status` reports it |
| `none` | the lock has no entry for the member, or there is no lock file; the checkout is on the declared branch at the tip of the remote's branch, or where it was found | `rwv push`; `rwv land` and `rwv sync`, for a member of the workspace they run in; `rwv clone`, which does not refuse: it completes, lists the member and exits with status 3; `rwv lock`, `rwv update` and `rwv status` report it |
| `diverged` | neither the checkout's commit nor the commit the lock pins is an ancestor of the other | `rwv push`; `rwv status` reports it |
| `off lineage` | the member's work is built on a commit the branch it would move into lacks: its base, the newest commit of its first-parent chain that a lock in the target's history pins or built on, listed with the receiving branch | `rwv sync`, for a member it would integrate, or that would follow while the branch has dependents; `rwv update --remote --merge-upstream` and `rwv pull --merge-upstream`, for a feature's member |
| `not on declared branch` | the lock pins a commit that neither the member's declared branch nor its upstream reaches, so the checkout sits detached at the pin | `rwv member pin`, for a commit that neither reaches; `rwv cherry-pick`, for a `dependency` or `reference` member's pin it would carry; `rwv update` and `rwv status` report it |
| `unreachable` | the lock pins a commit that no ref in the clone reaches; the checkout is on the declared branch at the tip of the remote's branch | `rwv push`; `rwv land`, for a pin of the target's lock as last fetched, naming `rwv fetch`; `rwv clone`, which does not refuse: it completes, lists the member and exits with status 3; `rwv cherry-pick`, for a pin the ported commit or its parent holds; `rwv status` reports it |
| `off remote` | a live lock pins a commit that no remote-tracking branch of the member reaches after the pruning fetch, listed with the locks that pin it. Choices print only in a workspace whose checked-out branch's lock holds the pin: a feature's offers publishing it, a declared branch's offers pinning another commit; elsewhere the state is a fact and offers waiting | nothing refuses; `rwv fetch` and `rwv pull` report it |
| `backwards` | recording the commit the checkout is at would move the pin to one that does not descend from the pinned commit: it is behind the pin or has diverged from it, or the pinned commit is in no ref here | `rwv lock`, with or without `--repo`; `rwv update --remote`; `rwv cherry-pick`, for a member `--except` names; `rwv land` and `rwv sync`, for a member of the workspace they run in, before anything moves. A pin moves backward only by name: `rwv member pin`, a setter, or a hand edit of the member's entry that `rwv lock --commit` commits |
| `unmerged` | a merge commit in the range being pushed joined two pins of one member that its parents changed, each under one declared branch, and the pushed tip pins neither merged with the other: a commit holding both. The merge and the pins are listed. A member the parents declare differently is listed and not refused. A commit at or after the merge that records re-pinning the member backward by name, to a commit reaching neither pin the merge left unmerged, waives it (`pin` waived); a commit made outside rwv records none, and a record whose old pin no parent's lock holds, or whose new pin its own lock does not, waives nothing and is listed. At a landing: the feature and the target each changed a `dependency` or `reference` member's pin since their merge base, and neither pin contains the other | `rwv push`; `rwv land`, before anything moves |
| `waived` | a merge in the range being pushed left one member's pins unmerged, and a commit in the range at or after it, the merge's own included, records re-pinning the member backward by name to a commit that does not reach the pin the merge left. The merge, the re-pin that waived it and the verb that made it are listed. A landing that rebased the member records that re-pin in its project commit, from the commit the feature pinned to the one the rebase made of it, and is listed as such | nothing refuses; `rwv push` reports it and goes on |
| `would be unreachable` | a live lock pins a commit that only the backups being dropped reach, listed with the locks that pin it | `rwv backup drop` and `rwv backup prune` |
| `held by no branch` | a project commit of the range being pushed pins the commit named, which no branch or remote-tracking branch reaches, listed with the project commits pinning it, the backups holding it, and the pushed tip's pin where that records a copy of it. A pin no backup holds is not listed where a commit of the range re-pinned the member by name off it or off a commit containing it. Only the pushed tip's pins must be reached, so the push publishes the range | nothing refuses; `rwv push` reports it |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `behind` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `behind` | C1: take in theirs | `rwv update --remote --repo <m>` | any workspace | fast-forward only |
| `ahead` | C5: record it: the lock, and `rwv.toml` as it stands | `rwv lock --commit` | a workspace with the branch checked out | records only |
| `ahead` | C7: publish it (again) | `rwv push --repo <m>` | here | adds only |
| `unpinned` | C5: record it: the lock, and `rwv.toml` as it stands | `rwv lock --commit` | a workspace with the branch checked out | records only |
| `unpinned` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `unpinned` | C3: take theirs; yours is kept in a backup | `rwv update --remote --reset-to-upstream <repo> --commit` | a workspace with the branch checked out | backup first |
| `unpinned`, for `rwv land` | open it | `rwv workweave create <ww> --branch <W>` | here | moves only what alignment moves |
| `none` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `none` | C1: take in theirs | `rwv update --remote --repo <m> --commit` | any workspace | fast-forward only |
| `diverged` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `diverged` | C3: take theirs; yours is kept in a backup | `rwv update --remote --reset-to-upstream <repo> --commit` | a workspace with the branch checked out | backup first |
| `diverged` | C22: squashed, rebase-merged or on another lineage: reset yours to the target's here | `rwv sync --reset-to-target <m>` | the workspace with that feature | backup kept; local only |
| `off lineage` | C22: squashed, rebase-merged or on another lineage: reset yours to the target's here | `rwv sync --reset-to-target <m>` | the workspace with that feature | backup kept; local only |
| `off lineage` | C11: declare a branch for it, or keep, restore or port the entry's value | `rwv member set-branch <m> <b>` | a workspace with the branch checked out | re-pins by name, backup kept |
| `off lineage`, for `rwv update` | C3: take theirs; yours is kept in a backup | `rwv update --remote --reset-to-upstream <repo> --commit` | a workspace with the branch checked out | backup first |
| `off lineage`, for `rwv update` | C27: publish this commit under a branch name you choose | VCS action (git: `git -C <checkout> push <r> <sha>:refs/heads/<name>`) | any workspace | creates or fast-forwards |
| `off lineage`, for `rwv pull` | C3: take theirs; yours is kept in a backup | `rwv pull --reset-to-upstream` | a workspace with the branch checked out | backup first |
| `off lineage`, for `rwv pull` | C27: publish this commit under a branch name you choose | VCS action (git: `git -C <checkout> push <r> <sha>:refs/heads/<name>`) | any workspace | creates or fast-forwards |
| `not on declared branch` | C19: wait until the PR lands or its branch is deleted, then rerun | — | nowhere | nothing changes |
| `not on declared branch` | C15: copy these commits onto `<b>` | VCS action (git: `git -C <checkout> cherry-pick -x <ids>`); then `rwv lock --commit` | where `<b>` is checked out | a copy adds only |
| `not on declared branch` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `not on declared branch` | C11: declare a branch for it, or keep, restore or port the entry's value | `rwv member set-branch <m> <b>` | a workspace with the branch checked out | re-pins by name, backup kept |
| `not on declared branch`, for `rwv cherry-pick` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit`; then rerun with `--except <m>` | here | re-pins by name; backup for a detached HEAD |
| `unreachable` | add a remote for `<url>`, under a name no remote has | VCS action (git: `git -C <checkout> remote add <r> <url>`) | any workspace | config only |
| `unreachable` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `unreachable` | C19: wait until the PR lands or its branch is deleted, then rerun | — | nowhere | nothing changes |
| `unreachable`, for `rwv land` | C33: look for what may be published but not yet fetched | `rwv fetch` | any workspace | downloads only |
| `off remote` | C22: squashed, rebase-merged or on another lineage: reset yours to the target's here | `rwv sync --reset-to-target <m>` | the workspace with that feature | backup kept; local only |
| `off remote` | C7: publish it (again) | `rwv push --repo <m>` | here | adds only |
| `off remote` | C27: publish this commit under a branch name you choose | VCS action (git: `git -C <checkout> push <r> <sha>:refs/heads/<name>`) | any workspace | creates or fast-forwards |
| `off remote` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `off remote` | C19: wait until the PR lands or its branch is deleted, then rerun | — | nowhere | nothing changes |
| `backwards` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `backwards`, for `rwv update` | C3: take theirs; yours is kept in a backup | `rwv update --remote --reset-to-upstream <repo> --commit` | a workspace with the branch checked out | backup first |
| `unmerged` | C26: merge `<pin>` in as a merge commit | VCS action (git: `git -C <checkout> merge --no-ff <pin>`) | the checkout | a merge adds only |
| `unmerged` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `would be unreachable` | C7: publish it (again) | `rwv push` | here | adds only |
| `would be unreachable` | C27: publish this commit under a branch name you choose | VCS action (git: `git -C <checkout> push <r> <sha>:refs/heads/<name>`) | any workspace | creates or fast-forwards |
| `held by no branch` | C27: publish this commit under a branch name you choose | VCS action (git: `git -C <checkout> push <r> <sha>:refs/heads/<name>`) | any workspace | creates or fast-forwards |

**Where this sits.** [Lock as derived](../explanation/joints/lock-as-derived.md); [Reconcile repos](../how-to/reconcile-repos.md); [Destructive operations](../explanation/destructive-operations.md).

#### `none`

**Error. Report-only.** The lock has no entry for a member the manifest lists. Only raised for a project that has a lock file; a project with no lock yet is a separate, unlocked state. **What to do:** `rwv member pin <repo> <rev> --commit` gives the member its pin, or `rwv update --remote --repo <repo> --commit` pins its upstream's tip. `rwv lock` gives a member no first pin.

#### `unreachable`

**Error. Report-only.** The lock pins a commit this clone cannot resolve: a lock written against history that was never fetched here. The entry is kept as a finding rather than dropped, because dropping it would leave the repo with no pin to compare against, which reads as healthy. **What to do:** `rwv fetch` brings in the history that may hold it, or `rwv member pin <repo> <rev>` pins a reachable commit if the lock is the stale side.

#### `behind`

**Error. Report-only.** The checkout is at an ancestor of its pin: the lock records commits the checkout lacks. **What to do:** `rwv update` moves the checkout to the pin, or `rwv member pin <repo> HEAD` pins the checkout's commit by name, moving the pin back.

#### `ahead`

**Warning. Report-only.** The checkout holds commits beyond its pin, and each is pushed or pinned by another project's live lock: the ordinary state between committing and `rwv lock`, so `rwv doctor` warns and exits as it does for any warning. **What to do:** `rwv lock` records the checkout's commit.

#### `unpinned`

**Error. Report-only.** The checkout holds commits beyond its pin that no remote-tracking branch and no live lock reaches, listed: nothing but this checkout holds them. **What to do:** `rwv lock --commit` records them.

#### `diverged`

**Error. Report-only.** Neither the checkout nor its pin is an ancestor of the other. **What to do:** `rwv member pin <repo> HEAD` pins the checkout's commit by name, `rwv member pin <repo> <rev>` pins another commit you choose, or `rwv update` moves the checkout to the pin once its own commits are kept on a branch.

### `lock-pinned-by-name`

**Warning. Report-only.** An `rwv.lock` entry holds a tag or branch name where a
commit id belongs. The entry resolves to wherever the name points now, and to
nothing once the name is gone, so it pins nothing. `path` names the repo,
`project` the lock's project, and `version` the name the entry holds. An entry
holding a full commit id raises nothing, and a lock that is absent or clean of
names raises nothing.

`rwv status` marks the same entry (`lock_pinned_by_name` in `--json`), and
`rwv doctor --locked` prints the same warning beside the entry. `rwv clone`,
`rwv update` and `rwv sync` warn about it on stderr and proceed.

**What to do:** run `rwv lock`, which records the commit id each name points at.

### `head-unreadable`

**Error. Report-only.** A repo present under a registry directory whose HEAD
could not be read. Every freshness comparison for that repo — pin,
drift, provenance — is unevaluated, and reporting the read failure is what
keeps the repo from looking clean.

**What to do:** the `error` field carries what git said. A directory that is
not a git repo, a corrupt `.git`, and a permissions problem all land here.

**Also a sync failure kind.** One condition, one name, so `rwv land --json`
reports a repo whose HEAD it could not read under this same kind, as a per-repo `failure.kind` rather than a `rwv doctor` violation.
The text lands in `message` there instead of `error`, and land raises it for
one case a scan cannot see: the source lock pinning a version the source
workspace itself could not resolve, which leaves land with no HEAD to compare
against for a reason that is nothing to do with the repo in front of it.

### `projects-dir-unreadable`

**Error. Report-only.** The `projects/` directory exists but could not be
listed — a permissions problem, most plausibly. Every project under it is
invisible to this scan, and without this finding that reads exactly like a
workspace that genuinely has none: a broken registry and an empty one would
otherwise print the same "clean" result. A `projects/` that does not exist
yet (before the first `rwv member add`) is a different, unremarkable state and does
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

The character policy behind the refusal is documented in
[formats.md — Names, and the characters they exclude](formats.md#names-and-the-characters-they-exclude).

**What to do:** rename the directory to a name that validates. Nothing inside
it needs to change — the name is the weave's, not the project repo's.

### `missing-lock-merge-driver`

A project repo's lock merge driver is not set up as this release sets it up: the checked-out branch's committed attribute line for `rwv.lock` is absent or names a driver an earlier release wrote, or the clone's config does not define the driver's command or still defines the earlier one.

**Why.** A merge rwv does not make merges `rwv.lock` through the lock merge driver: pins merge three-way, and a merge the driver cannot settle is left conflicted with both pins marked. Without the attribute line git merges the lock as text; without the config it falls back to text, or fails into a conflict. The driver an earlier release set up kept one side of every lock merge silently, so its line and config are migrated, never honored. Every verb that writes the project repo or its lock refuses until `rwv doctor --fix` migrates it, and `rwv doctor` reports it instead.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: missing-lock-merge-driver <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `attribute absent` | the checked-out branch's committed `.gitattributes` assigns `rwv.lock` no merge driver, so a merge rwv does not make merges the lock as text | every verb that writes the project repo or its lock, before it writes anything: `rwv lock`, `rwv sync`, `rwv land` (for the project checkout it composes onto too), `rwv pull`, `rwv update --remote`, the `rwv member` verbs, `rwv branch create`, `delete`, `rename` and `set-mode`, and `rwv workweave create` and `delete`; `rwv doctor` reports it instead |
| `attribute legacy` | the checked-out branch's committed `.gitattributes` assigns `rwv.lock` `merge=rwv-ours` or `merge=ours`, a driver an earlier release wrote that kept one side of every lock merge | every verb that writes the project repo or its lock, before it writes anything: `rwv lock`, `rwv sync`, `rwv land` (for the project checkout it composes onto too), `rwv pull`, `rwv update --remote`, the `rwv member` verbs, `rwv branch create`, `delete`, `rename` and `set-mode`, and `rwv workweave create` and `delete`; `rwv doctor` reports it instead |
| `config absent` | the clone's config does not define the lock merge driver's command, so git merges the lock as text | every verb that writes the project repo or its lock, before it writes anything: `rwv lock`, `rwv sync`, `rwv land` (for the project checkout it composes onto too), `rwv pull`, `rwv update --remote`, the `rwv member` verbs, `rwv branch create`, `delete`, `rename` and `set-mode`, and `rwv workweave create` and `delete`; `rwv doctor` reports it instead |
| `config legacy` | the clone's config defines a driver that keeps one side: the `merge.rwv-ours` entry an earlier release wrote, or `merge.rwv-lock` naming a command other than `rwv lock --merge-driver %O %A %B` | every verb that writes the project repo or its lock, before it writes anything: `rwv lock`, `rwv sync`, `rwv land` (for the project checkout it composes onto too), `rwv pull`, `rwv update --remote`, the `rwv member` verbs, `rwv branch create`, `delete`, `rename` and `set-mode`, and `rwv workweave create` and `delete`; `rwv doctor` reports it instead |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `attribute absent` | set up the lock merge driver: rewrite the attribute line, committing only that, and replace the config | `rwv doctor --fix` | the checkout | commits only the rewritten line; other edits stay uncommitted |
| `attribute legacy` | set up the lock merge driver: rewrite the attribute line, committing only that, and replace the config | `rwv doctor --fix` | the checkout | commits only the rewritten line; other edits stay uncommitted |
| `config absent` | set up the lock merge driver: rewrite the attribute line, committing only that, and replace the config | `rwv doctor --fix` | the checkout | commits only the rewritten line; other edits stay uncommitted |
| `config legacy` | set up the lock merge driver: rewrite the attribute line, committing only that, and replace the config | `rwv doctor --fix` | the checkout | commits only the rewritten line; other edits stay uncommitted |

**Where this sits.** [Lock as derived](../explanation/joints/lock-as-derived.md); [Upgrade rwv](../how-to/upgrade-rwv.md).

#### `attribute-absent`

**Warning. Auto-fixable.** The checked-out branch's committed `.gitattributes` has no `rwv.lock merge=rwv-lock` line, so a merge rwv does not make merges the lock as text.

**What to do:** `rwv doctor --fix`, run in the project's checkout. It adds the line to the working file and to the checked-out commit's file and commits only that line; any other edit to `.gitattributes` stays uncommitted, and anything else marked for the next commit stays marked. It declines, changing nothing, while a merge, rebase or cherry-pick is in progress in the project repo. Another branch carries its own file and is migrated where it is checked out.

#### `attribute-legacy`

**Warning. Auto-fixable.** The checked-out branch's committed `.gitattributes` assigns `rwv.lock` `merge=rwv-ours` or `merge=ours`, the driver an earlier release wrote, which kept one side of every lock merge. It is not honored.

**What to do:** `rwv doctor --fix`, as above: it rewrites the line to `rwv.lock merge=rwv-lock`, committing only that.

#### `config-absent`

**Warning. Auto-fixable.** The clone's config does not define `merge.rwv-lock.driver`. Config does not travel with a clone, so a clone rwv did not make has none; every workweave shares its clone's.

**What to do:** `rwv doctor --fix`. It sets `merge.rwv-lock.driver` to `rwv lock --merge-driver %O %A %B`, which git runs by finding `rwv` on `PATH`.

#### `config-legacy`

**Warning. Auto-fixable.** The clone's config still defines `merge.rwv-ours.driver`, the driver an earlier release planted, or defines `merge.rwv-lock.driver` as a command other than `rwv lock --merge-driver %O %A %B`.

**What to do:** `rwv doctor --fix`. It replaces the config: the earlier entries are removed and `merge.rwv-lock.driver` names rwv's command.

### `lock-merge-driver-unreadable`

**Warning. Report-only.** Reading the project repo's lock merge driver setup —
the checked-out commit's `.gitattributes` or the clone's config — failed, so it
is neither confirmed nor reported missing.

**What to do:** the `error` field carries what git said. Once it reads, re-run
`rwv doctor` to get the real answer.

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

### `unreadable-active-project`

**Error. Auto-fixable.** `.rwv-active` is empty, cannot be read, or holds text
that is not a project name. rwv never writes such a file, so it selects
nothing: a verb that would take its project from the pointer refuses with this
token rather than act as if no project were selected, and a
`rwv clone` into a directory holding one refuses before it clones. `rwv doctor`
runs with no project selected, so it can report the file. Verbs that do not
select a project (`rwv activate`, `abort`, `resolve`, `-w` addressing) are not
affected, and neither is a verb given `--project`.

**What to do:** `rwv activate <project>` replaces the file with a selection, or
`rwv doctor --fix` removes it.

### `weave-root-identity-conflict`

**Error.** A weave root carries both `.rwv-active` and `.rwv-workweave`. The
two files name the same fact — which project this tree belongs to — and are
mutually exclusive by design: a primary root carries the pointer, a workweave
root carries the marker. A tree holding both holds two copies of its own
identity with nothing keeping them in agreement.

Fixability turns on evidence held *outside* the tree, because the two files
are themselves the ambiguity. Primary-ness has no independent signature: a
primary root and a workweave root both hold `projects/` and registry
directories. The workweave index the primary's project clone keeps is the
outside witness — it is written only by `rwv workweave create` and records the
absolute path of every workweave it made, so a tree the index names is a
workweave on the authority of a file that tree does not contain and could not
have forged by being copied.

#### `registered-workweave`

**Auto-fixable.** The marker names this workspace's primary, and that
primary's registry records this exact directory. The tree is a workweave, so
`.rwv-active` is the redundant copy.

**What to do:** `rwv doctor --fix` deletes the pointer and leaves the marker.

#### `marker-unverifiable`

**Report-only.** The marker itself cannot witness what it claims: it is
unreadable, a legacy marker (YAML, or missing the required `parent` or `name` field),
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

**Warning. Auto-fixable.** A `.rwv-workweave` marker an earlier release wrote,
which this build cannot use as written: one with no `name` field, or YAML
(markers are JSON now), possibly also missing the `parent:` field required
before the format changed. It is reported for every workweave a checkout of
the project clone sits in, wherever its directory is, and for every directory
in a workweave container.

**What to do:** `rwv doctor --fix` rewrites the marker as JSON, backfilling
`parent: <primary value>` where it is absent, and with the name the project's
workweave index records for that directory. While the index an earlier release
kept at `projects/<project>/.rwv-workweave-index` stands, the name is read from
that file, so run the fix before anything removes it. A workweave no index
lists takes the name its directory's name `<project>--<name>` spells. One whose
name nothing records is reported with the verb that names it,
`rwv workweave create <name> --project <project> --dir <dir>` run from its
parent, which writes the name into the marker and records the workweave as it
stands. Run `--fix` from the primary weave: from inside the workweave the
marker refuses before any verb dispatches, naming `rwv -C <primary> doctor
--fix`.

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
<project>` first; the repair runs there once it is the active one. Where the
project generates no attested files, `rwv materialize` writes an empty
`.rwv-owned-digests` record so subsequent scans see a valid file.

`--fix` does not do it for you: rebuilding the record attests whatever is on
disk at that moment as accepted, and what was accepted before is exactly what
has been lost, so nothing can check the two against each other first. If the
current content matters, inspect it before you re-derive.

### `unreadable-workweave-index`

**Error. Auto-fixable.** A workweave index that exists and does not parse, in
the project clone's shared directory or at the old path an earlier release
kept it at (`projects/<project>/.rwv-workweave-index`): a corrupt file, one an
earlier release wrote, or one a newer release wrote with a field this one does
not know. Everything derived from it is unevaluated: the recorded placements.
Every verb that reads it, which is every verb run in one of the project's
workweaves, refuses with this kind, naming `rwv -C <primary> doctor --fix`.

While the index cannot be parsed, `unregistered-workweave` and
`stale-registry-entry` are suppressed for the project because attempting to
adopt or prune against an unreadable index would fail on the same parse error.

**What to do:** `rwv doctor --fix`, run in the primary or in any of the
project's workweaves, rebuilds the index from what the workweaves on disk
record: each checkout the project clone links, wherever its directory is,
sits in a workweave whose `.rwv-workweave` marker holds its project, name and
parent. What no marker holds is not rebuilt: the basedir the index recorded
(`rwv workweave set-default-create-basedir` records it again). The rebuild
removes a file at the old path.

### `workweave-tree-integrity`

Anomalies in the `.rwv-workweave` marker tree and the registry that records
it. The registry is advisory rather than authoritative, and this finding
surfaces divergence between the registry and on-disk state.

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
is not recorded in that project's workweave index. The workweave exists on disk
but the project clone does not know about it.

**What to do:** `rwv doctor --fix` adopts the entry, with the parent its
marker records. Read paths (`list`, `delete`) do not adopt on the fly —
adoption is an operator decision, so it happens where you asked for it. A verb
run from inside the workweave lists it again on its own when its marker and its
project checkout's link to the project clone agree.

**Scope.** The scan reads each checkout the project clone links, wherever its
directory is (`rwv workweave create <name> --dir <path>` included), and each
directory in the containers each project records. The name adopted is the one
the workweave's marker holds.

#### `stale-registry-entry`

**Warning. Auto-fixable.** A registered workweave whose recorded path is not a
valid workweave: missing directory, missing or unreadable marker, a marker that
names another primary or project or a different parent than the entry records.
This covers both "deleted out of band with the registry left behind" and "the
marker was lost or edited".

**What to do:** `rwv doctor --fix`. Where the directory is still a worktree of
the project clone, it writes the marker again from the entry and the worktree
relation (`primary`, `project` and the `parent` the entry keeps). Where nothing
the entry names is left, it removes the entry. A missing directory the project
clone still registers a worktree inside is not reported here: its registrations
are [`workweave-directory-gone`](#workweave-directory-gone), and the entry stays
with them. A verb that does not destroy
the workweave, run from inside it, restores the marker the same way and says
so; `rwv workweave delete` and `rwv land --retire` refuse instead, naming
`rwv doctor --fix`.

#### `foreign-primary`

**Warning. Report-only.** The marker's `primary:` path neither matches the
workspace this scan started from nor resolves to any workspace — for example
a workweave copied to another machine whose marker still holds the origin
machine's absolute path.

**What to do:** correct `primary:` by hand, or remove the copy.

#### `foreign-primary-other-workspace`

**Warning. Report-only.** The marker's `primary:` resolves to a different but
perfectly valid weave root — the normal shape when several weaves share
one workweave container. It is not a defect in this workweave, so it is the
one finding excluded from the text report; every sibling weave's doctor would
otherwise repeat this about every other sibling. It is still enumerated under
`--json`.

**What to do:** nothing.

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
whether the basename is one the marker's project could have rendered at all;
the scans name the workweave by its marker either way.

**What to do:** rename the directory to the name the finding reports (the
checkouts inside were registered under the recorded name, so restoring it
also restores their worktree back-pointers). When no record pins the intended
name, rename it to the `<project>--<name>` you intend, or remove the
directory. Never auto-fixed: a directory rename under live checkouts is not
rwv's to perform, and in the unrecoverable case the target is not derivable.

#### `duplicate-name`

**Warning. Report-only.** The markers of more than one directory claim one
workweave name, as a hand-copied or hand-edited marker leaves. The finding
lists every claimant. Until one claim stands, `rwv doctor --fix` adopts none of
them over the entry the index holds, the index rebuild lists none of them, a
command run inside one does not record it, and `rwv workweave delete <name>`
refuses as `registry-path-disagreement`, deleting nothing.

**What to do:** the finding prints, for each claimant but the one the index
records, the command that names it otherwise:

```bash
rwv -C <parent> workweave create <other-name> --project <project> --dir <claimant>
```

It writes the new name into that marker and records the workweave, changing
nothing else there. A claimant that is a copy of another workweave, whose
project checkout the VCS records in the other directory, is named as one: no
verb names it, and removing the directory leaves the original whole.

#### `nested-workweave-dir`

**Warning. Report-only.** A recorded workweave whose directory sits *below* a
workweave container instead of directly in it: a multi-segment project name
rendered with its `/` intact, as `chatly/web-app` plus `wtest` at
`<container>/chatly/web-app--wtest`, which leaves `chatly` behind as a
directory the container scan reads as a stray. rwv writes that `/` as `+`
(`<container>/chatly+web-app--wtest`), and this finding names the
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

A workspace's checkouts disagree with its member list: a workweave has no
worktree for a member its `rwv.lock` pins, or a checkout remains, in a
workweave or in the primary, for a member the manifest no longer lists. Every
member of a workweave, whatever its `role`, is a worktree of the canonical
clone on the workweave's ephemeral branch. Doctor checks the workspace it runs
in; from the primary, also every workweave of the projects in scope.

A symlink at a member's slot, dangling or not, counts as no worktree. rwv does
not operate on one, so the verbs that walk a workweave's members refuse with
this token until it is replaced. Replacing it unlinks the symlink and never
touches what it pointed at.

While a workweave records an operation in progress, a member it has no
worktree for is not reported, here or as
[`dangling-reference`](#dangling-reference), and `--fix` makes none: the
operation's `--continue` places it.

A checkout whose worktree link names a git directory that is not there also
counts as no worktree: git cannot open it, though its files stand. Doctor
reports it as such, never as live edits.

Every repair here gives the member back through what the canonical clone
still holds for it, and moves or deletes no branch:

- a registration of the checkout: only the link is rewritten, so HEAD, index
  and files are as they were;
- the workweave's branch: the worktree is put on it at its own tip, which may
  hold commits `rwv.lock` does not;
- neither: the branch is created at the revision `rwv.lock` pins, and `--fix`
  says so.

A registration of a removed checkout directory is dropped first, unless it is
locked. `rwv update` run in the workweave takes the same path.

#### `missing`

**Error. Auto-fixable.** Nothing is at the member's slot, and the canonical
clone holds the workweave's branch or the revision `rwv.lock` pins.

**What to do:** `rwv doctor --fix` creates the worktree on the workweave's
branch at its own tip, or at the pinned revision where the branch is gone. No
network is involved.

#### `missing-unfetched`

**Error. Report-only.** Nothing is at the member's slot, and the canonical
clone holds neither the workweave's branch nor the pinned revision, or is not
on disk.

**What to do:** `rwv update` run in the workweave fetches the pinned revision
into the canonical clone, cloning the canonical clone first where it is not on
disk, and creates the worktree. It moves no branch of the primary's. Doctor
never fetches.

#### `symlink`

**Error. Auto-fixable.** A symlink is at the member's slot, and the canonical
clone holds the revision `rwv.lock` pins — or, where the lock pins none, the
canonical clone's HEAD, which is what the link showed.

**What to do:** `rwv doctor --fix` replaces the link with a worktree at that
revision.

#### `symlink-unfetched`

**Error. Report-only.** A symlink is at the member's slot, and the canonical
clone does not hold the pinned revision, or is not on disk.

**What to do:** as for `missing-unfetched`; the worktree replaces the link.

#### `dangling-link`

**Error. Auto-fixable.** A checkout stands at the member's slot, but its link
to the canonical clone names a git directory that is not there: the canonical
clone was cloned again, or its registration was removed. The canonical clone
holds its registration of the checkout, the workweave's branch, or the pinned
revision.

**What to do:** `rwv doctor --fix` relinks the checkout, from the workweave or
from the primary weave, keeping every file in place. Where it is put on the
branch rather than through a surviving registration, the index is read from the
branch tip, so files that differ from it read as edits.

#### `dangling-link-unfetched`

**Error. Report-only by default.** As `dangling-link`,
and the canonical clone holds neither its registration of the checkout, the
workweave's branch nor the pinned revision.

**What to do:** if the pinned revision is on the remote, run
`rwv update --project <project>` at the primary weave, then `rwv doctor --fix`,
which relinks the checkout onto it. If it is in no clone — the workweave
committed it and the clone holding it was deleted before it was pushed —
`rwv doctor --fix --abandon-lost-pins` relinks the checkout onto the
workweave's branch created at the canonical clone's HEAD. Every file stays, so
the lost commits' changes stand as edits against that HEAD; commit them and
run `rwv lock` in the workweave. A canonical clone that is not
on disk at all is reported as
[`missing-canonical-clone`](#missing-canonical-clone) instead.

#### `extra`

**Warning. Report-only.** A checkout remains for a path `rwv.lock` names and
`rwv.toml` no longer does: a member removed by hand. Doctor reports it in its
default scope, from the primary (the canonical clone, with no `workweave`) and
from each workweave (that workweave's worktree). A clone another project's
manifest still lists is not a leftover, and a path neither file names raises
nothing here; `--all` reports such a clone as
[`orphaned-clone`](#orphaned-clone). `--fix` never deletes it.

**What to do:** `rwv member remove <path> --delete` deletes the checkout, keeping the
guards that refuse a dirty tree, unpushed commits or a clone another project
uses. `rwv member add <url>` keeps it as a member again.

### `member-permissions`

**Warning. Auto-fixable.** A member checkout whose tracked regular files do
not hold the permissions its role asks for: a `reference` member's files carry
no write bit, and every other member's are writable by their owner. The two
sub-kinds share that disposition and differ only in which way the files
disagree: `writable-reference` (a `reference` member has writable tracked
files) and `read-only-member` (a member of another role has read-only ones).
`files` counts them. Doctor checks the members of the workspace it runs in,
and from the primary also those of every registered workweave in scope. A
canonical clone in the primary that several projects list is checked against
the role its files hold for all of them (see
[`role-disagreement`](#role-disagreement)), so every project scope agrees on
it.

Only tracked regular files are counted or touched. A tracked symlink is never
chmod'ed, since that would change what it points at; directories keep their
write bits, so the VCS can still replace files during an update and untracked
files can still be created in a `reference` member.

Every verb that writes or moves a member's files settles them for its role,
so this finding appears after a hand edit of `role` in `rwv.toml`, a chmod by
hand, or in a weave whose `reference` members were checked out writable.

**What to do:** `rwv doctor --fix` settles the member: it removes the write
bits of a `reference` member's tracked files, and restores the owner's write
bit on another member's. A read-only file that shares its storage with
another workspace's copy is first replaced by a private copy, since write bits
belong to the file rather than to the name, and restoring them in place would
make every workspace's copy writable.

### `role-disagreement`

**Warning. Report-only.** A canonical clone in the primary that the projects
listing it disagree on: some declare it `reference`, others another role.
`roles` names every project listing it, with the role each declares, whether
or not that project is in scope. Doctor reports it from the primary, for each
clone a project in scope lists.

One clone serves every project in the primary that lists it, so its files can
hold only one set of permissions. They are read-only only when every project
listing the clone declares `reference`; otherwise they stay writable, whichever
project's verb last settled them. A project declaring `reference` therefore
gets a writable clone, and its `reference` role is not enforced there.
Workweaves are unaffected: each has its own checkout, settled for its own
project's role.

**What to do:** decide which role the clone should have and give it that role
in every project listing it, with `rwv member set-role --project <name> <repo> <role>`.
`--fix` does not choose between the projects.

### `history-disagreement`

**Warning. Report-only.** Two projects list the same member, and their live weave branches record different `history` for it.

**Why.** A member's clone is shared, so one member branch can be landed into by several projects. Each project's landings follow its own setting, so the branch takes rebased work from one project and merge commits from another, and neither project's setting describes its history. Nothing refuses: each project's setting is its own to keep, and `rwv doctor` reports the difference.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: history-disagreement <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `disagreement` | projects listing this member record different `history` for it on their live weave branches, each branch listed with its value | nothing refuses; `rwv doctor` reports it |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `disagreement` | C11: declare a branch for it, or keep, restore or port the entry's value | `rwv branch set-history <h> --repo <m>` | a workspace with the branch checked out | re-pins by name, backup kept |

**Where this sits.** [Formats](./formats.md) states the `history` keys.

`histories` names every live weave branch, of every project listing the member, that records a `history` for it, with the value it records: the member's own, else the branch's top-level one. Branches of one project that differ among themselves are not reported. Doctor reports it from any workspace, for each member a project in scope lists.

**What to do:** decide which shape the member's branches take, and give every project that setting with `rwv branch set-history <h> --repo <m>` in a workspace on each branch that records another, then push it. Leaving it is allowed: each project's landings follow its own setting.

### `stale-worktree-registration`

A git worktree registration that no longer leads to a worktree.

#### `directory-gone`

**Warning. Auto-fixable.** Nothing stands at the registered path.

**What to do:** `rwv doctor --fix` drops the registration. The repair is
information-preserving by construction: the only state dropped is a pointer to
a directory that already is not there. A locked registration is left in place
and still reported.

#### `directory-stands`

**Warning. Report-only.** A directory stands at the registered path, but
without the `.git` file that links it to the repo, as when something copied
over the checkout. Its files may be your work, so `--fix` keeps the
registration and says so.

**What to do:** move the directory away, keeping whatever you need from it,
then run `rwv doctor --fix`: the finding becomes `directory-gone`, and the fix
drops the registration.

#### `directory-moved`

**Warning. Auto-fixable.** Nothing stands at the registered path, and the
checkout it registers stands in a workweave directory elsewhere, moved by hand:
a workweave directory in a workweave container, or the workspace `rwv doctor`
runs in. The finding names both paths.

**What to do:** `rwv doctor --fix` records the checkout where it stands, through
the VCS's own repair for a moved checkout, so what the registration holds of it
(the commit it is on, the changes marked for its next commit) moves with it.
The workweave is then listed under the name its marker records. A workweave
moved anywhere else is out of sight of this scan, and its registrations read as
`workweave-directory-gone`.

#### `workweave-directory-gone`

**Warning. Report-only.** Nothing stands at the registered path, which lies in
the directory a workweave of this weave is recorded at, and that directory is
gone too: the workweave was moved where no scan looks, or removed by hand. The
registration holds what the VCS keeps of the checkout (the commit it is on, the
changes marked for its next commit), so `--fix` keeps it, and keeps the
workweave's entry in the index with it. The finding names the workweave.

**What to do:** if the workweave moved, run `rwv doctor --fix` inside it, which
records each checkout where it stands, as for `directory-moved`. If it is gone,
`rwv workweave delete <name> --project <project>` drops its registrations and its
entry.

---

## Clone topology

### `clone-topology`

**Error.** One of the manifest's repos is on disk in a shape that breaks the
bottom tier of the stability stack: the slot at `<weave>/<repo_path>` must be a
canonical store — a full clone — and every workweave checkout
`<workweave>/<repo_path>` must be a linked workspace whose object store
resolves to that canonical store.

All four sub-kinds are invisible to every higher-tier check, which operate on
revisions and content rather than on physical object-store topology — so this
finding is the only thing that reports them. `standalone-in-workweave` is the
one `--fix` repairs: the canonical store is already where it belongs, so the
repair replaces one checkout. The other three move an object store, which is
out of `--fix` scope.

The invariants and the reasoning are in
[Clone topology](../explanation/joints/clone-topology.md).

#### `standalone-in-workweave`

**Auto-fixable.** A workweave's checkout of a member is a full clone instead
of a worktree of the canonical clone at `<weave>/<repo_path>`. Commits made
there land in an object store nothing else reads, and when other workweaves
link into it, the canonical store has in effect migrated into this workweave.
The canonical clone usually reports `disconnected-weave-clone` alongside it,
which clears once this is repaired.

A member slot holding a symlink is not scanned: read through the link it is
the canonical store itself. It is reported as `workweave-drift`.

**What to do:** `rwv doctor --fix`, in text mode, replaces the full clone with
a worktree of the canonical clone on the workweave's `<project>--<workweave>`
branch, born at the commit the clone had checked out. Run inside a workweave it repairs that workweave; run
from the primary, every registered workweave of the projects in scope. It
prints:

```
[fixed] core: replaced the full clone of <repo> in workweave <project>--<name> with a worktree of the canonical clone at <sha> (<n> reflinked, <n> hardlinked, <n> written)
```

The repair removes the full clone, so it refuses, changing nothing, while the
clone holds anything the canonical clone lacks: refs whose commits no ref of
the canonical clone reaches, uncommitted or untracked files, ignored files
(build output and installed dependencies included), or other worktrees linking
into its store. It also refuses when there is no canonical clone to link into,
when the canonical slot is itself a worktree, when the clone's HEAD names no
commit, and when a `.<repo>.rwv-replaced` directory left by an interrupted
repair stands beside the clone. The refusal is one error line, so `--fix` exits
1, followed by every reason that applies, each with what it holds:

```
[error: core-finding] core: clone-topology fix left <project>--<name>/<repo> standing: not replacing the full clone at <path> with a worktree of the canonical clone:
  - it holds ignored files, which no commit records either. Move out any you need; if the rest are regenerable, such as build output or installed dependencies, `git -C <path> clean -X -d -n` lists what would go, and `git -C <path> clean -X -d -f` deletes it:
      node_modules/

Nothing was changed. Re-run `rwv doctor --fix` once each of these is resolved.
```

Land the commits in the canonical clone or delete the refs, commit, move or
delete the files, then re-run `rwv doctor --fix`. rwv never runs the `git
clean` it names. Do not repair by hand with `git worktree add`: a worktree rwv
did not create is not registered in the workweave's index.

**As a refusal.** `rwv workweave delete` refuses with this token when a
checkout in the workweave is itself a canonical store that other worktrees
link into, since deleting it would orphan them; neither discard flag waives
it. The `--fix` refusal above carries the token too. In both cases `rwv doctor`
reports the finding, and this entry is the one `rwv explain
standalone-in-workweave` serves.

#### `disconnected-weave-clone`

**Report-only.** The workspace at `<weave>/<repo_path>` is a full clone, but
one or more of this weave's workweave checkouts of the same repo resolve to a
*different* canonical store. The weave-path clone publishes a separate object
graph nobody syncs to; push and pull become asymmetric and silent.

**What to do:** where the other store is a workweave's full clone, reported as
`standalone-in-workweave` beside this, `rwv doctor --fix` repairs that and this
clears with it. Otherwise fetch any unique commits from the disconnected store
into the canonical clone at `<weave>/<repo_path>`, then recreate or re-link the
affected workweave worktrees so they resolve to `<weave>/<repo_path>`.

#### `wrong-parent-worktree`

**Report-only.** A linked worktree under `.workweaves/<workweave>/<repo_path>`
whose canonical store is not the weave canonical. Commits made there land in a
different object store than the canonical, and merged-checks across the two
answer "no" silently.

**What to do:** fetch any unique commits from that worktree into
`<weave>/<repo_path>`, remove the mislinked worktree directory, and run
`rwv doctor --fix` (or `rwv workweave create <name>`) to re-link it
against the weave canonical store.

#### `weave-clone-is-worktree`

**Report-only.** The weave path itself is a linked worktree of some other
clone — full inversion. There is no canonical store at the manifest slot at
all.

**What to do:** replace `<weave>/<repo_path>` with a full standalone clone (or
move the parent clone's `.git` directory into `<weave>/<repo_path>`) and re-link
any dependent workweave worktrees against it.

### `missing-canonical-clone`

**Warning. Report-only.** A workweave worktree whose canonical clone — the
primary-weave clone it was linked from — is gone from disk. git commands in
the dependent worktree then fail opaquely; this finding names the true root
cause instead of letting the failure be misread as live local edits.

Doctor reports it from the primary weave and from inside the workweave.

**What to do:** run `rwv update --project <project>` at the primary weave to
clone it again, then `rwv doctor --fix` there, which relinks every workweave
checkout of it as [`dangling-link`](#dangling-link) describes, keeping their
files. A branch that lived only in the deleted clone is gone with it, so the
workweave's branch is created again at the pinned revision. There is no
auto-fix for the clone itself because doctor never clones — network access
stays behind explicit verbs.

A verb that needs the canonical clone to give a workweave its worktree
refuses with this token, and `rwv doctor --fix` gives the same cause as a
reason it cannot replace a full clone standing in a workweave. Both name
`rwv update --project <project>`, `<project>` being the workweave's own. Run it
at the primary weave: a bare `rwv update` there updates only the active
project's members, so for a workweave of any other project it materializes
nothing. Then re-run the verb, or `--continue` the operation that parked on
it.

### `uninitialized-submodule`

A member checkout in a workweave has a `.gitmodules` file, but one or more of
its listed submodule paths are empty. A member whose manifest entry says
`recurse-submodules = false` is not reported, nor is a submodule git's own
`submodule.<name>.update = none` declines. Detection reads only local data.
Doctor reports it from the primary weave for every workweave, and from inside
a workweave for that one. See [Submodules](./formats.md#submodules) for how
rwv checks submodules out.

#### `fetched`

**Warning. Auto-fixable.** The canonical clone's copy of each listed submodule
holds the commit the member records.

**What to do:** `rwv doctor --fix` checks them out from those copies. No
network is involved.

#### `unfetched`

**Warning. Report-only.** The canonical clone holds no copy of each listed
submodule with the commit the member records.

**What to do:** run the `rwv update --repo <path>` the message names in that
workweave. It fetches the commits into the canonical clone's copies, without
moving a submodule already checked out there, and checks them out in the
workweave. `--fix` itself never reaches the network, so it leaves this
finding as it is.

### `provenance`

#### `origin-url-mismatch`

**Warning. Report-only.** The clone's `origin` names a different remote from
the manifest's `url`, registry shorthand read as the URL it resolves to. A
`url` that resolves to no remote is not compared. Until it is reconciled, a
push may publish to the wrong remote.

**What to do:** decide which is right and change the other. `--fix` has no arm
here because that decision is yours: a `reference`-role repo may point at a
different remote on purpose — a local mirror, say — and the message says so
when the role is `reference`.

#### `relative-remote`

**Warning. Report-only.** The clone records its remote as a relative path, such
as `../up.git`. git reads a relative path from the top of whichever checkout it
runs in, and every workweave checkout of a member shares the canonical clone's
remote, so the one path names a different remote from each checkout: a push
from a workweave goes somewhere other than a push from the primary weave. The
path is not compared with the manifest's `url`.

**What to do:** give the remote an absolute URL. The message names the command,
with the URL the manifest's `url` resolves to:
`git -C <repo> remote set-url origin <url>`. `rwv member add --adopt` refuses such a
clone with `no-remote-url`.

#### `unresolvable-url`

**Error. Report-only.** The manifest's `url` is neither a URL nor registry
shorthand, such as a plain path, so it names no remote. Every clone of the
member refuses with `unresolvable-repo-source`: `rwv clone` or `rwv update` on
another machine, and `rwv sync` into a weave without it. Doctor reports it whether or not a clone
is on disk; when none is, this is the only finding for the entry, with no
`dangling-reference` beside it.

**What to do:** write the `url` in `rwv.toml` as a URL. The message names the
`file://` spelling of an absolute path, which is how a local remote is written.
`--fix` has no arm here because `rwv.toml` is yours to edit.

#### `placement-disagreement`

**Warning. Report-only.** The manifest key disagrees with `placement(url)` —
the path the entry's own URL derives to under the canonical
`{registry}/{owner}/{repo}` layout. `--fix` has no arm here for the same
reason `origin-url-mismatch` has none: converging rewrites a key in the
hand-authored `rwv.toml` and may move a checkout, and whether the recorded
path is the wrong side is the operator's call. This is a different comparison
from `origin-url-mismatch` — that one compares the manifest URL against the
clone's `origin` URL; this one compares the manifest *path* against
`placement` of the manifest URL — so an entry can trip either, both, or
neither independently.

The URL it takes is the entry's URL on the project's default branch, the
one the member was added with, so a mirror URL that another branch's entry
carries (`rwv member set-url` on that branch) raises no finding. A member the
default branch does not list is compared by the URL it has where it is read.

`reference`-role and `fork`-role entries are exempt: a `reference` mirror URL
under the path of the upstream it mirrors, or a `fork` keyed on either its
fork coordinates or its upstream coordinates (per [Roles](./roles.md) §`fork`),
is an expected layout and raises no finding.

**What to do:** the message names both paths. Move the checkout to the
derived path unless it is already there, re-key the entry in `rwv.toml` from
the recorded path to the derived one, run `rwv lock`, which drops the old
key's entry from `rwv.lock`, and give the new key its pin with `rwv member pin
<path> <rev> --commit`. Moving the checkout alone leaves this finding and adds a
[`dangling-reference`](#dangling-reference) at the recorded path; re-keying
alone, with the checkout elsewhere, leaves a `dangling-reference` at the
derived path.

A workweave's checkout of the member is a worktree of the canonical clone, so
moving the canonical clone strands it. In the primary weave, when workweaves
hold worktrees of the clone, the message names each one and puts landing and
retiring it first: commit or set aside its edits and run `rwv land --retire`
from it, which lands its commits on the primary weave's branch. The `--json`
record's `converge` carries those workweaves as `{"here": {"retire_first":
[...]}}`. In a workweave, the finding names no move: `converge` is
`"from-primary"`, and the steps run in the primary weave.

---

## Branches

### `alignment`

A member's checkout, or the project repo's, is not where the checked-out project branch says it should be: on a branch other than the expected one, detached where it should be attached, or attached where it should be detached; or the workspace's record of what it last aligned for no longer matches the branch.

**Why.** The checked-out project branch's record, its member entries and its lock say which branch each member sits on, and where a member's branch is checked out in another workspace, or does not reach the commit the lock pins, that it sits detached at the pin. A checkout anywhere else builds something other than what the branch describes, and a lock written from it would record that. `rwv update` moves the members it can without leaving a commit on no ref, and reports the rest with the commits that would be left. A member two projects list has one checkout: activating a project leaves it where it is, so `rwv activate` refuses one that is not aligned for the incoming project and names `--update`, which aligns it first.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: alignment <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `aligned` | the checkout is in its expected state; the state is printed only when it carries a fact: `checkout`, the member's branch being checked out in another workspace (named), so the member sits detached at its pin and nothing moves it | `rwv status` reports it with the fact; `rwv update` reports the fact for each member it detaches |
| `on branch` | the checkout is on a branch other than the expected one, and no commit it holds would be left on no other branch or backup if it moved off | `rwv update` reports it for a checkout it did not move, one with uncommitted changes or an operation in progress; `rwv status` reports it; `rwv activate` refuses it for a member shared with the project the weave root presents, and `rwv update` and `rwv pull` for one of a project the weave root does not present |
| `on branch with stray commits` | the checkout is on a branch other than the expected one, which is neither a declared branch nor named after a recorded weave branch, and holds the commits listed that no other branch, remote-tracking branch or backup reaches | `rwv update` reports it and leaves the checkout; `rwv status` reports it; `rwv activate`, `rwv update` and `rwv pull` refuse it as `on branch` is refused |
| `detached` | the checkout is detached where it is not expected to be, and no commit reaches it that a branch lacks | `rwv update` reports it for a checkout it did not move; `rwv status` reports it; `rwv activate`, `rwv update` and `rwv pull` refuse it as `on branch` is refused |
| `detached with unpushed commits` | the checkout is detached and holds the commits listed, which no branch or remote-tracking branch reaches | `rwv update` reports it and leaves the checkout; `rwv status` reports it; `rwv activate`, `rwv update` and `rwv pull` refuse it as `on branch` is refused |
| `unaligned` | the lock has no entry for the member and its expected branch is found nowhere, so it stays where it is and its pin stays out of the lock | `rwv update` and `rwv status` report it |
| `project moved` | the project repo is on another branch than the one the workspace last aligned its members for, which still exists, named in the subject | nothing refuses at the project level; `rwv status` reports it as a fact, and each member not aligned for the checked-out branch reports its own line |
| `project unaligned` | the workspace has no record of the branch it last aligned its members for, or that branch is gone | `rwv status` reports it; `rwv update` records it |
| `project detached` | the project repo's HEAD is on no branch and the workspace was not made detached; the commits it holds that no branch reaches are listed | `rwv update`; `rwv branch create` with no spelling, `rwv branch rename` and `rwv workweave create`, which need a checked-out branch; `rwv status` reports it |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `on branch` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `on branch`, for `rwv activate` | C6: align members for the checked-out branch | `rwv activate <project> --update` | here | moves only what alignment moves |
| `on branch with stray commits` | C15: copy these commits onto `<b>` | VCS action (git: `git -C <b-checkout> cherry-pick -x <ids>`); then `rwv lock --commit` | where `<b>` is checked out | a copy adds only |
| `on branch with stray commits` | C14: keep this work on a branch, then return to the expected branch | VCS action (git: `git -C <checkout> branch <name> <sha>`); then `rwv update` | any workspace | the branch holds the work before anything moves |
| `detached` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `detached`, for `rwv activate` | C6: align members for the checked-out branch | `rwv activate <project> --update` | here | moves only what alignment moves |
| `detached with unpushed commits` | C14: keep this work on a branch, then return to the expected branch | VCS action (git: `git -C <checkout> branch <name> <sha>`); then `rwv update` | any workspace | the branch holds the work before anything moves |
| `detached with unpushed commits` | C15: copy these commits onto `<b>` | VCS action (git: `git -C <b-checkout> cherry-pick -x <ids>`); then `rwv lock --commit` | where `<b>` is checked out | a copy adds only |
| `unaligned` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `unaligned` | C18: start `<m>`'s `<b>` at a commit you choose | VCS action (git: `git -C <checkout> branch <name> <sha>`); then `rwv update` | any workspace | creates only |
| `project moved` | C30: return the project repo to a branch, or start one carrying the edits | `rwv switch <b>` | here | moves only what alignment moves |
| `project moved` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `project unaligned` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `project detached` | C14: keep this work on a branch, then return to the expected branch | VCS action (git: `git -C <checkout> branch <name> <sha>`); then `rwv switch <b>` | here | the branch holds the work before anything moves |
| `project detached` | C30: return the project repo to a branch, or start one carrying the edits | `rwv switch <b>` | here | moves only what alignment moves |
| `project detached` | C30: return the project repo to a branch, or start one carrying the edits | `rwv branch create <W> --target <T>`; `rwv branch create <W> --declared` | here | moves only what alignment moves |

**Where this sits.** [Alignment](../explanation/joints/alignment.md); [Clone topology](../explanation/joints/clone-topology.md); [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md); [Shared refs and drift](../explanation/joints/shared-refs-drift.md).

#### `on-branch`

**Warning. Report-only.** The checkout is on a branch the workspace does not expect, and moving it to the expected state leaves every commit it holds on some other ref. **What to do:** `rwv update`, which moves it.

#### `on-branch-with-stray-commits`

**Warning. Report-only.** The checkout is on a branch the workspace does not expect, and it holds commits that no other branch, remote-tracking branch or backup reaches, listed. `rwv update` leaves it. **What to do:** copy the commits onto the expected branch, or keep them on a branch of their own and run `rwv update`; the message names the command.

#### `detached`

**Warning. Report-only.** The checkout is detached where the workspace expects it on a branch, or at another commit than its pin, and no commit reaches it that a branch lacks. **What to do:** `rwv update`, which returns it.

#### `detached-with-unpushed-commits`

**Warning. Report-only.** The checkout is detached and holds commits no branch or remote-tracking branch reaches, listed. `rwv update` leaves it. **What to do:** keep the commits on a branch, then run `rwv update`; the message names the command.

#### `unaligned`

**Warning. Report-only.** The lock has no entry for the member and its expected branch exists nowhere, so it stays where it is. **What to do:** pin it at a commit you choose, or start its branch at one.

#### `project-detached`

**Warning. Report-only.** The project repo's HEAD is on no branch, and the workspace was not made detached, so no record says how its members should sit. The commits it holds that no branch reaches are listed. **What to do:** keep them on a branch if there are any, then switch the project repo back to a branch.

## Working state and in-flight operations

### `record-drift`

What the VCS records for a repo's paths does not match its HEAD tree. Where the
branch the checkout is on was moved forward from outside it (for git, by
`git update-ref` run in another worktree of the same store), the record still
holds the old tip's content (for git, the index entries). Read path by path, as
[Shared-refs drift](../explanation/joints/shared-refs-drift.md) describes; one
checkout can carry both sub-kinds.

#### `safe-to-fix`

**Warning. Auto-fixable.** The checkout's branch moved forward under it, and
at least one path the move changed still has the old tip's content recorded, with
a file holding the old tip's content or already HEAD's.

**What to do:** `rwv doctor --fix` gives those paths HEAD's records and
files, and reports each one. It reads the checkout again just before writing,
and the VCS refuses where a path changed after it was read; then nothing is
written, the paths are named as drift left in place with the reason, and the
fix reports an error. Running `rwv doctor --fix` again reads them afresh;
where the reason is unresolved conflicts in the checkout, resolve those first.

#### `live-recorded`

**Warning. Report-only.** At least one path's record differs from HEAD for a
reason the branch's move does not account for: a change of yours recorded for the
next commit (for git, staged), a deletion or revert included, or a path both the move and you changed.
It can also be drift rwv cannot show: an old commit's content in a checkout
whose history records no move under it, or one that already took part of the
move. `--fix` does not touch it.

**What to do:** when the checkout also has `safe-to-fix` drift, run
`rwv doctor --fix` first: committing or setting aside drift records or sets aside
the move's reverse with your change. Then commit it if it is your change, or
set it aside (for git, `git -C <checkout> stash push`) if it is not: nothing is committed
and nothing is lost. Then re-run.

Where a path both the move and you changed is among them, `--fix` writes
nothing to the checkout and refuses it as `edits` `moved and edited`:
`rwv doctor --fix --merge-edited-drift` keeps the working copy and the VCS's record
of it as a backup and has the VCS merge your edit onto the moved tip, and
`rwv doctor --fix --set-aside-edited-drift` has the VCS set your edit aside against
the tip your files held, gives the file the moved tip's content, and prints
the VCS action that merges it back (for git, `git stash apply`).

### `working-tree-drift`

A repo's working-tree files do not match its HEAD tree. Read path by path, as
for `record-drift`.

#### `safe-to-fix`

**Warning. Auto-fixable.** The checkout's branch moved forward under it, and
at least one file the move changed, with its record, still holds the old
tip's content, which a commit reachable from HEAD keeps.

**What to do:** `rwv doctor --fix` gives those files HEAD's content, and
reports each one. What it replaces is the old tip's content, which that tip
holds. A return to the old tip recorded for the next commit, made after a refresh by hand, cannot be
told from this drift, and is refreshed the same way; commit such a return
first to keep it.

#### `live-edits`

**Warning. Report-only.** At least one file differs from HEAD for a reason the
branch's move does not account for: your edit, whatever commit already holds
its content (a deletion, a file returned to an earlier version, a copy, a mode
change), or a file both the move and you changed. It can also be drift rwv
cannot show, as for `live-recorded`. `--fix` does not touch it.

**What to do:** when the checkout also has `safe-to-fix` drift, run
`rwv doctor --fix` first: committing or setting aside drift records or sets aside
the move's reverse with your edits. Then commit them if they are your changes,
or set them aside (for git, `git -C <checkout> stash push`) if they are not: nothing is
committed and nothing is lost. Then re-run.

Where a file both the move and you changed is among them, `--fix` writes
nothing to the checkout and refuses it as `edits` `moved and edited`:
`rwv doctor --fix --merge-edited-drift` keeps the working copy and the VCS's record
of it as a backup and has the VCS merge your edit onto the moved tip, and
`rwv doctor --fix --set-aside-edited-drift` has the VCS set your edit aside against
the tip your files held, gives the file the moved tip's content, and prints
the VCS action that merges it back (for git, `git stash apply`).

### `orphaned-savepoint`

The savepoints filed under a `refs/rwv/pre-op/<op-id>` key whose op id matches
no `.rwv-op` file in this workspace tree: a checkout's, and one per branch the
operation moved without a checkout. The finding judges every tip filed under
the key.

#### `redundant`

**Warning. Auto-fixable.** A branch reaches every tip filed under the key.

**What to do:** `rwv doctor --fix` drops the key's savepoints, first keeping as
a backup any tip that no branch reaches by the time it runs. No commit is lost.

#### `live`

**Warning. Auto-fixable.** Some tip filed under the key is reached by no
branch: a savepoint is the last pointer to it, the same reason the reflog is
never cut. A savepoint an earlier release kept after its operation ended (a
tombstone) is one.

**What to do:** `rwv doctor --fix` keeps each such tip as a backup, then drops
the key's savepoints: `rwv backup list` shows what it holds and `rwv backup drop` drops it
when it is not wanted. Nothing reads a savepoint as a backup before `--fix`
converts it, and no commit is lost in the conversion.

A savepoint that belongs to an operation still in flight is never reported as
orphaned: that covers both of a `land`'s savepoints, the one in the
workspace it replays in and the one in the workspace it advances, for as long
as the operation's record exists.

### `unrecognized-savepoint-ref`

**Warning. Report-only.** A ref below `refs/rwv/` in no namespace rwv writes
(`pre-op`, `backup`), or one under `refs/rwv/pre-op/` that no op id names.
rwv did not file it: it is a hand-made ref, a damaged name, or one written by
a different rwv. What it points at is unknown, so `--fix` never drops it. The
finding carries the `ref_name` and the holding repo's `absolute_path`. Refs
live in the store a clone shares with its workweave checkouts, so one ref is
one finding, however many checkouts see it.

**What to do:** inspect it, and delete it by hand with
`git -C <absolute_path> update-ref -d <ref_name>` once nothing needs it; the
text output prints that command filled in.

### `stale-op-state`

**Warning. Report-only.** A `.rwv-op` file is present at a weave root.
The finding reports the file's age, its path, and the `verb` that started the
op (`sync`, `land`, `pull`, `update-remote` or `cherry-pick`). A file whose op id, verb, project, workspaces and
start time parse is reported, and its savepoints held live, even when the
rest of it does not parse, since `rwv abort` can still restore from them. A
file whose op id does not parse, or that another release's record schema
wrote, is [`unreadable-op-record`](#unreadable-op-record) instead.

`--fix` leaves it: another terminal may be
mid-conflict-resolution, and rwv has no daemon that could know which workspace
the op record legitimately belongs to.

**What to do:** inspect it, then either resume with the `--continue` the
finding names — an op is only resumable under the verb that started it — or
`rwv abort` to roll back.

### `unreadable-op-record`

**Warning. Report-only.** A `.rwv-op` file this rwv does not read whole: a
field missing, one it does not know, a value of another type or variant, or a
record schema this one does not read. Nothing can resume the operation or restore from its
savepoints, and every verb that reads the record, run in its workspace or in a
workspace the operation leases, refuses with this kind and the same message.
`rwv status` reports it instead of refusing.

Without the record, which savepoints are the operation's cannot be told, so
while one stands no savepoint whose operation it may name is reported as
`orphaned-savepoint`, and `--fix` drops none of them.

**What to do:** if another release wrote it, that release resumes or aborts
the operation. Otherwise `rwv abort --discard-unreadable-record`, run in the
workspace the message names or in one the operation leases, ends it: it
removes the record, the leases it holds on other workspaces and its landing
claims, and moves nothing back, since only the record says which tips are the
operation's own. It lists each repo it leaves stopped in a VCS operation and
each savepoint it leaves; once no record claims a savepoint, `rwv doctor`
reports it as `orphaned-savepoint`, and `rwv doctor --fix` keeps each tip it
holds that no branch reaches as a backup before dropping it.

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

Verbs that read the op record (`rwv sync`, `rwv land`, `rwv abort`, `rwv
status`) refuse with this kind when they meet such a lease, naming the
workspace that holds it: with no owner record, nothing can resume or abort the
operation the lease names.

**What to do:** `rwv doctor --fix`, run in the workspace holding the lease,
removes it.

### `claim-owner-unreachable`

**Warning. Auto-fixable.** A target's landing claim names an operation whose
owner workspace is not here to read: the directory does not exist from where
`rwv doctor` runs. That is what a deleted workweave looks like, and also what
a workweave on another mount, or outside a sandbox the verb runs in, looks
like. Only a workspace that is here and records no such operation proves the
operation gone, so no landing takes this claim over: a landing into the target
waits on it and then refuses with this kind, naming the operation and its
workspace.

**What to do:** if the operation still runs where its workspace is, end it
there with its `--continue` or `rwv abort`. If it is gone, `rwv doctor --fix`
releases the claim, and the next landing into the target runs.

### `claim-unreadable`

**Warning. Report-only by default.** A target's landing claim, or a takeover
guard beside one, holds what this rwv does not read: it names no `format`
marker, names another release's format, or names this rwv's and holds
something else, so it is corrupt or another release wrote it. The finding names the file and shows what it holds. Whether the operation it stands for
still runs cannot be told, so no landing takes it over: a landing into the
target waits on it and then refuses with this kind. Bare `--fix` reports it
and changes nothing.

**What to do:** if a landing of another rwv release holds it, end that landing
with the release that started it, by its `--continue` or `rwv abort`. If none
does, the refusal a landing prints names the one command that releases the
claim on its target and no other:
`rwv doctor --fix --project <project> --release-unreadable-claims-on <target>`.
`rwv doctor --fix --release-unreadable-claims` releases every claim and guard
this run reports here, in every project the run covers. Either gives up the
claim a live landing of another release would hold, which is the consent the
flag stands for, and the next landing into the target runs. A file that holds
something else by the time the release reaches it, readable again or not, is
left for the next report. Both flags are refused without `--fix`.

### `claim-holder-unreachable`

**Warning. Report-only by default.** A fetch claim (`.rwv-fetch.lock` in an
object store), a workweave-index claim, or an owned-digest claim file on a
filesystem with no file locks records a holder on another host, or one this
host cannot name. Where the filesystem has file locks the lock shows whether the
holder runs, and this finding never arises. Here nothing on this host can show
whether the holder ended, so no run takes the claim over: a fetch, a pull or a
workweave edit that needs it waits and then refuses with `state-claim-held`.
The finding names the file and the pid and host the claim records. Bare `--fix`
reports it and changes nothing.

**What to do:** run the verb again from the host the claim names; a holder that
has ended there releases it. If that host is gone,
`rwv doctor --fix --release-unreachable-claims` releases every claim this run
reports here. It gives up the claim a holder that still runs would have, which
is the consent the flag stands for. A claim that records something else by the
time the release reaches it is left for the next report. The flag is refused
without `--fix`.

### `claim-unopenable`

**Warning. Report-only.** A target's landing claim, or a takeover guard beside
one, cannot be read at all: a file this user may not read, or something at its
path that is not a file. The finding names the path and the error. Whose claim
it is cannot be told, and its contents cannot be compared before a removal, so
nothing in rwv releases it: a landing into the target waits on it and then
refuses with this kind.

**What to do:** if it is a file this user may not read, a landing run as
another user holds it: end that landing as that user, or make the file
readable to this one, and `rwv doctor` then reports what it holds. If what
stands there is not a file, rwv did not put it there: remove it.

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
`severity`, the operator-facing `message`, and `safe_to_fix` — whether `--fix`
acts on that finding. It is `false` on every finding of a kind the Summary
index marks `Never`, and on a file region you hold the pen on, which `--fix`
reports and never overwrites.

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
it. The authoring verbs (`rwv member add`, `rwv member remove`,
`rwv update --remote`, `rwv member set-role`) run the same generation;
`rwv activate`, `rwv materialize` and `rwv workweave create` author nothing and
report the file missing.

### `managed-file-drift`

Owned content on disk differs from what `rwv doctor --fix` or an authoring verb
would write, or the owning tool can no longer read it.

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

Always `safe_to_fix: false`: taking over an unmarked region means overwriting
content rwv never wrote and has no record of. `--fix` reports it and moves on.

**What to do:** nothing, if the content is what you want — rwv keeps reporting
rather than acting. The exception is a finding whose message says the next
authoring verb writes the keys: none of them is on disk for that verb to defer
to, so it writes them. To hand the region over, do what the message says: cut
it over by hand, or write the ownership marker it names. The spelling differs
by file format, which is why the finding names it rather than this page.

### `surfacing`

A weave-root symlink out of step with what `rwv activate`'s
surfacing step would put there — the finding and `--fix` read the same
predicate, so they agree on what "in step" means. Most of what it reports
`--fix` (or `rwv activate` / `rwv materialize`, which run the same step)
resolves outright, because the divergence is exactly what a fresh pass already
corrects.

A lock file a tool wrote at its weave-root path instead of through the link is
the exception to report-only real content: `--fix` takes it into the project
and restores the link, unless the project's copy differs and is neither
committed as it stands nor the generation rwv recorded, in which case both
copies are named and neither is touched.

Real content occupying a link's path stays report-only, and so do a link at a
declared name that resolves somewhere surfacing never points and a link
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
reported as failed. The other verbs stop on the same block, each naming it and
the field: `rwv materialize` refuses before its first step, changing nothing;
`rwv member add`, `rwv member remove`, `rwv member set-role` and
`rwv update --remote` refuse and leave `rwv.toml` unchanged; and `rwv activate`
withholds its install hooks.

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
Doctor is the standing observation arm for it, and `rwv update --remote` reports
the same finding at the moment it creates one. Neither gates: nothing refuses on
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

An entry that records no inputs reads as stale: rwv accepted those bytes
without recording what produced them, so it cannot verify they still follow
from current inputs, and the next generation rewrites the entry in the attested
shape.

On `--json` this finding also appears in the `advisories` array, in the same
`{kind, remedy, inputs}` shape `rwv sync --json` emits, with `kind` spelled
`derived-state-stale`, so an agent branches on one vocabulary rather than two.
`inputs` names the paths that moved, and is empty exactly when the entry
records no inputs to compare.

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
