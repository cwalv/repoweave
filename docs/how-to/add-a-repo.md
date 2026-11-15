# Add a repo to a project

Clone a repository into the weave, register it in the active workspace's `rwv.toml`, and update ecosystem workspace files:

```bash
rwv member add https://github.com/example/some-lib.git --role dependency
```

`rwv member add` clones the repository to its canonical path under the weave root (`github/example/some-lib/`), populates `branch = "<branch>"` from the remote's default branch, writes the entry to the active workspace's `rwv.toml`, regenerates managed ecosystem files (`Cargo.toml`, `package.json`, `go.work`, `pyproject.toml`), and runs integration install hooks in the primary weave. It then commits the entry, the regenerated files and `rwv.lock` together, with the commit it cloned as the member's first pin. When invoked from inside a workweave, it writes to that workweave's `rwv.toml` and creates the member worktree there.

A hand edit is recoverable: if an entry reached `rwv.toml` without the verb, running `rwv member add <url>` with the matching URL converges it. It clones the repository if missing, creates the worktree when run inside a workweave, regenerates integration files, and gives the member the pin it lacks in a commit with the entry.

The argument is a URL or a registry shorthand (`github/example/some-lib`), and `rwv.toml` records it as you wrote it; every later clone, on any machine, resolves the shorthand through the same registry. To clone from a repository on local disk, spell it as a `file://` URL (`rwv member add file:///srv/repos/some-lib.git`); a plain path refuses with `unresolvable-repo-source`, and the message names both the `file://` spelling and `--adopt`.

## The commit an add makes

`rwv member add` commits what it did, so there is nothing left to record: `rwv.toml` with the new entry, the regenerated integration files and `rwv.lock` with the member's first pin land in one commit. The pin is the commit the clone is at, the tip of the declared branch just cloned. Another tracked file in `projects/<project>/` with uncommitted changes stays out of the commit and as it is, named in one line.

A member with no pin is reported as `pin` `none`, and the report names the verbs that give one: `rwv member pin <path> <rev> --commit`, or `rwv update --remote --repo <path> --commit` for its upstream's tip. `rwv lock` gives none, since it only moves a pin forward.

A role change and a removal commit themselves too, as `rwv member set-role` and `rwv member remove` below say; no `rwv lock --commit` follows them.

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
rwv member add https://github.com/interesting/library.git --role reference
```

Use `rwv member add --role reference` instead of an untracked `git clone` under the weave root so `rwv doctor` does not report the directory as an orphaned clone. See [Roles reference](../reference/roles.md) for full role semantics.

## Change a repository's role

`rwv member set-role` changes the role of a repository already in `rwv.toml`, for example to keep a repository as read-only study material, or to start working on it again:

```bash
rwv member set-role github/example/some-lib reference
rwv member set-role github/example/some-lib owned
```

It edits `rwv.toml`, regenerates the ecosystem workspace files (a `reference` repository is left out of every build graph), and sets the checkout's file permissions for the new role. The change is committed alone, with the files it regenerates. A change to `reference` refuses while the repository has uncommitted changes to tracked files; commit, stash or discard them first. See `rwv explain member` for the full behavior.

## Track a non-default branch

`rwv member add` records the remote's default branch. To track another branch, declare it with `rwv member set-branch`, which commits the entry and the member's new pin together:

```bash
rwv fetch
rwv member set-branch github/example/some-lib dev
```

The branch must exist locally or on a remote as last fetched; `rwv fetch` brings in a remote's. To create a branch that exists nowhere, add `--new`. The member switches onto the branch and is pinned at its tip, in either direction, by name.

## Create a new repository (`--new`)

With `--new`, the positional argument is a creation address (`<registry>/<owner>/<repo>` or bare `<registry>` with `--param key=value`). Creation is dispatched by registry:

```bash
# Hosted registry (github, gitlab, bitbucket): git init + remote set to minted URL
rwv member add github/chatly/auth --new

