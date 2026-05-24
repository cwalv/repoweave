# Push a cross-repo feature

`rwv push` coordinates pushes across the manifest repos that compose a project. It encodes per-role policy and lock-precondition checks that a hand-rolled `xargs git push` can't.

## Happy path

From the workspace whose work you want to push:

```bash
rwv push
```

`rwv push` walks the manifest, applying per-role push policy:

- `role: owned` — push the current branch; require the working tip to match `rwv.lock`.
- `role: fork` — skip; you push your fork repos manually to your fork's remote.
- `role: dependency` / `role: reference` — skip; you don't push upstream code.

The project repo is pushed last, so its lock-bearing commit lands after the manifest repos that lock pins are now reachable.

Anchored by `tests/doc_claims_push_test.rs`.

## Selector filters

`rwv push` accepts the shared selector grammar — same as `fetch` and `update`:

```bash
rwv push --role owned                       # only owned repos
rwv push --repo glob:'github/chatly/*'      # owned + path glob
rwv push --repo re:'^github/chatly/(server|web)$'
```

Patterns accept `Exact` (no prefix), `re:` (regex), and `glob:` (glob). Repeated flags are union. `--role` is case-insensitive. See [reference/cli — Selector grammar](../reference/cli.md#selector-grammar).

## Lock-precondition recovery

If `rwv push` refuses because the lock SHA doesn't match HEAD in some repo:

```text
error: lock precondition failed
  github/chatly/server: tip abc1234 ≠ lock e1f2a3b

hint: commit the project repo's lock update first, then re-run
```

The recovery decision tree:

1. **You meant to lock new state.** Make sure manifest-repo work is committed, then:
   ```bash
   rwv lock
   git -C projects/web-app commit -am "lock: feature X"
   rwv push
   ```
2. **You meant to push the existing lock.** The lock is what's authoritative; reset the manifest-repo tips back to what the lock says:
   ```bash
   rwv fetch --locked
   rwv push
   ```
3. **You meant to push past a divergence.** The lock-precondition is a safety check; `--force` bypasses it. Use with care:
   ```bash
   rwv push --force
   ```

The lock-precondition prevents the common footgun: pushing manifest-repo work before the project repo's lock-bearing commit, leaving downstream consumers seeing the new manifest tips with the old lock pointing at the old SHAs.

## Parallel push

```bash
rwv push -j 4
```

Parallel mode runs up to N pushes concurrently. The project repo is still pushed last after all manifest repos converge.

## When to use `rwv push` vs `git push`

`rwv push` earns its keep over a hand-rolled `xargs git push` for:

- **Role-aware policy.** Auto-skip `fork` repos (which usually have origin set to upstream-of-record and would 403 on push).
- **Lock-precondition check.** Prevents the manifest-pushed-but-lock-stale footgun.
- **Manifest-aware ordering.** Project repo pushed last after manifest repos.

For one-repo pushes, plain `git push` is fine. For ad-hoc cross-repo composition, use the `rwv status --json | jq | xargs` recipes in [run a command across repos](./run-a-command-across-repos.md) — `rwv push` is the principled answer when the coordination is itself the value, not the bulk.

See [verb-vs-composition](../explanation/joints/verb-vs-composition.md) for the design rationale on when an `rwv` verb earns its keep.

## Related

- [reference/cli — push](../reference/cli.md#push) — full flag surface
- [vcs-as-seam](../explanation/joints/vcs-as-seam.md) — `Vcs::push_with_role` and per-role policy
- [run a command across repos](./run-a-command-across-repos.md) — when unix composition is the right shape
- [bring workweave work home](./bring-workweave-work-home.md) — push usually follows sync
