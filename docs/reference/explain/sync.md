# rwv sync

## Purpose

Absorb a named workspace's committed state into CWD. `<source>` is required:
`rwv sync` always names an explicit source (`primary`, a bare workweave name,
or a path). Phase 1' replays CWD's unique project-repo commits onto the
source's tip (with `rwv.lock` excluded so lock-only commits become no-ops).
The manifest that replay produces decides CWD's members: one it names that
CWD lacks is materialized, and one it no longer names is pruned after every
repo has replayed, so a parked op removes no checkout. The source workspace's
`rwv.lock` drives Phase 2: each member repo in CWD is advanced to the SHA
the source has locked. Phase 3 regenerates CWD's `rwv.lock` from the
now-merged manifest tips and commits it. Per-repo
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

The **destination** (CWD) applies the same relaxation without the workweave
scoping: a destination repo whose lock is behind its HEAD is accepted with a
note per repo. Replay's targets come from the source, never from the
destination's lock, and Phase 3 regenerates that lock at op end — so the pull
performs the relock rather than demanding it first. Every other non-`ok`
destination relation still refuses. Unlike `sync-to`'s CWD there is no
op-start relock: the destination's project repo is itself a replay target, and
a relock commit made before Phase 1' would leave `--strategy ff` a project
repo it can no longer fast-forward.

Sync moves committed state only; it never fires integration hooks. Generated
ecosystem state that is rwv-owned and gitignored (a project's `Cargo.lock`
and its `.rwv-owned-digests` ledger) therefore stays exactly as workweave
creation left it, even when the source's own attested generation has moved
on. When the delivered changes touch inputs that state is derived from — the
project's `rwv.toml`, a member's detection manifest such as `Cargo.toml` —
sync prints a trailing note naming `rwv materialize`, the verb whose mandate
re-deriving it is. The note prints only in a checkout that materializes the
synced project (a workweave always; a primary only for the active project)
and only when an input actually moved: source-only deliveries stay quiet.

`role: reference` repos sync like every other member: each is a worktree on
the workweave's ephemeral branch. A member the source's lock pins and this
workspace lacks is created at the revision replay aims it at. A workweave
holding a member as a symlink — the shape older releases gave `reference`
repos — on either side refuses the sync with `workweave-drift`; run
`rwv doctor --fix` there first.

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

Run `rwv sync --help` for the full clap surface.

## Output

Default text output is one line per repo summarizing the outcome, plus a
trailing `note:` line naming `rwv materialize` when delivered changes
touched materialized inputs (text output only).

Under `--json` with `-j 1` (default) or `--json` alone, output is the
pretty-printed envelope:

```
{
  "$schema": "<url>",
  "outcomes": [ { "kind": "...", "path": "...", "absolute_path": "...", ... }, ... ],
  "placements": [ { "path": "...", "absolute_path": "...", "files": { ... } }, ... ],
  "members": [ { "kind": "...", "path": "...", "absolute_path": "...", ... }, ... ],
  "complete": true
}
```

Outcome `kind` tags include `converged`, `already-ahead`, `no-op`, and
`failed`. The `failed` variant carries a nested `failure` record whose own
`kind` tells you what failed (`head-unreadable`, `ff-impossible`,
`rebase-failed`) plus an optional structured `cause`
surfacing the underlying `VcsError`.

Every outcome carries a `containment` record naming how this checkout stood
against the source when the outcome was decided — `equal`, `ahead`, `behind`
or `diverged`, with the commit counts — and the `baseline` saying which read
of the source it was measured against. Branch on that rather than on the
prose in `failure.message`. It is absent only where the outcome was decided
before any pair could be read, such as a HEAD that would not resolve.

File counts are never on an outcome. Each checkout whose files the sync
placed — a member it created, or one whose moved files it shared again at
the end of the op — gets a placement record: `path`, `absolute_path` (the
checkout the files are in), and `files` (`reflinked`, `hardlinked`,
`written`). The envelope lists them under `placements`, empty when nothing
was placed.

`members` lists what replay did with each member checkout that its manifest
decides rather than a per-repo strategy: `kept` (the source's lock lacks the
member and the replayed manifest names it), `pruned` (the replayed manifest
no longer names it and its checkout was removed), and `prune-refused` (the
same, but removing the checkout was refused). A `prune-refused` record
carries the refusal's `refusal` token and the full `message`; its checkout is
left on disk with no manifest naming it. The op still runs to completion and
clears its op-state, then exits 1, so `rwv abort` does not apply and
re-running the sync does not prune the checkout: remove it by hand. The list
is empty, not absent, when there are none.

