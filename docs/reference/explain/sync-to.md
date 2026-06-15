# rwv sync-to

## Purpose

Advance a target workspace to CWD's tip via a three-step orchestration. The
user-facing promise: CWD's unique commits land linearly on top of target's
prior history; target absorbs CWD's state with CWD as the newest contribution.

This is the inverse of `rwv sync`: rather than pulling from a source into CWD,
`rwv sync-to` pushes CWD's state into a target — but all rewriting happens in
CWD, and target is only ever advanced via fast-forward.

### The three steps

1. **Step 1 — rebase CWD against target.** Equivalent to `rwv sync <target>
   --strategy=<X>` from CWD: the existing sync engine is called with target as
   source and CWD as destination. End state: CWD has target's history with CWD's
   unique commits replayed on top (per `--strategy`).

2. **Step 2 — auto-relock.** If step 1 moved any manifest-repo tip in CWD,
   regenerate `rwv.lock` and commit it in CWD's project repo with the message
   `lock: post-rebase refresh`. This keeps the lock consistent without requiring
   a separate `rwv lock` invocation.

3. **Step 3 — FF-advance target.** Fast-forward each manifest repo and the
   project repo in target to CWD's converged tips. This step is always FF
   regardless of `--strategy` — all rewriting happened in CWD during step 1.
   If FF is not possible (e.g. concurrent modification), the operation bails
   with an actionable error.

### End-state semantics

After `rwv sync-to <target> --strategy=rebase`:
- CWD: unique commits rebased onto target's prior tip (plus auto-relock commit).
- Target: fast-forwarded to CWD's new tip — same history as CWD.
- If target had unique commits since CWD's fork point, those form the BASE of
  the resulting linear history, with CWD's contributions ON TOP.

### Strategy semantics

- `--strategy=ff` — step 1 is a no-op; CWD must already be strictly ahead of
  target. Step 3 FFs target. If CWD isn't strictly ahead, bail with an error
  pointing at `--strategy=rebase`.
- `--strategy=rebase` (default) — step 1 rebases CWD's unique commits onto
  target's tip. Step 2 auto-relocks. Step 3 FFs target.

