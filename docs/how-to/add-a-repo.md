# Add a repo to a project

Clone a repository into the weave, register it in the active workspace's `rwv.toml`, and update ecosystem workspace files:

```bash
rwv add https://github.com/example/some-lib.git --role dependency
```

`rwv add` clones the repository to its canonical path under the weave root (`github/example/some-lib/`), populates `version = "<branch>"` from the remote's default branch (`origin/HEAD`), writes the entry to the active workspace's `rwv.toml`, regenerates managed ecosystem files (`Cargo.toml`, `package.json`, `go.work`, `pyproject.toml`), and runs integration install hooks in the primary weave. When invoked from inside a workweave, it writes to that workweave's `rwv.toml` and creates the member worktree there.

If you already added the entry to `rwv.toml` by hand, running `rwv add <url>` with the matching URL finishes the setup: it clones the repository if missing, creates the worktree when run inside a workweave, and regenerates integration files.

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
- **`local` registry:** Requires `--param root=<abs-path>` naming an existing directory outside the weave. `rwv` initializes a bare repository at `<root>/<owner>/<repo>` and clones the member from it so `git push -u origin` works immediately. Parameters can also be passed as JSON via `--params-json '{"root":"/srv/repos","owner":"chatly","repo":"auth"}'`.

In both cases, `rwv add --new` populates `version = "<branch>"` from the new repository's initial `HEAD` branch (for example, `"main"`), defaults `role` to `"owned"`, and updates ecosystem workspace files.

## Adopt an existing clone or project

If a clone already sits at its canonical path under the weave root, pass either its URL or its relative path:

```bash
mv ~/some-other-place/some-lib github/example/some-lib
rwv add github/example/some-lib --role dependency
```

`rwv add` skips the network clone, reads `origin` and its default branch to populate `url` and `version`, and updates `rwv.toml` and integration files.

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

## Related

- [Roles reference](../reference/roles.md) — role definitions and materialization behavior
- [File formats reference](../reference/formats.md) — `rwv.toml` schema
- [Workspace lens](../explanation/lenses/workspace.md) — how roles fit into the workspace model
