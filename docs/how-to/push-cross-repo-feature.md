# Push a cross-repo feature

Publish a weave branch from any workspace in dependency-safe order: member repositories first, then the branch's record, then the project repository:

```bash
rwv push
```

## Preconditions and default scope

Before pushing any remote, `rwv push` refuses unless all of these hold:

- **A recorded, named branch:** the project repo's checked-out branch must have a record (`target` `unrecorded` otherwise: say how the branch is used with `rwv branch set-mode`) and a name rwv publishes. The name `rwv workweave create` gives a workweave made without a branch of its own, `<project>--<name>`, is never published (`name` `minted`): give the workweave's branch a name of its own with `rwv branch rename <new>`, which renames the project branch and each member's branch of the same name, and moves the record. A project repo on a detached `HEAD` is a detached workspace, which runs no push (`alignment` `project detached`). To publish a branch the workspace has not checked out, name it: `rwv push --branch <branch>`.
- **Committed project repo:** `projects/<project>/` must have no uncommitted changes to tracked files and no untracked `rwv.toml` or `rwv.lock` (`edits`), because push publishes the project repo's last commit. When only `rwv.toml` or `rwv.lock` is uncommitted, the refusal offers `rwv lock --commit`; otherwise commit or stash with the commands it prints.
- **Reachable remotes:** `push` fetches the remotes first. One it could not reach that the push publishes to, or judges a pin by, refuses it (`push-branch` `unconfirmed`); run `rwv fetch` once the remote answers.
- **Aligned members with pins:** each member the push acts on must sit on the branch the checked-out branch's record expects (`rwv update` moves it), and is judged against the revision pinned in `rwv.lock`. A member with no pin (`pin` `none`), a pin no ref reaches (`unreachable`), a `HEAD` behind the pin (`behind`) or diverged from it (`diverged`), or commits beyond the pin that nothing else holds (`unpinned`) refuses, and the refusal prints the choice that clears it. `rwv push` never writes the lock.
- **Merges that join the members:** a merge commit in the range being published whose parents changed one member's pin to different commits must leave the lock pinning a commit that contains both (`pin` `unmerged`). The refusal names the merge and offers the VCS's merge for that member; `rwv lock --commit` then records it. A merge the operator abandons instead, by re-pinning the member backward by name (`rwv member pin <m> <rev> --commit`, the refusal's other choice) to a commit that does not reach the pin the merge left, is waived and listed (`pin` `waived`). Only a lock commit an rwv verb made by name records that re-pin; the same lock change committed with git waives nothing.

By default, `rwv push` pushes member repositories with `role = "owned"` and `role = "fork"` (`fork` URLs must point to your writable fork; see [Roles reference](../reference/roles.md#fork)), then the branch's record and the drops this machine has made, then the project repository. It pushes a `dependency` or `reference` repository only when `--role` or `--repo` names it. A feature's member branch is pushed only when it holds commits no remote-tracking branch reaches. The project repository is pushed last, only after every in-scope member push succeeded and every pin its lock holds is reached by what the remotes now hold, so collaborators never fetch a `rwv.lock` that references unpushed member commits. When a pin is not reached (a `dependency` member's commit no remote holds, say), the project branch is withheld (`publish` `withheld`) and the refusal lists the branches that do reach the pin; `rwv push --repo <member>` publishes the member, and the next `rwv push` publishes the project. A feature's member branch that holds nothing beyond the target's pin is not pushed, so where its pin is work the target holds and has not published (a landing without `--retire`, say), the refusal offers `rwv push --branch <target>` instead, which runs from any workspace; the next `rwv push` then publishes the feature.

Preview the plan. The preview runs the push's pruning fetch, so its verdict is the push's, and it writes nothing but what a fetch writes:

```bash
rwv push --dry-run
```

## Selector filters and parallel push

Override the default `[owned, fork]` scope with `--role` or `--repo` selectors (repeated flags combine as a union; regexes match against the full `github/org/repo` manifest path). A selector narrows the push to the repos it names: the project branch and its record are pushed only when `--repo projects/<project>` names the project repo.

```bash
rwv push --role owned                       # only owned repos
rwv push --repo glob:'github/chatly/*'      # path glob
rwv push --repo re:'proto'                  # regex matched against full manifest path
rwv push --repo github/chatly/dep --repo projects/chatly   # a dependency and the project branch
rwv push -j 4                               # fetch and push up to 4 member repos in parallel
```

Even with `-j <N>`, the project repository is pushed serially at the end after all member pushes succeed.

## Replace a rewritten branch

A branch that was amended or rebased after it was published has diverged from its push branch, and `rwv push` refuses it (`push-branch` `diverged`). To publish the rewrite, name the repo:

```bash
rwv push --overwrite github/chatly/server
```

The remote tip is kept as a backup first, and the replacement lands only if the push branch still holds the tip the push read, so a colleague's push in between refuses it. It is refused for a branch some weave branch declares and for a declared-mode project branch (`protected`), and while another live lock pins a commit only the replaced tip reaches. `rwv.lock` is not written; pin the amended member first with `rwv member pin <member> HEAD --commit`.

## Recover from an unpinned commit

If a member repository holds commits beyond its pin that nothing else holds, `rwv push` refuses before touching the network:

```text
rwv push: github/chatly/server: pin unpinned (HEAD abc1234def56 is 1 commit beyond the pin e1f2a3b4c5d6)
    abc1234def56 fix the handshake
choices:
  record it: the lock, and `rwv.toml` as it stands
    rwv lock --commit
  pin it at a commit you choose on its declared branch
    rwv member pin github/chatly/server abc1234def56789a0b1c2d3e4f5a6b7c8d9e0f12 --commit
  take theirs; yours is kept in a backup
    rwv update --remote --reset-to-upstream github/chatly/server --commit

rwv explain pin
```

- **To publish the new local commits:** Record the updated lock and commit it, then push. `rwv lock --commit` records every member's tip; `rwv member pin <member> <rev> --commit` pins one member at the commit you name (its `HEAD` for the commits listed):
  ```bash
  rwv lock --commit
  rwv push
  ```
- **To take the remote's work instead:** `rwv update --remote --reset-to-upstream github/chatly/server --commit` moves the member's branch to its upstream, keeps the unlocked commits in a backup, and commits the lock with the re-pin; then `rwv push`.
- **To publish the existing lock:** Move the member's branch back to the locked revision, keeping the unlocked commits on a branch of their own, then push:
  ```bash
  git -C github/chatly/server branch keep-local
  git -C github/chatly/server reset --keep <lock-sha>
  rwv push
  ```
  `rwv update` does not do this: it moves a branch forward to the lock, never backward. A detached checkout at the locked revision (`git checkout <lock-sha>`) does not either: `rwv push` refuses a detached member (`alignment` `detached`).

A member the lock has no pin for refuses as `pin` `none`, naming `rwv member pin <member> <rev> --commit`; `rwv lock` gives it none, since it records a pin forward only.

## Related

- [CLI reference — `rwv push`](../reference/cli.md#rwv-push-) — flags and the `--json` envelope
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave commits into the primary weave
- [Run a command across repos](./run-a-command-across-repos.md) — ad-hoc cross-repo commands with `rwv each`
- [Verb vs. composition](../explanation/joints/verb-vs-composition.md) — why coordinated publishing is a core verb
- [VCS as seam](../explanation/joints/vcs-as-seam.md) — remote-push contract