`complete` is true when the op ran its last phase and cleared its op-state,
leaving nothing to `--continue` or `rwv abort`, and false when a phase
stopped it with op-state held. It is what tells a complete op that exited 1
over a `prune-refused` record from a parked one. NDJSON has no envelope to
carry it; `rwv status` reports whether an op is still in progress.

Under `--json -j N` with `N > 1`, the envelope is dropped and output
switches to NDJSON — one JSON record per line, streamed to stdout the
moment each repo's sync completes. Lines arrive in completion order (not
input order); consumers demux by `path`. Every line is self-describing:
each record embeds its own `"$schema"` URL alongside the per-repo fields,
so a consumer can identify any line without out-of-band context. A
placement is streamed as its own line when it happens, often after its
repo's outcome line; its `$schema` names `placement-record.json`. A member
record is streamed as its own line too, with `$schema` naming
`member-record.json`. Lines
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
    "advisories",
    "complete",
    "members",
    "outcomes",
    "placements"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "advisories": {
      "description": "Standing advisories raised during this sync (e.g. delivered changes touching a materialized input). Empty, not absent, when there are none — a consumer branches on length rather than presence.\n\nPresent only in this envelope: `-j N` with `N > 1` streams NDJSON instead, one self-describing per-repo line with no envelope for an advisory to sit in, so a parallel `--json` sync carries no advisory at all. Run `-j 1` (or omit `-j`) to receive one.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/AdvisoryOutput"
      }
    },
    "complete": {
      "description": "True iff the op ran its last phase and cleared its op-state, so nothing remains to `--continue` or `rwv abort`; false when a phase stopped it with its op-state held. A complete op exits 1 when `members` carries a `prune-refused` entry.",
      "type": "boolean"
    },
    "members": {
      "description": "What replay did with each member checkout its per-repo outcomes do not cover: kept, pruned, or a prune that was refused. Empty, not absent, when there are none. A `prune-refused` entry is a checkout left on disk that no manifest names; branch on it. The op still completes, and then exits 1.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/MemberOutput"
      }
    },
    "outcomes": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/SyncOutcomeOutput"
      }
    },
    "placements": {
      "description": "How the files of each checkout the sync created or moved were placed. Empty, not absent, when it placed none.",
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
    }
  },
  "definitions": {
    "AdvisoryKindOutput": {
      "description": "Closed vocabulary for `AdvisoryOutput::kind`. Adding a member is additive — existing consumers keep matching the members they know.",
      "oneOf": [
        {
          "description": "Generated ecosystem state may no longer agree with the inputs it was derived from.",
          "type": "string",
          "enum": [
            "derived_state_stale"
          ]
        }
      ]
    },
    "AdvisoryOutput": {
      "description": "A condition worth an operator's attention that a verb's `--json` output reports alongside its result, without being a failure of the verb itself.\n\nEvery field is something a consumer branches on directly — `kind` a closed enum, `remedy` a verb string runnable in the checkout where the advisory appears, `inputs` the workspace-relative paths that raised it. None of the three is a sentence a consumer would have to parse to act on.\n\nShared across verbs so more than one `--json` surface can emit the same vocabulary: a sync-time note and a doctor-time standing finding both fit this shape without either owning it.",
      "type": "object",
      "required": [
        "inputs",
        "kind",
        "remedy"
      ],
      "properties": {
        "inputs": {
          "description": "Workspace-relative paths whose state raised this advisory.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "kind": {
          "$ref": "#/definitions/AdvisoryKindOutput"
        },
        "remedy": {
          "description": "The verb that resolves the advisory (e.g. `\"rwv materialize\"`).",
          "type": "string"
        }
      }
    },
    "ConflictOp": {
      "description": "In-flight VCS operation whose conflict needs human resolution.\n\nPassed to `Vcs::conflict_resolution_hint` so sync's conflict-bail messages embed VCS-appropriate \"how do I resume this?\" text without hardcoding git vocabulary.\n\nThe type admits all three variants — `Vcs::mid_op` constructs any of them from a repo's on-disk state — but a given field can be narrower than the type: every `VcsError::RebaseConflict` raised carries `Rebase`, so `Rebase` is the only value that field will actually carry.",
      "oneOf": [
        {
          "description": "Native rebase (`git rebase`).\n\nThe operator-facing resume path is `rwv sync --continue` / `rwv sync-to --continue` — not bare `git rebase --continue`. The VCS hint for this variant stops at staging (`git add <files>`); rwv core appends the `rwv <verb> --continue` line. Bare `git rebase --continue` remains a safe fallback (the durable `merge.rwv-ours.driver` config plant carries the exclusion), but it is not the primary operator path.",
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
    "ContainmentOutput": {
      "description": "What one repo's replay was decided from: how this checkout stood against the source, and which read of the source it was measured against.\n\nThe two travel together because a verdict without its baseline counts commits ahead of nothing in particular.",
      "type": "object",
      "required": [
        "baseline",
        "verdict"
      ],
      "properties": {
        "baseline": {
          "description": "Which read produced the source tip this verdict measured against.",
          "allOf": [
            {
              "$ref": "#/definitions/ReplayBaseline"
            }
          ]
        },
        "verdict": {
          "description": "The ancestry between this checkout and the source tip.",
          "allOf": [
            {
              "$ref": "#/definitions/ContainmentVerdictOutput"
            }
          ]
        }
      }
    },
    "ContainmentVerdictOutput": {
      "description": "How a repo's checkout stood against the source tip it was measured against: equal to it, containing it and carrying commits it does not, contained by it, or each holding commits the other lacks.",
      "oneOf": [
        {
          "type": "object",
          "required": [
            "relation"
          ],
          "properties": {
            "relation": {
              "type": "string",
              "enum": [
                "equal"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "commits",
            "relation"
          ],
          "properties": {
            "commits": {
              "type": "integer",
              "format": "uint",
              "minimum": 0.0
            },
            "relation": {
              "type": "string",
              "enum": [
                "ahead"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "commits",
            "relation"
          ],
          "properties": {
            "commits": {
              "type": "integer",
              "format": "uint",
              "minimum": 0.0
            },
            "relation": {
              "type": "string",
              "enum": [
                "behind"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "ahead",
            "behind",
            "relation"
          ],
          "properties": {
            "ahead": {
              "type": "integer",
              "format": "uint",
              "minimum": 0.0
            },
            "behind": {
              "type": "integer",
              "format": "uint",
              "minimum": 0.0
            },
            "relation": {
              "type": "string",
              "enum": [
                "diverged"
              ]
            }
          }
        }
      ]
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
    "MemberOutput": {
      "description": "What replay did with a member checkout outside its per-repo strategy: the member set the replayed manifest decides.",
      "oneOf": [
        {
          "description": "The source's lock does not name this member and the manifest replay produced does, so its checkout stays.",
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
                "kept"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "The replayed manifest no longer names this member, and its checkout was removed.",
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
                "pruned"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "The replayed manifest no longer names this member, but removing its checkout was refused. The checkout is still on disk, and once the op relocks no manifest or lock names it. The op runs to completion and then exits 1.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "message",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "prune-refused"
              ]
            },
            "message": {
              "description": "Free-form display message, the full cause chain. Not a typed discriminant.",
              "type": "string"
            },
            "path": {
              "type": "string"
            },
            "refusal": {
              "description": "The refusal token, resolvable with `rwv explain <token>`. Absent when the removal failed without a classified refusal.",
              "type": [
                "string",
                "null"
              ]
            }
          }
        }
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
          "type": "string"
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
    "ReplayBaseline": {
      "description": "Which read gave replay this repo's target: the source lock's entry for it, or the source checkout's committed tip pulled ahead of that entry (tips-as-truth).",
      "type": "string",
      "enum": [
        "source-lock-entry",
        "source-committed-tip"
      ]
    },
    "Resolution": {
      "description": "Resolved workspace coordinates for `--json` output and the plugin env-var envelope.\n\nCarries `workspace` (primary root abs path), `workweave` (the `<project>--<name>` identity the registry records), `project` (resolved project name), and `workweave_unregistered`. No `kind` or `location` field: the checkout is one of three states, and two of them are already carried by `workweave`'s presence.\n\nThe third state needs a field of its own. A workweave whose directory no registry entry names has no identity, so `workweave` is absent for it — and absent is what the primary looks like. Without `workweave_unregistered` the two serialize identically, and a consumer reading the documented meaning of that absence is told, positively, that it is at the primary checkout.\n\nResults only — provenance (which chain step resolved the project, which flag addressed the workspace) is deliberately excluded: anything in default `--json` output becomes depended on, and the assertion use case needs the result, not the mechanism. Provenance appears only in the human-facing \"target:\" line printed to stderr.\n\nIsomorphic to the plugin env-var envelope (`RWV_WORKSPACE`/`RWV_WORKWEAVE`/`RWV_WORKWEAVE_UNREGISTERED`/`RWV_PROJECT`): both surfaces are pure projections of `WorkspaceContext::resolution`, never independently computed.",
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
          "description": "Primary weave root, in the wire spelling.\n\nA `String` and not a `PathBuf` so the spelling is decided at construction rather than by serde: this is a published absolute path and it owes programs the composable form, which `crate::path_spelling::wire_path` is the only producer of.",
          "type": "string"
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
    "Step3AdvanceOutput": {
      "description": "Step-3 fast-forward advance record for one repo in `rwv sync-to --json` output.\n\nPresent in a per-repo outcome iff step 3 (advance-target) actually advanced that repo's branch pointer, and carrying the revisions that advance read and wrote. Omitted (`skip_serializing_if = \"Option::is_none\"`) on a no-op advance — the target was already at CWD's tip — which is the same condition under which the text line reads `already at <sha>`, because both render one outcome.",
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
      "description": "Wire-output mirror of `SyncFailure` for `--json` emission.\n\nCarries the same payload as the in-memory enum but with a `cause` represented as the serializable `VcsErrorOutput`. The `#[serde(rename)]` override below keeps the wire tag matching `SyncFailure::kind`.\n\n`message` is the human-readable display string of the failure (free-form text, not a typed discriminant). `cause` is the structured typed cause — always present on `ff-impossible` and `rebase-failed`, optional on `head-unreadable` — and consumers that want to branch on failure mode should inspect `cause.kind` rather than parsing `message`.",
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
            "cause",
            "kind",
            "message"
          ],
          "properties": {
            "cause": {
              "$ref": "#/definitions/VcsErrorOutput"
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
            "cause",
            "kind",
            "message"
          ],
          "properties": {
            "cause": {
              "$ref": "#/definitions/VcsErrorOutput"
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
            "containment": {
              "description": "How this checkout stood against the source when the strategy ran. The `kind` says what the strategy did; this says what it was up against — `behind` is a fast-forward, `diverged` is a rebase.",
              "anyOf": [
                {
                  "$ref": "#/definitions/ContainmentOutput"
                },
                {
                  "type": "null"
                }
              ]
            },
            "derived_content_dropped": {
              "description": "Repo-relative paths this repo declares derived whose replayed version the landed tree does not carry: the replay resolved them to the target's version instead. Regenerating them from their source of record and committing is what makes the landed tree describe itself again. Omitted when nothing was resolved away.",
              "type": "array",
              "items": {
                "type": "string"
              }
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
            "containment": {
              "description": "How this checkout stood against the source, and which read of the source `commits_ahead` counts against.",
              "anyOf": [
                {
                  "$ref": "#/definitions/ContainmentOutput"
                },
                {
                  "type": "null"
                }
              ]
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
            "source_position": {
              "description": "Where the SOURCE's own checkout stood against that same read of the source. `containment` proves this repo holds the target; only this says whether the source has since moved past it. Absent where the source's checkout could not be measured.",
              "anyOf": [
                {
                  "$ref": "#/definitions/ContainmentVerdictOutput"
                },
                {
                  "type": "null"
                }
              ]
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
            "containment": {
              "description": "How this checkout stood against the source, and which read of the source it was equal to.",
              "anyOf": [
                {
                  "$ref": "#/definitions/ContainmentOutput"
                },
                {
                  "type": "null"
                }
              ]
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
            "containment": {
              "description": "How this checkout stood against the source when the strategy ran. `failure.message` renders this in prose; branch on this instead. Absent where the outcome was decided before any pair was read.",
              "anyOf": [
                {
                  "$ref": "#/definitions/ContainmentOutput"
                },
                {
                  "type": "null"
                }
              ]
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
            "expected",
            "kind",
            "observed",
            "repo"
          ],
          "properties": {
            "expected": {
              "type": "string"
            },
            "kind": {
              "type": "string",
              "enum": [
                "stale-ref-witness"
              ]
            },
            "observed": {
              "type": "string"
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
            "operation",
            "repo"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "mid-operation"
              ]
            },
            "operation": {
              "type": "string"
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
            "repo",
            "stderr"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "hook-rejected"
              ]
            },
            "repo": {
              "type": "string"
            },
            "stderr": {
              "type": "string"
            }
          }
        },
        {
          "type": "object",
          "required": [
            "kind",
            "paths",
            "repo"
          ],
          "properties": {
            "kind": {
              "type": "string",
              "enum": [
                "untracked-collision"
              ]
            },
            "paths": {
              "type": "array",
              "items": {
                "type": "string"
              }
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

- `0` — every repo reached a converged or already-ahead state, and every
  member checkout the replayed manifest no longer names was pruned.
- non-zero — at least one repo failed (inspect outcomes with `kind:
  "failed"` for details), or the op completed but pruning a member checkout
  was refused. In the second case replay and relock finished and op-state is
  cleared, so `rwv abort` does not apply; the error names each checkout left
  on disk, and each has a `prune-refused` record under `members`.

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
  doesn't have `rwv.lock merge=rwv-ours` in `.gitattributes` (or still
  carries the legacy `merge=ours` spelling); sync's native rebase would
  carry user lock-edits through. Run `rwv doctor --fix`.
- *network-error* — a remote fetch failed. Retry after checking
  connectivity.
