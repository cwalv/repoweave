# rwv sync

## Purpose

Take new work into the checked-out weave branch. In a feature, `rwv sync` takes
in the branch the feature's record lands into (its target). On any weave branch,
`--from <branch>` merges that branch's history in instead. A declared branch has
no target: without `--from` it refuses as `target` `no target`.

To land this workspace's work in another workspace instead, use `rwv land`.

Every refusal is judged before anything moves, and all of them are reported
together. Then, under an op record:

1. **Members.** Each `owned` or `fork` member the target's lock pins takes in
   its merge source: the tip of the branch the member lands on — its branch
   named after the target when the target is itself a feature, else the branch
   the entries declare — read as the newer of that branch and its upstream, or
   the target's pin where that branch exists nowhere. Commits it holds beyond
   the target's pin are listed, since the landing will pin them.
   - A member with no new work **follows** its merge source: by fast-forward,
     or, where the merge source lacks the member's tip, by moving to it after a
     backup.
   - A member holding new work — commits its merge source lacks — is
     integrated as the target's `history` setting says, read from the
     target's own `rwv.toml`: the member's entry, else the top level. Under
     `linear` and `ff-only` its work is rebased onto the merge source after a
     `rebased` backup; under `merge` the merge source is merged in, the
     member's branch as first parent. A range holding a merge commit is never
     rebased, since the rebase would flatten it: under `linear` or `ff-only`
     it is integrated by a merge commit instead, reported `merged`. Where the
     target records no setting for the member, the sync refuses as `history`
     `unset` unless `--history` gives one for this run; a `--history` that
     differs from a recorded setting refuses as `history` `contradicted`, whether or
     not the sync needs the setting.
2. **The project branch** takes in the target's tip the same way, by the
   target's top-level `history`.
3. **The lock** is recorded and committed: each member's pin merged three ways
   against the merge base's lock, with every member the sync moved at its new
   tip.
4. **Alignment.** Members the new entries drop are pruned, missing ones are
   cloned, and every member — `dependency` and `reference` members included —
   is aligned as `rwv update` aligns it, to the lock just committed.

**The lineage rule.** A member holding new work, and one that would follow while
another weave branch lands into this one, moves only when the branch it lands
on contains its base: the newest commit of its first-parent chain that a lock in
the target's history pins. Otherwise the sync refuses as `pin` `off lineage`,
listing the base and the branch, with the fact `patch-equivalent` where commits
on that branch carry the same changes. The rule judges
after the sync's fetch of every repo, so a pin published on a branch or tag of
a member's remotes is judged like any other. A pin this clone still lacks then
does not decide the base. Where a lock commit of the target's history that this
branch does not reach holds it (a lock written since the two branches parted),
the sync reports it once, as `history pin absent` (under `--json`,
`history-pin-absent` in `reports`), naming the pin and its lock commit; an
older one is not reported.

**`--from <branch>`** merges the branch's history in for one run, whatever the
setting: each member that lacks the branch's pin merges it as a merge commit
with this branch's tip as first parent (a `--from` of the record's own target
may fast-forward instead), then the project branch takes the branch in, with the
relock. On a
declared branch, a member the named branch declares on another branch refuses
(`pin` `not on declared branch`) until that branch is declared here.

**`--reset-to-target <repo>`** moves the named repo's branch to its merge source
instead of integrating it, skipping the lineage rule and the push-branch
judgment for it: a member to the tip of the branch it lands on, `projects/<p>` to
the target's tip with the target's lock. The tip it leaves is kept as a backup,
and the new work it drops is listed with the commands that copy it back or merge
it in.

Before anything is fetched, `rwv sync` reads the checked-out branch's record,
with the records remotes' as last fetched merged in. A record that differs from
the one this workspace last saw, and that this workspace did not write, refuses
as `target` `record changed`, with the old and the new value, until `rwv update`
accepts it or `rwv branch set-mode` restores the old value. A workspace that has
stored no record for the branch stores the one it reads, without a report.

Sync runs no install hooks. Where the take-in changed an input of the project
the weave root presents — the project's `rwv.toml` or `rwv.lock`, or a member's
manifest an enabled integration reads, such as `Cargo.toml` — sync says so and
names `rwv materialize`, the verb that re-derives the generated state.

Where a rebase resolved a declared derived path to the target's version, sync
names the paths: what was replayed there is not what landed, and they want
regenerating from their source of record.

## Refusals

