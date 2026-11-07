# rwv pull

## Purpose

Take in what the remotes hold: run the pruning fetch of every repo, fast-forward
the project branch to its upstream, then run `rwv update`, so the members are
aligned to the lock the project branch now holds.

`pull` only ever moves the project branch forward. A branch that is behind its
upstream takes it in; one that is at its upstream, ahead of it, or has no
upstream has nothing to take in; one that has diverged — each side holding
commits the other lacks — is refused with nothing changed, as `push-branch`
`diverged`.

1. **Fetch.** The same pruning fetch as `rwv fetch`, of the project repo and
   every member. A repo the fetch could not complete refuses the pull before
   anything is taken in; `rwv update` aligns the members to the lock as it
   stands without the network.
2. **The project branch.** Before it moves, the project repo is judged for
   uncommitted changes to tracked files and a VCS operation in progress
   (`edits`); either refuses. A branch behind its upstream then fast-forwards,
   and `pull` reports what the commits it took in changed:
   - `re-pinned <member>: <was> -> <now>` for each member whose pin the pulled
     lock moves to a commit that does not descend from the old one;
   - `member-changed <member>: …` for each member entry the pulled `rwv.toml`
     adds, removes or changes, with each changed field and both values.
3. **Update.** `rwv update`, narrowed by `--role` and `--repo`: members missing
   on disk are cloned, and the rest are aligned with the branch as it now
   stands: each member where its record and the lock, as the branch now holds
   them, say. It writes no lock.

## Invocation

```
rwv pull [--project <name>] [--role <role>]...
         [--repo <selector>]... [-j <N>] [--json] [--no-materialize]
```

- `--role` / `--repo` — limit the update of step 3 to a subset of the manifest.
  Repeat for union. The fetch of step 1 covers every repo.
- `--project <name>` — operate on this project instead of the active project.
  Does not change `.rwv-active`. A member `<name>` shares with the active
  project that is not aligned for `<name>` refuses the update of step 3, as for
  `rwv update`.
- `-j <N>` — run up to `N` per-repo workers in parallel. Default:
  `min(nproc, 8)`.
- `--json` — emit machine-readable output (see Output below).
- `--no-materialize` — as for `rwv update`.

Run `rwv pull --help` for the full clap surface.

## Output

Default text output is the fetch's lines (`rwv fetch` documents them), one line
for the project branch (`rwv pull: projects/<name>: took '<branch>' from <was>
to <now>`, or the reason there was nothing to take in), the reports of step 2,
and the lines of the update.

Under `--json`, `pull` emits a single envelope when every step is done:

```
{
  "$schema": "<url>",
  "fetched":  [ { "path": "...", "status": "fetched", "moved": [ ... ] }, ... ],
  "project":  { "path": "projects/<name>", "branch": "main", "result": "advanced", ... },
  "outcomes": [ { "path": "...", "status": "ok" }, ... ],
  "placements": [ ... ],
  "skipped": [ ... ]
}
```

