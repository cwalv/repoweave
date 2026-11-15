# Migrate from git submodules or git-repo

Replace a parent repository's `.gitmodules` (or a `git-repo` manifest repository's `default.xml`) with a repoweave project in `projects/<name>/` and normal branch-attached clones under the weave root, then publish the project repo so teammates can `rwv clone` it:

```bash
mkdir -p ~/weave && cd ~/weave
rwv init web-app
rwv member add https://github.com/chatly/server.git --role owned
rwv member add https://github.com/chatly/client.git --role owned
rwv member add https://github.com/vendor/codec.git --role dependency
rwv member set-url projects/web-app https://github.com/chatly/web-app.git
rwv push
```

`rwv member add` clones each member from its remote, so work that exists only in your old checkouts never reaches the weave. Push it first.

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
rwv member add https://github.com/chatly/server.git --role owned
rwv member add https://github.com/chatly/client.git --role owned
rwv member add https://github.com/vendor/codec.git --role dependency
```

`rwv member add` records the remote's default branch as `branch`. For a submodule whose `submodule.<name>.branch` names another branch, follow [Track a non-default branch](./add-a-repo.md#track-a-non-default-branch) after this step.

If the old superproject itself also contains application code (not just submodule gitlinks), add the superproject repository as an `owned` member too, then remove its submodule gitlinks in Step 6.

### 4. Enable ecosystem integrations and pin `rwv.lock`

If the old superproject held a top-level `Cargo.toml`, `package.json`, `pnpm-workspace.yaml`, `go.work`, or `pyproject.toml`, enable the corresponding integration in `projects/web-app/rwv.toml`, generate the workspace files, and commit them:

```bash
rwv integration set-enabled <name> true   # enable [integrations.<name>]; authors its files
rwv materialize
rwv lock --commit
```

`rwv integration set-enabled` commits the key together with the workspace files it authors, and `rwv lock --commit` commits `rwv.lock`.

To start from the exact commits the superproject's gitlinks pinned rather than from branch tips, pin each member at its gitlink commit: `rwv member pin github/chatly/server <gitlink-sha> --commit`. The commit must be one the member's declared branch or its upstream reaches, or the verb refuses (`pin` `not on declared branch`). Your checkout stays on its branch at the tip, and a teammate's `rwv clone` puts the member at the pin. While the branch tip is past the pin, `rwv status` reports the member as `pin` `ahead`; when ready, `rwv lock --commit` records the tip.

### 5. Publish the project repository

Record the project repo's remote, then publish with `rwv push`, which sends the members' branches, the branch's record and the project branch:

```bash
rwv member set-url projects/web-app https://github.com/chatly/web-app.git
rwv push
```

Teammates can now replace their superproject checkout with `rwv clone https://github.com/chatly/web-app.git` in an empty directory.

### 6. Remove `.gitmodules` from the old superproject

Once your team switches to `projects/web-app/` and `rwv clone`, deinit and remove the submodule gitlinks from the old superproject repository, then push the result:

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

1. **Initialize the project:** Run `rwv init <project>` in a new weave root. To convert the existing manifest repository in place, run `rwv clone <manifest-repository-url> --init-manifest` instead: it writes the `rwv.toml` the repository lacks, commits it, and then clones as usual; without the flag `rwv clone` refuses (`not-a-project`) and prints this line.
2. **Add each `<project>` element from `default.xml`:** Construct the full remote URL from `<remote fetch="..."/>` + `<project name="..."/>` and run `rwv member add <url> --role <owned|fork|dependency>`. If `<project revision="..."/>` names a non-default branch, follow [Track a non-default branch](./add-a-repo.md#track-a-non-default-branch).
3. **Replace `<copyfile>` / `<linkfile>` entries:** Configure the [`static-files` integration](../reference/integrations/static-files.md) in `projects/<project>/rwv.toml` to surface shared build files (`Makefile`, `justfile`, `.editorconfig`) at the weave root.
4. **Commit and publish:** Commit the remaining project files, and publish the project repo as in Steps 4 and 5 above. `rwv status` shows the branch each member repo is on; compare it with `branch` in `rwv.toml`, because `rwv doctor` does not report a clone on a branch other than the one `rwv.toml` declares.

## Related

- [Create a project](./create-a-project.md) — initializing, configuring, and publishing `projects/<name>/`
- [Add a repo to a project](./add-a-repo.md) — adopting existing local clones without re-downloading
- [Alternatives comparison](../comparison.md) — architectural differences between `rwv`, `git submodules`, and `git-repo`
- [Roles reference](../reference/roles.md) — choosing between `owned`, `fork`, `dependency`, and `reference`
