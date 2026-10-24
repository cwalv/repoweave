# Migrate from git submodules or git-repo

Replace a parent repository's `.gitmodules` (or a `git-repo` manifest repository's `default.xml`) with a repoweave project in `projects/<name>/` and normal branch-attached clones under the weave root, then publish the project repo so teammates can `rwv fetch` it:

```bash
mkdir -p ~/weave && cd ~/weave
rwv init web-app
rwv add https://github.com/chatly/server.git --role owned
rwv add https://github.com/chatly/client.git --role owned
rwv add https://github.com/vendor/codec.git --role dependency
rwv lock --commit
git -C projects/web-app add -A && git -C projects/web-app commit -m "Initialize web-app project"
git -C projects/web-app remote add origin https://github.com/chatly/web-app.git
git -C projects/web-app push -u origin main
git -C projects/web-app remote set-head origin main
```

`rwv add` clones each member from its remote, so work that exists only in your old checkouts never reaches the weave. Push it first.

## Migrate from `git submodules`

### 1. Push every submodule's work

In the old superproject, list anything in a submodule that its remote does not have:

```bash
git -C ~/old-superproject submodule foreach --recursive 'git status --short; git log --oneline HEAD --branches --not --remotes'
```

Each `Entering '<path>'` line is followed by that submodule's uncommitted files and the commits no remote-tracking branch contains, including commits on a detached `HEAD`. Commit and push until every submodule prints nothing. Stashes are not listed; apply and commit any you want to keep.

### 2. List existing submodules

From the same checkout, inspect `.gitmodules` to collect each submodule's remote URL and branch:

```bash
git -C ~/old-superproject config --file .gitmodules --get-regexp 'submodule\..*\.(url|branch)'
```

### 3. Initialize the repoweave project and add members

Create a weave root, initialize the project, and add each submodule URL with the appropriate [role](../reference/roles.md) (`owned` for first-party code you push to, `fork` for writable forks, `dependency` for upstream third-party libraries):

```bash
mkdir -p ~/weave && cd ~/weave
rwv init web-app
rwv add https://github.com/chatly/server.git --role owned
rwv add https://github.com/chatly/client.git --role owned
rwv add https://github.com/vendor/codec.git --role dependency
```

`rwv add` records the remote's default branch as `version`. For a submodule whose `submodule.<name>.branch` names another branch, follow [Track a non-default branch](./add-a-repo.md#track-a-non-default-branch) after this step.

If the old superproject itself also contains application code (not just submodule gitlinks), add the superproject repository as an `owned` member too, then remove its submodule gitlinks in Step 6.

### 4. Enable ecosystem integrations and pin `rwv.lock`

If the old superproject held a top-level `Cargo.toml`, `package.json`, `pnpm-workspace.yaml`, `go.work`, or `pyproject.toml`, enable the corresponding integration in `projects/web-app/rwv.toml`, generate the workspace files, and commit the lock and project files:

```bash
$EDITOR projects/web-app/rwv.toml   # enable [integrations.<name>]
rwv doctor --fix
rwv materialize
rwv lock --commit
git -C projects/web-app add -A && git -C projects/web-app commit -m "Initialize web-app project"
```

`rwv lock --commit` commits `rwv.lock` and `rwv.toml`; the `git` commit carries `.gitattributes` and the generated workspace files.

To start from the exact commits the superproject's gitlinks pinned rather than from branch tips, check out each pinned commit in its canonical clone (`git -C github/chatly/server checkout --detach <gitlink-sha>`) and run `rwv lock --commit`. While a member is detached, `rwv lock` warns, `rwv doctor` reports it, `rwv push` refuses to push it, and a teammate's `rwv fetch` puts the member on the branch `rwv.toml` declares, at the pinned commit. When ready, re-attach with `git -C github/chatly/server switch main` and run `rwv lock --commit` again.

### 5. Publish the project repository

Push the project repo and set `origin/HEAD`, which `rwv push` requires:

```bash
git -C projects/web-app remote add origin https://github.com/chatly/web-app.git
git -C projects/web-app push -u origin main
git -C projects/web-app remote set-head origin main
```

Teammates can now replace their superproject checkout with `rwv fetch https://github.com/chatly/web-app.git` in an empty directory.

### 6. Remove `.gitmodules` from the old superproject

Once your team switches to `projects/web-app/` and `rwv fetch`, deinit and remove the submodule gitlinks from the old superproject repository, then push the result:

```bash
cd ~/old-superproject
git submodule deinit -f --all
git rm .gitmodules <submodule-paths...>
git commit -m "Retire git submodules in favor of repoweave project"
git push
```

Run this only after the Step 1 check prints nothing. Without `-f`, `git submodule deinit --all` stops (exit 128) at the first submodule whose checkout has uncommitted edits or differs from the commit the superproject records, after deinitializing the submodules before it. With `-f`, it deletes every submodule checkout, uncommitted edits included. Commits survive only in `.git/modules/<name>`, which neither command removes; delete `.git/modules/` once nothing in it is needed.

## Migrate from `git-repo` (`default.xml`)

A `git-repo` manifest repository (`manifests/default.xml`) already separates the composition record from member repositories, so it maps directly to `projects/<name>/rwv.toml`:

1. **Initialize the project:** Run `rwv init <project>` in a new weave root (or `rwv init <manifest-repository-url> --adopt` if you are converting the existing manifest repository in place).
2. **Add each `<project>` element from `default.xml`:** Construct the full remote URL from `<remote fetch="..."/>` + `<project name="..."/>` and run `rwv add <url> --role <owned|fork|dependency>`. If `<project revision="..."/>` names a non-default branch, follow [Track a non-default branch](./add-a-repo.md#track-a-non-default-branch).
3. **Replace `<copyfile>` / `<linkfile>` entries:** Configure the [`static-files` integration](../reference/integrations/static-files.md) in `projects/<project>/rwv.toml` to surface shared build files (`Makefile`, `justfile`, `.editorconfig`) at the weave root.
4. **Lock, commit, and publish:** Run `rwv lock --commit`, commit the remaining project files, and publish the project repo as in Steps 4 and 5 above. `rwv status` shows the branch each member repo is on; compare it with `version` in `rwv.toml`, because `rwv doctor` does not report a clone on a branch other than the one `rwv.toml` declares.

## Related

- [Create a project](./create-a-project.md) — initializing, configuring, and publishing `projects/<name>/`
- [Add a repo to a project](./add-a-repo.md) — adopting existing local clones without re-downloading
- [Alternatives comparison](../comparison.md) — architectural differences between `rwv`, `git submodules`, and `git-repo`
- [Roles reference](../reference/roles.md) — choosing between `owned`, `fork`, `dependency`, and `reference`
