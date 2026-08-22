# rwv add

## Purpose

Clone a repo and register it in the active project's `rwv.toml`, then
re-run integration activation so ecosystem files (Cargo workspace, npm
workspace, etc.) are updated to include the new member.

`add` is an **intent verb**: it mutates the manifest and regenerates
integration-managed content so the resulting files can be committed alongside
the `rwv.toml` change.

### Clone placement

Clones always land at the **canonical path** under the primary weave root,
regardless of whether `rwv add` is invoked from inside a workweave. This
reflects the shared-clone topology: manifest repos are global infrastructure,
with `git worktree` providing per-workweave isolation. (See
`rwv explain workweave` for the workweave topology model.)

If the clone already exists on disk at the canonical path, `add` skips the
network clone and registers the existing directory.

### Workweave-aware worktree materialization

When `rwv add` is invoked from inside a workweave, it also creates a
`git worktree` for the new repo inside the workweave directory. The
ephemeral branch follows the standard `{project}--{workweave}` naming
convention (matching `rwv workweave create`) — flat, with no third component
derived from the source branch. `add` records an ownership receipt for it, so
`rwv workweave delete` destroys it along with the rest of the workweave's
refs. If the canonical clone has no commits yet (from `--new`), the worktree
creation is skipped; run `rwv sync` after the first commit to materialize it.

### Local-path form

If the argument is a relative path (no URL scheme) and the path already exists
under the primary weave root, `add` infers the URL from the clone's `origin`
remote and registers it without a network clone.

## Invocation

```
rwv add <url> [--role <role>] [--new] [--project <name>]
```

- `<url>` — repository URL. Parsed through the registry topology to derive
  the canonical local path. Accepts `https://`, `git@`, or a relative path
  to an existing directory under the primary weave root.
- `--role <role>` — role for the repo in the manifest (`owned`, `fork`,
  `dependency`, `reference`). Defaults to `owned`.
- `--new` — initialize a new local repo (via `git init`) at the canonical
  path instead of cloning. Use when creating a new repo that has no upstream
  yet. The argument is the canonical path (`registry/owner/repo`); its URL is
  derived from the path via the registry topology.
- `--project <name>` — operate on this project rather than the active project.
  Does not change `.rwv-active`.

Run `rwv --help add` for the full clap surface.

## Output

Progress messages to stderr:

- `Added '<repo-path>' to manifest` — repo was registered.
- `Repository already exists in manifest at '<repo-path>'` — idempotent; no
  change made.
- `Directory already exists at '<path>', skipping clone` — clone present,
  not re-cloned.

Integration activation output (symlinks created/removed) goes to stderr.

## Exit codes

- `0` — repo added (or already present), manifest updated, activation
  succeeded.
- non-zero — URL could not be resolved, clone failed, manifest could not be
  written, or activation returned an error.

## Examples

Add an owned repo (clone + register):

```
rwv add https://github.com/myorg/myrepo
```

Add a dependency (read-only; won't be pushed or edited):

```
rwv add https://github.com/third-party/lib --role dependency
```

Register a repo that's already cloned:

```
rwv add github/myorg/existing-repo
```

Initialize a brand-new local repo:

```
rwv add github/myorg/new-project --new
```

## Common errors

- *unrecognized URL* — the URL does not parse through any built-in registry
  and has no derivable local path. Check the URL format.
- *clone failed* — network error or authentication problem; inspect git output.
- *manifest parse failure* — `rwv.toml` could not be loaded; verify the file
  is valid TOML.
