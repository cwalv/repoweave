# rwv clone

## Purpose

Clone a project into the workspace and bring up every member repo at the
commit the project's `rwv.lock` pins. `clone` reproduces a recorded state: it
never advances a pin, and it never writes the lock.

Given a `source` (a URL — `file://` for a local remote — or `owner/repo`
shorthand), `clone` clones the project repo into `projects/<name>/`, reads its
`rwv.lock`, and clones every member repo. Each member is born attached to the
local counterpart of the branch its `rwv.toml` entry declares, positioned at
the commit the lock pins for it. When no project is active, `clone` then
activates the project it cloned, as `rwv activate` would.

`clone` reads the records the remote holds for the project's branches first,
then records the branch it leaves checked out as `declared` when they hold none
for it (`rwv branch list` shows it).

A plain path is not a source: it names no remote, so `clone` refuses
(`unresolvable-repo-source`) and names the two spellings that are — `rwv clone
file://<absolute path>` to clone from a local remote, and `rwv update --project
<name>` for a project already at `projects/<name>/`.

### A member the lock cannot place

A clone completes whatever the lock holds. A member that the lock gives no
commit to be at is cloned on the branch its entry declares, at the tip of that
branch on the remote, and listed:

- **`pin` `none`** — the lock has no entry for the member, or there is no
  `rwv.lock` at all. A lock file that is absent pins nothing, so every member
  is listed.
- **`pin` `unreachable`** — the lock pins a commit that no ref in the clone
  reaches: the commit was rewritten away, or sits on a branch the remote no
  longer has.

`clone` writes no lock entry for either. `rwv explain pin` serves the page for
the state, with the choices to settle it. This is the check a CI job runs
where it once passed `--frozen`: a clone that exits `0` placed every member at
its pin.

## Invocation

```
rwv clone <source> [--allow-non-empty-dir] [--no-reference]
          [-j <N>] [--json] [--no-materialize]
```

- `<source>` — full clone URL (`file://` for a local remote) or `owner/repo` /
  `registry/owner/repo` shorthand.
- `--allow-non-empty-dir` — clone into a non-empty directory that is not
  already a workspace (normally an error).
- `--no-reference` — skip repos with `role: reference` (useful when mirrors
  are offline). A skipped member is not listed.
- `-j <N>` — run up to `N` per-member clones in parallel. Default:
  `min(nproc, 8)`.
- `--json` — emit machine-readable output (see Output below).
- `--no-materialize` — a clone that activates a project runs the integration
  install hooks; this skips them, and `rwv materialize` runs them later. The
  activation itself still happens.

Run `rwv clone --help` for the full clap surface.

## Output

Default text output is one line per member on the standard text channel,
saying whether it was cloned at its pin or at its branch's tip. Errors
(including per-member failures) are printed to stderr. A summary line reports
how many repos are ready. When members were left, each is then printed as
`rwv clone: <repo>: pin none` or `rwv clone: <repo>: pin unreachable`, with the
choices that settle it and the route line `rwv explain pin`.

Under `--json`, `clone` emits a single JSON envelope after all repos finish,
whatever `-j` is:

```
{
  "$schema": "<url>",
  "outcomes": [ { "path": "...", ... }, ... ],
  "left": [ { "path": "...", "axis": "pin", "state": "none", ... }, ... ],
  "placements": [ { "path": "...", "absolute_path": "...", "files": { ... } }, ... ],
  "skipped": [ { "effect": "install-hooks", "reason": "no-materialize", "remedy": "rwv materialize" }, ... ]
}
```

`outcomes` lists the repos sorted by repo path, however the workers finished.
Each record has the shape: `path` (manifest-relative), `absolute_path`
(on-disk), `status` (`ok` / `skipped` / `failed`), `message` (human-readable
detail; absent for `ok`, present for `skipped` and `failed`), and, on a
`failed` record whose cause is a refusal, `refusal`: its token, which `rwv
explain <token>` resolves.

`left` lists the members the clone completed without a commit to place them
at, one record each: `path`, `axis` (`pin`), `state` (`none` or `unreachable`)
and the `choices` that settle it. It is empty exactly when the exit status is
`0`.

