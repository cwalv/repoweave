# rwv sync

## Purpose

Reconcile each repo in the active project (or all projects in a weave) with
its locked SHA, using a rebase-or-merge strategy chosen per-repo. `sync` is
the central convergence command: it advances behind repos to the lock,
records new locks when repos are ahead, and surfaces conflicts as
structured outcomes rather than aborting the whole run.

The wire shape is engineered for agent consumption: each per-repo outcome
is a tagged record whose `kind` tells the agent what to do next (retry,
abort, escalate to a human). Failures embed a `failure` sub-record with its
own variant tag (e.g. `rebase-conflict`, `network-error`).

## Invocation

```
rwv sync [--json] [--strategy <rebase|merge>] [-j <N>] [--project <name>]
```

- `--json` emits machine-readable output (see Output below).
- `--strategy` picks the reconciliation strategy when both sides have
  diverged. Default behavior is documented in `rwv --help sync`.
- `-j <N>` runs up to `N` repos in parallel. Under `-j > 1` and `--json`,
  output switches to NDJSON: one record per repo per line, streamed as
  repos finish (consumers demux by `path`).

Run `rwv --help sync` for the full clap surface.

## Output

Default text output is one line per repo summarizing the outcome.

Under `--json` (serial mode), output is the envelope:

```
{
  "$schema": "<url>",
  "outcomes": [ { "kind": "...", "path": "...", "absolute_path": "...", ... }, ... ]
}
```

Outcome `kind` tags include `converged`, `already-ahead`, `no-op`,
`fast-forwarded`, `rebased`, `merged`, `failed`. The `failed` variant
carries a nested `failure` record whose own `kind` tells you what failed
(`rebase-conflict`, `merge-conflict`, `network-error`, etc.).

Under `--json -j N` with `N > 1`, the wrapper is dropped and each line is a
single `SyncOutcomeOutput` record (NDJSON). Consumers demux by `path`.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "SyncJsonOutput",
  "description": "Top-level envelope for `rwv sync --json` (serial mode).",
  "type": "object",
  "required": [
    "$schema",
    "outcomes"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "outcomes": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/SyncOutcomeOutput"
      }
    }
  },
  "definitions": {
    "ConflictOp": {
      "description": "In-flight VCS operation whose conflict needs human resolution.\n\nPassed to [`Vcs::conflict_resolution_hint`] so sync's conflict-bail messages embed VCS-appropriate \"how do I resume this?\" text without hardcoding git vocabulary.",
      "oneOf": [
        {
          "description": "Native rebase (`git rebase`) — resumes with `git rebase --continue`.",
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
        }
      ]
    },
    "SyncFailureOutput": {
      "description": "Wire-output mirror of [`SyncFailure`] for `--json` emission.\n\nCarries the same payload as the in-memory enum but with a `cause` represented as the serialisable [`VcsErrorOutput`]. The hand-rolled tag strings match [`SyncFailure::kind`] (verified via snapshot tests).",
      "oneOf": [
        {
          "type": "object",
          "required": [
            "error",
            "kind"
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
            "error": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "head-unreadable"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "error",
            "kind"
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
            "error": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "ff-impossible"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "error",
            "kind"
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
            "error": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "rebase-failed"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "error",
            "kind"
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
            "error": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "merge-failed"
              ]
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
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "converged"
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
            "commits_ahead",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "commits_ahead": {
              "type": "integer",
              "format": "uint",
              "minimum": 0.0
            },
            "kind": {
              "type": "string",
              "enum": [
                "already-ahead"
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
            "path"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "no-op"
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
            "failure",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
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
            }
          }
        }
      ]
    },
    "VcsErrorOutput": {
      "description": "Wire-output mirror of [`VcsError`] for `--json` emission.\n\n`VcsError` itself can't derive `Serialize` cleanly because tuple variants (and `io::Error`) don't play nicely with serde's internally-tagged enum representation. This struct-only mirror does: every variant carries named fields, the tag matches [`VcsError::kind`], and a `From<&VcsError>` impl converts at JSON-emission time.",
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
              "type": "string"
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
              "type": "string"
            },
            "rev": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "branch",
            "kind",
            "repo"
          ],
          "properties": {
            "branch": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "branch-already-exists"
              ]
            },
            "repo": {
              "type": "string"
            }
          }
        },
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
                "worktree-exists"
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
            "kind",
            "path"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "uncommitted-changes"
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
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "ctx",
            "error",
            "kind"
          ],
          "properties": {
            "ctx": {
              "type": "string"
            },
            "error": {
              "description": "Display form of the underlying `io::Error`. The native source is dropped at the wire boundary since `io::Error` does not serialize.",
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "io"
              ]
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
              "type": "string"
            },
            "stderr": {
              "type": "string"
            }
          }
        }
      ]
    }
  }
}
```

## Exit codes

- `0` — every repo reached a converged or already-ahead state.
- non-zero — at least one repo failed; inspect outcomes with `kind:
  "failed"` for details.

## Examples

Sync the active project and inspect outcomes:

```
rwv sync --json | jq '.outcomes[] | {path, kind}'
```

Find every failed repo and its failure kind:

```
rwv sync --json | jq '.outcomes[] | select(.kind == "failed") | {path, failure}'
```

Parallel sync, demux by path:

```
rwv sync --json -j 8 | jq -c 'select(.kind == "failed")'
```

## Common errors

- *rebase-conflict* — a repo's HEAD diverged from the lock and replay
  couldn't merge cleanly. Resolve manually in the repo, then commit and
  re-run.
- *missing-replay-exclusion* surfaced via `doctor` — the project repo
  doesn't have `rwv.lock merge=ours` in `.gitattributes`; sync's native
  rebase would carry user lock-edits through. Run `rwv doctor --fix`.
- *network-error* — a remote fetch failed. Retry after checking
  connectivity.
