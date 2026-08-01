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
    },
    "resolution": {
      "description": "Resolved workspace coordinates (workspace root, optional workweave identity, project). Absent when no project is resolved.",
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
    "LockRelation": {
      "description": "Relation between the current branch tip and the lock SHA.\n\nThe two clone-health variants (`Missing` / `Unreachable`) address distinct failure modes that `NoLock` previously masked:\n\n- `Missing` — the clone directory is absent from disk entirely (out-of-band `rm -rf`, never fetched, etc.). The lock entry may be fine; the repair verb is a re-clone / `rwv fetch`.\n\n- `Unreachable` — the clone directory exists but the SHA pinned in the lock is not present in the local object store (history rewritten, shallow clone, object pruned). The repair verb is a `git fetch` / `rwv fetch` to re-materialise the missing object.\n\nNeither state should be attributed to the lock file itself — surfacing them as `no-lock` misdirects operators at the wrong repair path.",
      "oneOf": [
        {
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
        {
          "description": "Clone directory is absent from disk (out-of-band removal, never fetched). Repair: re-clone / `rwv fetch`.",
          "type": "string",
          "enum": [
            "missing"
          ]
        },
        {
          "description": "Clone directory exists but the locked SHA is not in the local object store (history rewritten, shallow clone, object pruned). Repair: `git fetch` / `rwv fetch` to materialise the missing object.",
          "type": "string",
          "enum": [
            "unreachable"
          ]
        }
      ]
    },
    "ParentInfo": {
      "description": "Recorded-parent exposure for a per-repo status entry.\n\nParent identity comes from the workweave's `.rwv-workweave` marker (`parent:`), NOT from the branch name: workweave branches are stacked (`lab--wwb/lab--wwa/main`), so a constructed `basename(parent)/main` name silently breaks for a workweave whose parent is itself a workweave, and is also wrong after adoption re-points the parent to primary. Consumers that need the parent must read this field, never reconstruct it from `branch`.\n\n`path` is the recorded parent workspace path (identical for every repo in the workweave). `tip` is this specific repo's parent tip — the SHA that `git rev-parse HEAD` yields in the parent's checkout of the SAME repo — or `None` when the parent has no checkout of this repo (or HEAD is unreadable). The tip is what `git log <parent-tip>..HEAD` needs to compute the workweave's unique commits without re-deriving branch layout.",
      "type": "object",
      "required": [
        "path"
      ],
      "properties": {
        "path": {
          "description": "Recorded parent workspace path (from the `.rwv-workweave` marker).",
          "type": "string"
        },
        "tip": {
          "description": "This repo's HEAD in the parent's checkout, if resolvable.",
          "type": [
            "string",
            "null"
          ]
        }
      }
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
    },
    "Resolution": {
      "description": "Resolved workspace coordinates for `--json` output and (future) plugin env-var envelope.\n\nCarries exactly the three result fields — `workspace` (primary root abs path), `workweave` (`<project>--<name>` identity when in a workweave, absent at primary), and `project` (resolved project name). Presence of `workweave` encodes the checkout kind; no separate `kind` or `location` field is needed.\n\nResults only — provenance (which chain step resolved the project, which flag addressed the workspace) is deliberately excluded: anything in default `--json` output becomes depended on, and the assertion use case needs the result, not the mechanism. Provenance appears only in the human-facing \"target:\" line printed to stderr.\n\nIsomorphic to the plugin env-var envelope (`RWV_WORKSPACE`/`RWV_WORKWEAVE`/`RWV_PROJECT`): both surfaces are pure projections of `WorkspaceContext::resolution`, never independently computed.",
      "type": "object",
      "required": [
        "project",
        "workspace"
      ],
      "properties": {
        "project": {
          "description": "Resolved project name.",
          "type": "string"
        },
        "workspace": {
          "description": "Primary workspace root (absolute path).",
          "type": "string"
        },
        "workweave": {
          "description": "Workweave identity (`<project>--<name>`).\n\nPresent when the invocation resolved into a workweave; absent at the primary. Presence encodes the checkout kind — no separate `kind` field.",
          "type": [
            "string",
            "null"
          ]
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
- *manifest parse failure* — a project's `rwv.toml` is malformed; fix the
  YAML and retry.
- mid-op state populated (`merge`, `rebase`, `cherry-pick`, etc.) — the repo
  is in the middle of a git operation. Resolve it before retrying mutating
  commands.
