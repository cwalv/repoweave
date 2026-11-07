# rwv status

## Purpose

Report per-repo workspace state for the active project (or all projects in a
weave): branch tip, lock SHA, lock relation, mid-operation state, role, and
URL. This is the read-only orientation verb — it never mutates anything on
disk. Agents call it to decide what to do next (sync, recover from a mid-op,
inspect drift) without touching git plumbing themselves.

## Invocation

```
rwv status [--json] [--project <name>]
           [--role <role>]... [--repo <selector>]...
```

- `--json` emits machine-readable output (see Output below).
- `--project <name>` reports status for the named project without changing
  the active project (`.rwv-active` is untouched).
- `--role` / `--repo` narrow the report to matching repos, in both the table
  and the `--json` `repos` array. The grammar is the one `fetch`, `pull`,
  `update` and `push` take: a bare `--repo` value matches a manifest path exactly,
  `re:<pat>` as a regex, `glob:<pat>` as a glob; repeated flags, and `--role`
  with `--repo`, combine as a union. With neither flag every repo is
  reported. A filter that matches no repo is not an error: the table has no
  rows, `repos` is `[]`, and the exit code is 0.

Run `rwv status --help` for the full clap surface.

## Output

Default text output is a fixed-column table: one repo per row, with
`path branch tip lock: <sha> [<relation>] [<mid-op>]`.

When this workspace owns an in-flight `sync`/`sync-to` op, or leases one to
another workspace, a header line prints ahead of the table: verb, op id,
phase, age, and the owning workspace, followed by the two remedies
(`--continue` from the owning workspace, or `rwv abort`). This is the same
disclosure the in-flight refusal makes on a conflicting mutation — `status`
surfaces it read-only, so a clean `git status` in every repo is not mistaken
for "no op in progress".

Below the table, `status` prints what stands apart, reading refs as last
fetched:

- `<project> aligned for <branch>` (or `aligned detached`, or `aligned for no
  branch`): the branch the project's workspace last aligned its members for.
  It and the project repo's own `alignment` line print only without `--role` or
  `--repo`, which narrow the rest to the selected repos.
- One `alignment` line for each member that is not in its expected state, with
  the commits that a move off its branch would leave and the state's choices;
  one for a member carrying the fact `checkout` (its branch is checked out in
  another workspace, which is named, so it sits detached at its pin); and for
  the project repo, `project moved`, `project unaligned` or `project detached`
  against the aligned branch. A workspace state file that cannot be read is
  said so, and reads as absent.
- One `pin` line for each member whose HEAD is not at the commit the lock pins
  for it, with that state's choices: `none` (the lock has no entry for it),
  `unreachable`, `behind`, `diverged`, `unpinned` (commits beyond the pin that
  nothing else holds, listed) or `ahead` (commits beyond it that are pushed or
  pinned by another project's live lock, with the fact `pinned-by` naming the
  lock).
