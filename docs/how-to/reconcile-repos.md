# Reconcile repos with the lock

After a sync, a reset, or any operation that may have moved repo tips, you may need to verify that every clone matches what `rwv.lock` records — and repair the ones that don't.

Two kinds of divergence show up here: a clone that has drifted from the locked revision, and a clone that has been deleted entirely. Both share the same detect-then-repair pattern.

## Step 1 — detect with `rwv status`

```bash
rwv status
```

`rwv status` is read-only. It compares each repo's current HEAD against the pinned revision in `rwv.lock` and reports a `relation` for every repo:

| Relation | Meaning |
|---|---|
| `ok` | HEAD matches the lock |
| `ahead` | Local commits not yet recorded in the lock |
| `behind` | Lock records commits not yet in the local clone |
| `diverged` | Local and locked histories have parted ways |
| `no-lock` | No `rwv.lock` present for this project |
| `unreachable` | Clone is present but the locked SHA is not in its object store |
| `missing` | Clone directory is absent entirely |

A clean workspace shows all repos at `[ok]`. Any other relation identifies something to investigate. `missing` and `unreachable` both point toward the repair path below.

## Step 2 — confirm with `rwv doctor --locked`

```bash
rwv doctor --locked
```

`--locked` exits zero if and only if every repo tip matches its lock entry. Use this as a programmatic gate — in CI, in pre-op scripts, or after any recovery step:

```bash
rwv doctor --locked && echo "lock clean"
```

For repos that are `ahead` (local commits exist beyond the lock), the correct response depends on intent:

- **The local commits are deliberate and the lock should catch up:** run `rwv lock --commit` to re-derive the lock from current tips. See [lock-as-derived](../explanation/joints/lock-as-derived.md) for why this is always safe.
- **The local commits are unintended:** restore the clone to the locked revision manually (`git reset --hard <lock-sha>`) and re-run `rwv doctor --locked` to confirm.

## Repair — re-materialize missing or deleted clones

When `rwv status` shows `[missing]` and `rwv doctor` reports a `dangling reference`, the clone directory is gone. `rwv fetch` with no source re-clones every absent manifest member from the URL in `rwv.yaml` and checks out the revision pinned by the lock:

```bash
rwv fetch
```

Run from the workspace root (where `.rwv-active` lives). `rwv fetch` leaves already-present clones untouched and only acts on absent ones. After it completes, verify:

```bash
rwv doctor --locked
```

If the lock SHA is not in the remote's history (the `[unreachable]` case — the upstream was force-pushed or history was rewritten), `rwv fetch` will error on that repo. In that case, update the lock to reflect a reachable state: if the correct revision is on the branch tip, run `rwv lock --commit`; if the project needs to track a specific upstream commit, update the manifest's `version` field and re-run `rwv fetch`.

## What `rwv fetch` does not do

`rwv fetch` with no source is the in-place repair mode. It:

- Clones repos whose canonical directory is absent, pinning to the lock revision.
- Checks out the lock revision in repos that already exist, if their current HEAD differs.
- Does **not** fetch from the network when the lock SHA is already in the local object store.
- Does **not** advance lock entries — the lock is read-only during fetch; see [lock-as-derived](../explanation/joints/lock-as-derived.md).

## Related

- [lock-as-derived](../explanation/joints/lock-as-derived.md) — why the lock is output-only and why `rwv lock` always produces the right answer
- [CLI reference: `rwv status`](../reference/cli.md#rwv-status---json-) — full column definitions
- [CLI reference: `rwv doctor`](../reference/cli.md#rwv-doctor-) — `--locked` flag and check descriptions
- [CLI reference: `rwv fetch`](../reference/cli.md#rwv-fetch-source-) — in-place repair mode and `--frozen` flag