# Local registry: initializes a bare repo at /srv/repos/chatly/auth and clones from it
rwv member add local/chatly/auth --new --param root=/srv/repos
```

- **Hosted registries (`github`, `gitlab`, `bitbucket`):** `rwv` initializes the repository at `github/chatly/auth/` and adds a remote with the minted URL without contacting the network. Create the remote repository on the forge when you are ready to push.
- **`local` registry:** Requires `--param root=<dir>` naming an existing directory outside the weave. A relative `<dir>` resolves against the directory you run `rwv` in (or the `-C`/`-w` directory), and the manifest records the absolute path. `rwv` initializes a bare repository at `<root>/<owner>/<repo>` and clones the member from it so `git push -u origin` works immediately. Parameters can also be passed as one JSON object, with the bare registry name as the address: `rwv member add local --new --params-json '{"root":"/srv/repos","owner":"chatly","repo":"auth"}'`. Each parameter may come from only one place, so the address shorthand, `--param`, and `--params-json` cannot name the same one.

In both cases, `rwv member add --new` populates `branch = "<branch>"` from the new repository's initial `HEAD` branch (for example, `"main"`), defaults `role` to `"owned"`, and updates ecosystem workspace files. The new repository has no commits yet, so it has no pin, and `rwv doctor` reports an error for it until it has one. Make its first commit, then pin it: `rwv member pin github/chatly/auth HEAD --commit`.

## Adopt an existing clone or project

If a clone already sits at its canonical path under the weave root, register it with `--adopt` and its path:

```bash
mkdir -p github/example
mv ~/some-other-place/some-lib github/example/some-lib
rwv member add --adopt github/example/some-lib --role dependency
```

A relative path resolves against the directory you run `rwv` in, so from `github/example/` the same clone is `rwv member add --adopt some-lib`; the member path recorded is always its position under the weave root. `rwv member add --adopt` skips the network clone, reads `origin` and its default branch to populate `url` and `branch`, updates `rwv.toml` and integration files, and commits them with the member's first pin: the clone's HEAD where it sits on the declared branch or that branch's upstream reaches it. A clone holding commits only it has is added without a pin, reported as `pin` `none`. With no clone at that path it refuses with `no-clone-to-adopt`.

Passing the URL instead also skips the clone when the clone at the placement already has a remote with that URL. When none of its remotes names that repository, `rwv member add <url>` refuses with `foreign-clone-at-placement` and writes nothing; the message names `rwv member add --adopt <path>` to register the clone under the URL it records. A directory or file at the placement that is not a clone refuses with `non-clone-at-placement`; move it aside and re-run.

If the *project repository* itself already exists on a remote and carries an `rwv.toml`, clone it into `projects/<name>/`, member repositories included, with `rwv clone`:

```bash
rwv clone https://github.com/example/my-project.git
```

## Remove a repo

```bash
rwv member remove github/example/some-lib
```

This removes the entry from `rwv.toml` and updates integration files while leaving the checkout on disk. To also delete the checkout in the current workspace (the canonical clone in the primary weave, or the member worktree in a workweave):

```bash
rwv member remove github/example/some-lib --delete
```

`--delete` refuses if the checkout has uncommitted changes, commits unreachable from any other clone (a submodule store it removes included), a remote that does not answer, or (in the primary weave) live workweave worktrees or references from another project's `rwv.toml` (pass `--delete-shared-clone` to override the shared-project check). If you already removed the entry from `rwv.toml` by hand, running `rwv member remove <path> [--delete]` finishes regenerating integration files and deleting the leftover checkout. For a clone whose remote is gone for good, `rwv member remove <path> --delete --discard-unpublished` removes it, listing what no remote is confirmed to hold.

The removal is committed with the member's pin dropped and the files it regenerated, leaving every other uncommitted edit where it is.

## Related

- [Roles reference](../reference/roles.md) — role definitions and materialization behavior
- [File formats reference](../reference/formats.md) — `rwv.toml` schema
- [Workspace lens](../explanation/lenses/workspace.md) — how roles fit into the workspace model
