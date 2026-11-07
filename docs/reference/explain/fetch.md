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

### One fetch at a time per object store

An rwv that fetches into an object store holds that store's fetch claim, and
another rwv fetching into the same store waits for it. Parallel seats that fetch
at once therefore queue instead of failing on a ref lock. A claim held past the
wait budget refuses (`state-claim-held`) naming the file to delete if no rwv is
running.

### A branch the remote rewrote

When a fetch moves a remote-tracking branch to a tip that does not descend from
the old one, `fetch` reports the branch as `rewritten` and keeps the old tip
under `refs/rwv/superseded/<remote>/<branch>/<old tip>`. Nothing deletes the
commits a rewrite dropped from the remote, so work built on them stays
reachable. A branch the remote deleted is `pruned`, and a prune supersedes
nothing.

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
