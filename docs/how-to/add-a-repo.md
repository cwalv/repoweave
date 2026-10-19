# Add a repo to a project

Clone a repository into the weave, register it in the active workspace's `rwv.toml`, and update ecosystem workspace files:

```bash
rwv add https://github.com/example/some-lib.git --role dependency
```

`rwv add` clones the repository to its canonical path under the weave root (`github/example/some-lib/`), populates `version = "<branch>"` from the remote's default branch (`origin/HEAD`), writes the entry to the active workspace's `rwv.toml`, regenerates managed ecosystem files (`Cargo.toml`, `package.json`, `go.work`, `pyproject.toml`), and runs integration install hooks in the primary weave. When invoked from inside a workweave, it writes to that workweave's `rwv.toml` and creates the member worktree there.

If you already added the entry to `rwv.toml` by hand, running `rwv add <url>` with the matching URL finishes the setup: it clones the repository if missing, creates the worktree when run inside a workweave, and regenerates integration files.

The argument is a URL or a registry shorthand (`github/example/some-lib`). To clone from a repository on local disk, spell it as a `file://` URL (`rwv add file:///srv/repos/some-lib.git`); a plain path refuses with `unresolvable-repo-source`, and the message names both the `file://` spelling and `--adopt`.

## Record the change

`rwv add` changes `rwv.toml` and the regenerated integration files but adds no `rwv.lock` entry. `rwv lock --commit` writes the entry and commits `rwv.lock` with `rwv.toml`:

```bash
rwv lock --commit
```

It refuses while another tracked file in `projects/<project>/` has uncommitted changes, such as a `package.json` the add regenerated. Then write the lock and commit the project repo with git:

```bash
rwv lock
git -C projects/<project> add -A
git -C projects/<project> commit -m "Add some-lib"
```

## Set the repository role

`--role <role>` sets the member's role within the project (default `owned`):

| Role | Meaning |
|---|---|
| `owned` | Your code. Change freely. |
| `fork` | Forked upstream (`url` points to your writable fork). Pushed by default like `owned`. |
| `dependency` | Upstream code you build against. Excluded from default `rwv push`. |
| `reference` | Cloned for reading and study with read-only tracked files; excluded from ecosystem build graphs. |

To add a repository for reading without wiring it into build graphs:

```bash
rwv add https://github.com/interesting/library.git --role reference
```

Use `rwv add --role reference` instead of an untracked `git clone` under the weave root so `rwv doctor` does not report the directory as an orphaned clone. See [Roles reference](../reference/roles.md) for full role semantics.

## Create a new repository (`--new`)

With `--new`, the positional argument is a creation address (`<registry>/<owner>/<repo>` or bare `<registry>` with `--param key=value`). Creation is dispatched by registry:

```bash
# Hosted registry (github, gitlab, bitbucket): git init + remote set to minted URL
rwv add github/chatly/auth --new

# Local registry: initializes a bare repo at /srv/repos/chatly/auth and clones from it
rwv add local/chatly/auth --new --param root=/srv/repos
```

- **Hosted registries (`github`, `gitlab`, `bitbucket`):** `rwv` initializes the repository at `github/chatly/auth/` and sets `origin` to the minted URL without contacting the network. Create the remote repository on the forge when you are ready to push.
- **`local` registry:** Requires `--param root=<dir>` naming an existing directory outside the weave. A relative `<dir>` resolves against the directory you run `rwv` in (or the `-C`/`-w` directory), and the manifest records the absolute path. `rwv` initializes a bare repository at `<root>/<owner>/<repo>` and clones the member from it so `git push -u origin` works immediately. Parameters can also be passed as one JSON object, with the bare registry name as the address: `rwv add local --new --params-json '{"root":"/srv/repos","owner":"chatly","repo":"auth"}'`. Each parameter may come from only one place, so the address shorthand, `--param`, and `--params-json` cannot name the same one.

In both cases, `rwv add --new` populates `version = "<branch>"` from the new repository's initial `HEAD` branch (for example, `"main"`), defaults `role` to `"owned"`, and updates ecosystem workspace files. The new repository has no commits yet: until you make its first commit, `rwv lock` refuses (its message ends "make an initial commit, then re-run rwv lock") and `rwv doctor` reports an error for it.

## Adopt an existing clone or project

If a clone already sits at its canonical path under the weave root, register it with `--adopt` and its path:

```bash
mkdir -p github/example
mv ~/some-other-place/some-lib github/example/some-lib
rwv add --adopt github/example/some-lib --role dependency
```

A relative path resolves against the directory you run `rwv` in, so from `github/example/` the same clone is `rwv add --adopt some-lib`; the member path recorded is always its position under the weave root. `rwv add --adopt` skips the network clone, reads `origin` and its default branch to populate `url` and `version`, and updates `rwv.toml` and integration files. With no clone at that path it refuses with `no-clone-to-adopt`.

Passing the URL instead also skips the clone when the clone at the placement already has that URL as its `origin`. When its `origin` names a different repository, `rwv add <url>` refuses with `foreign-clone-at-placement` and writes nothing; the message names `rwv add --adopt <path>` to register the clone under the URL it records. A directory or file at the placement that is not a clone refuses with `non-clone-at-placement`; move it aside and re-run.

If the *project repository* itself already exists on a remote and carries an `rwv.toml`, adopt it into `projects/<name>/` with `rwv init --adopt` and then materialize its member repositories:

```bash
rwv init https://github.com/example/my-project.git --adopt
rwv fetch
```

## Remove a repo

```bash
rwv remove github/example/some-lib
```

This removes the entry from `rwv.toml` and updates integration files while leaving the checkout on disk. To also delete the checkout in the current workspace (the canonical clone in the primary weave, or the member worktree in a workweave):

```bash
rwv remove github/example/some-lib --delete
```

`--delete` refuses if the checkout has uncommitted changes, commits unreachable from any other clone, or (in the primary weave) live workweave worktrees or references from another project's `rwv.toml` (pass `--delete-shared-clone` to override the shared-project check). If you already removed the entry from `rwv.toml` by hand, running `rwv remove <path> [--delete]` finishes regenerating integration files and deleting the leftover checkout.

Record a removal as you record an addition (see [Record the change](#record-the-change)). When the project repo tracks its `.code-workspace` file, a removal changes that file, so `rwv lock --commit` refuses and the `rwv lock` plus `git commit` route applies.

## Related

- [Roles reference](../reference/roles.md) — role definitions and materialization behavior
- [File formats reference](../reference/formats.md) — `rwv.toml` schema
- [Workspace lens](../explanation/lenses/workspace.md) — how roles fit into the workspace model
