# Op-state records

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

The on-disk records `rwv sync` and `rwv sync-to` use to make a multi-workspace
operation resumable across a crash. Written and cleared by the verbs; read on
`--continue` and by `rwv abort`.

Nothing outside `rwv` should parse these. A plugin that needs to know whether an
operation is in flight calls back into `rwv` — `rwv status --json`, and probe
the field rather than gating on a version. [`../reference/plugin-protocol.md`](../reference/plugin-protocol.md)
names both files under the write prohibition; the field tables below exist for
someone changing `rwv`'s own resume logic, not for a consumer.

## `.rwv-op` — owner op-state record

Written at the **initiating workspace** (the owner) when a `rwv sync` or
`rwv sync-to` operation starts. Holds all op parameters plus the current phase.
It is the sole copy of mutable op state. Cleared on success, precondition
refusal, and after `rwv abort`; preserved on phase failure so `--continue` and
`rwv abort` can resume.

Schema v2 (no back-compat with v1; in-flight v1 ops must be resolved with
`rwv abort` before upgrading). JSON, every field mandatory — a record missing
a key fails to parse rather than defaulting.

```json
{
  "id": "1779769917405921588",
  "verb": "sync",
  "strategy": "rebase",
  "source": "/abs/path/src",
  "target": "/abs/path/tgt",
  "retire": false,
  "phase": "replay",
  "advanced_tips": {},
  "converged_tips": {},
  "overrides": [],
  "started_at": "2026-06-10T21:14:03Z"
}
```

| Field | Description |
|---|---|
| `id` | Unique operation identifier (nanosecond wall-clock string). Shared with savepoint refs and lease files |
| `verb` | Which top-level verb started this op: `sync` or `sync-to` |
| `strategy` | Strategy supplied to the op: `ff` or `rebase` |
| `source` | Absolute path of the source workspace |
| `target` | Absolute path of the target workspace. For `sync`: same as the owner workspace. For `sync-to`: the named target workspace |
| `retire` | Whether `--retire` was passed |
| `phase` | Current phase in execution order: `replay` → `relock` → `advance-target` (sync-to only) → `retire` (`--retire` only). Persisted before entering each phase so a crash re-enters the same phase on resume |
| `advanced_tips` | Replay-phase intent per repo. Key: repo path relative to workspace root. Value: SHA string. Empty before replay entry; cleared at relock in the same write that populates `converged_tips` |
| `converged_tips` | Per-repo converged tips written at relock completion. Empty before. Consumed by advance-target and abort's HEAD check |
| `overrides` | Named overrides supplied at invocation (e.g. `allow-stale-lock`, `discard-local-commits`). Recorded for audit fidelity on `--continue` — resume re-applies the same consents |
| `started_at` | RFC3339 UTC timestamp when the op started |

Lives at the workspace root (same directory as `.rwv-active`). Source:
`src/op_state.rs`.

## `.rwv-op-lease` — thin lease pointer

Written at every **other workspace the op mutates** (never at the owner
workspace). Immutable once written. Provides mutex semantics (prevents
concurrent ops on the same workspace) and a pointer back to the owner record
for `--continue` and `rwv abort`. Cleared after the owning op completes or is
aborted.

```json
{
  "id": "1779769917405921588",
  "owner": "/abs/path/to/owner/workspace",
  "created_at": "2026-06-10T21:14:03Z"
}
```

| Field | Description |
|---|---|
| `id` | Unique operation identifier. Same as the owner record's `id` |
| `owner` | Absolute path to the owner workspace. Follow this pointer to load the full op state |
| `created_at` | RFC3339 UTC timestamp at which the lease was written. Surfaced by `rwv doctor` as observability-only context, never a decision input |

Lives at the workspace root of the non-owner mutated workspace. Source:
`src/op_state.rs`.

## Where else to look

- [`../reference/formats.md`](../reference/formats.md) — the published entry for
  both files: what they are, which verbs write and clear them.
- [`../how-to/resume-or-abort-mid-op-sync.md`](../how-to/resume-or-abort-mid-op-sync.md)
  — the operator-facing recovery path (`--continue`, `rwv abort`).
- [`../explanation/joints/sync-semantics.md`](../explanation/joints/sync-semantics.md)
  — the phase machine these records persist.
