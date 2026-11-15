# rwv fetch

## Purpose

Bring every remote's branches and tags into each repo's object store, drop the
remote-tracking branches a remote no longer has, and report what moved. `fetch`
is git's `fetch` for the whole project: it moves no local branch, no checkout
and no lock, so it is safe to run at any time, and what it brought is what
`rwv update` and `rwv pull` act on.

It covers the project repo and every member present on disk. A member that is
not on disk is reported as skipped; `rwv update` clones it. A `--role` or
`--repo` selector narrows the fetch to the members it names and leaves the
project repo out. Run in a workweave, `fetch` fetches the canonical clones the
workweave's worktrees share.

### A member's url remote

A member's clone needs a remote that has the member's `url`. When it has none,
`fetch` adds one before fetching, named by the url's owner segment. When that
name is already a remote with another url, `fetch` refuses before it adds or
fetches anything: `name` `taken`, naming the remote and both urls. The choice
printed beneath it configures where the repo pushes and pulls; `rwv explain
name` has the page.

### One fetch at a time per object store

An rwv that fetches into an object store holds that store's fetch claim, and
another rwv fetching into the same store waits for it. Parallel seats that fetch
at once therefore queue instead of failing on a ref lock. A claim held past the
wait budget refuses (`state-claim-held`) naming the file to delete if no rwv is
running.

### A branch the remote rewrote

When a fetch moves a remote-tracking branch to a tip that does not descend from
the old one, `fetch` reports the branch as `rewritten` and keeps the old tip
as a backup marked `superseded`, named `<remote>/<branch>/superseded-<id>`.
Nothing deletes the commits a rewrite dropped from the remote, so work built on
them stays reachable; `rwv backup list` shows the backup and `rwv backup drop`
drops it. A branch the remote deleted is `pruned`, and a prune supersedes
nothing.

### A pin no remote-tracking branch reaches

After the fetch, a member that has a remote reports each commit that some
project's live lock pins that no remote-tracking branch of the member reaches:
`pin` `off remote`, listing the locks that hold it. A lock is live when it is
at the tip of a local or remote-tracking branch of any project's repo. Choices
print only in a workspace whose checked-out branch's lock holds the pin:
publishing it, for a feature's; pinning another commit, for a declared
branch's. Elsewhere the report offers waiting and nothing else. It is a report
only: it changes no branch and does not change the exit status. `--json`
carries it as `pins_off_remote` on the member's record.

## Invocation

```
rwv fetch [--no-reference] [--role <role>...] [--repo <selector>...]
          [-j <N>] [--json] [--project <name>]
```

- `--role <role>` / `--repo <selector>` — fetch only the members they name.
  Repeat for union. Bare strings match exactly; `re:<pat>` is regex;
  `glob:<pat>` is glob.
- `--no-reference` — skip repos with `role: reference`.
- `-j <N>` — run up to `N` repos in parallel. Default: `min(nproc, 8)`.
- `--json` — emit machine-readable output (see Output below).
- `--project <name>` — operate on this project instead of the active project.
  Does not change `.rwv-active`.

Run `rwv fetch --help` for the full clap surface.

## Output

Default text output is one line per branch that moved, or `nothing moved` for a
repo whose remote-tracking branches all stayed, then a summary line:

```
rwv fetch: github/acme/server: origin/main 1a2b3c4d5e6f..9f8e7d6c5b4a
rwv fetch: github/acme/server: origin/feat rewritten 7a7a7a7a7a7a..8b8b8b8b8b8b (the previous tip is kept as superseded)
rwv fetch: github/acme/server: origin/old pruned (was 3c3c3c3c3c3c)
rwv fetch: github/acme/web: nothing moved
rwv fetch: done (2 repo(s) fetched)
```

A repo whose remote could not be completed is `unconfirmed`, the cause named on
stderr; later verbs read its remote-tracking branches as last fetched, and the
run exits `1` after trying every repo.

Under `--json`, `fetch` emits a single envelope after all repos finish, whatever
`-j` is:

```
{
  "$schema": "<url>",
  "repos": [ { "path": "...", "status": "fetched", "moved": [ ... ] }, ... ]
}
```

`repos` lists the project repo first, then the members, sorted by path. Each
record carries `path` (`projects/<name>` for the project repo), `absolute_path`,
`status` (`fetched`, `unconfirmed` or `skipped`), `moved`, and, for
`unconfirmed` or `skipped`, `message`, with `refusal` when the cause is a
refusal token `rwv explain <token>` resolves. Each entry of `moved` carries
`branch` (`origin/main`), `move` (`created`, `advanced`, `rewritten` or
`pruned`) and the tips `from` and `to` that apply.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "FetchJsonOutput",
  "description": "Top-level envelope for `rwv fetch --json`.\n\nShape: `{ \"$schema\": \"<url>\", \"repos\": [<FetchedRepoOutput>, ...] }`.",
  "type": "object",
  "required": [
    "$schema",
    "repos"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "repos": {
      "description": "The repos sorted by path: the project repo first, then the members.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/FetchedRepoOutput"
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
    "Resolution": {
      "description": "Resolved workspace coordinates for `--json` output and the plugin env-var envelope.\n\nCarries `workspace` (primary root abs path), `workweave` (the `<project>--<name>` identity the registry records), `project` (resolved project name), and `workweave_unregistered`. No `kind` or `location` field: the checkout is one of three states, and two of them are already carried by `workweave`'s presence.\n\nThe third state needs a field of its own. A workweave whose directory no registry entry names has no identity, so `workweave` is absent for it — and absent is what the primary looks like. Without `workweave_unregistered` the two serialize identically, and a consumer reading the documented meaning of that absence is told, positively, that it is at the primary checkout.\n\nResults only — provenance (which chain step resolved the project, which flag addressed the workspace) is deliberately excluded: anything in default `--json` output becomes depended on, and the assertion use case needs the result, not the mechanism. Provenance appears only in the human-facing \"target:\" line printed to stderr.\n\nIsomorphic to the plugin env-var envelope (`RWV_WORKSPACE`/`RWV_WORKWEAVE`/`RWV_WORKWEAVE_UNREGISTERED`/`RWV_PROJECT`): both surfaces are pure projections of the one resolution of the workspace, never independently computed.",
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
    "WirePath": {
      "description": "Absolute path, `/`-separated on every platform.",
      "type": "string"
    }
  }
}
```

## Exit codes

- `0` — every repo was fetched or skipped.
- `1` — at least one repo was left `unconfirmed`. Under `--json` the envelope is
  emitted before exit even on failure.

## Examples

Fetch the whole project:

```
rwv fetch
```

Fetch one member and emit the JSON envelope:

```
rwv fetch --repo github/acme/server --json
```

List the branches the remotes rewrote:

```
rwv fetch --json | jq -r '.repos[].moved[] | select(.move == "rewritten") | .branch'
```

## Common errors

- *state-claim-held* — another rwv holds the fetch claim on the object store
  past the wait budget; if no rwv is running, delete the file the message
  names.
- *unconfirmed* — the remote was unreachable, refused, or a ref could not be
  updated; the message carries git's cause. Rerun once the remote answers.
