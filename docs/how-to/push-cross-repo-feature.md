# Push a cross-repo feature

Publish locked cross-repo changes from the primary weave to remotes in dependency-safe order (member repositories first, project repository last):

```bash
rwv push
```

## Preconditions and default scope

Before pushing any remote, and under `--dry-run` too, `rwv push` refuses unless all of these hold:

- **Primary weave only:** `rwv push` refuses when invoked inside a workweave (`push-from-workweave`) so ephemeral workweave branches are never pushed to shared remotes. Land workweave changes into the primary weave with `rwv sync-to` first.
- **Canonical branch in the project repo:** `projects/<project>/` must be on its tracked branch; a detached or unborn `HEAD` refuses with `head-not-on-branch`, and any other branch with `project-repo-off-canonical-branch`.
- **The remote's default branch recorded:** the project repo must know the default branch of the remote it publishes to (`no-remote-default-branch`). A clone made by `rwv clone` has it; after creating the remote yourself, run the `git remote set-head <remote> -a` the refusal names, to record the branch the remote reports as its default.
- **Committed project repo:** `projects/<project>/` must have no uncommitted changes to tracked files and no untracked `rwv.toml` or `rwv.lock` (`edits`), because push publishes the project repo's last commit. When only `rwv.toml` or `rwv.lock` is uncommitted, the refusal offers `rwv lock --commit`; otherwise commit or stash with the commands it prints.
- **Pins:** Every member repository is judged against the revision pinned in `rwv.lock`. A member with no pin (`pin` `none`), a pin no ref reaches (`unreachable`), a `HEAD` behind the pin (`behind`) or diverged from it (`diverged`), or commits beyond the pin that nothing else holds (`unpinned`) refuses, and the refusal prints the choice that clears it.
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

## Recover from an unpinned commit

If a member repository holds commits beyond its pin that nothing else holds, `rwv push` refuses before touching the network:

```text
rwv push: github/chatly/server: pin unpinned (HEAD abc1234def56 is 1 commit beyond the pin e1f2a3b4c5d6)
    abc1234def56 fix the handshake
choices:
  record it: the lock, and `rwv.toml` as it stands
    rwv lock --commit

rwv explain pin
```

- **To publish the new local commits:** Record the updated lock and commit it, then push:
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
  `rwv update` does not do this: it moves a branch forward to the lock, never backward. A detached checkout at the locked revision (`git checkout <lock-sha>`) does not either: `rwv push` refuses a detached member (`unpushable-repo-branch`).

A member the lock has no pin for refuses as `pin` `none`, naming `rwv member pin <member> <rev> --commit`; `rwv lock` gives it none, since it records a pin forward only.

## Related

- [CLI reference — `rwv push`](../reference/cli.md#rwv-push-) — flags and the `--json` envelope
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave commits into the primary weave before pushing
- [Run a command across repos](./run-a-command-across-repos.md) — ad-hoc cross-repo commands with `rwv each`
- [Verb vs. composition](../explanation/joints/verb-vs-composition.md) — why coordinated publishing is a core verb
- [VCS as seam](../explanation/joints/vcs-as-seam.md) — remote-push contract
