# rwv doctor

## Purpose

Run workspace-wide convention checks: orphaned clones, dangling references,
stale locks, workweave drift, index drift, working-tree drift, missing
replay-exclusion attributes, and integration-specific issues. `doctor`
exists so agents and humans can verify the workspace is structurally sound
before running mutating commands.

The check is intentionally pure: filesystem scanning happens up front, then
a closed enum (`CheckViolation`) is reduced to violations. Each variant has
a stable kebab-case `kind` tag — agents key off `kind` to dispatch
follow-up actions.

## Invocation

```
rwv doctor [--locked] [--json] [--fix]
```

- `--locked` exits zero iff every repo's tip matches its `rwv.lock`
  entry. Prints per-repo `ok` / `tip ≠ lock` lines to stdout. Useful
  as a scriptable precondition before `rwv sync`. Mutually exclusive
  with `--fix` and `--json`.
- `--json` emits machine-readable output (see Output below). Mutually
  exclusive with `--locked` and `--fix`.
- `--fix` attempts auto-remediation for variants that are safe to fix:
  index drift where the displaced tree is a known ancestor, working-tree
  drift where on-disk content matches a known blob, missing
  `rwv.lock merge=ours` replay-exclusion, and legacy `role: primary`
  manifest spellings (rewritten to `role: owned` in place — preserves
  comments and key order). Idempotent. Mutually exclusive with
  `--locked` and `--json`.

Run `rwv --help doctor` for the full clap surface.

## Output

Default text output is one human-readable line per violation, grouped by
severity. Under `--json`, output is the envelope:

```
{
  "$schema": "<url>",
  "violations": [ { "kind": "...", ... }, ... ]
}
```

The `$schema` URL points to the committed schema artifact. Variants are
discriminated by the `kind` tag — `orphaned-clone`, `dangling-reference`,
`missing-role`, `stale-lock`, `workweave-drift`, `index-drift`,
`working-tree-drift`, `missing-replay-exclusion`, `legacy-role-primary`.
Every per-repo variant carries `path` (manifest-relative) and
`absolute_path` (fully resolved). Variants with subkinds
(`workweave-drift`, `index-drift`, `working-tree-drift`) carry an
additional `sub_kind` field. `legacy-role-primary` carries `project` and
`manifest_path` so the caller can locate the file `--fix` will rewrite.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "DoctorEnvelope",
  "description": "Generator-local mirror of the `rwv doctor --json` envelope. The runtime envelope in `src/check.rs` is built via `serde_json::json!` (no real struct exists). Mirroring it here avoids touching Agent A's file just to pull a schemars derive.",
  "type": "object",
  "required": [
    "$schema",
    "violations"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "violations": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/ViolationOutput"
      }
    }
  },
  "definitions": {
    "DriftKind": {
      "oneOf": [
        {
          "description": "Manifest lists it, but no worktree exists.",
          "type": "string",
          "enum": [
            "missing"
          ]
        },
        {
          "description": "Worktree exists, but manifest doesn't list it.",
          "type": "string",
          "enum": [
            "extra"
          ]
        }
      ]
    },
    "IndexDriftKind": {
      "description": "How a stale index should be treated.",
      "oneOf": [
        {
          "description": "Index tree matches the tree of some recent ancestor commit. Safe to auto-fix with `git reset` — the displaced tree is permanently in the DAG.",
          "type": "string",
          "enum": [
            "safe-to-fix"
          ]
        },
        {
          "description": "Index tree is not found in recent ancestor trees. The user has live staged content; `--fix` must not touch this.",
          "type": "string",
          "enum": [
            "live-staged"
          ]
        }
      ]
    },
    "ViolationOutput": {
      "description": "One violation as it appears in `rwv doctor --json` output.",
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
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "orphaned-clone"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "dangling-reference"
              ]
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "missing-role"
              ]
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "actual",
            "kind",
            "locked",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "actual": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "stale-lock"
              ]
            },
            "locked": {
              "type": "string"
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "sub_kind",
            "workweave"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "workweave-drift"
              ]
            },
            "path": {
              "type": "string"
            },
            "sub_kind": {
              "$ref": "#/definitions/DriftKind"
            },
            "workweave": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "sub_kind"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "index-drift"
              ]
            },
            "path": {
              "type": "string"
            },
            "sub_kind": {
              "$ref": "#/definitions/IndexDriftKind"
            },
            "workweave": {
              "description": "`None` for the primary weave.",
              "type": [
                "string",
                "null"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "sub_kind"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "working-tree-drift"
              ]
            },
            "path": {
              "type": "string"
            },
            "sub_kind": {
              "$ref": "#/definitions/WorkingTreeDriftKind"
            },
            "workweave": {
              "description": "`None` for the primary weave.",
              "type": [
                "string",
                "null"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "project"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "missing-replay-exclusion"
              ]
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "manifest_path",
            "project"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "legacy-role-primary"
              ]
            },
            "manifest_path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "error",
            "kind",
            "manifest_path",
            "project"
          ],
          "properties": {
            "error": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "unparseable-project"
              ]
            },
            "manifest_path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        }
      ]
    },
    "WorkingTreeDriftKind": {
      "description": "How stale working-tree files should be treated.",
      "oneOf": [
        {
          "description": "All modified files' on-disk content matches blobs reachable from HEAD. Safe to restore with `git checkout HEAD -- <files>` — no work is lost.",
          "type": "string",
          "enum": [
            "safe-to-fix"
          ]
        },
        {
          "description": "At least one modified file has on-disk content not found in any recent ancestor's tree. The user has active edits; `--fix` must not touch this.",
          "type": "string",
          "enum": [
            "live-edits"
          ]
        }
      ]
    }
  }
}
```

## Exit codes

- `0` — no violations found.
- non-zero — violations found, or an error occurred resolving the
  workspace.

## Examples

Get a JSON report of all violations:

```
rwv doctor --json
```

Find every stale lock and the paths involved:

```
rwv doctor --json | jq '.violations[] | select(.kind == "stale-lock")'
```

Auto-fix safe drift (index trees that match a known ancestor) and
migrate any manifests still using the legacy `role: primary` spelling:

```
rwv doctor --fix
```

## Common errors

- *missing-replay-exclusion* on a project repo — the project repo lacks
  `rwv.lock merge=ours` in `.gitattributes`. Either run `rwv doctor --fix`
  to append it, or add the line manually.
- *legacy-role-primary* — a project `rwv.yaml` still uses `role: primary`
  (renamed to `role: owned`; the back-compat alias has since been dropped).
  Run `rwv doctor --fix` to migrate every affected manifest in place;
  comments and key order are preserved.
- *index-drift* with `sub_kind: live-staged` — the user has staged content
  that doesn't match a known tree. `--fix` will refuse; resolve manually.
- *orphaned-clone* — a directory under a registry path that isn't listed in
  any `rwv.yaml`. Either add it to a manifest or remove it.
