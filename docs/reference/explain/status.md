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
```

- `--json` emits machine-readable output (see Output below).
- `--project <name>` reports status for the named project without changing
  the active project (`.rwv-active` is untouched).

Run `rwv --help status` for the full clap surface.

## Output

Default text output is a fixed-column table: one repo per row, with
`path branch tip lock: <sha> [<relation>] [<mid-op>]`.

Under `--json`, output is the envelope:

```
{
  "$schema": "<url>",
  "repos": [ { "path": "...", ... }, ... ]
}
```

The `$schema` URL points to the committed schema artifact. Each element of
`repos` is a `RepoStatus` record: `path` (manifest-relative), `absolute_path`
(fully resolved), `branch`, `tip`, `lock_sha`, `relation`
(`ok`/`ahead`/`behind`/`diverged`/`no_lock`/`unknown`), `mid_op`, `role`,
`url`, `project`.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "StatusJsonOutput",
  "description": "Top-level envelope for `rwv status --json`. Matches the convention adopted by doctor (`$schema` + `violations`) and sync (`$schema` + `outcomes`): `{ \"$schema\": \"<url>\", \"repos\": [<RepoStatus>, ...] }`.",
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
      "type": "array",
      "items": {
        "$ref": "#/definitions/RepoStatus"
      }
    }
  },
  "definitions": {
    "LockRelation": {
      "description": "Relation between the current branch tip and the lock SHA.",
      "type": "string",
      "enum": [
        "ok",
        "ahead",
        "behind",
        "diverged",
        "no_lock",
        "unknown"
      ]
    },
    "RepoStatus": {
      "description": "Per-repo status entry.",
      "type": "object",
      "required": [
        "absolute_path",
        "path",
        "project",
        "relation",
        "role",
        "url"
      ],
      "properties": {
        "absolute_path": {
          "type": "string"
        },
        "branch": {
          "type": [
            "string",
            "null"
          ]
        },
        "lock_sha": {
          "type": [
            "string",
            "null"
          ]
        },
        "mid_op": {
          "type": [
            "string",
            "null"
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
          "type": "string"
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
    }
  }
}
```

## Exit codes

- `0` — status rendered successfully (independent of whether repos are
  drifted, ahead, behind, or mid-op — those are data, not failures).
- non-zero — workspace could not be resolved, manifest parse failure, or an
  I/O error.

## Examples

Read all repos in the active project as JSON, pipe to jq:

```
rwv status --json | jq -r '.repos[] | "\(.path)\t\(.relation)"'
```

Find every repo that has drifted from its lock:

```
rwv status --json | jq '.repos[] | select(.relation != "ok" and .relation != "no_lock")'
```

Get absolute paths for all forks (e.g. to run a command across each repo):

```
rwv status --json | jq -r '.repos[] | select(.role == "fork") | .absolute_path'
```

## Common errors

- *workspace could not be resolved* — `cwd` is not inside a weave or
  workweave. Run from inside the workspace tree.
- *manifest parse failure* — a project's `rwv.yaml` is malformed; fix the
  YAML and retry.
- mid-op state populated (`merge`, `rebase`, `cherry-pick`, etc.) — the repo
  is in the middle of a git operation. Resolve it before retrying mutating
  commands.
