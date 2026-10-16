# gita

[gita](https://github.com/nosarthur/gita) provides a multi-repo dashboard (`gita ll`), cross-repo git delegation (`gita super`), groups, and context scoping.

| | |
|---|---|
| Default enabled | no (opt-in) |
| Auto-detects | all active repos (`owned`, `fork`, `upstream`) |
| Generates | `gita/repos.csv`, `gita/groups.csv` |
| Install hook | — |

**Opt-in.** Repoweave's default recommendation for bulk multi-repo operations is [unix composition](../../how-to/run-a-command-across-repos.md) using `rwv status --json`, `jq`, and `xargs`. Gita is supported as an alternative for users who prefer a dedicated CLI with summary views.

To enable:

```toml
[integrations.gita]
enabled = true
```

Authoring verbs (`rwv add`, `remove`, `update`, `role`, `doctor --fix`) write `gita/repos.csv` and `gita/groups.csv` into `projects/<project>/gita/`, and `rwv activate` (or `rwv materialize`) surfaces them into `gita/` at the weave or workweave root via symlinks. Point gita at this directory via `GITA_PROJECT_HOME`:

```bash
# .envrc in weave or workweave dir
export GITA_PROJECT_HOME="$PWD/gita"
```

`GITA_PROJECT_HOME` replaces (not supplements) gita's default config directory. Each workweave gets its own `gita/` config — gita commands are always scoped to the current context's repos.

For build/test/lint across packages, prefer the ecosystem's own workspace commands (`npm run --workspaces test`, `pnpm -r run test`, `cargo test --workspace`) — they understand package dependency ordering. Gita's value is at the **git layer**: status, bulk fetch/pull, and seeing which repos have uncommitted work.

## Generated files

Two CSV files in `projects/<project>/gita/` (surfaced at `<weave>/gita/`):

**`repos.csv`** — header row, then one line per active repo:

```csv
path,name,flags
/home/dev/workspace/github/chatly/server,server,
/home/dev/workspace/github/chatly/web,web,
/home/dev/workspace/github/chatly/protocol,protocol,
```

- **path**: absolute path to the repo in the current weave or workweave
- **name**: display name used in gita commands (basename of the repo path)
- **flags**: extra args inserted after `git` in delegated commands (empty)

**`groups.csv`** — header row, then one row per role (`owned`, `fork`, `upstream`) that has at least one repo:

```csv
group,repos
fork,engine-io
owned,server web protocol
```

This enables role-scoped gita commands:

```bash
gita ll owned          # dashboard for owned repos only
gita super owned pull  # pull only owned repos
```

## Deactivation

Removes `gita/repos.csv` and `gita/groups.csv`, and removes the `gita/` directory only if it is empty (preserving any other files placed in `gita/`). When `gita` is disabled in `rwv.toml`, `rwv materialize` only removes `gita/repos.csv` and `gita/groups.csv` if their bytes match what `rwv` would author; hand-edited CSVs are left intact and reported as `USER-HELD` by `rwv doctor`.

## Check

`rwv doctor` reports:

- `malformed-settings` (error) if `[integrations.gita]` contains unknown keys.
- `tool-missing` (warning) if `gita` is not on `PATH` when active repos are present.

## Verify (drift)

`rwv doctor` and context verbs (`rwv activate`, `rwv fetch`, `rwv workweave add`) verify `gita/repos.csv` and `gita/groups.csv`:

- When enabled, missing (`managed-file-missing`) or drifted (`managed-file-drift`) CSVs are reported as warnings and are auto-fixable via `rwv doctor --fix`.
- When disabled (`enabled = false`), leftover `gita/repos.csv` or `gita/groups.csv` files in `projects/<project>/gita/` are reported as `orphaned-owned-region` (`DRIFT` if their bytes match `rwv`'s output, or `USER-HELD` if modified by hand).

## Why not `gita freeze` / `gita clone`?

Gita has its own serialization format (`gita freeze` outputs CSV with URL, name, path, branch) and can bootstrap from it via `gita clone`. This overlaps with `rwv lock` / `rwv fetch`, but `gita freeze` records branch names rather than pinned commit SHAs, so it is less precise for reproducibility. Repoweave stores role annotations and project configuration in `rwv.toml` and pins exact 40-character commit SHAs in JSON in `rwv.lock`. Because the two mechanisms would overlap awkwardly, the `gita` integration only generates the runtime CSV config files that gita reads.

## Related

- [run a command across repos](../../how-to/run-a-command-across-repos.md) — the default unix-composition recipe gita is an alternative to