`fetched` holds the records of `rwv fetch --json`'s `repos`, and `outcomes`,
`placements` and `skipped` the update's, as `rwv update --json` has them.
`project` carries `path`, `branch` (absent when the project repo is detached),
`result` (`advanced`, `even`, `ahead`, `no-upstream` or `detached`), `from` and
`to` for `advanced`, `re_pinned` (`path`, `from`, `to`) and `member_changes`
(`path`, `change`, `fields`).

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "PullJsonOutput",
  "description": "Top-level envelope for `rwv pull --json`.\n\nThe update's records are the envelope's own, as `rwv update --json` has them.",
  "type": "object",
  "required": [
    "$schema",
    "fetched",
    "judged",
    "outcomes",
    "placements",
    "project"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "fetched": {
      "description": "The pruning fetch of each repo, as `rwv fetch --json` reports it.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/FetchedRepoOutput"
      }
    },
    "judged": {
      "description": "Each member the update left where it was, judged.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/JudgedOutput"
      }
    },
    "outcomes": {
      "description": "The update's per-member outcomes.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/MemberOutcomeOutput"
      }
    },
    "placements": {
      "description": "How the files of each checkout the update moved or created were placed. Empty, not absent, when it placed none.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/PlacementOutput"
      }
    },
    "project": {
      "description": "What taking in the project branch did.",
      "allOf": [
        {
          "$ref": "#/definitions/ProjectTakeInOutput"
        }
      ]
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
    "skipped": {
      "description": "The materialization side effects the update skipped, each naming the command that performs it. Empty, not absent, when it skipped none.",
      "default": [],
      "type": "array",
      "items": {
        "$ref": "#/definitions/SkippedEffect"
      }
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
          "description": "Where a workspace's checkouts sit against where the checked-out branch's record says they should.",
          "type": "string",
          "enum": [
            "alignment"
          ]
        }
      ]
    },
    "BranchMoveKind": {
      "description": "How a remote-tracking branch moved, as `rwv fetch --json` spells it.",
      "oneOf": [
        {
          "description": "The remote has the branch and the repo had no record of it.",
          "type": "string",
          "enum": [
            "created"
          ]
        },
        {
          "description": "The new tip descends from the old one.",
          "type": "string",
          "enum": [
            "advanced"
          ]
        },
        {
          "description": "The new tip does not descend from the old one. The old tip is kept as the branch's superseded tip.",
          "type": "string",
          "enum": [
            "rewritten"
          ]
        },
        {
          "description": "The remote no longer has the branch.",
          "type": "string",
          "enum": [
            "pruned"
          ]
        }
      ]
    },
    "BranchMoveOutput": {
      "description": "One remote-tracking branch a fetch moved.",
      "type": "object",
      "required": [
        "branch",
        "move"
      ],
      "properties": {
        "branch": {
          "description": "The branch, qualified by its remote (`origin/main`).",
          "type": "string"
        },
        "from": {
          "description": "The tip before the fetch. Absent for a `created` branch.",
          "type": [
            "string",
            "null"
          ]
        },
        "move": {
          "$ref": "#/definitions/BranchMoveKind"
        },
        "to": {
          "description": "The tip after the fetch. Absent for a `pruned` branch.",
          "type": [
            "string",
            "null"
          ]
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
    "Effect": {
      "description": "The side effect that did not happen.",
      "oneOf": [
        {
          "description": "The integrations' install hooks (`npm install`, `uv sync`, `cargo fetch`, …).",
          "type": "string",
          "enum": [
            "install-hooks"
          ]
        },
        {
          "description": "Regenerating the integrations' managed and generated files.",
          "type": "string",
          "enum": [
            "integration-files"
          ]
        },
        {
          "description": "Refreshing the weave root's links onto the project's files.",
          "type": "string",
          "enum": [
            "weave-root-links"
          ]
        },
        {
          "description": "Activating a cloned project: selecting it, surfacing its links and running its install hooks.",
          "type": "string",
          "enum": [
            "activation"
          ]
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
    "FetchStatus": {
      "description": "How a repo's fetch ended.",
      "oneOf": [
        {
          "description": "Every remote answered.",
          "type": "string",
          "enum": [
            "fetched"
          ]
        },
        {
          "description": "A remote could not be completed, the cause named in `message`. Later verbs read the repo's remote-tracking branches as last fetched.",
          "type": "string",
          "enum": [
            "unconfirmed"
          ]
        },
        {
          "description": "The repo was not fetched, the reason named in `message`.",
          "type": "string",
          "enum": [
            "skipped"
          ]
        }
      ]
    },
    "FetchedRepoOutput": {
      "description": "One repo's record in `rwv fetch --json`.",
      "type": "object",
      "required": [
        "absolute_path",
        "moved",
        "path",
        "pins_off_remote",
        "status"
      ],
      "properties": {
        "absolute_path": {
          "$ref": "#/definitions/WirePath"
        },
        "message": {
          "description": "The cause of an `unconfirmed` fetch, or the reason for a `skipped` one.",
          "type": [
            "string",
            "null"
          ]
        },
        "moved": {
          "description": "Each remote-tracking branch that moved, by branch.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/BranchMoveOutput"
          }
        },
        "path": {
          "description": "The repo's path: a member's manifest path, or `projects/<name>` for the project repo.",
          "type": "string"
        },
        "pins_off_remote": {
          "description": "Each commit some live lock pins that no remote-tracking branch of this repo reaches after the fetch (`pin` off remote), with the locks that hold it and the choices that apply in this workspace.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/JudgedOutput"
          }
        },
        "refusal": {
          "description": "The refusal token of an `unconfirmed` repo whose cause is a refusal, resolvable with `rwv explain <token>`.",
          "type": [
            "string",
            "null"
          ]
        },
        "status": {
          "$ref": "#/definitions/FetchStatus"
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
    "MemberOutcomeOutput": {
      "description": "Per-member outcome record for `rwv update --json` and `rwv clone --json`.\n\n`status` is one of `\"ok\"`, `\"skipped\"`, or `\"failed\"`. `message` carries a human-readable description of the outcome (always present for `\"failed\"`; present for `\"skipped\"` to say why; `null` for `\"ok\"`).",
      "type": "object",
      "required": [
        "absolute_path",
        "path",
        "status"
      ],
      "properties": {
        "absolute_path": {
          "$ref": "#/definitions/WirePath"
        },
        "facts": {
          "description": "What holds for the member in its expected state, such as the branch it would sit on being checked out in another workspace.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/FactOutput"
          }
        },
        "message": {
          "type": [
            "string",
            "null"
          ]
        },
        "moved_from": {
          "description": "The branch the checkout was moved off, when it was neither a declared branch nor named after a recorded weave branch.",
          "type": [
            "string",
            "null"
          ]
        },
        "path": {
          "type": "string"
        },
        "refusal": {
          "description": "The refusal token of a `failed` repo, resolvable with `rwv explain <token>`. Absent when the failure carries no classified refusal.",
          "type": [
            "string",
            "null"
          ]
        },
        "status": {
          "$ref": "#/definitions/MemberOutcomeStatus"
        }
      }
    },
    "MemberOutcomeStatus": {
      "description": "Status discriminant for `MemberOutcomeOutput`.",
      "type": "string",
      "enum": [
        "ok",
        "skipped",
        "failed"
      ]
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
    "ProjectTakeInOutput": {
      "description": "What `rwv pull` did to the project branch.",
      "type": "object",
      "required": [
        "member_changes",
        "path",
        "re_pinned",
        "result"
      ],
      "properties": {
        "branch": {
          "description": "The branch the project repo is on. Absent when it is detached.",
          "type": [
            "string",
            "null"
          ]
        },
        "from": {
          "description": "The branch's tip before. Present for `advanced`.",
          "type": [
            "string",
            "null"
          ]
        },
        "member_changes": {
          "description": "Member entries the pulled `rwv.toml` added, removed or changed.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/MemberChangeOutput"
          }
        },
        "path": {
          "description": "The project repo's path, `projects/<name>`.",
          "type": "string"
        },
        "re_pinned": {
          "description": "Members the pulled lock re-pinned backward.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/RePinnedOutput"
          }
        },
        "result": {
          "$ref": "#/definitions/TakeIn"
        },
        "to": {
          "description": "The branch's tip after. Present for `advanced`.",
          "type": [
            "string",
            "null"
          ]
        }
      }
    },
    "RePinnedOutput": {
      "description": "A member whose pin the pulled lock moved backward, or to a commit that does not descend from the old one.",
      "type": "object",
      "required": [
        "from",
        "path",
        "to"
      ],
      "properties": {
        "from": {
          "type": "string"
        },
        "path": {
          "type": "string"
        },
        "to": {
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
    "SkipReason": {
      "description": "Why the effect did not happen.",
      "oneOf": [
        {
          "description": "`--no-materialize` was passed.",
          "type": "string",
          "enum": [
            "no-materialize"
          ]
        },
        {
          "description": "An authoring verb ran inside a workweave, where it runs no hooks.",
          "type": "string",
          "enum": [
            "inside-workweave"
          ]
        },
        {
          "description": "The CLI the integration's hook drives is not on `PATH`.",
          "type": "object",
          "required": [
            "tool-missing"
          ],
          "properties": {
            "tool-missing": {
              "type": "object",
              "required": [
                "tool"
              ],
              "properties": {
                "tool": {
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "The verb acted on a project the weave root does not present.",
          "type": "object",
          "required": [
            "not-presented"
          ],
          "properties": {
            "not-presented": {
              "type": "object",
              "required": [
                "project"
              ],
              "properties": {
                "project": {
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "`rwv update --remote` was narrowed by `--role` or `--repo`.",
          "type": "string",
          "enum": [
            "narrowed"
          ]
        },
        {
          "description": "A project was already active, so the verb selected none.",
          "type": "object",
          "required": [
            "project-already-active"
          ],
          "properties": {
            "project-already-active": {
              "type": "object",
              "required": [
                "project"
              ],
              "properties": {
                "project": {
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        }
      ]
    },
    "SkippedEffect": {
      "description": "One materialization side effect a verb skipped on this run.",
      "type": "object",
      "required": [
        "effect",
        "reason",
        "remedy"
      ],
      "properties": {
        "effect": {
          "$ref": "#/definitions/Effect"
        },
        "integration": {
          "description": "The integration whose part of `effect` was skipped; absent when the whole effect was.",
          "type": [
            "string",
            "null"
          ]
        },
        "reason": {
          "$ref": "#/definitions/SkipReason"
        },
        "remedy": {
          "description": "The command that performs the effect.",
          "type": "string"
        }
      }
    },
    "TakeIn": {
      "description": "What taking in the project branch came to.",
      "oneOf": [
        {
          "description": "The branch was behind its upstream and now is at it.",
          "type": "string",
          "enum": [
            "advanced"
          ]
        },
        {
          "description": "The branch is at its upstream.",
          "type": "string",
          "enum": [
            "even"
          ]
        },
        {
          "description": "The branch holds commits its upstream lacks, and nothing it has not.",
          "type": "string",
          "enum": [
            "ahead"
          ]
        },
        {
          "description": "The branch has no upstream, or its upstream is not on the remote.",
          "type": "string",
          "enum": [
            "no-upstream"
          ]
        },
        {
          "description": "The project repo is on no branch.",
          "type": "string",
          "enum": [
            "detached"
          ]
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

- `0` — every step completed.
- `1` — a refusal stopped the pull: a repo the fetch could not complete, a
  project branch that has diverged, uncommitted changes the fast-forward would
  disturb, or any refusal of the update.

## Examples

Take in the project branch and align the members to its lock:

```
rwv pull
```

Narrow the update to one member:

```
rwv pull --repo github/acme/server
```

## Common errors

- *push-branch diverged* — the project branch and its upstream each hold
  commits the other lacks. `pull` takes in a branch only by fast-forward.
- *edits dirty* — the project repo has uncommitted changes to tracked files;
  commit or stash them, then rerun.
- *pull could not complete the fetch* — a remote was unreachable or refused;
  `rwv update` aligns the members to the lock as it stands.
- *target unrecorded* — the checked-out branch of the project repo has no record
  of how it is used, so this verb, which aligns members, refuses before changing
  anything. The record may be published but not yet fetched (`rwv fetch`);
  otherwise say how the branch is used with `rwv branch set-mode`.
