# rwv land

## Purpose

Land the checked-out feature into the branch its record lands into (its
target): the work is composed in this workspace, then the target and each
member's receiving branch fast-forward where the target is checked out, or
move as refs where nothing has it checked out. The
target is the record's, or `--target <T>`'s for one run; the record is
unchanged. A declared branch lands nowhere: it refuses as `target`
`no target`, naming `rwv branch set-mode <W> feature --target <T>`.

Every refusal is judged before anything moves, and all of them are reported
together, the source's own (its edits, its alignment, its lock) included when
nothing could be landed into. Then, under an op record:

1. **Compose (replay).** Each `owned` or `fork` member the target lists that
   holds commits its **receiving branch** lacks is moved: composed onto the tip
   of that branch, its branch named after
   the target when the target is itself a feature, else the branch the merged
   entries declare, read as the newer of that branch and its upstream. A
   member holding nothing that branch lacks is not moved and keeps the
   target's pin, unless the feature changed that pin, which the relock then
   takes. Commits a moved member's receiving branch holds beyond the target's
   pin, and commits the new pin of a member it does not move holds beyond
   it, are listed before composing: the landing pins them into the target.
   Under `--json` each is in `reports` as `beyond-pin`, and a member not
   moved has the outcome `no-op`.
   A member whose work that branch
   lacks lands as the target's `history` setting says, read from the target's
   own `rwv.toml` (the member's entry, else the top level): under `linear` its
   work is rebased onto the branch's tip after a `rebased` backup; under
   `merge` a merge commit joins them, the receiving branch's tip as first
   parent; under `ff-only` it refuses as `history` `not fast-forward`, naming
   `rwv sync`, where the rebase happens. A member whose branch already holds
   the receiving tip on its first-parent chain needs no setting: the
   receiving branch fast-forwards. One holding it only through a merge's
   second parent (after `rwv sync`, or `rwv sync --from`) lands by a merge
   commit, first parent the receiving tip, under every setting. A range a rebase
   would replay that holds a merge commit is never rebased: it lands by a
   merge commit instead, reported `merged`. The project branch is composed
   with the target's tip, again the newer of the target and its upstream, the
   same way by the target's top-level `history`. A target or receiving branch
   behind its upstream is so taken in, fast-forward only, and the
   landing reports what it took in of the target's upstream: its commits,
   each pin they changed (`re-pinned` where the new pin does not descend
   from the old) and each member entry they changed (`member-changed`). Each
   pin of the target's lock, or of its upstream's, that this clone lacks
   refuses (`pin` `unreachable`), naming `rwv fetch`. A declared branch the
   upstream changed is taken in as a ref, and the target's workspace moves
   onto it when it aligns. Every composition
   happens in this workspace; nothing outside it moves. Where a repo needs a
   setting and the target records none, the landing refuses as `history`
   `unset`, unless `--history` gives one for this run.
2. **Relock.** The lock is recorded and committed: each member's pin merged
   three ways against the merge base's lock, every composed member at its
   composed tip. A `dependency` or `reference` member takes the pin the merge
   gives it. A project branch composed by a merge gets one commit, the merge
   (first parent the target's tip), and it carries this lock, so it pins
   each member's landing merge.
3. **Fast-forward (advance-target).** Where the target is checked out, each
   receiving branch fast-forwards in its checkout, then the target's project
   branch: each the branch the landing planned, to exactly what was composed
   and recorded, never a commit made in this workspace since. Nothing there
   is ever rebased or merged. A receiving branch checked out in a third
   workspace, the primary or another workweave, whichever project it
   presents, fast-forwards in place in that checkout, which must be clean
   (`edits`); the landing leases that workspace until it releases the target,
   and `rwv abort`, run in the landing's own workspace or in a workspace the
   landing leases, puts that checkout back with the rest (one its operator
   has since moved off the branch onto a tip the landing did not write is
   left as it stands, the branch put back as a ref): it rolls the landing
   back whole, or refuses before anything moves, naming what is in the way and
   the command that ends it. A
   receiving branch checked out anywhere else refuses (`checkout`). For a
   project the target's workspace does not present, a member the presented
   project shares is that project's checkout there: the landing judges
   neither its pin nor its alignment, fast-forwards it only where it is on
   the receiving branch, and otherwise moves the receiving branch as a ref
   or in place; the alignment that follows names `rwv activate <p>
   --update` for a shared member it may not move, and each `rwv` choice it
   prints for the target carries `--project <p>`. Where the
   target is checked out nowhere, it and
   each receiving branch move as refs instead, each by compare-and-swap
   against the tip the landing read; one another verb moved since stops the
   landing as `target` `moved mid-operation` with the refs already moved left
   as moved, and `rwv abort` puts each back from its savepoint. The target
   is then reported `push-branch` `ahead` where its push branch lacks the
   landing, naming `rwv push --branch <T>`; under `--json` it is in
   `reports` as `push-branch-ahead`. A resumed landing repeats every report.
4. **Align.** The workspace with the target checked out aligns its members as
   `rwv update` does, storing the entries without reporting them; then this
   workspace's members follow the target's new pins. From the primary, where
   the target landed as refs, the primary switches to the target first. A member alignment leaves
   where it was is reported: by the state that kept it there, with that
   state's choices, or else with `rwv update --project <project>` there to
   finish it. A member the target's workspace shares with a project it
   presents stays where that project has it.
5. **Retire** (`--retire`). The workweave is removed, and the deletion rule
   decides each of the feature's branches: one that landed is deleted, one
   that did not survives and is listed with its reason. From the primary
   nothing is removed, and the rule applies to the branches of the feature
   it landed. When the workweave
   was the directory the verb ran in, the last line names the workspace to
   continue in.

Before anything is judged, `rwv land` reads the checked-out branch's record,
with the records remotes' as last fetched merged in. A record that differs from
the one this workspace last saw, and that this workspace did not write, refuses
as `target` `record changed`, with the old and the new value, until `rwv update`
accepts it or `rwv branch set-mode` restores the old value.

**The `as` check.** A feature's record stores the target's mode when it was
written. A target used another way now refuses as `target` `mode changed`:
rebind the feature (`rwv branch set-mode <W> feature --target <T>`) or restore
the target's mode (`rwv branch set-mode <T> <as>`). A landing is never
redirected. `--target <T>` takes the target's mode now.

**A record published on a remote** names a target this machine reads only
through that remote: the target published there, and a local branch of that
name taking in from that branch there, or configured to publish there and
taking in from no branch of another name. Anything else, a local branch with
a foreign upstream included, refuses as `target` `gone`, never landing into an
unrelated branch that shares the name. A target that exists nowhere refuses
the same way. The recovery rebinds the feature onto a target the operator
chooses, `rwv branch set-mode <W> feature --target <T>`, listing the
candidates nearest first: every recorded branch but the gone target and the
branches that land into the feature, which the rebind would refuse as a
cycle. A sole candidate is named in the line; where none qualifies, the
choice is `rwv branch set-mode <W> declared`.

**The lineage rule.** A member holding work its receiving branch lacks moves
only when that branch contains its base: the newest commit of its first-parent
chain that a lock in the target's history pins. Otherwise the landing refuses as
`pin` `off lineage`, listing the base and the branch, with the fact
`patch-equivalent` where commits on that branch carry the same changes. A pin
this clone lacks does not decide the base, and the landing contacts no remote
for it. Where a lock commit of the target's history that the source's project
branch does not reach holds it (a lock written since the two branches parted),
the landing reports it once, as `history pin absent` (under `--json`,
`history-pin-absent` in `reports`), naming the pin and its lock commit: this
clone does not hold it, and `rwv sync`, which fetches every repo first, judges
it, as do `rwv fetch` and a rerun. A lacked pin from before the branches parted
is not reported. So a landing made without a sync, on a clone that lacks a
pin, can pass work that a clone holding the pin refuses as `pin` `off
lineage`: with the note where the pin is from since the branches parted, and
silently where it is older, as for work carried onto a published branch the
target moved off before then. That work was built on commits the target
dropped, and the landing brings them back, as merging them would in
single-repo git. `rwv sync` first catches either case wherever a branch or tag
of the member's remotes still holds the pin.

**The source's lock.** A member whose checkout is beyond its pin is relocked
forward at op start, after the savepoints, so `rwv abort` rolls the relock back
with the rest; one line per member names the pin it took. A member whose relock
would move its pin backward refuses as `pin` `backwards`; one with no pin as
`pin` `none`.

**The target's checkouts.** A receiving branch holding commits beyond the
target's pin that no live lock and no remote reaches refuses as `pin`
`unpinned`, naming `rwv lock --commit` in the target's workspace: the landing
would pin work the target never recorded. Commits another live lock pins, or a
remote holds, are pinned by the landing. A receiving branch that does not reach
the target's pin refuses as `pin` `not on declared branch`. These, the
receiving branch's own `push-branch` `diverged` and its checkout where the
target is checked out are judged for moved members only; a member the landing
does not move is left as it is. Where the target is checked out, the edits
there are judged in each repo the landing moves: the project repo, each
receiving branch's checkout, and each member whose pin the landing changes,
whose checkout aligning moves; `--continue` judges them again before it
relocks.

Where the target is checked out nowhere, a branch it declares for a member the
landing does not move, holding commits beyond the target's pin that no other
lock or remote holds, refuses as `pin` `unpinned`, naming
`rwv workweave create <T> --branch <T>`, which opens a workspace on the
target, and `rwv lock --commit` there. A receiving branch checked out outside
the target's workspace refuses as `checkout` `in another workspace`. From the primary, a target a workweave has checked out refuses as
`checkout` `in another workspace`.

**Member changes.** A landing changes the target's member entries and its
top-level `history` only where `--allow-member-change` names them: any field
of the entry of a member the target lists, its removal, or a `dependency` or
`reference` member's pin moved backward refuses as `target` `member changes`
until the member is named, and the top-level `history` until `projects/<p>` is.
The refusal lists each changed field with the target's value and the
feature's, then the rebind onto each live weave branch whose entries match the
feature's, the setter that keeps the target's value here, and last the
consent, which says it changes the target. Adding a member is no change, and
the other top-level tables (`[integrations]`, `[workweave]`) land as content.
Each named change is reported, with the target's value and the landed one, at
the start and again on each `--continue`, and under `--json` in `reports` as
`entry-changed` or `entry-removed`. A named backward pin is reported `re-pinned`, and the lock commit
records each named re-pin by name. A name the landing changes nothing for
refuses as `inapplicable-flag`.

A landing stopped on a conflict judges again on `--continue`: `rwv.toml` as
the resolution leaves it, against the target's tip the landing read, under the
names it started with. A change none of them covers refuses as `target`
`member changes`, the op still stopped, offering a rerun with
`--allow-member-change <m>`, which adds the name, and `rwv abort`.

A `dependency` or `reference` member takes the feature's pin where the feature
changed it since the merge base, which the branch it sits on in the target must
reach (`pin` `not on declared branch`); where the target changed it too, one
of the two pins must contain the other (`pin` `unmerged`).

**Receiving branches.** One that exists nowhere is created at the target's pin
(at the feature's, for a member the target does not pin), reported `created`.
Where the landing changes a member's declared branch, the new branch moves as a
branch no checkout holds, the commits the target's pin reaches that it lacks
are listed with the copy that ports them, and the target's workspace switches
to it. Such a member holding no new work takes the new branch's tip as its pin,
merging nothing; its branch here moves there, the tip it leaves kept in a
backup. `rwv abort` deletes a branch the landing created where it still sits
where it was created and another branch reaches it.

Another weave branch that lands into this one refuses as
`target` `dependents`, one line per dependent, before anything moves. A
declared branch, which lands nowhere, lists none.

**A target that moved.** The fast-forward compares each branch against what
was composed for it. A target or receiving branch that another verb moved
after the landing read it, or a checkout there switched to another branch,
stops the landing as `target` `moved mid-operation`, with its op record kept
and the refs it already moved left as moved. `rwv abort` restores every ref
the landing moved and leaves every other repo there where it stands; then
`rwv land` again judges the new tips. `rwv land --continue` refuses it, since the plan no longer describes
the landing.

## Refusals

| Axis | State | When |
|---|---|---|
| `target` | `no target` | the checked-out branch is declared |
| `target` | `record changed` | the branch's record changed since this workspace last saw it |
| `target` | `mode changed` | the target is used another way than the record says |
| `target` | `gone` | the target exists nowhere, or only as a branch another machine's record does not name |
| `target` | `member changes` | the landing would change the target's entries or top-level `history`, unnamed |
| `target` | `dependents` | another weave branch lands into this one |
| `target` | `moved mid-operation` | the target or a receiving branch moved after the landing read it |
| `history` | `unset` | a repo needs more than a fast-forward and the target records no `history` for it, and no `--history` gives one |
| `history` | `contradicted` | `--history` differs from the setting the target records for a repo |
| `history` | `not fast-forward` | the target records `ff-only` for a repo and the landing is not a fast-forward |
| `pin` | `off lineage` | a member's base is not on its receiving branch |
| `pin` | `unreachable` | the target, or its upstream as last fetched, pins a commit this clone lacks |
| `pin` | `not on declared branch` | a receiving branch does not reach the target's pin, or a `dependency` or `reference` member's branch does not reach the feature's pin |
| `pin` | `unmerged` | the feature and the target changed a `dependency` or `reference` member's pin, neither containing the other |
| `pin` | `unpinned` | a receiving branch holds commits nothing records |
| `pin` | `backwards` | the source's relock would move a pin backward |
| `pin` | `none` | the source's lock has no pin for a member |
| `push-branch` | `diverged` | the target or a receiving branch has diverged from its upstream |
| `name` | `declared` | a member branch of the feature's name is one some weave branch declares, so the landing would rebase a declared branch |
| `edits` | `dirty` | a repo the landing moves holds uncommitted changes; under `--retire`, any checkout of the workweave |
| `edits` | `mid-operation` | a repo the landing moves is in the middle of a rebase, merge or other operation |
| `checkout` | `in another workspace` | a receiving branch, or from the primary the target, is checked out elsewhere |
| `alignment` | any | a member here, or in the target's workspace, is not where its branch expects it |

`missing-lock-merge-driver` refuses where
the project repo's lock merge driver is absent or was set up by an earlier
release, in this workspace or in the target's;
run `rwv doctor --fix` where the refusal names. A conflict while composing stops the landing
with its op record kept (`op-parked`): resolve it in the checkout named, then
`rwv land --continue`, or `rwv abort` to put every repo back at its savepoint.

## Op record

Op-state is written to this workspace and the target's before anything moves.
The owner record (`.rwv-op`) here holds the landing's plan — the target, its tip
and each receiving branch's tip it composes onto, and this workspace's project
tip, and the tip of each member its lock was behind, read once before anything moved — and the current phase (replay → relock →
advance-target → retire); the target's workspace holds a lease (`.rwv-op-lease`)
pointing back here. A verb that hits a workspace an op holds refuses, naming
the op and its two exits: `rwv land --continue` from the owning workspace, or
`rwv abort`.

Landings into one target run one at a time. Before it reads the target's tips,
a landing claims the target, and it holds the claim for as long as its
operation stands: stopped on a conflict or a lost race, it still holds it. A
second landing into the target waits for that claim, then lands on the tips
the first one left. While the landing ahead stops making progress, waiting runs
out after a few seconds and refuses as `state-claim-held`, naming the landing
that holds the claim, the workspace it runs in, and its two exits there,
`rwv land --continue` and `rwv abort`. The claim and the lease on the target's
workspace are given up once the target's workspace has taken the landing, or
the operation ends, before `--retire` removes this workspace. A landing killed
before it released the claim leaves it to its operation, which `rwv land
--continue` resumes and `rwv abort` ends; a claim whose operation is no
longer recorded is taken over by the next landing.

`rwv abort` restores every ref a landing moved, at any phase. Stopped in its
retire, the landing has given up the target: abort, from this workspace, first
takes the claim and the target's workspace back, refusing while another
operation holds either, then restores. Typed in the target's workspace, or
in a workspace the landing moved a checkout in place in, `rwv abort` takes up only a landing whose retire is recorded as stopped; for any
other landing it refuses, printing `rwv -C <workspace> abort` to run from the
landing's own workspace. The target is restored
whole or not at all. Where any branch the landing moved there, in a checkout,
in place or as a ref, stands somewhere other than where the landing left it
(another landing built on it, a commit, a deletion), or is checked out where
abort may not move it, abort refuses before either side moves, naming each
and the commits on it since. `rwv abort --leave-target` then ends the landing
with its source restored and its target left as it stands; at the retire,
`rwv land --continue` finishes it instead.

A resume recorded at `advance-target` re-enters `relock` first, and says so:
relock is what makes the lock it lands agree with the tips it lands.

## Invocation

```
rwv land [--target <T>] [--allow-member-change <repo>]... [--history <linear|merge|ff-only>] [--retire] [--json] [-j <N>] [--project <name>]
rwv land --continue [--allow-member-change <repo>]... [--json] [--project <name>]
```

- `--target <T>` — land into `T` instead of the record's target, for one run.
- `--allow-member-change <repo>` — let the landing change the target's entry
  for this member, or with `projects/<p>` its top-level `history`. Repeat per
  repo. With `--continue`, adds to the names the landing started with.
- `--history` — how a repo holding work its receiving branch lacks lands, for
  this run, where the target records no `history` for it: `linear`, `merge`
  or `ff-only`. A value differing from a recorded one is refused. The
  target's own move is a fast-forward whatever the setting.
- `--retire` — once the landing is done, remove the workweave and apply the
  deletion rule to the feature's branches. Its dirt, untracked files included,
  is judged with every other refusal before anything moves. From the primary
  there is no workweave to remove, and it is a no-op with a warning.
- `--json` — emit the envelope described under Output.
- `-j <N>` — run up to `N` per-repo steps in parallel. Default `1`.
- `--project <name>` — operate on this project instead of the active one.
- `--continue` — resume a landing a conflict stopped. Every parameter comes
  from the op record; `--project` asserts the recorded project and refuses
  another.

Run `rwv land --help` for the full clap surface.

## Output

Default text output reports each phase with one line per repo.

Under `--json`, output is the pretty-printed envelope, whatever `-j` is:

```
{
  "$schema": "<url>",
  "source_workweave": "...",
  "target": "...",
  "retired": false,
  "outcomes": [ { "kind": "...", "path": "...", "absolute_path": "...", ... }, ... ],
  "placements": [ { "path": "...", "absolute_path": "...", "files": { ... } }, ... ],
  "members": [ { "kind": "...", "path": "...", "absolute_path": "...", ... }, ... ],
  "complete": true,
  "refused": [],
  "project_repo_advance": { "from_sha": "...", "to_sha": "..." },
  "left_behind": [ { "path": "...", "was": "...", "now": "...", "commits": [ ... ], "choices": [ ... ] }, ... ],
  "reports": [ { "kind": "...", "path": "...", ... }, ... ]
}
```

`outcomes` holds one record per composed repo, sorted by repo path. Each
fast-forward that moved a branch is reported as its repo's `step3_advance`, or
as `project_repo_advance` for the project repo. `target` is the workspace that
has the target checked out, null where it landed as refs. `complete` is true once every phase ran and
op-state is cleared. `left_behind` holds, for each member whose declared
branch the landing changes, the commits the target's pin reaches that the new
branch lacks, newest first, with the copy that ports them; the text output
prints the same list. `--continue --json` emits the same envelope with a
`resumed` field naming the phase the stopped landing had reached; a value past
`replay` leaves `outcomes` empty. A resumed landing lists `left_behind` again,
in text and under `--json`. `reports` holds each move the landing reported as
it made it: a receiving branch it `created`, a member that `followed`, a
`re-pinned` member, and `taken-in`, the target's upstream it took in (`from`,
`to`, the upstream's `commits`, each pin it changed in `pins` with `backward`,
and each entry in `member_changes`); a resumed landing reports them again.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "LandJsonOutput",
  "description": "Top-level envelope for `rwv land --json`.\n\nExtends `SyncJsonOutput` with land-specific observability fields: - `source_workweave` — the workweave the op is rooted in (null when that is the primary weave). - `target` — the absolute path of the target workspace that was advanced. - `retired` — true iff the workweave was deleted. - `resumed` — the phase a `--continue` resumed from; absent on a fresh run. - `project_repo_advance` — step-3 advance of `projects/<project>/.git`; omitted when the project repo was already at CWD's tip (no-op advance). - per-outcome `step3_advance` — step-3 advance SHA pair for each member repo; omitted on a no-op advance. - `placements` — file placements in both workspaces, the target's included.\n\nKept as a separate type so the generated schema artifact (`docs/reference/schemas/land.json`) has its own title/description.",
  "type": "object",
  "required": [
    "$schema",
    "complete",
    "left_behind",
    "members",
    "outcomes",
    "placements",
    "refused",
    "reports",
    "retired"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "complete": {
      "description": "True iff the op ran its last phase and cleared its op-state, so nothing remains to `--continue` or `rwv abort`; false when a phase stopped it with its op-state held. A complete op exits 1 when `members` carries a `prune-refused` entry.",
      "type": "boolean"
    },
    "left_behind": {
      "description": "For each declared-branch change the landing makes, the commits the target's pin reaches that the new receiving branch lacks, with the copy that ports them. Empty, not absent, when it leaves none; a resumed op reports the same list its start did.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/LeftBehindOutput"
      }
    },
    "members": {
      "description": "What replay did with each member checkout its per-repo outcomes do not cover: kept, pruned, or a prune that was refused. Empty, not absent, when there are none. A `prune-refused` entry is a checkout left on disk that no manifest names; branch on it. The op still completes, and then exits 1.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/MemberOutput"
      }
    },
    "outcomes": {
      "description": "One record per repo, sorted by repo path whatever `-j` is.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/SyncOutcomeOutput"
      }
    },
    "placements": {
      "description": "How the files of each checkout the op created or moved were placed, in this workspace and in the target, each under its own `absolute_path`. Empty, not absent, when it placed none. Unlike `step3_advance`, a placement a resume makes is reported whatever phase it re-entered at.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/PlacementOutput"
      }
    },
    "project_repo_advance": {
      "description": "Step-3 advance of the project repo (`projects/<project>/.git`). Omitted when the project repo was already at CWD's tip (no-op fast-forward).",
      "anyOf": [
        {
          "$ref": "#/definitions/Step3AdvanceOutput"
        },
        {
          "type": "null"
        }
      ]
    },
    "refused": {
      "description": "Each repo the op judged when it refused, with that state's choices. Empty, not absent, when it refused on none.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/JudgedOutput"
      }
    },
    "reports": {
      "description": "Each move the landing reported as it made it: a receiving branch created, a member that followed its receiving branch, a pin moved backward by name. Empty, not absent, when it made none; a resumed op reports the moves its earlier runs made as well.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/LandingReportOutput"
      }
    },
    "resolution": {
      "description": "Resolved workspace coordinates (weave root, optional workweave identity, project). Absent when no project is resolved.",
      "anyOf": [
        {
          "$ref": "#/definitions/Resolution"
        },
        {
          "type": "null"
        }
      ]
    },
    "resumed": {
      "description": "The phase the interrupted op had reached when `--continue` resumed it; absent on a fresh run. Presence encodes \"this envelope describes a resumed op\".\n\nDecides what `outcomes` below describes. `replay` means replay re-ran in this invocation, so `outcomes` carries every repo (already-converged ones as `no-op`). Any later phase means replay had already completed before the interruption: this invocation re-entered downstream of it, and `outcomes` is empty — not because no repos were synced, but because the syncing happened in the interrupted invocation. A per-repo `step3_advance` performed by such a resume has no outcome to ride on and is not reported; the target's tips remain readable via `rwv status --json` in the target workspace.",
      "anyOf": [
        {
          "$ref": "#/definitions/OpPhase"
        },
        {
          "type": "null"
        }
      ]
    },
    "retired": {
      "description": "True iff retire deleted the workweave. False when `--retire` was not passed, when retire refused, and when retire was skipped (the op is rooted in the primary weave, where there is no workweave to delete).",
      "type": "boolean"
    },
    "source_workweave": {
      "description": "The workweave the op is rooted in; null when that is the primary weave. On a resumed op this is the workspace that owns the op record, which is not necessarily the one `--continue` was invoked from.",
      "type": [
        "string",
        "null"
      ]
    },
    "survivors": {
      "description": "The branches of the retired workweave the deletion rule kept, and why. Absent when it kept none.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/Survivor"
      }
    },
    "target": {
      "description": "Absolute path of the target workspace that step-3 fast-forwarded, as the machine resolved it; null where the target is checked out nowhere and landed as refs.",
      "anyOf": [
        {
          "$ref": "#/definitions/WirePath"
        },
        {
          "type": "null"
        }
      ]
    }
  },
  "definitions": {
    "Axis": {
      "description": "What a refusal judged a repo on. Its token is the kind `rwv explain` serves; the order here is the order a repo's lines print in.",
      "oneOf": [
        {
          "description": "Uncommitted work, or a VCS operation in progress, in a checkout.",
          "type": "string",
          "enum": [
            "edits"
          ]
        },
        {
          "description": "Another checkout derives from what the verb would remove.",
          "type": "string",
          "enum": [
            "checkout"
          ]
        },
        {
          "description": "A branch and the branch it takes in from or publishes to.",
          "type": "string",
          "enum": [
            "push-branch"
          ]
        },
        {
          "description": "A name the verb was given is unusable.",
          "type": "string",
          "enum": [
            "name"
          ]
        },
        {
          "description": "What `rwv push` withheld.",
          "type": "string",
          "enum": [
            "publish"
          ]
        },
        {
          "description": "A project whose `rwv.toml` does not load.",
          "type": "string",
          "enum": [
            "unparseable-project"
          ]
        },
        {
          "description": "The commit a lock pins for a member, and whether a ref reaches it.",
          "type": "string",
          "enum": [
            "pin"
          ]
        },
        {
          "description": "A weave branch's record: how it is used and the branch it lands into.",
          "type": "string",
          "enum": [
            "target"
          ]
        },
        {
          "description": "How a branch accepts landings and takes its target's work in.",
          "type": "string",
          "enum": [
            "history"
          ]
        },
        {
          "description": "Projects sharing a member that record different `history` for it.",
          "type": "string",
          "enum": [
            "history-disagreement"
          ]
        },
        {
          "description": "Where a workspace's checkouts sit against where the checked-out branch's record says they should.",
          "type": "string",
          "enum": [
            "alignment"
          ]
        },
        {
          "description": "A branch rwv never rewrites, renames or deletes.",
          "type": "string",
          "enum": [
            "protected"
          ]
        },
        {
          "description": "A project repo whose lock merge driver is not set up as this release sets it up.",
          "type": "string",
          "enum": [
            "missing-lock-merge-driver"
          ]
        },
        {
          "description": "A project commit `rwv cherry-pick` cannot port as it stands.",
          "type": "string",
          "enum": [
            "port"
          ]
        }
      ]
    },
    "ChoiceOutput": {
      "type": "object",
      "required": [
        "phrase",
        "run"
      ],
      "properties": {
        "id": {
          "description": "The choice's id on the axis's page; null for one it does not number.",
          "type": [
            "string",
            "null"
          ]
        },
        "phrase": {
          "type": "string"
        },
        "run": {
          "description": "The commands, one per line, as the terminal prints them.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "where": {
          "description": "The directory it must run in; null where it runs here or names its own directory.",
          "anyOf": [
            {
              "$ref": "#/definitions/WirePath"
            },
            {
              "type": "null"
            }
          ]
        }
      }
    },
    "CommitId": {
      "description": "A full commit id: 40 lowercase hex digits, or 64 in a SHA-256 repository.",
      "type": "string",
      "pattern": "^([0-9a-f]{40}|[0-9a-f]{64})$"
    },
    "ConflictOp": {
      "description": "In-flight VCS operation whose conflict needs human resolution.\n\nPassed to `Vcs::conflict_resolution_hint` so sync's conflict-bail messages embed VCS-appropriate \"how do I resume this?\" text without hardcoding git vocabulary.\n\n`Vcs::mid_op` constructs any variant from a repo's on-disk state, but a given field can be narrower than the type: every `VcsError::RebaseConflict` raised carries `Rebase`, so `Rebase` is the only value that field will actually carry.",
      "oneOf": [
        {
          "description": "Native rebase (`git rebase`).\n\nThe operator-facing resume path is `rwv sync --continue` / `rwv land --continue` — not bare `git rebase --continue`. The VCS hint for this variant stops at marking the files resolved (`git add <files>`); rwv core appends the `rwv <verb> --continue` line. Bare `git rebase --continue` resumes without rwv's hold on the lock merge driver, so the clone's own definition merges the lock of each remaining pick.",
          "type": "string",
          "enum": [
            "rebase"
          ]
        },
        {
          "description": "Merge (`git merge`) — resumes with `git merge --continue`.",
          "type": "string",
          "enum": [
            "merge"
          ]
        },
        {
          "description": "Cherry-pick (`git cherry-pick`) — resumes with `git cherry-pick --continue`. Used by sync's project-repo rebase-with-lock-exclusion path.",
          "type": "string",
          "enum": [
            "cherry-pick"
          ]
        },
        {
          "description": "Revert (`git revert`) — resumes with `git revert --continue`.",
          "type": "string",
          "enum": [
            "revert"
          ]
        }
      ]
    },
    "ContainmentOutput": {
      "description": "What one repo's replay was decided from: how this checkout stood against the source, and which read of the source it was measured against.\n\nThe two travel together because a verdict without its baseline counts commits ahead of nothing in particular.",
      "type": "object",
      "required": [
        "baseline",
        "verdict"
      ],
      "properties": {
        "baseline": {
          "description": "Which read produced the source tip this verdict measured against.",
          "allOf": [
            {
              "$ref": "#/definitions/ReplayBaseline"
            }
          ]
        },
        "verdict": {
          "description": "The ancestry between this checkout and the source tip.",
          "allOf": [
            {
              "$ref": "#/definitions/ContainmentVerdictOutput"
            }
          ]
        }
      }
    },
    "ContainmentVerdictOutput": {
      "description": "How a repo's checkout stood against the source tip it was measured against: equal to it, containing it and carrying commits it does not, contained by it, or each holding commits the other lacks.",
      "oneOf": [
        {
          "type": "object",
          "required": [
            "relation"
          ],
          "properties": {
            "relation": {
              "type": "string",
              "enum": [
                "equal"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "commits",
            "relation"
          ],
          "properties": {
            "commits": {
              "type": "integer",
              "format": "uint",
              "minimum": 0.0
            },
            "relation": {
              "type": "string",
              "enum": [
                "ahead"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "commits",
            "relation"
          ],
          "properties": {
            "commits": {
              "type": "integer",
              "format": "uint",
              "minimum": 0.0
            },
            "relation": {
              "type": "string",
              "enum": [
                "behind"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "ahead",
            "behind",
            "relation"
          ],
          "properties": {
            "ahead": {
              "type": "integer",
              "format": "uint",
              "minimum": 0.0
            },
            "behind": {
              "type": "integer",
              "format": "uint",
              "minimum": 0.0
            },
            "relation": {
              "type": "string",
              "enum": [
                "diverged"
              ]
            }
          }
        }
      ]
    },
    "EntryChange": {
      "description": "How the pulled `rwv.toml` changed a member's entry.",
      "type": "string",
      "enum": [
        "added",
        "removed",
        "changed"
      ]
    },
    "FactKind": {
      "description": "A fact about a judged repo. Facts are reported; they never select a choice.",
      "oneOf": [
        {
          "description": "A superseded tip was recorded for the branch.",
          "type": "string",
          "enum": [
            "rewritten"
          ]
        },
        {
          "description": "The branch is an ancestor of where it lands.",
          "type": "string",
          "enum": [
            "landed"
          ]
        },
        {
          "description": "Another project's live lock pins the commits.",
          "type": "string",
          "enum": [
            "pinned-by"
          ]
        },
        {
          "description": "Other live weave branches declare the branch.",
          "type": "string",
          "enum": [
            "shared"
          ]
        },
        {
          "description": "The pin is on the branch it lands into.",
          "type": "string",
          "enum": [
            "pin-landed"
          ]
        },
        {
          "description": "The member was left detached because its branch is checked out elsewhere.",
          "type": "string",
          "enum": [
            "checkout"
          ]
        },
        {
          "description": "The unlanded commits carry changes equivalent to commits where they land.",
          "type": "string",
          "enum": [
            "patch-equivalent"
          ]
        },
        {
          "description": "How many commits the branch holds that its push branch lacks.",
          "type": "string",
          "enum": [
            "ahead"
          ]
        },
        {
          "description": "How many commits the push branch holds that the branch lacks.",
          "type": "string",
          "enum": [
            "behind"
          ]
        }
      ]
    },
    "FactOutput": {
      "type": "object",
      "required": [
        "fact"
      ],
      "properties": {
        "about": {
          "type": [
            "string",
            "null"
          ]
        },
        "fact": {
          "$ref": "#/definitions/FactKind"
        }
      }
    },
    "FilePlacement": {
      "description": "How a member's tracked files reached a workspace.",
      "type": "object",
      "required": [
        "hardlinked",
        "reflinked",
        "written"
      ],
      "properties": {
        "hardlinked": {
          "description": "Linked to another workspace's read-only copy.",
          "type": "integer",
          "format": "uint",
          "minimum": 0.0
        },
        "reflinked": {
          "description": "Cloned from another workspace's copy, sharing its storage until either is written.",
          "type": "integer",
          "format": "uint",
          "minimum": 0.0
        },
        "written": {
          "description": "Written by the version control system.",
          "type": "integer",
          "format": "uint",
          "minimum": 0.0
        }
      }
    },
    "JudgedOutput": {
      "description": "One judged repo, as `--json` carries it.",
      "type": "object",
      "required": [
        "axis",
        "choices",
        "facts",
        "listed",
        "path",
        "state"
      ],
      "properties": {
        "axis": {
          "description": "What it was judged on; `rwv explain <axis>` serves the axis's page.",
          "allOf": [
            {
              "$ref": "#/definitions/Axis"
            }
          ]
        },
        "choices": {
          "description": "What this state can run, in the order the page lists it.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/ChoiceOutput"
          }
        },
        "facts": {
          "type": "array",
          "items": {
            "$ref": "#/definitions/FactOutput"
          }
        },
        "listed": {
          "description": "The files, paths or commits the state lists.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "path": {
          "description": "The repo's path, `projects/<name>` for the project repo.",
          "type": "string"
        },
        "state": {
          "description": "The state it was judged in, as that page names it.",
          "type": "string"
        },
        "subject": {
          "description": "What the state names, when it names something.",
          "type": [
            "string",
            "null"
          ]
        },
        "workspace": {
          "description": "The workspace the repo was judged in, when that is not the one the verb ran in.",
          "anyOf": [
            {
              "$ref": "#/definitions/WirePath"
            },
            {
              "type": "null"
            }
          ]
        }
      }
    },
    "LandingReportOutput": {
      "description": "One move a landing reported as it made it.",
      "oneOf": [
        {
          "description": "A receiving branch that existed nowhere, created at `at`.",
          "type": "object",
          "required": [
            "at",
            "branch",
            "kind",
            "path"
          ],
          "properties": {
            "at": {
              "$ref": "#/definitions/CommitId"
            },
            "branch": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "created"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "A member holding no new work moved to its receiving branch's tip `to`; the tip `was` it left is kept in the backup `backup`.",
          "type": "object",
          "required": [
            "backup",
            "kind",
            "path",
            "to",
            "was"
          ],
          "properties": {
            "backup": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "followed"
              ]
            },
            "path": {
              "type": "string"
            },
            "to": {
              "$ref": "#/definitions/CommitId"
            },
            "was": {
              "$ref": "#/definitions/CommitId"
            }
          }
        },
        {
          "description": "A range holding the merge `merge` that a rebasing setting integrated by a merge commit, first parent the receiving tip, rather than flatten.",
          "type": "object",
          "required": [
            "kind",
            "merge",
            "path"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "merged"
              ]
            },
            "merge": {
              "$ref": "#/definitions/CommitId"
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "A branch a rebase rewrote; the tip `was` it left is kept in the `rebased` backup `backup`.",
          "type": "object",
          "required": [
            "backup",
            "kind",
            "path",
            "was"
          ],
          "properties": {
            "backup": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "rebased"
              ]
            },
            "path": {
              "type": "string"
            },
            "was": {
              "$ref": "#/definitions/CommitId"
            }
          }
        },
        {
          "description": "A dependent's branch `branch` carried along the landing's rebase, from `was`, kept in the `rebased` backup `backup`, to `now`.",
          "type": "object",
          "required": [
            "backup",
            "branch",
            "kind",
            "now",
            "path",
            "was"
          ],
          "properties": {
            "backup": {
              "type": "string"
            },
            "branch": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "carried"
              ]
            },
            "now": {
              "$ref": "#/definitions/CommitId"
            },
            "path": {
              "type": "string"
            },
            "was": {
              "$ref": "#/definitions/CommitId"
            }
          }
        },
        {
          "description": "A pin a replayed project commit held on a commit the rebase of `path` dropped, moved to what that rebase made of the commits before it.",
          "type": "object",
          "required": [
            "kind",
            "now",
            "path",
            "was"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "remapped"
              ]
            },
            "now": {
              "$ref": "#/definitions/CommitId"
            },
            "path": {
              "type": "string"
            },
            "was": {
              "$ref": "#/definitions/CommitId"
            }
          }
        },
        {
          "description": "The source's checkout of `path`, `now`, recorded in place of the pin `was` the source's lock committed: it holds commits the target's pin lacks.",
          "type": "object",
          "required": [
            "kind",
            "now",
            "path",
            "was"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "relocked"
              ]
            },
            "now": {
              "$ref": "#/definitions/CommitId"
            },
            "path": {
              "type": "string"
            },
            "was": {
              "$ref": "#/definitions/CommitId"
            }
          }
        },
        {
          "description": "A pin the landing moved backward, named by `--allow-member-change`.",
          "type": "object",
          "required": [
            "kind",
            "now",
            "path",
            "was"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "re-pinned"
              ]
            },
            "now": {
              "$ref": "#/definitions/CommitId"
            },
            "path": {
              "type": "string"
            },
            "was": {
              "$ref": "#/definitions/CommitId"
            }
          }
        },
        {
          "description": "A field of the target's entry for `path`, or of its top-level settings, changed from `was` to `now`, named by `--allow-member-change`.",
          "type": "object",
          "required": [
            "field",
            "kind",
            "now",
            "path",
            "was"
          ],
          "properties": {
            "field": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "entry-changed"
              ]
            },
            "now": {
              "type": "string"
            },
            "path": {
              "type": "string"
            },
            "was": {
              "type": "string"
            }
          }
        },
        {
          "description": "The target's entry for `path` removed, named by `--allow-member-change`.",
          "type": "object",
          "required": [
            "kind",
            "path"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "entry-removed"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "The target's project branch, behind its upstream as last fetched, taken in from `from` to `to`, fast-forward only: the upstream's commits, and each pin and member entry they changed.",
          "type": "object",
          "required": [
            "branch",
            "commits",
            "from",
            "kind",
            "member_changes",
            "path",
            "pins",
            "to"
          ],
          "properties": {
            "branch": {
              "type": "string"
            },
            "commits": {
              "description": "The upstream's commits, newest first.",
              "type": "array",
              "items": {
                "$ref": "#/definitions/LeftCommitOutput"
              }
            },
            "from": {
              "$ref": "#/definitions/CommitId"
            },
            "kind": {
              "type": "string",
              "enum": [
                "taken-in"
              ]
            },
            "member_changes": {
              "type": "array",
              "items": {
                "$ref": "#/definitions/MemberChangeOutput"
              }
            },
            "path": {
              "type": "string"
            },
            "pins": {
              "type": "array",
              "items": {
                "$ref": "#/definitions/PinTakenInOutput"
              }
            },
            "to": {
              "$ref": "#/definitions/CommitId"
            }
          }
        },
        {
          "description": "A checkout, in either workspace, whose branch something outside it moved forward from `moved_from` to `moved_to`: before judging its edits the landing gave `files` (the file and what the VCS records for it) and `recorded_only` (what the VCS records alone, the file already holding it) `moved_to`'s content, since they still held `moved_from`'s. A refused landing refreshes only a checkout whose own edits it refused.",
          "type": "object",
          "required": [
            "absolute_path",
            "files",
            "kind",
            "moved_from",
            "moved_to",
            "path",
            "recorded_only"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "files": {
              "type": "array",
              "items": {
                "type": "string"
              }
            },
            "kind": {
              "type": "string",
              "enum": [
                "refreshed"
              ]
            },
            "moved_from": {
              "$ref": "#/definitions/CommitId"
            },
            "moved_to": {
              "$ref": "#/definitions/CommitId"
            },
            "path": {
              "type": "string"
            },
            "recorded_only": {
              "type": "array",
              "items": {
                "type": "string"
              }
            }
          }
        },
        {
          "description": "The commits `path`'s new pin in the branch `into` holds beyond the pin `into` held, pinned there with the feature's work.",
          "type": "object",
          "required": [
            "commits",
            "into",
            "kind",
            "path"
          ],
          "properties": {
            "commits": {
              "type": "array",
              "items": {
                "$ref": "#/definitions/LeftCommitOutput"
              }
            },
            "into": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "beyond-pin"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "A pin of `path` that `lock` in the target's history held, which this clone lacked, so it did not decide the base.",
          "type": "object",
          "required": [
            "kind",
            "lock",
            "path",
            "pin"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "history-pin-absent"
              ]
            },
            "lock": {
              "$ref": "#/definitions/CommitId"
            },
            "path": {
              "type": "string"
            },
            "pin": {
              "$ref": "#/definitions/CommitId"
            }
          }
        },
        {
          "description": "`branch` holds the landing and its push branch does not: the landing is not published until `branch` is pushed.",
          "type": "object",
          "required": [
            "branch",
            "kind",
            "path"
          ],
          "properties": {
            "branch": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "push-branch-ahead"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        }
      ]
    },
    "LeftBehindOutput": {
      "description": "One declared-branch change's left-behind commits.",
      "type": "object",
      "required": [
        "choices",
        "commits",
        "feature_pin",
        "now",
        "path",
        "was"
      ],
      "properties": {
        "choices": {
          "description": "The copy that ports them onto `now`, in the order the text prints it.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/ChoiceOutput"
          }
        },
        "commits": {
          "description": "The commits that side's pin reaches that `now` lacks, newest first.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/LeftCommitOutput"
          }
        },
        "feature_pin": {
          "description": "Whether the commits listed are those this feature's pin reaches, the landing keeping the target's declared branch; else the target's pin's.",
          "type": "boolean"
        },
        "now": {
          "description": "The receiving branch the landing declares.",
          "type": "string"
        },
        "path": {
          "description": "The member's manifest-relative path.",
          "type": "string"
        },
        "was": {
          "description": "The branch the target's entry declared.",
          "type": "string"
        }
      }
    },
    "LeftCommitOutput": {
      "type": "object",
      "required": [
        "sha",
        "subject"
      ],
      "properties": {
        "sha": {
          "$ref": "#/definitions/CommitId"
        },
        "subject": {
          "type": "string"
        }
      }
    },
    "MemberChangeOutput": {
      "description": "A member entry the pulled `rwv.toml` added, removed or changed.",
      "type": "object",
      "required": [
        "change",
        "fields",
        "path"
      ],
      "properties": {
        "change": {
          "$ref": "#/definitions/EntryChange"
        },
        "fields": {
          "description": "For a changed entry, each field that differs as `<field>: <was> -> <now>`.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "path": {
          "type": "string"
        }
      }
    },
    "MemberOutput": {
      "description": "What replay did with a member checkout outside its per-repo strategy: the member set the replayed manifest decides.",
      "oneOf": [
        {
          "description": "The source's lock does not name this member and the manifest replay produced does, so its checkout stays.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "kind": {
              "type": "string",
              "enum": [
                "kept"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "The replayed manifest no longer names this member, and its checkout was removed.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "kind": {
              "type": "string",
              "enum": [
                "pruned"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "The replayed manifest no longer names this member, but removing its checkout was refused. The checkout is still on disk, and once the op relocks no manifest or lock names it. The op runs to completion and then exits 1.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "message",
            "path",
            "refused"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "kind": {
              "type": "string",
              "enum": [
                "prune-refused"
              ]
            },
            "message": {
              "description": "Free-form display message, the full cause chain. Not a typed discriminant.",
              "type": "string"
            },
            "path": {
              "type": "string"
            },
            "refusal": {
              "description": "The refusal token, resolvable with `rwv explain <token>`, when the removal was refused on a condition. Absent when it was refused on the checkout's state, which `refused` then carries, or failed without a classified refusal.",
              "type": [
                "string",
                "null"
              ]
            },
            "refused": {
              "description": "Each state the checkout was judged in that refused the removal, with that state's choices; `rwv explain <axis>` serves each axis. Empty, not absent, when none was.",
              "type": "array",
              "items": {
                "$ref": "#/definitions/JudgedOutput"
              }
            }
          }
        }
      ]
    },
    "MidOperation": {
      "description": "The operation a repository is stopped in: a conflict being resolved, or a bisect.",
      "type": "string",
      "enum": [
        "mid-rebase",
        "mid-merge",
        "mid-cherry-pick",
        "mid-revert",
        "mid-bisect"
      ]
    },
    "OpPhase": {
      "description": "Current phase of the in-flight operation.\n\nPhases are listed in execution order.\n\n```text guard → mark → savepoint → replay → relock → advance-target → retire → cleanup (land only)   (--retire only) ```\n\nThe persisted phase is always the phase in progress. Every phase is idempotent and re-runnable from the record alone.",
      "oneOf": [
        {
          "description": "Project repo, then member repos: Phase 1' and Phase 2.",
          "type": "string",
          "enum": [
            "replay"
          ]
        },
        {
          "description": "Regenerate and commit `rwv.lock` (today's Phase 3). On completion, converged tips are written into the owner record.",
          "type": "string",
          "enum": [
            "relock"
          ]
        },
        {
          "description": "FF-advance every target repo to its converged tip (land only).",
          "type": "string",
          "enum": [
            "advance-target"
          ]
        },
        {
          "description": "Merged-check then workweave removal (`--retire` only).",
          "type": "string",
          "enum": [
            "retire"
          ]
        }
      ]
    },
    "PinTakenInOutput": {
      "description": "A member's pin as the upstream a landing took in changed it.",
      "type": "object",
      "required": [
        "backward",
        "path"
      ],
      "properties": {
        "backward": {
          "description": "`now` does not descend from `was`: reported `re-pinned`.",
          "type": "boolean"
        },
        "now": {
          "description": "Absent where the upstream's lock does not pin the member.",
          "type": [
            "string",
            "null"
          ]
        },
        "path": {
          "type": "string"
        },
        "was": {
          "description": "Absent where the target's lock did not pin the member.",
          "type": [
            "string",
            "null"
          ]
        }
      }
    },
    "PlacementOutput": {
      "description": "How the files of one checkout were placed when a verb created or moved it.",
      "type": "object",
      "required": [
        "absolute_path",
        "files",
        "path"
      ],
      "properties": {
        "absolute_path": {
          "description": "The checkout whose files were placed.",
          "allOf": [
            {
              "$ref": "#/definitions/WirePath"
            }
          ]
        },
        "files": {
          "description": "How many files were cloned, linked or written.",
          "allOf": [
            {
              "$ref": "#/definitions/FilePlacement"
            }
          ]
        },
        "path": {
          "description": "Manifest-relative path of the repository.",
          "type": "string"
        }
      }
    },
    "Reason": {
      "description": "Why a branch that belongs survived the rule.",
      "oneOf": [
        {
          "description": "It holds commits the branch it lands into does not reach.",
          "type": "string",
          "enum": [
            "unlanded"
          ]
        },
        {
          "description": "A live lock pins a commit that only it reaches.",
          "type": "string",
          "enum": [
            "pinned"
          ]
        },
        {
          "description": "Its published copy holds commits the branch it lands into does not reach: a pull request may be open against it.",
          "type": "string",
          "enum": [
            "open-head"
          ]
        },
        {
          "description": "A member branch that belongs to it survives.",
          "type": "string",
          "enum": [
            "members"
          ]
        },
        {
          "description": "A remote it has a copy on could not be fetched, or did not take the delete.",
          "type": "string",
          "enum": [
            "unconfirmed"
          ]
        }
      ]
    },
    "ReplayBaseline": {
      "description": "Which read gave replay this repo's target.",
      "type": "string",
      "enum": [
        "source-lock-entry",
        "source-committed-tip"
      ]
    },
    "Resolution": {
      "description": "Resolved workspace coordinates for `--json` output and the plugin env-var envelope.\n\nCarries `workspace` (primary root abs path), `workweave` (the `<project>--<name>` identity the registry records), `project` (resolved project name), and `workweave_unregistered`. No `kind` or `location` field: the checkout is one of three states, and two of them are already carried by `workweave`'s presence.\n\nThe third state needs a field of its own. A workweave whose directory no registry entry names has no identity, so `workweave` is absent for it — and absent is what the primary looks like. Without `workweave_unregistered` the two serialize identically, and a consumer reading the documented meaning of that absence is told, positively, that it is at the primary checkout.\n\nResults only — provenance (which chain step resolved the project, which flag addressed the workspace) is deliberately excluded: anything in default `--json` output becomes depended on, and the assertion use case needs the result, not the mechanism. Provenance appears only in the human-facing \"target:\" line printed to stderr.\n\nIsomorphic to the plugin env-var envelope (`RWV_WORKSPACE`/`RWV_WORKWEAVE`/`RWV_WORKWEAVE_UNREGISTERED`/`RWV_PROJECT`): both surfaces are pure projections of `WorkspaceContext::resolution`, never independently computed.",
      "type": "object",
      "required": [
        "workspace"
      ],
      "properties": {
        "project": {
          "description": "Resolved project name; absent when the workspace resolved but no project did (a primary with no project selected).",
          "type": [
            "string",
            "null"
          ]
        },
        "workspace": {
          "description": "Primary weave root.",
          "allOf": [
            {
              "$ref": "#/definitions/WirePath"
            }
          ]
        },
        "workweave": {
          "description": "Workweave identity (`<project>--<name>`), as the primary-side registry records it.\n\nAbsent at the primary, and absent for a workweave whose directory no registry entry names — identity is by record, so an unregistered workweave has no identity to report and rwv will not spell one from the directory name. Those two absences are told apart by `workweave_unregistered`, not by this field.",
          "type": [
            "string",
            "null"
          ]
        },
        "workweave_unregistered": {
          "description": "`true` when the invocation resolved into a workweave whose directory no registry entry names, so `workweave` above is absent for a reason that is not \"this is the primary\".\n\nSerialized only in that state, so the primary and a registered workweave emit exactly the bytes they emitted before this field existed. `rwv doctor --fix` registers such a directory, after which this is absent and `workweave` carries the identity.",
          "type": "boolean"
        }
      }
    },
    "Step3AdvanceOutput": {
      "description": "Step-3 fast-forward advance record for one repo in `rwv land --json` output.\n\nPresent in a per-repo outcome iff step 3 (advance-target) actually advanced that repo's branch pointer, and carrying the revisions that advance read and wrote. Omitted (`skip_serializing_if = \"Option::is_none\"`) on a no-op advance — the target was already at CWD's tip — which is the same condition under which the text line reads `already at <sha>`, because both render one outcome.",
      "type": "object",
      "required": [
        "from_sha",
        "to_sha"
      ],
      "properties": {
        "from_sha": {
          "description": "Target repo's HEAD SHA before the fast-forward.",
          "type": "string"
        },
        "to_sha": {
          "description": "Target repo's HEAD SHA after the fast-forward (== CWD's tip).",
          "type": "string"
        }
      }
    },
    "Survivor": {
      "description": "A branch the rule kept.",
      "type": "object",
      "required": [
        "branch",
        "choices",
        "detail",
        "path",
        "reason"
      ],
      "properties": {
        "branch": {
          "type": "string"
        },
        "choices": {
          "description": "What the operator can run to give the branch up.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/ChoiceOutput"
          }
        },
        "detail": {
          "description": "What the reason rests on: the commits, the locks, the remote's cause.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "path": {
          "type": "string"
        },
        "reason": {
          "$ref": "#/definitions/Reason"
        }
      }
    },
    "SyncFailureOutput": {
      "description": "Wire-output mirror of `SyncFailure` for `--json` emission.\n\nCarries the same payload as the in-memory enum but with a `cause` represented as the serializable `VcsErrorOutput`.\n\n`message` is the human-readable display string of the failure (free-form text, not a typed discriminant). `cause` is the structured typed cause — always present on `merge-failed` and `rebase-failed`, optional on `head-unreadable` — and consumers that want to branch on failure mode should inspect `cause.kind` rather than parsing `message`.",
      "oneOf": [
        {
          "type": "object",
          "required": [
            "kind",
            "message"
          ],
          "properties": {
            "cause": {
              "anyOf": [
                {
                  "$ref": "#/definitions/VcsErrorOutput"
                },
                {
                  "type": "null"
                }
              ]
            },
            "kind": {
              "type": "string",
              "enum": [
                "head-unreadable"
              ]
            },
            "message": {
              "description": "Free-form display message for this failure. Not a typed discriminant.",
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "cause",
            "kind",
            "message"
          ],
          "properties": {
            "cause": {
              "$ref": "#/definitions/VcsErrorOutput"
            },
            "kind": {
              "type": "string",
              "enum": [
                "merge-failed"
              ]
            },
            "message": {
              "description": "Free-form display message for this failure. Not a typed discriminant.",
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "cause",
            "kind",
            "message"
          ],
          "properties": {
            "cause": {
              "$ref": "#/definitions/VcsErrorOutput"
            },
            "kind": {
              "type": "string",
              "enum": [
                "rebase-failed"
              ]
            },
            "message": {
              "description": "Free-form display message for this failure. Not a typed discriminant.",
              "type": "string"
            }
          }
        }
      ]
    },
    "SyncOutcomeOutput": {
      "description": "One per-repo record in `rwv sync --json` output.",
      "oneOf": [
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "containment": {
              "description": "How this checkout stood against the source when the strategy ran. The `kind` says what the strategy did; this says what it was up against — `behind` is a fast-forward, `diverged` is a rebase.",
              "anyOf": [
                {
                  "$ref": "#/definitions/ContainmentOutput"
                },
                {
                  "type": "null"
                }
              ]
            },
            "derived_content_dropped": {
              "description": "Repo-relative paths this repo declares derived whose replayed version the landed tree does not carry: the replay resolved them to the target's version instead. Regenerating them from their source of record and committing is what makes the landed tree describe itself again. Omitted when nothing was resolved away.",
              "type": "array",
              "items": {
                "type": "string"
              }
            },
            "kind": {
              "type": "string",
              "enum": [
                "converged"
              ]
            },
            "path": {
              "type": "string"
            },
            "step3_advance": {
              "description": "Step-3 fast-forward advance for this repo; present only in `rwv land --json` output when step 3 advanced this repo.",
              "anyOf": [
                {
                  "$ref": "#/definitions/Step3AdvanceOutput"
                },
                {
                  "type": "null"
                }
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "commits_ahead",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "commits_ahead": {
              "type": "integer",
              "format": "uint",
              "minimum": 0.0
            },
            "containment": {
              "description": "How this checkout stood against the source, and which read of the source `commits_ahead` counts against.",
              "anyOf": [
                {
                  "$ref": "#/definitions/ContainmentOutput"
                },
                {
                  "type": "null"
                }
              ]
            },
            "kind": {
              "type": "string",
              "enum": [
                "already-ahead"
              ]
            },
            "path": {
              "type": "string"
            },
            "source_position": {
              "description": "Where the SOURCE's own checkout stood against that same read of the source. `containment` proves this repo holds the target; only this says whether the source has since moved past it. Absent where the source's checkout could not be measured.",
              "anyOf": [
                {
                  "$ref": "#/definitions/ContainmentVerdictOutput"
                },
                {
                  "type": "null"
                }
              ]
            },
            "step3_advance": {
              "description": "Step-3 fast-forward advance for this repo; present only in `rwv land --json` output when step 3 advanced this repo.",
              "anyOf": [
                {
                  "$ref": "#/definitions/Step3AdvanceOutput"
                },
                {
                  "type": "null"
                }
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "containment": {
              "description": "How this checkout stood against the source, and which read of the source it was equal to.",
              "anyOf": [
                {
                  "$ref": "#/definitions/ContainmentOutput"
                },
                {
                  "type": "null"
                }
              ]
            },
            "kind": {
              "type": "string",
              "enum": [
                "no-op"
              ]
            },
            "path": {
              "type": "string"
            },
            "step3_advance": {
              "description": "Step-3 fast-forward advance for this repo; present only in `rwv land --json` output when step 3 advanced this repo.",
              "anyOf": [
                {
                  "$ref": "#/definitions/Step3AdvanceOutput"
                },
                {
                  "type": "null"
                }
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "failure",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "containment": {
              "description": "How this checkout stood against the source when the strategy ran. `failure.message` renders this in prose; branch on this instead. Absent where the outcome was decided before any pair was read.",
              "anyOf": [
                {
                  "$ref": "#/definitions/ContainmentOutput"
                },
                {
                  "type": "null"
                }
              ]
            },
            "failure": {
              "$ref": "#/definitions/SyncFailureOutput"
            },
            "kind": {
              "type": "string",
              "enum": [
                "failed"
              ]
            },
            "path": {
              "type": "string"
            },
            "step3_advance": {
              "description": "Step-3 fast-forward advance for this repo; present only in `rwv land --json` output when step 3 advanced this repo. Typically absent when the repo failed in step 1.",
              "anyOf": [
                {
                  "$ref": "#/definitions/Step3AdvanceOutput"
                },
                {
                  "type": "null"
                }
              ]
            }
          }
        }
      ]
    },
    "VcsErrorOutput": {
      "description": "Wire-output mirror of `VcsError` for `--json` emission.\n\n`VcsError` itself can't derive `Serialize` cleanly because tuple variants (and `io::Error`) don't play nicely with serde's internally-tagged enum representation. This struct-only mirror does: every variant carries named fields, and a `From<&VcsError>` impl converts at JSON-emission time.",
      "oneOf": [
        {
          "type": "object",
          "required": [
            "kind",
            "path"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "not-a-repo"
              ]
            },
            "path": {
              "$ref": "#/definitions/WirePath"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "repo",
            "rev"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "revision-not-found"
              ]
            },
            "repo": {
              "$ref": "#/definitions/WirePath"
            },
            "rev": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "op",
            "repo"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "rebase-conflict"
              ]
            },
            "op": {
              "$ref": "#/definitions/ConflictOp"
            },
            "repo": {
              "$ref": "#/definitions/WirePath"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "expected",
            "kind",
            "observed",
            "repo"
          ],
          "properties": {
            "expected": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "stale-ref-witness"
              ]
            },
            "observed": {
              "type": "string"
            },
            "repo": {
              "$ref": "#/definitions/WirePath"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "operation",
            "repo"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "mid-operation"
              ]
            },
            "operation": {
              "$ref": "#/definitions/MidOperation"
            },
            "repo": {
              "$ref": "#/definitions/WirePath"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "repo",
            "stderr"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "hook-rejected"
              ]
            },
            "repo": {
              "$ref": "#/definitions/WirePath"
            },
            "stderr": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "paths",
            "repo"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "untracked-collision"
              ]
            },
            "paths": {
              "type": "array",
              "items": {
                "type": "string"
              }
            },
            "repo": {
              "$ref": "#/definitions/WirePath"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "recording_off",
            "repo"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "checkout-unrecorded"
              ]
            },
            "recording_off": {
              "type": "boolean"
            },
            "repo": {
              "$ref": "#/definitions/WirePath"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "head",
            "kind",
            "repo",
            "stood_at"
          ],
          "properties": {
            "head": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "branch-rewritten"
              ]
            },
            "repo": {
              "$ref": "#/definitions/WirePath"
            },
            "stood_at": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "paths",
            "repo"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "move-taken-in-part"
              ]
            },
            "paths": {
              "type": "array",
              "items": {
                "type": "string"
              }
            },
            "repo": {
              "$ref": "#/definitions/WirePath"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "ctx",
            "kind",
            "message"
          ],
          "properties": {
            "ctx": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "io"
              ]
            },
            "message": {
              "description": "Display form of the underlying `io::Error`. The native source is dropped at the wire boundary since `io::Error` does not serialize. Named `message` (not `error`) to make clear this is free-form display text, not a typed discriminant that consumers can branch on.",
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "args",
            "kind",
            "repo",
            "stderr"
          ],
          "properties": {
            "args": {
              "type": "array",
              "items": {
                "type": "string"
              }
            },
            "kind": {
              "type": "string",
              "enum": [
                "command-failed"
              ]
            },
            "repo": {
              "$ref": "#/definitions/WirePath"
            },
            "stderr": {
              "type": "string"
            }
          }
        }
      ]
    },
    "WirePath": {
      "description": "Absolute path, `/`-separated on every platform.",
      "type": "string"
    }
  }
}
```

## Exit codes

- `0` — the landing completed; the target is at this workspace's tip.
- `3` — the landing completed and the workweave was retired, and the deletion
  rule kept at least one of its branches. `survivors` lists each with its
  `repo`, `branch`, `reason` and `detail`.
- non-zero — a refusal, or a phase stopped with its op record kept.

## Examples

Land this workweave's feature into its target:

```
rwv land
```

Land, and remove the workweave in one step:

```
rwv land --retire
```

Land a feature that moves `lib` onto another declared branch:

```
rwv land --allow-member-change github/o/lib
```

Land into another branch for one run:

```
rwv land --target stable
```

Resume after resolving a conflict:

```
rwv land --continue
```

Put everything back after a stopped landing:

```
rwv abort
```

The landing writes no relock commit of the source's own. Where an `owned` or `fork` member's checkout is ahead of the pin the source's lock commits and holds commits the target's pin lacks, it prints
`<member>: lock behind HEAD by <count> — the landing records <tip> in place of the pin <pin>`, the count spelled `1 commit` or `3 commits`,
and the one project commit it lands records that tip; the refusals and the member changes it reports are judged against that tip too. A checkout the target's pin already reaches brings nothing to land, so its pin lands as the source committed it: a pin put back by `rwv member pin` lands as named. A `dependency` or `reference` member's pin lands as committed whatever its checkout holds, since it moves only by name; `rwv status` reports that checkout as `pin` `ahead`. That commit, where it is not a merge commit, reads:

```
lock: relock for the landing of <source>
```
