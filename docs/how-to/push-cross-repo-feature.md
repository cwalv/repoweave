# Push a cross-repo feature

Publish locked cross-repo changes from the primary weave to remotes in dependency-safe order (member repositories first, project repository last):

```bash
rwv push
```

## Preconditions and default scope

Before pushing any remote, and under `--dry-run` too, `rwv push` refuses unless all of these hold:

- **Primary weave only:** `rwv push` refuses when invoked inside a workweave (`push-from-workweave`) so ephemeral workweave branches are never pushed to shared remotes. Land workweave changes into the primary weave with `rwv sync-to` first.
- **Canonical branch in the project repo:** `projects/<project>/` must be on its tracked branch, not a detached `HEAD` (`head-not-on-branch`).
- **`origin/HEAD` recorded:** the project repo must know its remote's default branch (`no-remote-default-branch`). A clone made by `rwv fetch` has it; after creating the remote yourself, run `git -C projects/<project> remote set-head origin main`.
- **Committed project repo:** `projects/<project>/` must have no uncommitted changes to tracked files and no untracked `rwv.toml` or `rwv.lock` (`dirty-checkout`), because push publishes the project repo's last commit. Commit with `rwv lock --commit`, `rwv update --commit`, or `git -C projects/<project> commit`.
- **Lock agreement:** Every member repository's `HEAD` must match the revision pinned in `rwv.lock` (`lock-state-mismatch`).
- **Attached member branches:** every member repository in scope must be on a branch, not a detached `HEAD` (`unpushable-repo-branch`).

By default, `rwv push` pushes member repositories with `role = "owned"` and `role = "fork"` (`fork` URLs must point to your writable fork; see [Roles reference](../reference/roles.md#fork)) and skips `dependency` and `reference` repositories. The project repository (`projects/<project>/`) is pushed last, only after every in-scope member push succeeds, so collaborators never fetch a `rwv.lock` that references unpushed member commits.

Preview the plan without contacting remotes:

```bash
rwv push --dry-run
```

## Selector filters and parallel push

Override the default `[owned, fork]` scope with `--role` or `--repo` selectors (repeated flags combine as a union; regexes match against the full `github/org/repo` manifest path):

```bash
rwv push --role owned                       # only owned repos
rwv push --repo glob:'github/chatly/*'      # path glob
rwv push --repo re:'proto'                  # regex matched against full manifest path
rwv push -j 4                               # push up to 4 member repos in parallel
```

Even with `-j <N>`, the project repository is pushed serially at the end after all member pushes succeed.

## Recover from a lock-state mismatch

If a member repository's `HEAD` differs from `rwv.lock`, `rwv push` refuses before touching the network:

```text
rwv push: 1 repo(s) disagree with /home/user/work/projects/web-app/rwv.lock:
  - github/chatly/server: HEAD abc1234 differs from lock e1f2a3b
Hint: run `rwv lock --commit` to record each listed repo's HEAD in the lock, or move the repo's branch to its lock commit without detaching it: `git reset --keep <lock commit>` in the repo (after `git branch <name>` to keep the commits it moves off).
Error: lock-state mismatch — refusing to push before clone state and lock agree

rwv explain lock-state-mismatch
```

- **To publish the new local commits:** Snapshot and commit the updated lock, then push:
  ```bash
  rwv lock --commit
  rwv push
  ```
- **To publish the existing lock:** Move the member's branch back to the locked revision, keeping the unlocked commits on a branch of their own, then push:
  ```bash
  git -C github/chatly/server branch keep-local
  git -C github/chatly/server reset --keep <lock-sha>
  rwv push
  ```
  `rwv fetch` does not do this: it refuses to move a branch backward (not a fast-forward). A detached checkout at the locked revision (`rwv fetch --detach-checkouts` or `git checkout <lock-sha>`) does not either: `rwv push` refuses a detached member (`unpushable-repo-branch`).

## Related

- [CLI reference — `rwv push`](../reference/cli.md#rwv-push-) — flags and `--json` / NDJSON output shapes
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave commits into the primary weave before pushing
- [Run a command across repos](./run-a-command-across-repos.md) — ad-hoc cross-repo commands via `rwv status --json`
- [Verb vs. composition](../explanation/joints/verb-vs-composition.md) — why coordinated publishing is a core verb
- [VCS as seam](../explanation/joints/vcs-as-seam.md) — remote-push contract