File counts are never on an outcome. Each checkout whose files the clone
placed gets a placement record: `path`, `absolute_path` (the checkout the
files are in), and `files` (`reflinked`, `hardlinked`, `written`). The
envelope lists them under `placements`, empty when nothing was placed.

A clone that does not activate a project says so in one line naming the
command that does it: `rwv clone: skipping auto-activate (project '<active>'
already active); `rwv activate <cloned>` selects it and runs its install
hooks` (on stdout in text mode, stderr under `--json`). An activation run with
`--no-materialize` says: `[skipped] core: the install hooks did not run
(--no-materialize); `rwv materialize` runs them`. A hook whose tool is not on
`PATH` gets its `tool-missing` warning. The envelope records the same facts in
`skipped`. Each record carries `effect` (`install-hooks`,
`integration-files`, `weave-root-links` or `activation`), `integration`
(present only when one integration's part was skipped), `reason`
(`no-materialize`, `inside-workweave` or `narrowed`, or an object naming what
it concerns: `{"tool-missing": {"tool": …}}`, `{"not-presented": {"project":
…}}`, `{"project-already-active": {"project": …}}`) and `remedy`, the command
that performs the effect.

The `$schema` URL points to the committed schema artifact.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "CloneJsonOutput",
  "description": "Top-level envelope for `rwv clone --json`.\n\nShape: `{ \"$schema\": \"<url>\", \"outcomes\": [...], \"left\": [...] }`.",
  "type": "object",
  "required": [
    "$schema",
    "left",
    "outcomes",
    "placements"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "left": {
      "description": "The members the clone completed without a commit to place them at: the lock has no entry for them (`pin` `none`), or pins a commit no ref in the clone reaches (`pin` `unreachable`). Empty, and the exit status `0`, when every member sits at its pin.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/JudgedOutput"
      }
    },
    "outcomes": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/MemberOutcomeOutput"
      }
    },
    "placements": {
      "description": "How the files of each checkout the clone created were placed. Empty, not absent, when it placed none.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/PlacementOutput"
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
    "skipped": {
      "description": "The materialization side effects this clone skipped, each naming the command that performs it. Empty, not absent, when it skipped none.",
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
    "WirePath": {
      "description": "Absolute path, `/`-separated on every platform.",
      "type": "string"
    }
  }
}
```

## Exit codes

- `0` — the project repo and every non-reference member repo cloned, and every
  member sits at its pin. Reference repos that failed are warnings, not
  errors.
- `3` — the clone completed, and left at least one member without a commit to
  place it at (`pin` `none` or `pin` `unreachable`). Everything else was done;
  `rwv explain pin` serves the page for the state. Under `--json` the envelope
  is complete and `left` names the members.
- `1` — the project repo failed to clone, at least one non-reference repo
  failed, or the current directory is not a workspace and
  `--allow-non-empty-dir` was not passed. Under `--json` the envelope is
  emitted before exit even on failure, so consumers always get parseable
  output.
- `2` — the arguments did not parse.

## Examples

Clone a project from a GitHub shorthand:

```
rwv clone owner/myproject
```

Same, with a full URL:

```
rwv clone https://github.com/owner/myproject.git
```

CI — fail unless every member sits at the commit the lock pins:

```
rwv clone owner/myproject
```

Skip reference repos (faster when mirrors are offline):

```
rwv clone owner/myproject --no-reference
```

Clone with 4 workers and emit the JSON envelope:

```
rwv clone owner/myproject -j 4 --json
```

Get the paths of the members the lock left unpinned:

```
rwv clone owner/myproject --json | jq -r '.left[].path'
```

## Common errors

- *cannot clone project 'X': projects/X already exists* — if that is the
  project you meant, `rwv update --project X` brings up its members in place;
  otherwise run `rwv clone <source>` in an empty directory, which becomes a
  weave of its own.
- *no repoweave workspace found and … is not empty* — the current directory
  is not a workspace; use `--allow-non-empty-dir` to clone here anyway.
- *is a plain path, which names no remote* — spell a local remote as
  `file://<absolute path>`, or run `rwv update --project <name>` for a project
  already in the weave.
- *repository not found* — the source URL is wrong or the remote is private
  and credentials aren't configured.
- *branch … does not exist on the remote* — a member's `branch` names a branch
  the remote does not have, so there is no tip to place the member at.
- *network-error* — connectivity issue; retry.