- One `push-branch` line for each member whose checked-out branch needs
  attention, with that state's choices: `behind`, `diverged`, `gone`, `foreign
  upstream` or `no push target`. A branch `ahead` of its push branch is the
  ordinary state of work in progress and prints no line. A `gone` line prints
  only while the branch holds commits no remote-tracking branch reaches, and
  lists them.
- `operator branch <repo>  <branch>  <n> unpushed`, with ` (upstream gone)`
  where it applies, for each local branch of a member that holds unpushed
  commits nothing else accounts for: one that is checked out nowhere, is named
  after no recorded weave branch of any project in the weave, and is no
  declared branch of that member. Such a branch is legal, so no choice prints.
- `backup <repo>  <name>  <tip>` for each backup a repo holds, the listing
  `rwv backup list` gives.

Under `--json`, output is the envelope:

```
{
  "$schema": "<url>",
  "repos": [ { "path": "...", ... }, ... ],
  "unreadable_projects": [ { "project": "...", "error": "..." } ],
  "aligned": [ { "project": "...", "branch": "main", "detached": false } ],
  "judged": [ { "path": "...", "axis": "alignment", "state": "on-branch", "choices": [ ... ] } ],
  "operator_branches": [ { "path": "...", "branch": "...", "unpushed": 2, "commits": [ ... ], "gone_upstream": false } ],
  "backups": [ { "path": "...", ... } ],
  "op": { "id": "...", "verb": "sync", "phase": "replay", ... }
}
```

The `$schema` URL points to the committed schema artifact. Each element of
`repos` is a `RepoStatus` record: `path` (manifest-relative), `absolute_path`
(fully resolved), `branch`, `tip`, `lock_sha`, `relation`
(`ok`/`ahead`/`behind`/`diverged`/`no-lock`/`unknown`/`missing`/`unreachable`),
`mid_op` (`mid-rebase`/`mid-merge`/`mid-cherry-pick`/`mid-bisect`, or `null`),
`role`, `url`, `project`.

`aligned` has one element for each project: the `branch` its workspace last
aligned for (absent where none is recorded or the workspace is detached),
`detached`, and `unreadable` with the reason when the state file could not be
read.

`judged` carries the `alignment`, `pin` and `push-branch` lines above as the table's
generated choices: each element has the repo's `path`, the `axis` and `state`
it was judged on, its `facts` and its `choices`. Empty, not absent, when no
member stands apart.

`operator_branches` carries the `operator branch` lines: the member's `path`,
the `branch`, the count `unpushed`, the unpushed `commits` themselves (each
with its `sha`, `short` id and `subject`), and `gone_upstream`. `backups` carries the
backups each repo holds, as `rwv backup list --json` does.

`unreadable_projects` lists each project whose `rwv.toml` or `rwv.lock` did
not load, with the load `error` naming the file and line; none of its repos is
in `repos`. The table prints a warning for each instead, and the exit code is
still 0. Empty, not absent, when every project loaded.

`op` is present only when this workspace owns or leases an in-flight op: an
`OpStatus` record carrying `id`, `verb` (`sync`/`sync-to`), `phase`
(`replay`/`relock`/`advance-target`/`retire`), `started_at`, the `owner`
workspace (where `--continue`/`abort` must run), `source`, `target`, and
`overrides`. Absent (not `null`) when no op is in progress.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "StatusJsonOutput",
  "description": "Top-level envelope for `rwv status --json`. Matches the convention adopted by doctor (`$schema` + `violations`) and sync (`$schema` + `outcomes`): `{ \"$schema\": \"<url>\", \"repos\": [<RepoStatus>, ...] }`.",
  "type": "object",
  "required": [
    "$schema",
    "aligned",
    "backups",
    "judged",
    "operator_branches",
    "records",
    "repos",
    "unreadable_projects"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "aligned": {
      "description": "The branch each project's workspace last aligned its members for.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/AlignedStatus"
      }
    },
    "backups": {
      "description": "The backups each repo holds, with what each keeps that no branch reaches. Only repos that hold one.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/RepoBackupsOutput"
      }
    },
    "judged": {
      "description": "Each repo judged on the axes `rwv status` reads, with that state's choices: `alignment` (a member not in its expected state, or carrying a fact such as `checkout`, and the project repo against the aligned branch), and `push-branch` where the branch needs attention: behind, diverged, gone with commits of its own, with a foreign upstream or with no push target. A branch that is ahead of its push branch is the ordinary state of work in progress and has no line; the per-repo `relation` and `operator_branches` carry what is unpublished. Empty, not absent, when none stands apart.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/JudgedOutput"
      }
    },
    "op": {
      "description": "The sync/sync-to op parked at this workspace, or leased to one elsewhere, if any. Absent when no op is in progress — the same disclosure the in-flight refusal makes, before an operator has to attempt a mutation to learn it.",
      "anyOf": [
        {
          "$ref": "#/definitions/OpStatus"
        },
        {
          "type": "null"
        }
      ]
    },
    "operator_branches": {
      "description": "Each member's local branch holding unpushed commits that nothing else accounts for: checked out nowhere, named after no recorded weave branch of any project here, and no declared branch of that member.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/OperatorBranch"
      }
    },
    "records": {
      "description": "Each project's checked-out branch with its record and the branches that land into it, read in memory from the records as last fetched. A project whose repo is not on a branch has none.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/BranchRecordStatus"
      }
    },
    "repos": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/RepoStatus"
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
    "unreadable_projects": {
      "description": "Projects whose `rwv.toml` or `rwv.lock` did not load: none of their repos is in `repos`.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/UnreadableProject"
      }
    }
  },
  "definitions": {
    "AlignedStatus": {
      "description": "What a project's workspace last aligned its members for.",
      "type": "object",
      "required": [
        "detached",
        "project"
      ],
      "properties": {
        "branch": {
          "description": "The branch it last aligned for; absent where none is recorded or the workspace is detached.",
          "type": [
            "string",
            "null"
          ]
        },
        "detached": {
          "description": "Whether the workspace was made detached.",
          "type": "boolean"
        },
        "project": {
          "type": "string"
        },
        "unreadable": {
          "description": "Why the workspace state, which reads as absent, could not be read.",
          "type": [
            "string",
            "null"
          ]
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
        }
      ]
    },
    "BackupMark": {
      "description": "What a backup says about the step that made it. An ordinary backup says nothing.",
      "oneOf": [
        {
          "description": "Made before a rebase rewrote the branch.",
          "type": "string",
          "enum": [
            "rebased"
          ]
        },
        {
          "description": "The tip `rwv abort --abandon-foreign-tip` gave up.",
          "type": "string",
          "enum": [
            "abandoned"
          ]
        }
      ]
    },
    "BackupOutput": {
      "type": "object",
      "required": [
        "held",
        "name",
        "redundant",
        "tip"
      ],
      "properties": {
        "held": {
          "description": "The commits the backup holds that no branch reaches, newest first.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/HeldCommitOutput"
          }
        },
        "mark": {
          "description": "`rebased` for a branch a rebase rewrote, `abandoned` for a tip `rwv abort --abandon-foreign-tip` gave up; absent for any other.",
          "anyOf": [
            {
              "$ref": "#/definitions/BackupMark"
            },
            {
              "type": "null"
            }
          ]
        },
        "name": {
          "description": "The name `rwv backup drop` takes.",
          "type": "string"
        },
        "redundant": {
          "description": "A branch reaches everything the backup holds, so dropping it loses nothing.",
          "type": "boolean"
        },
        "tip": {
          "description": "The commit the backup holds.",
          "allOf": [
            {
              "$ref": "#/definitions/CommitId"
            }
          ]
        }
      }
    },
    "BranchRecordStatus": {
      "description": "A project repo's checked-out branch and what its record reads as, with the branches that land into it, as `rwv status` shows it.",
      "type": "object",
      "required": [
        "branch",
        "dependents",
        "project",
        "record"
      ],
      "properties": {
        "branch": {
          "type": "string"
        },
        "dependents": {
          "description": "The branches whose records name this one as their target.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "project": {
          "type": "string"
        },
        "record": {
          "$ref": "#/definitions/RecordOutput"
        }
      }
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
    "HeldCommitOutput": {
      "type": "object",
      "required": [
        "sha",
        "short",
        "subject"
      ],
      "properties": {
        "sha": {
          "$ref": "#/definitions/CommitId"
        },
        "short": {
          "type": "string"
        },
        "subject": {
          "type": "string"
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
    "LockRelation": {
      "description": "Relation between the current branch tip and the lock SHA.\n\nThe two clone-health variants (`Missing` / `Unreachable`) name distinct failure modes, neither of which is `NoLock`:\n\n- `Missing` — the clone directory is absent from disk entirely (out-of-band `rm -rf`, never fetched, etc.). The lock entry may be fine; the repair verb is a re-clone / `rwv update`.\n\n- `Unreachable` — the clone directory exists but the SHA pinned in the lock is not present in the local object store (history rewritten, shallow clone, object pruned). The repair is `git fetch` in that clone, which re-materializes the missing object; `rwv update` does not fetch into a clone that is present.\n\nNeither state should be attributed to the lock file itself — surfacing them as `no-lock` misdirects operators at the wrong repair path.",
      "oneOf": [
        {
          "type": "string",
          "enum": [
            "ok",
            "ahead",
            "behind",
            "diverged",
            "no-lock",
            "unknown"
          ]
        },
        {
          "description": "Clone directory is absent from disk (out-of-band removal, never fetched). Repair: re-clone / `rwv update`.",
          "type": "string",
          "enum": [
            "missing"
          ]
        },
        {
          "description": "Clone directory exists but the locked SHA is not in the local object store (history rewritten, shallow clone, object pruned). Repair: `git fetch` in that clone to materialize the missing object; `rwv update` does not fetch into a clone that is present.",
          "type": "string",
          "enum": [
            "unreachable"
          ]
        }
      ]
    },
    "LogCommit": {
      "type": "object",
      "required": [
        "sha",
        "short",
        "subject"
      ],
      "properties": {
        "sha": {
          "$ref": "#/definitions/CommitId"
        },
        "short": {
          "type": "string"
        },
        "subject": {
          "type": "string"
        }
      }
    },
    "MidOperation": {
      "description": "The operation a repository is stopped in: a conflict being resolved, or a bisect.",
      "type": "string",
      "enum": [
        "mid-rebase",
        "mid-merge",
        "mid-cherry-pick",
        "mid-bisect"
      ]
    },
    "Mode": {
      "description": "How a weave branch is used.",
      "oneOf": [
        {
          "description": "Work onto a target branch: its `owned` and `fork` members sit on branches named after it.",
          "type": "string",
          "enum": [
            "feature"
          ]
        },
        {
          "description": "Members sit on the branches its member entries declare.",
          "type": "string",
          "enum": [
            "declared"
          ]
        }
      ]
    },
    "OpPhase": {
      "description": "Current phase of the in-flight operation (schema v2).\n\nPhases are listed in execution order.\n\n```text guard → mark → savepoint → replay → relock → advance-target → retire → cleanup (sync-to only)   (--retire only) ```\n\nThe persisted phase is always the phase in progress. Every phase is idempotent and re-runnable from the record alone.",
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
          "description": "FF-advance every target repo to its converged tip (sync-to only).",
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
    "OpStatus": {
      "description": "The in-flight op recorded at this workspace (owner) or the workspace this one leases to (owner elsewhere), disclosed read-only on `rwv status`.\n\nThe record minus the tip tables (`advanced_tips`/`converged_tips`): those are replay bookkeeping an operator deciding `--continue` vs `abort` doesn't need. `verb`, `phase` and `overrides` are the op-state crate's own types, not a re-encoding of them — a second vocabulary for the same three facts is the thing this disclosure must not become.",
      "type": "object",
      "required": [
        "id",
        "overrides",
        "owner",
        "phase",
        "source",
        "started_at",
        "target",
        "verb"
      ],
      "properties": {
        "id": {
          "type": "string"
        },
        "overrides": {
          "type": "array",
          "items": {
            "$ref": "#/definitions/Override"
          }
        },
        "owner": {
          "description": "Workspace holding the full op record (`.rwv-op`) — the workspace `--continue`/`abort` must run from. Equal to the reporting workspace unless this workspace only holds a lease.",
          "allOf": [
            {
              "$ref": "#/definitions/WirePath"
            }
          ]
        },
        "phase": {
          "$ref": "#/definitions/OpPhase"
        },
        "source": {
          "$ref": "#/definitions/WirePath"
        },
        "started_at": {
          "type": "string"
        },
        "target": {
          "$ref": "#/definitions/WirePath"
        },
        "verb": {
          "$ref": "#/definitions/OpVerb"
        }
      }
    },
    "OpVerb": {
      "description": "Which top-level verb started this op.",
      "oneOf": [
        {
          "description": "Single-step sync (existing `rwv sync`).",
          "type": "string",
          "enum": [
            "sync"
          ]
        },
        {
          "description": "Two-step sync-to.",
          "type": "string",
          "enum": [
            "sync-to"
          ]
        }
      ]
    },
    "OperatorBranch": {
      "description": "A member's local branch holding unpushed commits that nothing else accounts for.",
      "type": "object",
      "required": [
        "branch",
        "commits",
        "gone_upstream",
        "path",
        "unpushed"
      ],
      "properties": {
        "branch": {
          "type": "string"
        },
        "commits": {
          "type": "array",
          "items": {
            "$ref": "#/definitions/LogCommit"
          }
        },
        "gone_upstream": {
          "description": "The branch takes in from a remote branch its remote no longer has.",
          "type": "boolean"
        },
        "path": {
          "description": "The member's path.",
          "type": "string"
        },
        "unpushed": {
          "description": "How many commits no remote-tracking branch reaches.",
          "type": "integer",
          "format": "uint",
          "minimum": 0.0
        }
      }
    },
    "Override": {
      "description": "A named consent supplied at invocation and recorded on the owner record.\n\nEach variant is one CLI flag, and serializes to that flag's name without the leading dashes — the spelling already on disk in every `.rwv-op` written so far. `--continue` re-derives the op's consent from this list and `cleanup` reads it to decide whether the project savepoint survives as the only remaining pointer to discarded commits, so mint and read must be the same value rather than the same text.",
      "oneOf": [
        {
          "description": "The lock-freshness precondition was waived on both sides.",
          "type": "string",
          "enum": [
            "allow-stale-lock"
          ]
        },
        {
          "description": "Phase 1' may hard-reset the project repo past commits the source does not carry.",
          "type": "string",
          "enum": [
            "discard-local-commits"
          ]
        }
      ]
    },
    "ParentInfo": {
      "description": "Recorded-parent exposure for a per-repo status entry.\n\nParent identity comes from the workweave's `.rwv-workweave` marker (`parent:`), never from the branch name: a workweave's branch is the flat `<project>--<name>`, which names no parent, and adoption can re-point the parent to primary. Consumers that need the parent must read this field.\n\n`path` is the recorded parent workspace path (identical for every repo in the workweave). `tip` is this specific repo's parent tip — the SHA that `git rev-parse HEAD` yields in the parent's checkout of the SAME repo — or `None` when the parent has no checkout of this repo (or HEAD is unreadable). The tip is what `git log <parent-tip>..HEAD` needs to compute the workweave's unique commits without re-deriving branch layout.",
      "type": "object",
      "required": [
        "path"
      ],
      "properties": {
        "path": {
          "description": "Recorded parent workspace path (from the `.rwv-workweave` marker).",
          "allOf": [
            {
              "$ref": "#/definitions/WirePath"
            }
          ]
        },
        "tip": {
          "description": "This repo's HEAD in the parent's checkout, if resolvable.",
          "anyOf": [
            {
              "$ref": "#/definitions/CommitId"
            },
            {
              "type": "null"
            }
          ]
        }
      }
    },
    "RecordOutput": {
      "description": "One weave branch's record, as `rwv branch list --json` carries it.",
      "oneOf": [
        {
          "type": "object",
          "required": [
            "state"
          ],
          "properties": {
            "state": {
              "type": "string",
              "enum": [
                "declared"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "as",
            "state",
            "target_branch"
          ],
          "properties": {
            "as": {
              "$ref": "#/definitions/Mode"
            },
            "state": {
              "type": "string",
              "enum": [
                "feature"
              ]
            },
            "target_branch": {
              "description": "The branch the feature lands into.",
              "type": "string"
            }
          }
        },
        {
          "description": "The branch has no record here.",
          "type": "object",
          "required": [
            "state"
          ],
          "properties": {
            "state": {
              "type": "string",
              "enum": [
                "unrecorded"
              ]
            }
          }
        },
        {
          "description": "Two writers recorded the branch differently; both values are listed.",
          "type": "object",
          "required": [
            "state",
            "values"
          ],
          "properties": {
            "state": {
              "type": "string",
              "enum": [
                "conflict"
              ]
            },
            "values": {
              "type": "array",
              "items": {
                "$ref": "#/definitions/ValueOutput"
              }
            }
          }
        },
        {
          "description": "The entry does not read as a record.",
          "type": "object",
          "required": [
            "reason",
            "state"
          ],
          "properties": {
            "reason": {
              "type": "string"
            },
            "state": {
              "type": "string",
              "enum": [
                "unreadable"
              ]
            }
          }
        },
        {
          "description": "The branch's target chain returns to it.",
          "type": "object",
          "required": [
            "chain",
            "state"
          ],
          "properties": {
            "chain": {
              "type": "array",
              "items": {
                "type": "string"
              }
            },
            "state": {
              "type": "string",
              "enum": [
                "cycle"
              ]
            }
          }
        }
      ]
    },
    "RepoBackupsOutput": {
      "type": "object",
      "required": [
        "backups",
        "path"
      ],
      "properties": {
        "backups": {
          "type": "array",
          "items": {
            "$ref": "#/definitions/BackupOutput"
          }
        },
        "path": {
          "description": "The repo as `rwv backup drop` names it: its workspace-relative path, or `projects/<name>` for the project repo.",
          "type": "string"
        }
      }
    },
    "RepoStatus": {
      "description": "Per-repo status entry.",
      "type": "object",
      "required": [
        "absolute_path",
        "lock_pinned_by_name",
        "path",
        "project",
        "relation",
        "role",
        "url"
      ],
      "properties": {
        "absolute_path": {
          "$ref": "#/definitions/WirePath"
        },
        "branch": {
          "type": [
            "string",
            "null"
          ]
        },
        "lock_pinned_by_name": {
          "description": "`true` when the lock entry holds a tag or branch name rather than a commit id; `rwv lock` records the commit id.",
          "type": "boolean"
        },
        "lock_sha": {
          "type": [
            "string",
            "null"
          ]
        },
        "mid_op": {
          "anyOf": [
            {
              "$ref": "#/definitions/MidOperation"
            },
            {
              "type": "null"
            }
          ]
        },
        "parent": {
          "description": "Recorded parent (path + per-repo parent tip) when CWD is a workweave; `None` in the primary weave (no marker, hence no recorded parent).",
          "anyOf": [
            {
              "$ref": "#/definitions/ParentInfo"
            },
            {
              "type": "null"
            }
          ]
        },
        "path": {
          "type": "string"
        },
        "project": {
          "type": "string"
        },
        "relation": {
          "$ref": "#/definitions/LockRelation"
        },
        "role": {
          "$ref": "#/definitions/Role"
        },
        "tip": {
          "type": [
            "string",
            "null"
          ]
        },
        "url": {
          "type": "string"
        }
      }
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
    "Role": {
      "description": "How freely code in this repo may be modified within the owning project.\n\n**Naming.** The variant is spelled `owned` everywhere on the wire (the manifest, `--role` CLI arguments, `--json` output). The legacy `primary` spelling — used before the rename — is **not** accepted by the parser.",
      "oneOf": [
        {
          "description": "Your code. Change freely.",
          "type": "string",
          "enum": [
            "owned"
          ]
        },
        {
          "description": "Forked upstream. Changes ideally go upstream.",
          "type": "string",
          "enum": [
            "fork"
          ]
        },
        {
          "description": "Build dependency. Changes need upstream acceptance.",
          "type": "string",
          "enum": [
            "dependency"
          ]
        },
        {
          "description": "Read-only study material. No local changes.",
          "type": "string",
          "enum": [
            "reference"
          ]
        }
      ]
    },
    "UnreadableProject": {
      "type": "object",
      "required": [
        "error",
        "project"
      ],
      "properties": {
        "error": {
          "description": "The load error, as the warning on stderr prints it.",
          "type": "string"
        },
        "project": {
          "type": "string"
        }
      }
    },
    "ValueOutput": {
      "type": "object",
      "required": [
        "mode"
      ],
      "properties": {
        "as": {
          "anyOf": [
            {
              "$ref": "#/definitions/Mode"
            },
            {
              "type": "null"
            }
          ]
        },
        "mode": {
          "$ref": "#/definitions/Mode"
        },
        "target_branch": {
          "type": [
            "string",
            "null"
          ]
        }
      }
    },
    "WirePath": {
      "description": "Absolute path, `/`-separated on every platform.",
      "type": "string"
    }
  }
}
```

## Exit codes

- `0` — status rendered successfully (independent of whether repos are
  drifted, ahead, behind, or mid-op — those are data, not failures).
- non-zero — workspace could not be resolved, or an I/O error. A project
  whose manifest does not parse is skipped with a warning, not a failure.

## Examples

Read all repos in the active project as JSON, pipe to jq:

```
rwv status --json | jq -r '.repos[] | "\(.path)\t\(.relation)"'
```

Find every repo that has drifted from its lock:

```
rwv status --json | jq '.repos[] | select(.relation != "ok" and .relation != "no-lock")'
```

Get absolute paths for all forks (e.g. to run a command across each repo):

```
rwv status --json | jq -r '.repos[] | select(.role == "fork") | .absolute_path'
```

## Common errors

- *workspace could not be resolved* — `cwd` is not inside a weave or
  workweave. Run from inside the workspace tree.
- *manifest parse failure* — a project's `rwv.toml` is malformed; fix the
  YAML and retry.
- `mid_op` set (`mid-rebase`, `mid-merge`, `mid-cherry-pick` or `mid-bisect`)
  — the repo is in the middle of a git operation. Resolve it before retrying
  mutating commands.