| Axis | State | When |
|---|---|---|
| `target` | `no target` | the branch is declared and no `--from` names a branch to merge |
| `target` | `gone` | the target exists neither here nor on a remote |
| `target` | `record changed` | the branch's record changed since this workspace last saw it |
| `target` | `dependents` | another weave branch lands into this one, and the sync would rewrite a branch it is built on |
| `name` | `no-such-branch` | `--from` names a branch that exists nowhere |
| `history` | `unset` | a repo needs more than a fast-forward and the target records no `history` for it, and no `--history` gives one |
| `history` | `contradicted` | `--history` differs from the setting the target records for a repo |
| `pin` | `off lineage` | a member's base is not on the branch it lands on |
| `pin` | `unreachable` | the target pins a commit this clone lacks |
| `pin` | `not on declared branch` | `--from` on a declared branch, for a member the named branch declares elsewhere |
| `push-branch` | `behind` | a branch the sync would move is behind its push branch |
| `push-branch` | `diverged` | a branch the sync would move, or the target, has diverged from its upstream |
| `protected` | `declared` | a rebase, follow or reset would rewrite a member branch some weave branch declares |
| `edits` | `dirty` | a repo the sync would move holds uncommitted changes to tracked files |
| `checkout` | `in another workspace` | a member branch the sync would move is checked out in another workspace |
| `alignment` | any | a member is not where the branch expects it |

A branch diverged from its push branch is not refused when everything it holds
beyond the push branch is already on the branch it lands on: that is reported
as landed, and the sync goes on. It refuses when a superseded tip of the
upstream is all that reaches those commits (the fact `rewritten`: the upstream
was rewritten), and when it holds commits never pushed.

`inapplicable-flag` refuses `--target` with `--from`, `--history` with `--from`, and a `--reset-to-target`
naming a repo the sync takes nothing into; `partial-run-aborted` refuses when
the fetch could not complete. A conflict stops the sync with its op record kept
(`op-parked`): resolve it in the checkout named, then `rwv sync --continue`, or
`rwv abort` to put every repo back at its savepoint.

## Invocation

```
rwv sync [--from <branch>] [--target <T>] [--reset-to-target <repo>]...
         [--history <linear|merge|ff-only>] [--json] [-j <N>] [--project <name>]
rwv sync --continue
```

- `--from <branch>` — merge this weave branch's history in for one run.
- `--target <T>` — take in `T` instead of the record's target, for one run.
  The record is unchanged.
- `--reset-to-target <repo>` — move this repo's branch to its merge source
  after a backup; repeat per repo. `projects/<name>` names the project branch.
- `--history` — how a repo holding new work takes in, for this run, where the
  target records no `history` for it: `linear`, `merge` or `ff-only`. A value
  differing from a recorded one is refused.
- `--json` — emit machine-readable output (see Output below).
- `-j <N>` — run up to `N` per-repo fetches in parallel. Default `1`.
- `--project <name>` — operate on this project instead of the active project.
- `--continue` — resume a sync a conflict stopped; every parameter comes from
  the op record.

Run `rwv sync --help` for the full clap surface.

## Output

Text output is one line per repo that moved (`fast-forwarded`, `follows`,
`rebased onto`, `merged`, `reset`), a `merged:` line for each range holding a
merge that a rebasing setting integrated by a merge commit, the commits beyond the target's pin each
member's landing branch holds, a line per member file placement, the
`rwv materialize` line when a materialized input changed, and a closing
`rwv sync: took in <branch>`. With nothing to take in it prints
`<project>: already contains <branch>`.

Under `--json`, output is one pretty-printed envelope:

```
{
  "$schema": "<url>",
  "outcomes": [],
  "placements": [ { "path": "...", "absolute_path": "...", "files": { ... } }, ... ],
  "members": [ { "kind": "...", "path": "...", "absolute_path": "...", ... }, ... ],
  "complete": true,
  "refused": [ ... ],
  "advisories": [ ... ],
  "reports": [ { "kind": "merged" | "rebased", "path": "...", ... }, ... ]
}
```

`outcomes` is empty: `rwv land` fills it. `reports` lists each range the sync
integrated by a merge commit (`merged`, with the `merge`) and each branch it
rebased (`rebased`, with the tip it `was` and the `backup` holding it); a
resumed sync reports the ones its earlier runs made as well. Each checkout whose files the sync
moved gets a placement record under `placements`: `path`, `absolute_path` (the
checkout the files are in), and `files` (`reflinked`, `hardlinked`, `written`).

`members` lists the member checkouts a dropped entry pruned (`pruned`) or could
not prune (`prune-refused`). A `prune-refused` record carries `refused`, each
state the checkout was judged in that refused the removal, with that state's
choices, or the `refusal` token when the removal was refused on a condition, and
the full `message`. Its checkout is left on disk with no manifest naming it: the
sync still completes and clears its op record, then exits 1, so `rwv abort` does
not apply and re-running the sync does not prune it; remove it by hand.

`refused` carries each judged line of a refusal with its choices. `advisories`
carries a `derived-state-stale` record, naming `rwv materialize` and the inputs
that moved, when the take-in changed a materialized input.

