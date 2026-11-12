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
`dead-op-lease`, `index-drift`, `legacy-workweave-index`, `member-permissions`,
`missing-replay-exclusion`, `orphaned-savepoint`, `pin`, `provenance`,
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
| [`missing-replay-exclusion`](#missing-replay-exclusion) (`absent`, `legacy-spelling`, `legacy-alongside-current`) | Warning | Auto-fixable |
| [`replay-exclusion-unreadable`](#replay-exclusion-unreadable) | Warning | Report-only |
| [`missing-merge-driver-config`](#missing-merge-driver-config) | Warning | Auto-fixable |
| [`merge-driver-config-unreadable`](#merge-driver-config-unreadable) | Warning | Report-only |
| [`phantom-merge-driver`](#phantom-merge-driver) | Warning | Report-only |
| [`dangling-active-project`](#dangling-active-project) | Error | Auto-fixable |
| [`unreadable-active-project`](#unreadable-active-project) | Error | Auto-fixable |
| [`weave-root-identity-conflict`](#weave-root-identity-conflict) / `registered-workweave` | Error | Auto-fixable |
| [`weave-root-identity-conflict`](#weave-root-identity-conflict) / `marker-unverifiable`, `unwitnessed` | Error | Report-only |
| [`legacy-workweave-marker`](#legacy-workweave-marker) | Warning | Auto-fixable |
| [`legacy-workweave-index`](#legacy-workweave-index) (`before-receipts`, `old-path`) | Warning | Auto-fixable |
| [`unreadable-owned-state`](#unreadable-owned-state) | Warning | Report-only |
| [`unreadable-workweave-index`](#unreadable-workweave-index) | Error | Report-only |
| [`workweave-tree-integrity`](#workweave-tree-integrity) / `dangling-parent`, `unregistered-workweave`, `stale-registry-entry` | Warning | Auto-fixable |
| [`workweave-tree-integrity`](#workweave-tree-integrity) / `parent-chain-anomaly`, `unregistered-dir`, `foreign-primary`, `foreign-primary-other-workspace`, `unreadable-marker`, `misnamed-dir`, `nested-workweave-dir` | Warning | Report-only |
| [`workweave-drift`](#workweave-drift) / `missing`, `symlink`, `dangling-link` | Error | Auto-fixable |
| [`workweave-drift`](#workweave-drift) / `missing-unfetched`, `symlink-unfetched` | Error | Report-only |
| [`workweave-drift`](#workweave-drift) / `dangling-link-unfetched` | Error | Report-only by default (`--abandon-lost-pins`) |
| [`workweave-drift`](#workweave-drift) / `extra` | Warning | Report-only |
| [`member-permissions`](#member-permissions) (`writable-reference`, `read-only-member`) | Warning | Auto-fixable |
| [`role-disagreement`](#role-disagreement) | Warning | Report-only |
| [`stale-worktree-registration`](#stale-worktree-registration) / `directory-gone` | Warning | Auto-fixable |
| [`stale-worktree-registration`](#stale-worktree-registration) / `directory-stands` | Warning | Report-only |
| [`clone-topology`](#clone-topology) / `standalone-in-workweave` | Error | Auto-fixable |
| [`clone-topology`](#clone-topology) / `disconnected-weave-clone`, `wrong-parent-worktree`, `weave-clone-is-worktree` | Error | Report-only |
| [`missing-canonical-clone`](#missing-canonical-clone) | Warning | Report-only |
| [`uninitialized-submodule`](#uninitialized-submodule) / `fetched` | Warning | Auto-fixable |
| [`uninitialized-submodule`](#uninitialized-submodule) / `unfetched` | Warning | Report-only |
| [`provenance`](#provenance) / `origin-url-mismatch`, `relative-remote`, `placement-disagreement` | Warning | Report-only |
| [`provenance`](#provenance) / `unresolvable-url`, `lock-sha-unreachable` | Error | Report-only |
| [`alignment`](#alignment) / `on-branch`, `on-branch-with-stray-commits`, `detached`, `detached-with-unpushed-commits`, `unaligned`, `project-detached` | Warning | Report-only |
| [`index-drift`](#index-drift) / `safe-to-fix` | Warning | Auto-fixable |
| [`index-drift`](#index-drift) / `live-staged` | Warning | Report-only |
| [`working-tree-drift`](#working-tree-drift) / `safe-to-fix` | Warning | Auto-fixable |
| [`working-tree-drift`](#working-tree-drift) / `live-edits` | Warning | Report-only |
| [`orphaned-savepoint`](#orphaned-savepoint) / `redundant` | Warning | Auto-fixable |
| [`orphaned-savepoint`](#orphaned-savepoint) / `live` | Warning | Auto-fixable |
| [`unrecognized-savepoint-ref`](#unrecognized-savepoint-ref) | Warning | Report-only |
| [`stale-op-state`](#stale-op-state) | Warning | Report-only |
| [`dead-op-lease`](#dead-op-lease) (`owner-record-absent`, `owner-op-id-mismatch`) | Warning | Auto-fixable |
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
leaves this one standing. Or remove the directory yourself. Note that without
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
| `malformed` | the project's `rwv.toml` or `rwv.lock` does not parse, listed with the file that failed and the parser's error | `rwv sync` and `rwv sync-to`, when the project does not load after the project repo is replayed; `rwv doctor` reports it for every project |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `here` | rewrite the keys an earlier release wrote, committing only that | `rwv doctor --fix` | here | commits only the rewrite; other edits stay uncommitted |

**Where this sits.** [Formats](./formats.md) states the member-entry keys; [Upgrade rwv](../how-to/upgrade-rwv.md) walks through the rewrite.

#### `pre-schema`

**Error. Auto-fixable.** The project's `rwv.toml` names member-entry keys an earlier release wrote: `version` where `branch` is now expected, or `submodules` where `recurse-submodules` is. Every verb that reads the manifest at the checked-out tip refuses with this kind, and `rwv explain unparseable-project` is its route line.

**What to do:** `rwv doctor --fix`, run in the project's checkout. It renames the keys in place — `version` to `branch`, `submodules = "skip"` to `recurse-submodules = false`, and drops `submodules = "local"`, which is what an absent `recurse-submodules` means — keeping the file's comments and key order. It writes the rename into the working file and into the project's HEAD commit and commits only that, in one commit: any other edit to `rwv.toml`, and any edit to another file, stays uncommitted where it was. It declines, changing nothing, while another file is staged or a rebase, merge or cherry-pick is in progress in the project repo, and says so; run it again once that is dealt with. A value of `submodules` other than `"local"` or `"skip"`, or an entry that names both a key and its replacement, is not guessed at: the repair names it and leaves the file.

A commit other than the checked-out one that names the old keys is not reported and is not refused. It declares nothing to the verbs that read it, and its `rwv.lock` is read as any lock is.

#### `malformed`

**Error. Report-only.** Any other reason the project did not load. The message names which file failed and carries that file's remedy — the manifest is yours to edit, at the line the parser names; the lock is restored from version control, or written afresh by `rwv lock`, which then pins nothing until `rwv member pin` or `rwv update --remote` gives each member its pin. A repository entry with no `role`, an unknown top-level key, an unknown `[integrations.<name>]` table, an unknown key in one, and an `enabled` that is not `true` or `false` are all parse failures of this kind, each naming the line and what was expected. `rwv sync` and `rwv sync-to` refuse with it when the project does not load after the project repo is replayed, and `rwv abort` rolls that operation back.

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
| `ahead` | the checkout holds commits beyond the commit the lock pins, and each is pushed or pinned by another live lock, which the facts name; the commits are published with `rwv push` only where none is pushed yet | `rwv status` reports it; `rwv push` reports it and goes on |
| `unpinned` | the checkout holds the commits listed beyond the commit the lock pins, which no remote-tracking branch and no live lock reaches | `rwv push`; `rwv status` reports it |
| `none` | the lock has no entry for the member, or there is no lock file; the checkout is on the declared branch at the tip of the remote's branch, or where it was found | `rwv push`; `rwv clone`, which does not refuse: it completes, lists the member and exits with status 3; `rwv lock`, `rwv update` and `rwv status` report it |
| `diverged` | neither the checkout's commit nor the commit the lock pins is an ancestor of the other | `rwv push`; `rwv status` reports it |
| `not on declared branch` | the lock pins a commit that neither the member's declared branch nor its upstream reaches, so the checkout sits detached at the pin | `rwv member pin`, for a commit that neither reaches; `rwv update` and `rwv status` report it |
| `unreachable` | the lock pins a commit that no ref in the clone reaches; the checkout is on the declared branch at the tip of the remote's branch | `rwv push`; `rwv clone`, which does not refuse: it completes, lists the member and exits with status 3; `rwv status` reports it |
| `off remote` | a live lock pins a commit that no remote-tracking branch of the member reaches after the pruning fetch, listed with the locks that pin it. Choices print only in a workspace whose checked-out branch's lock holds the pin: a feature's offers publishing it, a declared branch's offers pinning another commit; elsewhere the state is a fact and offers waiting | nothing refuses; `rwv fetch` and `rwv pull` report it |
| `backwards` | recording the commit the checkout is at would move the pin to one that does not descend from the pinned commit: it is behind the pin or has diverged from it, or the pinned commit is in no ref here | `rwv lock`, for a member `--repo` does not name; `rwv update --remote` |
| `unmerged` | a merge commit in the range being pushed joined two pins of one member that its parents changed, each under one declared branch, and the pushed tip pins neither merged with the other: a commit holding both. The merge and the pins are listed. A member the parents declare differently is listed and not refused | `rwv push` |
| `waived` | a merge in the range being pushed left one member's pins unmerged, and a later lock in the range, the merge's own included, re-pinned the member backward by name to a commit that does not reach the pin the merge left. The merge and the re-pin that waived it are listed | nothing refuses; `rwv push` reports it and goes on |
| `would be unreachable` | a live lock pins a commit that only the backups being dropped reach, listed with the locks that pin it | `rwv backup drop` and `rwv backup prune` |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `behind` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `behind` | C1: take in theirs | `rwv update --remote --repo <m>` | any workspace | fast-forward only |
| `ahead` | C5: record it: the lock, and `rwv.toml` as it stands | `rwv lock --commit` | a workspace with the branch checked out | records only |
| `ahead` | C7: publish it (again) | `rwv push --repo <m>` | here | adds only |
| `unpinned` | C5: record it: the lock, and `rwv.toml` as it stands | `rwv lock --commit` | a workspace with the branch checked out | records only |
| `none` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `none` | C1: take in theirs | `rwv update --remote --repo <m> --commit` | any workspace | fast-forward only |
| `diverged` | C5: record it: the lock, and `rwv.toml` as it stands | `rwv lock --repo <m> --commit` | a workspace with the branch checked out | records only |
| `not on declared branch` | C19: wait until the PR lands or its branch is deleted, then rerun | — | nowhere | nothing changes |
| `not on declared branch` | C15: copy these commits onto `<b>` | VCS action (git: `git -C <checkout> cherry-pick <ids>`); then `rwv lock --commit` | where `<b>` is checked out | a copy adds only |
| `not on declared branch` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `not on declared branch` | C11: declare a branch for it, or keep, restore or port the entry's value | `rwv member set-branch <m> <b>` | a workspace with the branch checked out | re-pins by name, backup kept |
| `unreachable` | C17: choose where it pushes and pulls | VCS action (git: `git -C <checkout> config branch.<b>.pushRemote <r>`, `git -C <checkout> config remote.pushDefault <r>`, `git -C <checkout> config checkout.defaultRemote <r>`, `git -C <checkout> remote add <r> <url>`, `git -C <checkout> branch --unset-upstream`) | any workspace | config only |
| `unreachable` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `unreachable` | C19: wait until the PR lands or its branch is deleted, then rerun | — | nowhere | nothing changes |
| `off remote` | C7: publish it (again) | `rwv push --repo <m>` | here | adds only |
| `off remote` | C27: publish this commit under a branch name you choose | VCS action (git: `git -C <checkout> push <r> <sha>:refs/heads/<name>`) | any workspace | creates or fast-forwards |
| `off remote` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `off remote` | C19: wait until the PR lands or its branch is deleted, then rerun | — | nowhere | nothing changes |
| `backwards` | C5: record it: the lock, and `rwv.toml` as it stands | `rwv lock --repo <m> --commit` | a workspace with the branch checked out | records only |
| `unmerged` | C26: merge `<rev>` in as a merge commit | VCS action (git: `git -C <checkout> merge --no-ff <rev>`) | the checkout | a merge adds only |
| `would be unreachable` | C7: publish it (again) | `rwv push` | here | adds only |
| `would be unreachable` | C27: publish this commit under a branch name you choose | VCS action (git: `git -C <checkout> push <r> <sha>:refs/heads/<name>`) | any workspace | creates or fast-forwards |

**Where this sits.** [Lock as derived](../explanation/joints/lock-as-derived.md); [Reconcile repos](../how-to/reconcile-repos.md); [Destructive operations](../explanation/destructive-operations.md).

#### `none`

**Error. Report-only.** The lock has no entry for a member the manifest lists. Only raised for a project that has a lock file; a project with no lock yet is a separate, unlocked state. **What to do:** `rwv member pin <repo> <rev> --commit` gives the member its pin, or `rwv update --remote --repo <repo> --commit` pins its upstream's tip. `rwv lock` gives a member no first pin.

#### `unreachable`

**Error. Report-only.** The lock pins a commit this clone cannot resolve: a lock written against history that was never fetched here. The entry is kept as a finding rather than dropped, because dropping it would leave the repo with no pin to compare against, which reads as healthy. **What to do:** `rwv fetch` brings in the history that may hold it, or `rwv member pin <repo> <rev>` pins a reachable commit if the lock is the stale side.

#### `behind`

**Error. Report-only.** The checkout is at an ancestor of its pin: the lock records commits the checkout lacks. **What to do:** `rwv update` moves the checkout to the pin, or `rwv lock --repo <repo>` records the checkout's commit by name, moving the pin back.

#### `ahead`

**Warning. Report-only.** The checkout holds commits beyond its pin, and each is pushed or pinned by another project's live lock: the ordinary state between committing and `rwv lock`, so `rwv doctor` warns and exits as it does for any warning. **What to do:** `rwv lock` records the checkout's commit.

#### `unpinned`

**Error. Report-only.** The checkout holds commits beyond its pin that no remote-tracking branch and no live lock reaches, listed: nothing but this checkout holds them. **What to do:** `rwv lock --commit` records them.

#### `diverged`

**Error. Report-only.** Neither the checkout nor its pin is an ancestor of the other. **What to do:** `rwv lock --repo <repo>` records the checkout's commit by name, `rwv member pin <repo> <rev>` pins a commit you choose, or `rwv update` moves the checkout to the pin once its own commits are kept on a branch.

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

A project's workweave index is still at `projects/<name>/.rwv-workweave-index`, where earlier releases kept it, in whatever shape they wrote it.

**Why.** The index of a project's workweaves lives in the project clone's shared directory, where a workweave reaches it from its own project checkout and a clean of the primary's project checkout cannot remove it. A file at the old location is a second index: acting on it would trust placements no current writer validated, and ignoring it would lose the workweaves it records. Every verb that reads the index refuses until the file is converted, and `rwv doctor` reports it instead.

**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: legacy-workweave-index <state>`, with what the state lists beneath it, and refuses unless its row below says it completes.

| State | Means | Refused by |
|---|---|---|
| `old path` | the project's workweave index is at the old path, listed; no verb reads it as the index | every verb that reads the workweave index: `rwv workweave create`, `list`, `delete`, `set-default-create-basedir` and `-w` addressing; `rwv doctor` reports it instead |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `old path` | convert the index and move it into the project clone | `rwv doctor --fix` | here | every recorded workweave is kept; the old file is removed only after the new one is written |

**Where this sits.** [Weave root](../explanation/joints/weave-root.md); [Upgrade rwv](../how-to/upgrade-rwv.md).

#### `before-receipts`

**Warning. Auto-fixable.** The old file has no `receipts` key: an earlier release wrote it before that key was kept. `rwv workweave create`, `list`, `delete`, `set-default-create-basedir` and `-w` addressing refuse until it is converted, and `rwv explain legacy-workweave-index` is their route line.

**What to do:** `rwv doctor --fix`. It writes the index into the project clone's shared directory — `container` becomes `basedir`, each workweave gains the parent its marker records, and `receipts` is not carried over — and then removes the old file.

#### `old-path`

**Warning. Auto-fixable.** The old file carries the `receipts` key, which a release that still kept the index in the project checkout wrote, empty or not. The same verbs refuse, and the same repair converts it.

**What to do:** `rwv doctor --fix`, as above. A copy of the old file committed to the project repo is converted all the same; removing the working file leaves a deletion for you to commit.

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

**Error. Report-only.** A workweave index that exists and does not parse, in
the project clone's shared directory or at the old path. Everything derived
from it is unevaluated: the recorded placements and, for a file at the old
path, whether it can be converted by the `legacy-workweave-index` repair.

While the index cannot be parsed, `unregistered-workweave` and
`stale-registry-entry` are suppressed for the project because attempting to
adopt or prune against an unreadable index would fail on the same parse error.

**What to do:** repair the file, or delete it and re-run `rwv doctor --fix`,
which re-adopts the workweaves on disk into a fresh index. `--fix` never
rewrites it for you: a corrupt index gives no way to tell which entries it
meant to hold.

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

**Scope.** The scan enumerates the containers each project records, so this
finding covers a directory sitting in one of them. A workweave placed outside
them all — `rwv workweave create <name> --dir <path>` — is not among
the candidates: an unrecorded one there produces no finding and `--fix` has
nothing to adopt. Retire the directory and create the workweave again instead,
which does not depend on where it sits.

With no registry entry the name comes from the basename, read against the
marker's own project — so a directory in a container whose basename that
project does not render carries no name to adopt, and reports
[`misnamed-dir`](#misnamed-dir) instead of this.

#### `stale-registry-entry`

**Warning. Auto-fixable.** A registered workweave whose recorded path is not a
valid workweave: missing directory, missing or unreadable marker, a marker that
names another primary or project or a different parent than the entry records.
This covers both "deleted out of band with the registry left behind" and "the
marker was lost or edited".

**What to do:** `rwv doctor --fix`. Where the directory is still a worktree of
the project clone, it writes the marker again from the entry and the worktree
relation (`primary`, `project` and the `parent` the entry keeps). Where nothing
the entry names is left, it removes the entry. A verb that does not destroy
the workweave, run from inside it, restores the marker the same way and says
so; `rwv workweave delete` and `rwv sync-to --retire` refuse instead, naming
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

#### `lock-sha-unreachable`

**Error. Report-only.** The SHA `rwv.lock` pins is absent from the clone's
object store.

**What to do:** run `rwv fetch --repo <repo>` to pull the missing object from
the remote (or `rwv member pin <repo> <rev>` to pin a reachable revision if the
lock is the stale side). `rwv update` does not contact the network for an already-present clone.

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
retiring it first: commit or stash its edits and run `rwv sync-to --retire`
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
| `project detached` | the project repo's HEAD is on no branch and the workspace was not made detached; the commits it holds that no branch reaches are listed | `rwv update`; `rwv status` reports it |

**Choices.** Each state's choices print under `choices:` in this order, with a directory named where a choice does not run where the verb did. A state with no row here prints none.

| State | Choice | Spelled | Where | Keeps work because |
|---|---|---|---|---|
| `on branch` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `on branch`, for `rwv activate` | C6: align members for the checked-out branch | `rwv activate <project> --update` | here | moves only what alignment moves |
| `on branch with stray commits` | C15: copy these commits onto `<b>` | VCS action (git: `git -C <checkout> cherry-pick <ids>`); then `rwv lock --commit` | where `<b>` is checked out | a copy adds only |
| `on branch with stray commits` | C14: keep this work on a branch, then return to the expected branch | VCS action (git: `git -C <checkout> branch <name> <sha>`); then `rwv update` | any workspace | the branch holds the work before anything moves |
| `detached` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `detached`, for `rwv activate` | C6: align members for the checked-out branch | `rwv activate <project> --update` | here | moves only what alignment moves |
| `detached with unpushed commits` | C14: keep this work on a branch, then return to the expected branch | VCS action (git: `git -C <checkout> branch <name> <sha>`); then `rwv update` | any workspace | the branch holds the work before anything moves |
| `detached with unpushed commits` | C15: copy these commits onto `<b>` | VCS action (git: `git -C <checkout> cherry-pick <ids>`); then `rwv lock --commit` | where `<b>` is checked out | a copy adds only |
| `unaligned` | C25: pin it at a commit you choose on its declared branch | `rwv member pin <m> <rev> --commit` | a workspace with the branch checked out | re-pins by name; backup for a detached HEAD |
| `unaligned` | C18: start `<m>`'s `<b>` at a commit you choose | VCS action (git: `git -C <checkout> branch <name> <sha>`); then `rwv update` | any workspace | creates only |
| `project moved` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `project unaligned` | C6: align members for the checked-out branch | `rwv update` | here | moves only what alignment moves |
| `project detached` | C14: keep this work on a branch, then return to the expected branch | VCS action (git: `git -C <checkout> branch <name> <sha>`); then `rwv update` | any workspace | the branch holds the work before anything moves |

**Where this sits.** [Clone topology](../explanation/joints/clone-topology.md); [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md); [Shared refs and drift](../explanation/joints/shared-refs-drift.md).

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

### `index-drift`

A repo's index does not match its HEAD tree — the silent stale index left
when the branch a checkout is on is moved from outside it (by `git update-ref`
run in another worktree of the same store, for one). See
[Shared-refs drift](../explanation/joints/shared-refs-drift.md).

#### `safe-to-fix`

**Warning. Auto-fixable.** The index tree matches the tree of a recent
ancestor commit, so the displaced tree is permanently in the commit graph.

**What to do:** `rwv doctor --fix` resets the index.

#### `live-staged`

**Warning. Report-only.** The index tree is in no recent ancestor. You have
live staged content, and `--fix` must not touch it.

**What to do:** commit or stash it, then re-run.

### `working-tree-drift`

A repo's working-tree files do not match its HEAD tree, after the branch the
checkout is on was moved from outside it, as for `index-drift`.

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

**Warning. Auto-fixable.** The savepoint tip is *not* reachable. The ref is the
last pointer to commits that would otherwise become unreachable — the same
reason the reflog is never cut. A savepoint an earlier release's `rwv sync
--discard-local-commits` kept after its operation ended (a tombstone) is one.

**What to do:** `rwv doctor --fix` keeps the commits as a backup, then drops the
savepoint: `rwv backup list` shows what it holds and `rwv backup drop` drops it
when it is not wanted. Nothing reads a savepoint as a backup before `--fix`
converts it, and no commit is lost in the conversion.

A savepoint that belongs to an operation still in flight is never reported as
orphaned: that covers both of a `sync-to`'s savepoints, the one in the
workspace it replays in and the one in the workspace it advances, for as long
as the operation's record exists.

### `unrecognized-savepoint-ref`

**Warning. Report-only.** A ref under `refs/rwv/pre-op/` that no op id names.
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
op (`sync` or `sync-to`). A file whose op id, verb, project, workspaces and
start time parse is reported, and its savepoints held live, even when the
rest of it does not parse, since `rwv abort` can still restore from them.

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

Verbs that read op-state (`rwv sync`, `rwv sync-to`, `rwv abort`, `rwv
status`) refuse with this kind when they meet such a lease, naming the
workspace that holds it: with no owner record, nothing can resume or abort the
operation the lease names.

**What to do:** `rwv doctor --fix`, run in the workspace holding the lease,
removes it.

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
