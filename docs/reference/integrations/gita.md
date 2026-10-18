# gita

[gita](https://github.com/nosarthur/gita) provides a multi-repo dashboard (`gita ll`), cross-repo git delegation (`gita super`), groups, and context scoping.

| | |
|---|---|
| Default enabled | no (opt-in) |
| Auto-detects | all active member repos (`owned`, `fork`, `dependency`) |
| Generates | `gita/repos.csv`, `gita/groups.csv` |
| Install hook | — |

**Opt-in.** Repoweave's default recommendation for bulk multi-repo operations is [unix composition](../../how-to/run-a-command-across-repos.md) using `rwv status --json`, `jq`, and `xargs`. Gita is supported as an alternative for users who prefer a dedicated CLI with summary views.

For build/test/lint across packages, prefer the ecosystem's own workspace commands (`npm run --workspaces test`, `pnpm -r run test`, `cargo test --workspace`) — they understand package dependency ordering. Gita's value is at the **git layer**: status, bulk fetch/pull, and seeing which repos have uncommitted work.

## Configuration

The table takes only `enabled`:

```toml
[integrations.gita]
enabled = true
```

After enabling it, `rwv doctor` reports both CSVs as `managed-file-missing`, and `rwv doctor --fix` writes them into `projects/<project>/gita/` and surfaces them at the weave or workweave root as `gita/repos.csv` and `gita/groups.csv`. From then on the authoring verbs (`rwv add`, `remove`, `update`, `role`) keep them current.

gita looks for its files in `$GITA_PROJECT_HOME/gita/`, so point `GITA_PROJECT_HOME` at the weave or workweave root itself, not at its `gita/` directory:

```bash
# .envrc at the weave or workweave root
export GITA_PROJECT_HOME="$PWD"
```

`GITA_PROJECT_HOME` replaces (not supplements) gita's default config directory. Each workweave gets its own `gita/` config — gita commands are always scoped to the current context's repos.

## Managed and generated files

Two headerless CSV files in `projects/<project>/gita/` (surfaced at `<weave>/gita/`), in the format gita itself writes. Rows are sorted by repo name; `reference` repos are left out.

**`repos.csv`** — one row per active repo, `path,name,type,flags`:

```csv
/home/dev/workspace/github/chatly/protocol,protocol,,
/home/dev/workspace/github/chatly/server,server,,
/home/dev/workspace/github/chatly/web,web,,
```

- **path**: absolute path to the repo in the current weave or workweave, spelled as gita spells one on the host: backslash-separated on Windows (`C:\…\github\chatly\protocol`)
- **name**: display name used in gita commands (basename of the repo path)
- **type**, **flags**: empty

**`groups.csv`** — one row per role (`owned`, `fork`, `dependency`) that has at least one active repo, `name:repos:path`, with the repo names space-separated:

```text
fork:engine-io:
owned:protocol server web:
```

This enables role-scoped gita commands:

```bash
gita ll owned          # dashboard for owned repos only
gita super owned pull  # pull only owned repos
```

Both files are fully rwv-owned and rewritten in full by each authoring pass. Because `repos.csv` holds absolute paths, a committed copy names the repos of the checkout it was written in; in any other weave or workweave the verify pass reports it as `managed-file-drift`, and `rwv doctor --fix` rewrites it.

### Why not `gita freeze` / `gita clone`?

Gita has its own serialization format (`gita freeze` outputs CSV with URL, name, path, branch) and can bootstrap from it via `gita clone`. This overlaps with `rwv lock` / `rwv fetch`, but `gita freeze` records branch names rather than pinned commit SHAs, so it is less precise for reproducibility. Repoweave stores role annotations and project configuration in `rwv.toml` and pins exact 40-character commit SHAs in JSON in `rwv.lock`. Because the two mechanisms would overlap awkwardly, the `gita` integration only generates the runtime CSV config files that gita reads.

## Install hook

None.

## Deactivation

Removes `gita/repos.csv` and `gita/groups.csv`, and removes the `gita/` directory only if it is empty (preserving any other files placed in `gita/`).

- **Disabled** (`enabled = false`): `rwv materialize` removes each CSV whose bytes match what `rwv` would author, and removes the weave-root links. A CSV you edited by hand is not `rwv`'s to remove: it stays in `projects/<project>/gita/`, and `rwv doctor` does not report it.

## Diagnostics

Reported by `rwv doctor`. Verify rows also print on the context verbs listed under [Hook points](./index.md#hook-points).

| Kind | Severity | Condition | Repair |
|---|---|---|---|
| [`tool-missing`](../doctor-findings.md#tool-missing) | warning | The integration is enabled, but `gita` is not on `PATH`. | Install `gita`. |
| [`managed-file-missing`](../doctor-findings.md#managed-file-missing) | warning | A CSV is absent while the project has at least one active repo. | `rwv doctor --fix` |
| [`managed-file-drift`](../doctor-findings.md#managed-file-drift) | warning | A CSV's bytes differ from what `rwv.toml` implies, or it cannot be read. | `rwv doctor --fix` |
| [`disabled-integration-artifact`](../doctor-findings.md#disabled-integration-artifact) | warning | The integration is disabled, and a CSV whose bytes match what `rwv` would author is still in `projects/<project>/gita/`. | `rwv materialize` |

## Related

- [run a command across repos](../../how-to/run-a-command-across-repos.md) — the default unix-composition recipe gita is an alternative to
- [static-files](./static-files.md) — surfacing an `.envrc` that exports `GITA_PROJECT_HOME`