`complete` is true when the sync ran to its end and cleared its op record, and
false when it refused or a conflict stopped it.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "SyncJsonOutput",
  "description": "Top-level envelope for `rwv sync --json`.",
  "type": "object",
  "required": [
    "$schema",
    "advisories",
    "complete",
    "members",
    "outcomes",
    "placements",
    "refused",
    "reports"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "advisories": {
      "description": "Standing advisories raised during this sync (e.g. delivered changes touching a materialized input). Empty, not absent, when there are none — a consumer branches on length rather than presence.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/AdvisoryOutput"
      }
    },
    "complete": {
      "description": "True iff the op ran its last phase and cleared its op-state, so nothing remains to `--continue` or `rwv abort`; false when a phase stopped it with its op-state held. A complete op exits 1 when `members` carries a `prune-refused` entry.",
      "type": "boolean"
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
      "description": "How the files of each checkout the sync created or moved were placed. Empty, not absent, when it placed none.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/PlacementOutput"
      }
    },
    "refused": {
      "description": "Each repo the op judged when it refused, with that state's choices. Empty, not absent, when it refused on none.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/JudgedOutput"
      }
    },
    "reports": {
      "description": "Each range the sync integrated by a merge commit rather than flatten, and each branch a rebase rewrote with the backup holding the tip it left. Empty, not absent, when it made none; a resumed op reports the ones its earlier runs made as well.",
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
    }
  },
  "definitions": {
    "AdvisoryKindOutput": {
      "description": "Closed vocabulary for `AdvisoryOutput::kind`. Adding a member is additive — existing consumers keep matching the members they know.",
      "oneOf": [
        {
          "description": "Generated ecosystem state may no longer agree with the inputs it was derived from.",
          "type": "string",
          "enum": [
            "derived-state-stale"
          ]
        }
      ]
    },
    "AdvisoryOutput": {
      "description": "A condition worth an operator's attention that a verb's `--json` output reports alongside its result, without being a failure of the verb itself.\n\nEvery field is something a consumer branches on directly — `kind` a closed enum, `remedy` a verb string runnable in the checkout where the advisory appears, `inputs` the workspace-relative paths that raised it. None of the three is a sentence a consumer would have to parse to act on.\n\nShared across verbs so more than one `--json` surface can emit the same vocabulary: a sync-time note and a doctor-time standing finding both fit this shape without either owning it.",
      "type": "object",
      "required": [
        "inputs",
        "kind",
        "remedy"
      ],
      "properties": {
        "inputs": {
          "description": "Workspace-relative paths whose state raised this advisory.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "kind": {
          "$ref": "#/definitions/AdvisoryKindOutput"
        },
        "remedy": {
          "description": "The verb that resolves the advisory (e.g. `\"rwv materialize\"`).",
          "type": "string"
        }
      }
    },
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
          "description": "A project's workweave index at the path earlier releases kept it at.",
          "type": "string",
          "enum": [
            "legacy-workweave-index"
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
          "description": "Native rebase (`git rebase`).\n\nThe operator-facing resume path is `rwv sync --continue` / `rwv land --continue` — not bare `git rebase --continue`. The VCS hint for this variant stops at staging (`git add <files>`); rwv core appends the `rwv <verb> --continue` line. Bare `git rebase --continue` resumes without rwv's hold on the lock merge driver, so the clone's own definition merges the lock of each remaining pick.",
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
          "description": "A checkout, in either workspace, whose branch something outside it moved forward from `moved_from` to `moved_to`: before judging its edits the landing gave `files` (content and index entry) and `index_entries` (index entry alone) `moved_to`'s content, since they still held `moved_from`'s. A refused landing refreshes only a checkout whose own edits it refused.",
          "type": "object",
          "required": [
            "absolute_path",
            "files",
            "index_entries",
            "kind",
            "moved_from",
            "moved_to",
            "path"
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
            "index_entries": {
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
        },
        {
          "description": "The source's lock names this member and no checkout of it is on disk here, but creating one failed. It gets no per-repo outcome, and the op parks.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "message",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "kind": {
              "type": "string",
              "enum": [
                "materialize-failed"
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
              "description": "The refusal token, resolvable with `rwv explain <token>`. Absent when the failure carries no classified refusal.",
              "type": [
                "string",
                "null"
              ]
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

- `0` — the sync completed, or there was nothing to take in.
- `1` — it refused before anything moved, a conflict stopped it with its op
  record kept, or it completed with a checkout it could not prune.

## Examples

Take the target's new work in, as the target's `history` setting says:

```
rwv sync
```

Take it in by rebasing where the target records no setting:

```
rwv sync --history linear
```

Merge a release branch's history into this one:

```
rwv sync --from stable
```

List the states a sync refused on:

```
rwv sync --json | jq '.refused[] | {path, axis, state}'
```
