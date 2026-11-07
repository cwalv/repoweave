# Reconcile repos with the lock

Check whether every member repository matches `rwv.lock`, and repair missing or drifted checkouts:

```bash
rwv status            # inspect per-repo lock relation
rwv doctor --locked   # exit 0 iff every repo HEAD matches rwv.lock
rwv update            # re-clone missing repos and fast-forward behind checkouts to rwv.lock
```

## Step 1 — detect with `rwv status`

`rwv status` is read-only. It compares each repository's current `HEAD` against `rwv.lock` and reports a `relation`:

| Relation | Meaning |
|---|---|
| `ok` | `HEAD` matches `rwv.lock` |
| `ahead` | Local commits exist beyond the locked revision |
| `behind` | `rwv.lock` pins a newer commit than the local checkout |
| `diverged` | Local and locked histories have diverged |
| `no-lock` | No `rwv.lock` entry exists for this repository |
| `unreachable` | Checkout is present, but the locked SHA is not in its local object store |
| `missing` | Checkout directory is absent from disk |

## Step 2 — reconcile `ahead` or `diverged` repos

Use `rwv doctor --locked` as a scriptable check that exits `0` only when every member repository's `HEAD` equals its `rwv.lock` entry:

```bash
rwv doctor --locked && echo "lock clean"
```

When a repository is `ahead` or `diverged`, pick the action that matches your intent:

- **Keep the local commits and update the lock:** Run `rwv lock --commit` to record the current repository tips in `rwv.lock` and commit the updated lock.
- **Inspect or build at the locked revision without moving the branch:** Run `git -C <repo> switch --detach <lock-sha>`. `rwv status` reports the detached checkout, and `rwv update` returns it to its branch when it holds nothing a branch lacks.
- **Discard unintended local commits on the branch:** Run `git -C <repo> reset --hard <lock-sha>`, then verify with `rwv doctor --locked`. `rwv` provides no resting-workspace rewind verb because resting workspaces have no pre-op savepoint to recover discarded commits from; see [Destructive operations](../explanation/destructive-operations.md).
- **Finish a hand-edited `rwv.toml` removal:** If you deleted a repository entry from `rwv.toml` by hand and its checkout or `rwv.lock` entry is still present, run `rwv member remove <path> [--delete]` to regenerate integration files and remove the leftover checkout, then record the change. When only `rwv.toml` changed, `rwv lock --commit` does it; when the removal also changed a tracked generated file such as the `.code-workspace` file, `rwv lock --commit` refuses, so run `rwv lock` and commit with `git -C projects/<project> commit -am "Remove <path>"`.

## Step 3 — repair `missing`, `behind`, or `unreachable` repos

When `rwv status` shows `missing` or `behind`, run `rwv update` from the weave root (for a project that is not active, `rwv update --project <name>`):

```bash
rwv update
rwv doctor --locked
```

`rwv update`:

- Clones any missing member repository from its `url` in `rwv.toml`, attached to the branch declared in `branch = "..."` and positioned at the revision in `rwv.lock`.
- Inside a workweave, also creates the missing member's worktree at that revision: on the workweave's `<project>--<workweave>` branch for an `owned` or `fork` member, detached for a `dependency` or `reference` member.
- Aligns existing checkouts with the branch the project repo has checked out: a checkout `behind` the lock is fast-forwarded to it on its branch, and one on a branch the workspace does not expect moves to the expected state when every commit it holds stays on another ref. A checkout that would leave commits behind stays where it is and is reported with them and the commands that keep them.
- Contacts the network only to clone a missing member repository; it does not fetch into an existing checkout. `rwv fetch` does.
- Never moves an `rwv.lock` entry and writes no lock. A member repository the lock has no entry for is cloned at the tip of its declared branch when missing and left where it is when present; `rwv lock` records it.

When `rwv status` shows `unreachable`, the locked SHA is missing from the local object store:

1. If the remote still holds the commit, run `rwv fetch` to pull the missing objects into every repository, then `rwv update` to align the checkout. `rwv update` alone leaves that repository where it is and reports `pin` `unreachable`, because it does not fetch into an existing checkout.
2. If upstream rewrote history and the locked SHA no longer exists on the remote, align the checkout to the desired commit and run `rwv lock --commit` (or `rwv update --remote --commit`) to record a reachable revision.

## Related

- [Lock as derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is derived output that `rwv update` aligns checkouts to
- [Destructive operations](../explanation/destructive-operations.md) — why resting-workspace branch rewinds require explicit `git` commands
- [CLI reference — `rwv status`](../reference/cli.md#rwv-status---json-) — status columns and JSON output
- [CLI reference — `rwv doctor`](../reference/cli.md#rwv-doctor-) — `--locked` and `--fix` behavior
- [CLI reference — `rwv update`](../reference/cli.md#rwv-update-) — repair mode
- [CLI reference — `rwv fetch`](../reference/cli.md#rwv-fetch-) — downloading new commits without moving anything
