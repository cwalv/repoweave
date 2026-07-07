# rwv sync

## Purpose

Absorb a named workspace's committed state into CWD. `<source>` is required:
`rwv sync` always names an explicit source (`primary`, a bare workweave name,
or a path). The source workspace's `rwv.lock` drives Phase 2: each manifest
repo in CWD is advanced to the SHA the source has locked. Phase 1' then
replays CWD's unique project-repo commits onto the source's tip (with
`rwv.lock` excluded so lock-only commits become no-ops). Phase 3 regenerates
CWD's `rwv.lock` from the now-merged manifest tips and commits it. Per-repo
conflicts are reported with non-zero exit; already-converged repos are
no-ops, so re-runs are cheap.

When the source is a **workweave** (not the primary weave) and one or more of
its repos have new commits since the source's last lock (`LockRelation::Ahead`
— lock is behind HEAD), `rwv sync` treats those repos as **tips-as-truth**: it
pulls the source's committed tips directly, leaving the source's lock file
untouched (the source's next op will heal its lock). A note is printed per
affected repo. This relaxation applies only to workweave sources; a primary
source with a lock behind HEAD is still refused (primary locks are a
reproducibility anchor).

`role: reference` repos materialized as symlinks (the default; see
`rwv explain workweave`) are **excluded from the sync graph**: they
are read-only aliases of the single canonical clone, identical across
workweaves, so no phase savepoints, advances, or mutates them — the shared
canonical store is never touched through the symlink. They stay pinned in
`rwv.lock` for reproducibility. A `reference` repo created with
`--worktree-references` is a real worktree and syncs normally.

The wire shape is engineered for agent consumption: each per-repo outcome
is a tagged record whose `kind` tells the agent what to do next (retry,
abort, escalate to a human). Failures embed a `failure` sub-record with its
own variant tag (e.g. `rebase-failed`, `ff-impossible`).

To push CWD's state into a target workspace instead (the landing direction),
use `rwv sync-to`.

## Invocation

```
rwv sync <source> [--json] [--strategy <ff|rebase>] [-j <N>] [--allow-stale-lock] [--discard-local-commits] [--project <name>]
```

- `<source>` is the source workspace: `primary`, a bare workweave name, or
  a path. Required — `rwv sync` has no auto-target.
- `--json` emits machine-readable output (see Output below).
- `--strategy` picks the reconciliation strategy (`ff` default or
  `rebase`). `rebase` replays CWD's project commits onto the
  source tip with `rwv.lock` excluded. (`merge` is not offered; `ff`
  requires CWD to already be strictly ahead of source.)
- `--allow-stale-lock` skips the lock-freshness precondition on both source
  and destination. Use when the lock is intentionally stale. Usual fix without
  this flag: run `rwv lock --commit` in the relevant workspace.
  Recorded as `allow-stale-lock` in the op-state `overrides` field for audit
  fidelity on `--continue`.
- `--discard-local-commits` hard-resets the CWD project repo to the source
  tip, discarding any destination-only committed divergence. The pre-op
  savepoint is kept as a tombstone at `refs/rwv/pre-op/<id>` so
  `git reset --hard` can recover them manually. Refused if the project repo
  has uncommitted changes (those would be destroyed unrecoverably by the
  reset). Recorded as `discard-local-commits` in the op-state `overrides`
  field; `--continue` resumes with the same consent without requiring the
  flag to be re-supplied.
- `-j <N>` runs up to `N` per-repo manifest syncs (Phase 2) in parallel.
  Default is `1` (serial), unlike `rwv fetch` / `rwv update` whose default
  auto-resolves to a small worker pool. Sync's default is `1` so that
  the `--json` envelope shape is the unsurprising default and the
  envelope/NDJSON switch only happens when the user opts in with `-j > 1`.
  Phase 1' (project repo) and Phase 3 (re-lock + commit) are inherently
  serial and run on the caller thread regardless of `-j`.

Run `rwv --help sync` for the full clap surface.

## Output

Default text output is one line per repo summarizing the outcome.

Under `--json` with `-j 1` (default) or `--json` alone, output is the
pretty-printed envelope:

```
{
  "$schema": "<url>",
  "outcomes": [ { "kind": "...", "path": "...", "absolute_path": "...", ... }, ... ]
}
```

Outcome `kind` tags include `converged`, `already-ahead`, `no-op`, and
`failed`. The `failed` variant carries a nested `failure` record whose own
`kind` tells you what failed (`head-unreadable`, `ff-impossible`,
`rebase-failed`) plus an optional structured `cause`
surfacing the underlying `VcsError`.

Under `--json -j N` with `N > 1`, the envelope is dropped and output
switches to NDJSON — one JSON record per line, streamed to stdout the
moment each repo's sync completes. Lines arrive in completion order (not
input order); consumers demux by `path`. Every line is self-describing:
each record embeds its own `"$schema"` URL alongside the per-repo fields,
so a consumer can identify any line without out-of-band context. Lines
are mutex-guarded so concurrent workers can't tear a single record's
bytes. The text-prefix Reporter wrapper (`[<repo>] <line>`) used by
`fetch`/`update` under `-j > 1` is **bypassed** in JSON mode — workers
never call into the subprocess Reporter, so JSON output is pristine.

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
      "description": "In-flight VCS operation whose conflict needs human resolution.\n\nPassed to `Vcs::conflict_resolution_hint` so sync's conflict-bail messages embed VCS-appropriate \"how do I resume this?\" text without hardcoding git vocabulary.",
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
    "Step3AdvanceOutput": {
      "description": "Step-3 fast-forward advance record for one repo in `rwv sync-to --json` output.\n\nPresent in a per-repo outcome iff step 3 (advance-target) actually advanced that repo's branch pointer. Omitted (`skip_serializing_if = \"Option::is_none\"`) in two cases: (a) no-op advance — target was already at CWD's tip; or (b) the pre-advance HEAD read failed (`head_revision` returned `Err`) — in that case `target_tip_before` is `None` and no record is emitted even if the ff succeeded.",
      "type": "object",
      "required": [
        "from_sha",
        "to_sha"
      ],
      "properties": {
        "from_sha": {
          "description": "Target repo's HEAD SHA before the fast-forward.",
          "type": "string"
        },
        "to_sha": {
          "description": "Target repo's HEAD SHA after the fast-forward (== CWD's tip).",
          "type": "string"
        }
      }
    },
    "SyncFailureOutput": {
      "description": "Wire-output mirror of `SyncFailure` for `--json` emission.\n\nCarries the same payload as the in-memory enum but with a `cause` represented as the serialisable `VcsErrorOutput`. The hand-rolled tag strings match `SyncFailure::kind` (verified via snapshot tests).\n\n`message` is the human-readable display string of the failure (free-form text, not a typed discriminant). `cause` is the structured typed cause when the failure originated from a `crate::vcs::VcsError` call — consumers that want to branch on failure mode should inspect `cause.kind` rather than parsing `message`.",
      "oneOf": [
        {
          "type": "object",
          "required": [
            "kind",
            "message"
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
            "kind": {
              "type": "string",
              "enum": [
                "head-unreadable"
              ]
            },
            "message": {
              "description": "Free-form display message for this failure. Not a typed discriminant.",
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "message"
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
            "kind": {
              "type": "string",
              "enum": [
                "ff-impossible"
              ]
            },
            "message": {
              "description": "Free-form display message for this failure. Not a typed discriminant.",
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "message"
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
            "kind": {
              "type": "string",
              "enum": [
                "rebase-failed"
              ]
            },
            "message": {
              "description": "Free-form display message for this failure. Not a typed discriminant.",
              "type": "string"
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
            },
            "step3_advance": {
              "description": "Step-3 fast-forward advance for this repo; present only in `rwv sync-to --json` output when step 3 advanced this repo.",
              "anyOf": [
                {
                  "$ref": "#/definitions/Step3AdvanceOutput"
                },
                {
                  "type": "null"
                }
              ]
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
            },
            "step3_advance": {
              "description": "Step-3 fast-forward advance for this repo; present only in `rwv sync-to --json` output when step 3 advanced this repo.",
              "anyOf": [
                {
                  "$ref": "#/definitions/Step3AdvanceOutput"
                },
                {
                  "type": "null"
                }
              ]
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
            },
            "step3_advance": {
              "description": "Step-3 fast-forward advance for this repo; present only in `rwv sync-to --json` output when step 3 advanced this repo.",
              "anyOf": [
                {
                  "$ref": "#/definitions/Step3AdvanceOutput"
                },
                {
                  "type": "null"
                }
              ]
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
            },
            "step3_advance": {
              "description": "Step-3 fast-forward advance for this repo; present only in `rwv sync-to --json` output when step 3 advanced this repo. Typically absent when the repo failed in step 1.",
              "anyOf": [
                {
                  "$ref": "#/definitions/Step3AdvanceOutput"
                },
                {
                  "type": "null"
                }
              ]
            }
          }
        }
      ]
    },
    "VcsErrorOutput": {
      "description": "Wire-output mirror of `VcsError` for `--json` emission.\n\n`VcsError` itself can't derive `Serialize` cleanly because tuple variants (and `io::Error`) don't play nicely with serde's internally-tagged enum representation. This struct-only mirror does: every variant carries named fields, the tag matches `VcsError::kind`, and a `From<&VcsError>` impl converts at JSON-emission time.",
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
            "kind",
            "message"
          ],
          "properties": {
            "ctx": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "io"
              ]
            },
            "message": {
              "description": "Display form of the underlying `io::Error`. The native source is dropped at the wire boundary since `io::Error` does not serialize. Named `message` (not `error`) to make clear this is free-form display text, not a typed discriminant that consumers can branch on.",
              "type": "string"
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
