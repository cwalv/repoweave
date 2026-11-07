# rwv log

## Purpose

Read history across the repos of a project in one call: the VCS's own log in
every member and in the project repo, or, with `--against`, the commits a
checkout holds that a weave branch's pins do not. It is read-only and runs in a
primary weave or a workweave. Agents call it to see what a feature has not
landed on its target, or to resolve a revision range across every repo at once.

## Invocation

```
rwv log [<range>] [--against <W>] [--diff] [--json] [--project <name>]
```

- `<range>` is a range in the VCS's own syntax (for git, `main..feat-x`,
  `feat-x..`, `v1.2...main`). Each endpoint resolves in each repo as the VCS
  would resolve it. With neither `<range>` nor `--against`, each repo lists
  the VCS's log of its HEAD. There is no default weave branch.
- `--against <W>` lists, per member, the commits HEAD reaches that `<W>`'s pin
  for it does not, and in the project repo those `<W>` lacks. `<W>` is a branch
  of the project repo, local or remote-tracking as last fetched, whose tip
  carries `rwv.toml`; its pins are the ones in its `rwv.lock`. A member `<W>`
  does not pin is listed as `not in <W>`. A `<range>` and `--against` together
  are refused: a weave branch's pins have their own spelling.
- `--diff`, with `--against` only, shows each repo's diff from the common
  ancestor of its pin and HEAD to HEAD in place of the commit listing. Anchoring
  at the common ancestor keeps a pin that moved on after HEAD forked from
  showing as reversals of work HEAD never had.
- `--json` emits the envelope below.
- `--project <name>` reads the named project instead of the active one.

To read a workweave from elsewhere, address it with `-w`: `rwv -w
<project>--<name> log --against <T>`.

## Output

Text output is a header line, then one `=== <path> ===` section per member and
a last `=== (project) ===` for the project repo. A section lists one
`<12-character sha> <subject>` line per commit, newest first, or the diff under
`--diff`, or says nothing was found.

A `<range>` endpoint that names nothing in a repo prints `name no-such-branch`
and the endpoint in that repo's section. The other repos are still listed, and
the verb exits non-zero, printing the refusal on stderr with its choices and a
route line, `rwv explain name`.

Under `--json`, output is the envelope:

```
{
  "$schema": "<url>",
  "range": "feat-x..",
  "diff": false,
  "repos": [ { "path": "github/org/lib", "head": "...", "commits": [...] }, ... ]
}
```

`range` is present only for a range, and `against` (the weave branch) only for
`--against`. Each element of `repos` is one repo: `path` (manifest-relative,
or `(project)`), `head`, and `commits` (each `{ sha, short, subject }`). With
`--against`, `pin` is the commit `<W>` pins for the repo, or `<W>`'s own tip for
the project repo, and is absent for a member `<W>` does not pin. With a range,
`endpoints` lists the commit each endpoint names in that repo. Under `--diff`,
`diff_base` and `diff` carry the anchor and the unified diff. `note` explains a
repo that could not be read in full, such as a missing checkout. `refused` holds
the refusal records of a repo where an endpoint named nothing (`rwv explain
name`).

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "LogJsonOutput",
  "description": "Top-level envelope for `rwv log --json`.",
  "type": "object",
  "required": [
    "$schema",
    "diff",
    "repos"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "against": {
      "description": "The weave branch `--against` read the pins of.",
      "type": [
        "string",
        "null"
      ]
    },
    "diff": {
      "description": "Whether each repo carries a diff in place of its commits (`--diff`).",
      "type": "boolean"
    },
    "range": {
      "description": "The range as spelled; absent for a bare log and with `--against`.",
      "type": [
        "string",
        "null"
      ]
    },
    "repos": {
      "description": "Every member of the workspace's project, then the project repo.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/LogRepo"
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
    "LogEndpoint": {
      "type": "object",
      "required": [
        "id",
        "spelled"
      ],
      "properties": {
        "id": {
          "$ref": "#/definitions/CommitId"
        },
        "spelled": {
          "description": "The endpoint as spelled in the range.",
          "type": "string"
        }
      }
    },
    "LogRepo": {
      "type": "object",
      "required": [
        "commits",
        "path"
      ],
      "properties": {
        "commits": {
          "description": "The commits, newest first; empty with `--diff`.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/LogCommit"
          }
        },
        "diff": {
          "description": "With `--diff`, the unified diff from `diff_base` to HEAD.",
          "type": [
            "string",
            "null"
          ]
        },
        "diff_base": {
          "description": "With `--diff`, the common ancestor of the pin and HEAD that the diff starts from.",
          "anyOf": [
            {
              "$ref": "#/definitions/CommitId"
            },
            {
              "type": "null"
            }
          ]
        },
        "endpoints": {
          "description": "With a range, the commit each endpoint of the range names in this repo.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/LogEndpoint"
          }
        },
        "head": {
          "description": "The checkout's HEAD, if readable.",
          "anyOf": [
            {
              "$ref": "#/definitions/CommitId"
            },
            {
              "type": "null"
            }
          ]
        },
        "note": {
          "description": "Why this repo could not be listed in full. The repo is still listed, so the gap is seen rather than silently omitted.",
          "type": [
            "string",
            "null"
          ]
        },
        "path": {
          "description": "The repo's manifest-relative path, or `(project)` for the project repo.",
          "allOf": [
            {
              "$ref": "#/definitions/String"
            }
          ]
        },
        "pin": {
          "description": "With `--against`, the commit the weave branch pins for this repo: the branch's own tip for the project repo. Absent for a member the branch does not pin.",
          "anyOf": [
            {
              "$ref": "#/definitions/CommitId"
            },
            {
              "type": "null"
            }
          ]
        },
        "refused": {
          "description": "The state that made the verb exit non-zero in this repo.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/JudgedOutput"
          }
        }
      }
    },
    "String": {
      "type": "string"
    },
    "WirePath": {
      "description": "Absolute path, `/`-separated on every platform.",
      "type": "string"
    }
  }
}
```

## Exit codes

- `0` — every repo was listed. A repo carrying a `note` was still listed.
- non-zero — a `<range>` endpoint names nothing in at least one repo (the
  others are still listed), `--against` names no weave branch, or the workspace
  could not be resolved.

## Examples

What the feature in this workspace has not landed on `main`, per repo:

```
rwv log --against main
```

The same as a diff, anchored where each pin meets HEAD:

```
rwv log --against main --diff
```

A feature's commits in every repo that has a branch of that name:

```
rwv log main..feat-x
```

Read a workweave's unlanded commits as JSON from the primary:

```
rwv -w web-app--fix-auth log --against main --json | jq -r '.repos[] | "\(.path) \(.commits | length)"'
```

## Common errors

- *`name no-such-branch`* — a range endpoint resolves to nothing in that repo,
  or `--against` names no weave branch. Check the spelling; the branch may be
  published but not yet fetched.
- *`--diff` without `--against`* — a diff is read against a weave branch's
  pins, so `--diff` needs `--against`.