(A `merge` strategy is not offered; see
[sync semantics](../explanation/joints/sync-semantics.md#why-no-merge-strategy).)

### Multi-workspace op-state

Op-state is written to both CWD and target before step 1. The owner record at
CWD holds all op parameters plus the current phase (replay → relock →
advance-target → retire); the target workspace holds a thin lease pointing back
at CWD. Named overrides supplied at invocation (`--allow-stale-lock`,
`--discard-local-commits`) are recorded in the `overrides` field so `--continue`
resumes with the same consents. If any step fails, op-state is left in place so
the operator can resolve and rerun with `--continue`, or use `rwv abort` to roll
back both workspaces.

## Invocation

```
rwv sync-to [<target>] [--json] [--strategy <ff|rebase>] [-j <N>] [--allow-stale-lock] [--discard-local-commits] [--retire] [--project <name>] [--continue]
```

- `<target>` is the target workspace: `primary`, a bare workweave name, or
  a path (absolute, or relative to the primary workspace root). Omit inside
  a workweave to auto-target the parent recorded in `.rwv-workweave`. Required
  in a primary weave.
- `--json` emits machine-readable output (see Output below).
- `--strategy` picks the step-1 strategy (`rebase` default or `ff`).
  Step 3 is always FF regardless of this flag. (`merge` is not offered; see
  [sync semantics](../explanation/joints/sync-semantics.md#why-no-merge-strategy).)
- `--allow-stale-lock` skips the lock-freshness precondition on both sides.
  Recorded as `allow-stale-lock` in the op-state `overrides` field for audit
  fidelity on `--continue`.
- `--discard-local-commits` hard-resets CWD's project repo to target's tip,
  discarding local committed divergence (pre-op savepoint preserved as
  tombstone; refused if uncommitted changes are present). Recorded as
  `discard-local-commits` in `overrides`; `--continue` resumes with the same
  consent.
- `--retire` deletes the current workweave on success (after all three steps
  complete). Requires a workweave context; emits a warning and is a no-op in
  a primary weave. Use to close out a workweave in one step.
- `-j <N>` runs up to `N` per-repo manifest syncs in parallel during step 1.
  Default is `1` (serial).
- `--continue` resumes a sync-to that was interrupted mid-op (e.g. after
  resolving a conflict). The recorded parameters must match — mismatch is an error.

Run `rwv --help sync-to` for the full clap surface.

## Output

Default text output reports each step with one line per repo.

Under `--json` with `-j 1` (default) or `--json` alone, output is the
pretty-printed envelope:

```
{
  "$schema": "<url>",
  "outcomes": [ { "kind": "...", "path": "...", "absolute_path": "...", ... }, ... ]
}
```

Outcome `kind` tags and the `--json` / NDJSON shape are identical to `rwv sync`
— only the `$schema` URL differs (pointing at `docs/reference/schemas/sync-to.json`).

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "SyncToJsonOutput",
  "description": "Top-level envelope for `rwv sync-to --json` (serial mode).\n\nExtends [`SyncJsonOutput`] with sync-to-specific observability fields: - `source_workweave` — the workweave the command was invoked from (null when invoked from the primary weave). - `target` — the absolute path of the target workspace that was advanced. - `retired` — true iff `--retire` was passed AND the workweave was deleted. - `project_repo_advance` — step-3 advance of `projects/<project>/.git`; omitted when the project repo was already at CWD's tip (no-op advance). - per-outcome `step3_advance` — step-3 advance SHA pair for each manifest repo; omitted on a no-op advance.\n\nKept as a separate type so the generated schema artifact (`docs/reference/schemas/sync-to.json`) has its own title/description.",
  "type": "object",
  "required": [
    "$schema",
    "outcomes",
    "retired",
    "target"
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
    },
    "project_repo_advance": {
      "description": "Step-3 advance of the project repo (`projects/<project>/.git`). Omitted when the project repo was already at CWD's tip (no-op fast-forward).",
      "anyOf": [
        {
          "$ref": "#/definitions/Step3AdvanceOutput"
        },
        {
          "type": "null"
        }
      ]
    },
    "retired": {
      "description": "True iff `--retire` was passed AND retire actually fired (the workweave was deleted). False when `--retire` was not passed, or when retire was skipped (e.g. invoked from the primary weave).",
      "type": "boolean"
    },
    "source_workweave": {
      "description": "The workweave name the command was invoked from; null when invoked from the primary weave.",
      "type": [
        "string",
        "null"
      ]
    },
    "target": {
      "description": "Absolute path of the target workspace that step-3 fast-forwarded.",
      "type": "string"
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
      "description": "Wire-output mirror of [`SyncFailure`] for `--json` emission.\n\nCarries the same payload as the in-memory enum but with a `cause` represented as the serialisable [`VcsErrorOutput`]. The hand-rolled tag strings match [`SyncFailure::kind`] (verified via snapshot tests).\n\n`message` is the human-readable display string of the failure (free-form text, not a typed discriminant). `cause` is the structured typed cause when the failure originated from a [`crate::vcs::VcsError`] call — consumers that want to branch on failure mode should inspect `cause.kind` rather than parsing `message`.",
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

- `0` — all three steps completed; target is at CWD's tip.
- non-zero — at least one step failed; inspect the error output for details.

## Examples

Advance the recorded parent (bare form, inside a workweave):

```
rwv sync-to
```

Land work and delete the workweave in one step:

```
rwv sync-to --retire
```

Advance an explicit target:

```
rwv sync-to primary
```

Advance primary, show per-repo outcomes:

```
rwv sync-to primary --json | jq '.outcomes[] | {path, kind}'
```

Resume after resolving a conflict:

```
# (resolve conflicts in the relevant repos)
rwv sync-to --continue
```

Roll back after a failed sync-to:

```
rwv abort
```

## Common errors

- *step 1 conflict* — a repo's HEAD diverged from the lock and replay couldn't
  merge cleanly. Resolve manually in the repo, then `rwv sync-to --continue`.
- *ff-impossible (--strategy=ff)* — CWD is not strictly ahead of target. Use
  `--strategy=rebase` to replay CWD's commits onto target's tip first.
- *step 3 FF-advance failed* — target's repo was modified concurrently after
  step 1 completed. Investigate, then `rwv sync-to --continue` or
  `rwv abort`.
- *missing-replay-exclusion* — the project repo doesn't have `rwv.lock merge=ours`
  in `.gitattributes`. Run `rwv doctor --fix`.
- *bare sync-to in primary weave* — `rwv sync-to` (no target) requires a
  workweave context. Provide an explicit `<target>` or run from inside a workweave.
