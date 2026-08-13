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
- **The local commits are unintended:** `rwv fetch --detach-checkouts` puts the checkout on the locked revision without moving or discarding the branch — enough to build and test against the lock. Plain `rwv fetch` refuses, and no verb rewinds the branch itself: that stays a deliberate act of yours (`git reset --hard <lock-sha>` in the affected repo). Re-run `rwv doctor --locked` afterwards to confirm.

  The absence of a verb there is a decision, and the reason is that rwv's destructive verbs are all paired with a savepoint — `rwv abort` can undo itself because `rwv sync` wrote one first, and `rwv sync --discard-local-commits` savepoints what it discards. A rewind-to-lock verb has no such pairing: it is reached from a resting workspace, with no operation in flight to have recorded a pre-op tip, so it would destroy commits held nowhere else and offer nothing to recover them from. `sync --discard-local-commits` is the near-verb for the case that actually recurs — divergence found during a sync, where the savepoint exists — and it is the one to reach for where it fits. Raw `git reset --hard` here is not a gap in the command surface; it is the operation staying where its consequences are visible.

## Repair — re-materialize missing or deleted clones

When `rwv status` shows `[missing]` and `rwv doctor` reports a `dangling reference`, the clone directory is gone. `rwv fetch` with no source re-clones every absent manifest member from the URL in `rwv.toml` and checks out the revision pinned by the lock:

```bash
rwv fetch
```

Run from the workspace root (where `.rwv-active` lives). It does not act only on the absent ones: a present clone the lock covers is realigned too, and that realignment can refuse — see [What `rwv fetch` does not do](#what-rwv-fetch-does-not-do) below. After it completes, verify:

```bash
rwv doctor --locked
```

`[unreachable]` has two sub-cases, and in-place `rwv fetch` repairs neither (with the clone present it performs a local checkout, no network fetch). If the remote still has the locked revision (the local object store lost it — a prune, a shallow history), plain `git fetch` in the affected repo re-pulls the object; re-run `rwv status` to confirm `ok`. If the lock SHA is not in the remote's history either (the upstream was force-pushed or history was rewritten), `rwv fetch` will error on that repo. In that case, update the lock to reflect a reachable state: if the correct revision is on the branch tip, run `rwv lock --commit`; if the project needs to track a specific upstream commit, update the manifest's `version` field and re-run `rwv fetch`.

## What `rwv fetch` does not do

`rwv fetch` with no source is the in-place repair mode. It:

- Clones repos whose canonical directory is absent, born attached to the branch `version:` declares, positioned at the lock revision.
- Moves repos that already exist onto the lock revision *without changing what HEAD is attached to* — it fast-forwards the branch the checkout is on, and a repo already at the pin is left alone.
- Refuses when it cannot do that: when the pin is not a fast-forward of that branch, and when the checkout is on a branch the manifest does not declare. `--detach-checkouts` waives both by materializing the pin on a detached HEAD, moving no branch.
- Does **not** fetch from the network when the lock SHA is already in the local object store.
- Does **not** advance lock entries — the lock is read-only during fetch; see [lock-as-derived](../explanation/joints/lock-as-derived.md).

## Related

- [lock-as-derived](../explanation/joints/lock-as-derived.md) — why the lock is output-only and why `rwv lock` always produces the right answer
- [CLI reference: `rwv status`](../reference/cli.md#rwv-status---json-) — full column definitions
- [CLI reference: `rwv doctor`](../reference/cli.md#rwv-doctor-) — `--locked` flag and check descriptions
- [CLI reference: `rwv fetch`](../reference/cli.md#rwv-fetch-source-) — in-place repair mode and `--frozen` flag
