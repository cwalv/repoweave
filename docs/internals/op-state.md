# Op-state records

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

The on-disk records `rwv sync`, `rwv land`, and `rwv pull` and `rwv update
--remote` with `--merge-upstream` or `--reset-to-upstream` use to make a
multi-workspace operation resumable across a crash. Written and cleared by the verbs; read on
`--continue` and by `rwv abort`.

Nothing outside `rwv` should parse these. A plugin that needs to know whether an
operation is in flight calls back into `rwv` — `rwv status --json`, and probe
the field rather than gating on a version. [`../reference/plugin-protocol.md`](../reference/plugin-protocol.md)
names both files under the write prohibition; the field tables below exist for
someone changing `rwv`'s own resume logic, not for a consumer.

## `.rwv-op` — owner op-state record

Written at the **initiating workspace** (the owner) when a `rwv sync` or
`rwv land` operation starts. Holds all op parameters plus the current phase.
It is the sole copy of mutable op state. Cleared on success, precondition
refusal, and after `rwv abort`; preserved on phase failure so `--continue` and
`rwv abort` can resume.

Schema v3. Every reader refuses a record whose `schema` is another number or
absent, naming the release that wrote it (`release`, or "a release before" the
running one for a record that has none): it is finished or aborted with that
release, and nothing of it is read. JSON, every field mandatory — a record
missing a key fails to parse rather than defaulting.

```json
{
  "schema": 3,
  "release": "0.25.0",
  "id": "1779769917405921588",
  "verb": "sync",
  "strategy": "rebase",
  "project": "web-app",
  "source": "/abs/path/src",
  "target": "/abs/path/tgt",
  "retire": false,
  "phase": "replay",
  "advanced_tips": {},
  "converged_tips": {},
  "overrides": [],
  "started_at": "2026-06-10T21:14:03Z",
  "take_in": null
}
```

| Field | Description |
|---|---|
| `schema` | The record schema, `3`. A record carrying another, or none, is refused |
| `release` | The rwv release that wrote the record |
| `id` | Unique operation identifier (nanosecond wall-clock string). Shared with savepoint refs and lease files |
| `verb` | Which top-level verb started this op: `sync`, `land`, `pull` or `update-remote` |
| `strategy` | Strategy supplied to the op: `ff` or `rebase` |
| `project` | The project the op operates on. Both `source` and `target` are resolved under it; neither workspace's `.rwv-active` is consulted, since an operator may retarget a pointer while the op is parked. `rwv abort` reads it to know which manifest to enumerate on each side |
| `source` | Absolute path of the source workspace |
| `target` | Absolute path of the target workspace. For `sync`: same as the owner workspace. For `land`: the named target workspace |
| `retire` | Whether `--retire` was passed |
| `phase` | Current phase in execution order: `replay` → `relock` → `advance-target` (land only) → `retire` (`--retire` only) |
| `advanced_tips` | Replay-phase intent per repo. Key: repo path relative to workspace root, or `(project)` for the project repo (the flag spelling `projects/<name>` is the operator's, and is not stored). Value: SHA string. Empty before replay entry; cleared at relock in the same write that populates `converged_tips` |
| `converged_tips` | Per-repo converged tips written at relock completion. Empty before. Consumed by advance-target and abort's HEAD check. Keyed as `advanced_tips` is |
| `overrides` | Named consents supplied at invocation; `land` and `sync` take none yet. Recorded so `--continue` re-applies the same consents |
| `started_at` | RFC3339 UTC timestamp when the op started |
| `take_in` | `null` for `land`. For `sync`: the branch it takes in (`sync_from`: `{"kind": "target" or "from", "branch": ...}`), the repos `--reset-to-target` named (`reset_to_target`), and in `upstream_tips` the tip each member takes in (its merge source) and, under `project`, the tip of the branch taken in; the strategy is the record's `strategy`. For `pull` and `update-remote`: what `--continue` re-applies (`merge_upstream`, `reset_to_upstream`, the `--role` and `--repo` selection, `dirty`, `commit` and `message`, `no_materialize`), the upstream tip each repo was merged or reset against (`upstream_tips`, so a resumed op acts on the tips the first run planned), and each branch moved as a ref with no checkout (`ref_moves`: repo, branch, from, to) |

Lives at the workspace root (same directory as `.rwv-active`). Source:
`src/op_state.rs`.

### `pull` and `update --remote`

These two take the owner record with `source` and `target` both the workspace
they ran in, and no leases. Their phases are `replay` (the member merges, the
resets, the project merge up to its commit) and `relock`. Each checkout the op
may move gets a savepoint under the op's own id; each tip it writes is journaled
in `advanced_tips`. A branch the op moves as a ref, because no checkout has it
attached, has no HEAD to savepoint, so the move goes in `take_in.ref_moves`
instead, and `rwv abort` puts it back by compare-and-swap, refusing a branch
that has moved since as a foreign tip. Every step is idempotent: a `--continue`
finishes a merge left staged in a checkout, skips one already made, and
recomputes the rest.

### What the record's refs hold

An op's savepoints (`refs/rwv/pre-op/<id>`, `<id>-target` on a `land`
target's side) live exactly as long as the record: cleanup drops them with it,
and `rwv abort` consumes them. They are the only refs an op owns. Abort writes
none of its own: every tip it resets is a commit the op made, and a tip the op
did not make is refused and left in place. The one thing abort keeps is a
foreign tip `--abandon-foreign-tip` moves a branch off, and that is a backup
(`refs/rwv/backup/<branch>/<id>`, marked `abandoned`), which outlives the
record and belongs to `rwv backup`. A savepoint that outlives its record is not
a backup either: `rwv doctor` reports it as `orphaned-savepoint`, and `--fix`
is what turns one that holds the last reference to its commits into a backup.

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
