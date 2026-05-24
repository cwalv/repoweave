# Add a repo to a project

`rwv add` clones the repo (if it isn't on disk), registers it in the active project's `rwv.yaml`, and re-runs integration hooks to wire it into ecosystem workspace files.

## Add by URL

```bash
rwv add https://github.com/example/some-lib.git --role dependency
```

Clones to the canonical path `github/example/some-lib/`, adds the entry to `rwv.yaml`, regenerates ecosystem files. Run the ecosystem install command afterward to pick up the new package:

```bash
npm install   # or: uv sync, cargo build, etc.
```

`rwv add` writes to the *active workspace's* manifest, which is the one in CWD's workspace. From inside a workweave, the entry lands in the workweave's `rwv.yaml`; from the primary weave, it lands in primary's. (This was previously a footgun where `rwv add` always wrote to primary — now resolved.)

## Role

`--role <role>` sets the role for this repo within the project. Roles signal change resistance:

| Role | Meaning |
|---|---|
| `owned` | Your code. Change freely. |
| `fork` | Forked upstream. Ideally accept changes upstream. Cloned with remote name `upstream`, not `origin`. |
| `dependency` | Code you build against. Changes need upstream acceptance. |
| `reference` | Cloned for reading/study. No local changes; excluded from build graphs. |

If `--role` is omitted, the default is `owned`. See [reference/roles](../reference/roles.md) for the full definitions.

Note: the on-disk YAML currently spells this `role: primary`; the docs use `owned` because the rename to `owned` is in progress. Both spellings will parse during the transition.

## Add as a reference repo

```bash
rwv add https://github.com/interesting/library.git --role reference
```

Reference repos are visible in the workspace but excluded from ecosystem workspace configs (they don't appear in `go.work`, `package.json` workspaces, etc.). Use this instead of a manual `git clone` so the repo is tracked — `rwv doctor` reports untracked repos as orphans.

## Add a new repo

To create a brand-new repo at the canonical path:

```bash
rwv add github/chatly/auth --new
```

This `git init`s `github/chatly/auth/`, infers the URL from the path convention, adds it as `role: owned`, and updates ecosystem files. Push manually once you have a remote.

## Brownfield: adopt an existing clone

You already have a clone somewhere on disk and want to bring it into the project at the canonical path.

```bash
# move the clone to the canonical path
mv ~/some-other-place/some-lib github/example/some-lib

# register it
rwv add https://github.com/example/some-lib.git --role dependency
```

`rwv add` notices the directory already exists, skips the clone step, and just updates the manifest. The clone needs to be at the canonical `github/example/some-lib/` path — the manifest is keyed by path, and `rwv fetch` on another machine clones to that exact location.

For a project-wide brownfield migration (you already have many clones and want to create a project that adopts them), use `rwv init --adopt`:

```bash
rwv init my-project --adopt
```

This scans the working tree, builds an initial `rwv.yaml` from what it finds, and writes the project repo.

## Remove a repo

```bash
rwv remove github/example/some-lib
```

Removes the entry from `rwv.yaml` and re-runs integrations. The clone stays on disk — other projects might depend on it. To also delete the clone:

```bash
rwv remove github/example/some-lib --delete
```

`--delete` checks that no other project's manifest references the path, then removes the directory. Use `--force` to bypass the safety check.

## Related

- [reference/roles](../reference/roles.md) — full role definitions and change-resistance semantics
- [reference/formats](../reference/formats.md) — `rwv.yaml` shape
- [workspace lens](../explanation/lenses/workspace.md) — how roles factor into the workspace model
