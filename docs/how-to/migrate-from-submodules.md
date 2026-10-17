# Migrate from git submodules or git-repo

Replace a parent repository's `.gitmodules` (or a `git-repo` manifest repository's `default.xml`) with a repoweave project in `projects/<name>/` and normal branch-attached clones under the weave root:

```bash
mkdir -p ~/weave && cd ~/weave
rwv init web-app
rwv add https://github.com/chatly/server.git --role owned
rwv add https://github.com/chatly/client.git --role owned
rwv add https://github.com/vendor/codec.git --role dependency
rwv lock --commit
git -C projects/web-app add -A && git -C projects/web-app commit -m "Initialize web-app project"
```

## Migrate from `git submodules`

### 1. List existing submodules

From your existing superproject checkout, inspect `.gitmodules` to collect each submodule's remote URL and branch:

```bash
git -C ~/old-superproject config --file .gitmodules --get-regexp 'submodule\..*\.(url|branch)'
```

### 2. Initialize the repoweave project and add members

Create a weave root, initialize the project, and add each submodule URL with the appropriate [role](../reference/roles.md) (`owned` for first-party code you push to, `fork` for writable forks, `dependency` for upstream third-party libraries):

```bash
mkdir -p ~/weave && cd ~/weave
rwv init web-app
rwv add https://github.com/chatly/server.git --role owned
rwv add https://github.com/chatly/client.git --role owned
rwv add https://github.com/vendor/codec.git --role dependency
```

If the old superproject itself also contains application code (not just submodule gitlinks), add the superproject repository as an `owned` member too, then remove its submodule gitlinks in Step 4.

### 3. Enable ecosystem integrations and pin `rwv.lock`

If the old superproject held a top-level `Cargo.toml`, `package.json`, `pnpm-workspace.yaml`, `go.work`, or `pyproject.toml`, enable the corresponding integration in `projects/web-app/rwv.toml`, generate the workspace files, and commit the lock and project files:

```bash
$EDITOR projects/web-app/rwv.toml   # enable [integrations.<name>]
rwv doctor --fix
rwv activate web-app
rwv lock --commit
git -C projects/web-app add -A && git -C projects/web-app commit -m "Initialize web-app project"
```

If you need to preserve the exact commit SHAs the superproject's gitlinks pinned before advancing to branch tips, check out those commits in each canonical clone, run `rwv lock --commit`, and then re-attach to the tracked branch when ready.

### 4. Remove `.gitmodules` from the old superproject

Once your team switches to `projects/web-app/` and `rwv fetch`, deinit and remove the submodule gitlinks from the old superproject repository:

```bash
cd ~/old-superproject
git submodule deinit --all
git rm .gitmodules <submodule-paths...>
git commit -m "Retire git submodules in favor of repoweave project"
```

## Migrate from `git-repo` (`default.xml`)

A `git-repo` manifest repository (`manifests/default.xml`) already separates the composition record from member repositories, so it maps directly to `projects/<name>/rwv.toml`:

1. **Initialize the project:** Run `rwv init <project>` in a new weave root (or `rwv init <manifest-repo-url> --adopt` if you are converting the existing manifest repository in place).
2. **Add each `<project>` element from `default.xml`:** Construct the full remote URL from `<remote fetch="..."/>` + `<project name="..."/>` and run `rwv add <url> --role <owned|fork|dependency>`. If `<project revision="..."/>` names a non-default branch, set `version = "<branch>"` on that entry in `projects/<project>/rwv.toml` and run `rwv fetch`.
3. **Replace `<copyfile>` / `<linkfile>` entries:** Configure the [`static-files` integration](../reference/integrations/static-files.md) in `projects/<project>/rwv.toml` to surface shared build files (`Makefile`, `justfile`, `.editorconfig`) at the weave root.
4. **Lock and verify:** Run `rwv lock --commit` and `rwv doctor` to verify that all member clones are attached to their tracked branches and `rwv.lock` is current.

## Related

- [Create a project](./create-a-project.md) — initializing, configuring, and publishing `projects/<name>/`
- [Add a repo to a project](./add-a-repo.md) — adopting existing local clones without re-downloading
- [Alternatives comparison](../comparison.md) — architectural differences between `rwv`, `git submodules`, and `git-repo`
- [Roles reference](../reference/roles.md) — choosing between `owned`, `fork`, `dependency`, and `reference`
