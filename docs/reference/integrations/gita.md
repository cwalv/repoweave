# gita

[gita](https://github.com/nosarthur/gita) provides a multi-repo dashboard (`gita ll`), cross-repo git delegation (`gita super`), groups, and context scoping.

| | |
|---|---|
| Default enabled | no (opt-in) |
| Auto-detects | all repos |
| Generates | `gita/` directory (CSV configs) |
| Lock hook | — |

**Opt-in.** Repoweave's default recommendation for bulk multi-repo operations is [unix composition](../../how-to/run-a-command-across-repos.md) using `rwv status --json`, `jq`, and `xargs`. Gita is supported as an alternative for users who prefer a dedicated CLI with "summary sugar."

To enable:

```toml
[integrations.gita]
enabled = true
```

The activation hook generates gita's config files into a `gita/` directory inside the weave (or workweave) directory, scoped to the project's repos. Point gita at this directory via `GITA_PROJECT_HOME`:

```bash
# .envrc in weave or workweave dir
export GITA_PROJECT_HOME="$PWD/gita"
```

`GITA_PROJECT_HOME` replaces (not supplements) gita's default config directory. Each workweave gets its own gita config — gita commands are always scoped to the current context's repos.

For build/test/lint across packages, prefer the ecosystem's own workspace commands (`npm run --workspaces test`, `pnpm -r run test`, `cargo test --workspace`) — they understand package dependency ordering. gita's value is at the **git layer**: status, bulk fetch/pull, seeing which repos have uncommitted work.

## Generated files

Two CSV files in `gita/`:

**`repos.csv`** — header row, then one line per repo:

```csv
path,name,flags
/home/dev/workspace/github/chatly/server,server,
/home/dev/workspace/github/chatly/web,web,
/home/dev/workspace/github/chatly/protocol,protocol,
```

- **path**: absolute path to the repo (or worktree in a workweave)
- **name**: display name used in gita commands (basename of the repo path)
- **flags**: extra args inserted after `git` in delegated commands (currently unused)

**`groups.csv`** — header row, groups derived from role annotations:

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

Removes the entire `gita/` directory.

## Check

Warns if `gita` is not on PATH.

## Why not `gita freeze` / `gita clone`?

gita has its own serialization format (`gita freeze` outputs CSV with URL, name, path, branch) and can bootstrap from it via `gita clone`. This overlaps with `rwv lock` / `rwv fetch`, but `gita freeze` records branch names rather than pinned SHAs, so it's less precise for reproducibility. repoweave's `rwv.lock` also carries role annotations and YAML structure. The two mechanisms would overlap awkwardly, so the gita integration only generates the config files that gita needs at runtime — it doesn't use gita's own freeze/clone flow.

## Related

- [run a command across repos](../../how-to/run-a-command-across-repos.md) — the default unix-composition recipe gita is an alternative to
