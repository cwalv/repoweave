# Push a cross-repo feature

Publish locked cross-repo changes from the primary weave to remotes in dependency-safe order (member repositories first, project repository last):

```bash
rwv push
```

## Preconditions and default scope

Before pushing any remote, `rwv push` enforces three preconditions:

- **Primary weave only:** `rwv push` refuses when invoked inside a workweave so ephemeral workweave branches are never pushed to shared remotes. Land workweave changes into the primary weave with `rwv sync-to` first.
- **Canonical branch in the project repo:** `projects/<project>/` must be on its tracked branch, not a detached `HEAD`.
- **Lock agreement:** Every member repository's `HEAD` must match the revision pinned in `rwv.lock`.

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
Hint: run `rwv lock` to capture local state, or `git checkout` in each repo to align with the lock.
Error: lock-state mismatch — refusing to push before clone state and lock agree
```

- **To publish the new local commits:** Snapshot and commit the updated lock, then push:
  ```bash
  rwv lock --commit
  rwv push
  ```
- **To publish the existing lock:** Align the diverged checkout to the locked revision (`rwv fetch` or `git checkout`), then re-run `rwv push`.

## Related

- [CLI reference — `rwv push`](../reference/cli.md#rwv-push-) — flags and `--json` / NDJSON output shapes
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave commits into the primary weave before pushing
- [Run a command across repos](./run-a-command-across-repos.md) — ad-hoc cross-repo commands via `rwv status --json`
- [Verb vs. composition](../explanation/joints/verb-vs-composition.md) — why coordinated publishing is a core verb
- [VCS as seam](../explanation/joints/vcs-as-seam.md) — remote-push contract
