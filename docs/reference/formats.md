# File formats

Reference for the on-disk files repoweave reads and writes.

## `rwv.toml` — project manifest

The source of truth for which repos belong to a project. Committed in the project repo at `projects/<name>/rwv.toml`. The one file on this page you author by hand; rwv writes the rest.

```toml
[repositories."github/chatly/server"]
type = "git"
url = "https://github.com/chatly/server.git"
version = "main"
role = "owned"

[repositories."github/chatly/web"]
type = "git"
url = "https://github.com/chatly/web.git"
version = "main"
role = "owned"

[repositories."github/chatly/protocol"]
type = "git"
url = "https://github.com/chatly/protocol.git"
version = "main"
role = "owned"

[repositories."github/socketio/engine.io"]
type = "git"
url = "https://github.com/chatly/engine.io.git"
version = "main"
role = "fork"

[integrations.npm-workspaces]
enabled = false

[integrations.pnpm-workspaces]
enabled = true
```

Repo paths contain `/`, so they are quoted table keys. A project with no repos yet is a bare `[repositories]`.

### Moving from `rwv.yaml`

Earlier versions read a YAML manifest at `projects/<name>/rwv.yaml`. A project directory still holding one is reported by `rwv doctor` as `legacy-manifest-format` and does not load; rewrite the file as `rwv.toml` in the shape above, then delete the old one.

There is no `--fix` arm. The manifest is yours — the comments and the key order in it are things you wrote, and neither has a mechanical translation between the two formats. A conversion that guessed at them would hand back a file you would have to audit line by line anyway.

`rwv.toml` accepts only three top-level tables: `repositories`, `integrations`, and `workweave` (unknown top-level keys are rejected). Project identity comes from the `projects/<name>/` directory path rather than a `[project]` table, and the legacy `[lock]` table is retired because `rwv.lock` always records 40-character commit SHAs.

### `repositories` entries

Keyed by canonical local path (`<registry>/<owner>/<repo>`). Each entry:

| Field | Required | Description |
|---|---|---|
| `type` | yes | VCS type. Currently `git`. |
| `url` | yes | Clone source: a URL (`https://…`, `git@…`, `file://` for a local remote) or registry shorthand (`github/owner/repo`), as `rwv add` was given it. Every clone resolves shorthand through the built-in registries, so `rwv fetch` on another machine clones the same remote. |
| `version` | yes | Tracked branch name (manifest); 40-character commit SHA (lock). |
| `role` | yes | One of `owned`, `fork`, `dependency`, `reference`. |
| `submodules` | no | `"local"` (the default) or `"skip"`. See [Submodules](#submodules). |

Any other key in an entry is rejected, naming it.

`version` in `rwv.toml` is branch-only by design: the manifest declares which branch to *track*, while `rwv.lock` records the exact commit SHA you *are* on. Specifically:

- **`rwv add <url>`** populates `version` from the remote's default branch (`origin/HEAD`), and `rwv add --new` from the branch the new repo is on.
- A `version` that reads as a commit id (seven or more hex digits) or a release tag (`v2.0`, `v0.3.4-rc1`) fails to parse, naming the value and its line, and `rwv add` refuses with [`version-is-a-pin`](./refusals.md#version-is-a-pin) rather than record such a branch.
- **`rwv update`** reads `version` to know which remote branch tip to advance each repo to before re-snapshotting `rwv.lock`.
- **`rwv fetch`** checks a new clone out on the local counterpart of the branch `version` names, and refuses to move a present checkout attached to any other branch. **`rwv doctor --fix --reattach-checkouts`** reattaches a detached canonical clone to that same branch.
- **`rwv fetch`** and **`rwv sync`** align checkouts to the 40-character commit SHA in `rwv.lock` (falling back to `version` only on `rwv fetch` without `--frozen` when a newly added repo has no lock entry yet).

The role for your own code is spelled `role = "owned"`; a manifest spelling it `primary` fails to parse. See [reference/roles — Migrating from `role: primary`](./roles.md#migrating-from-role-primary).

#### Submodules

When rwv creates a checkout of a member — `rwv workweave create`, or a member a workweave lacks created by `rwv fetch`, `rwv add`, `rwv sync`, `rwv sync-to` or `rwv doctor --fix` — it checks out each submodule the member records from the copy of that submodule's repository the canonical clone already holds, nested submodules included. It never uses the network for this. The files are cloned or linked from the same submodule in the weave's other checkouts where the filesystem allows, as a member's own files are. A `reference` member's submodule files are read-only like the rest of its files.

A submodule whose recorded commit the canonical clone does not hold stays empty, and `rwv doctor` reports it as [`uninitialized-submodule`](./doctor-findings.md#uninitialized-submodule) / `unfetched`. Fetch the commit into the canonical clone (for example `git submodule update --init` there), then run `rwv doctor --fix`.

`submodules = "skip"` leaves a member's submodules empty in every checkout rwv creates, and doctor does not report them.

The repo set is modeled on vcstool's `.repos` format, extended with `role` and optional `integrations` and `workweave` tables.

### `integrations` block

Optional. Only overrides need to be listed — integrations not mentioned use their own defaults.

```toml
[integrations.npm-workspaces]
enabled = false

[integrations.pnpm-workspaces]
enabled = true

[integrations.static-files]
enabled = true
files = ["turbo.json", ".eslintrc.json"]
```

Per-integration config keys vary; see [reference/integrations](./integrations/index.md).

The block is as strict as the rest of the file. An `[integrations.<name>]` table whose `<name>` is not a built-in integration, or that holds a key its integration does not read, fails to parse, naming the line, the accepted names and the nearest match:

```
unknown key `hide-dotfile` in `[integrations.vscode-workspace]`, expected one of `enabled`, `hide-dotfiles`; did you mean `hide-dotfiles`?
```

So does an `enabled` that is not `true` or `false`, such as `enabled = "false"`, which names the table and the line of the value:

```
`enabled` in `[integrations.cargo-workspace]`: invalid type: string "false", expected `true` or `false`
```

Like any manifest parse error, it stops every verb that loads the manifest: `rwv doctor` reports the project as [`unparseable-project`](./doctor-findings.md#unparseable-project), and `rwv status` skips it with a warning. A value of the wrong type for a `cargo-workspace`, `go-work`, `static-files` or `vscode-workspace` key the integration does read, and an unknown key nested deeper (inside `[integrations.cargo-workspace.members."<repo>"]`), are reported by `rwv doctor` as [`malformed-settings`](./doctor-findings.md#malformed-settings) instead.

### `workweave` block

Optional. Declares gitignored paths (relative to the weave root) that `rwv workweave <project> create` should carry from the source workspace into each new workweave:

```toml
[workweave]
link = [".env.local", "config/dev.key"]
copy = [".cache/build-seed"]
```

| Field | Required | Description |
|---|---|---|
| `link` | no | Gitignored paths to symlink from the source workspace into the new workweave. |
| `copy` | no | Gitignored paths to deep-copy from the source workspace into the new workweave. |

Both lists default to empty and only apply to gitignored artifacts (secrets, local caches, local overrides). Tracked files inside member repositories do not need to be listed here: `rwv workweave <project> create` provisions every member as a git worktree and automatically shares file storage with the source checkout via copy-on-write reflinks (or hardlinks for read-only `reference` files on Linux) when the filesystem supports them.

## `rwv.lock` — pinned revisions

Generated by `rwv lock` or `rwv update`. Same repository shape as `rwv.toml` but JSON, and `version` holds each repo's commit id — never a tag or branch name, even when a tag points at HEAD. A name can be moved or deleted after the lock is written; a commit id cannot.

```json
{
  "repositories": {
    "github/chatly/server": {
      "type": "git",
      "url": "https://github.com/chatly/server.git",
      "version": "c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3"
    },
    "github/chatly/web": {
      "type": "git",
      "url": "https://github.com/chatly/web.git",
      "version": "e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0"
    },
    "github/chatly/protocol": {
      "type": "git",
      "url": "https://github.com/chatly/protocol.git",
      "version": "7a3b2c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b"
    }
  }
}
```

An older or hand-edited lock may still carry an entry pinned by a tag or branch name. Verbs that read the lock resolve it to wherever the name points now. `rwv fetch` and `rwv sync` warn, naming the entry; `rwv status` marks it and `rwv doctor` reports it as [`lock-pinned-by-name`](./doctor-findings.md#lock-pinned-by-name), in `--json` too. The next `rwv lock`, or a sync's relock, records the commit id.

`rwv.lock` is derived state. Do not hand-edit; it is the *output* of `rwv lock` and the *output* of `rwv sync` Phase 3. See [lock-as-derived](../explanation/joints/lock-as-derived.md).

`sha256sum rwv.lock` is the project fingerprint — two locks with the same checksum pin the same commit of every repo. They are pins, not a description of the checkouts: a working tree whose HEAD has moved past its lock reads the same checksum, and `rwv status` or `rwv doctor --locked` is what compares the two.

Each workspace (primary weave or workweave) owns its own `rwv.lock`. A `rwv lock` from a workweave updates the workweave's lock, not primary's.

## `.rwv-active` — active project marker

Lives at the weave root. Single line: the active project name.

```
web-app
```

`.rwv-active` is the **single source of truth** for the active project within a workspace. There is no CWD-based override; `cd projects/<name>/` does not switch projects.

Updated by `rwv activate <project>`, by `rwv init`, and by the `rwv fetch` that activates a project when none is. Action verbs (`rwv lock`, `rwv add`, `rwv sync`) read it; pass `--project <name>` for a one-shot override without changing the file. A verb that writes into its project, run from inside `projects/<other>/` while the file names a different project, refuses (`cwd-in-inactive-project`) rather than act on the one the file names.

## `.rwv-workweave` — workweave marker

Lives at every workweave directory root, and is a workweave root's *only* identity file — a workweave never carries `.rwv-active` beside it, and `rwv doctor` reports a root holding both. Records the workweave's identity and parent.

Written by `rwv workweave <project> create <name>`, which is also the verb that duplicates an existing workweave (`--from <source>`); removed with the workweave by `rwv workweave <project> delete <name>` or `rwv sync-to --retire`.

```json
{
  "primary": "/home/user/work",
  "project": "web-app",
  "parent": "/home/user/.workweaves/web-app--feat"
}
```

| Field | Description |
|---|---|
| `primary` | Absolute path to the primary weave |
| `project` | Project name this workweave is forked for |
| `parent` | Absolute path to the workspace the workweave was forked from. `primary` value when forked from primary; the parent-workweave path when forked from inside another workweave |

All three fields are required. A YAML-format marker (written before markers were JSON) or one missing `parent:` (written before parent tracking was introduced) is a legacy marker and must be migrated before the workweave can be used — any `rwv` invocation from inside a workweave with a legacy marker will fail with a clear error message naming the file. Run `rwv doctor --fix` from the primary weave to migrate it to JSON, backfilling `parent:` where it is absent — `--fix` cannot run from inside the workweave it would repair, because the marker is resolved before any verb dispatches. See [upgrade-rwv — Step 1](../how-to/upgrade-rwv.md#step-1--a-legacy-workweave-marker).

Consumed by:

- `rwv sync-to` (bare) — auto-targets the recorded parent.
- `rwv sync-to --retire` — advances parent, then deletes the workweave on success.

See [workweave hierarchy](../explanation/joints/workweave-hierarchy.md) for one-hop semantics.

## `.rwv-workweave-index` — workweave registry and ref receipts

Lives at `<primary>/projects/<project>/.rwv-workweave-index`. Machine-local JSON state (excluded from git through the project repo's `.git/info/exclude`), written and updated by `rwv workweave <project> create`, `delete`, `set-container`, and `rwv doctor --fix`.

It records the project's workweave container (`<parent-of-primary>/.workweaves` unless `set-container` recorded another), the absolute path of every workweave created for the project, which `-w <project>--<name>` and `rwv workweave <project> list` resolve through, and an ownership receipt for every ephemeral `<project>--<workweave>` branch `rwv` created, so `delete` and `doctor --fix` delete only branches `rwv` itself minted. Its field-level schema is an implementation detail; read it through `rwv workweave <project> list` and `rwv doctor`, and let `rwv doctor --fix` repair it.

## `.rwv-owned-digests` — attested generated-file ledger

Lives at `<workspace>/projects/<project>/.rwv-owned-digests` (with a transient `.rwv-owned-digests.lock` sibling while a write is in flight). Machine-local JSON state (excluded via `.git/info/exclude`) that records SHA-256 digests of ecosystem files produced by integration install hooks (such as `Cargo.lock`, `package-lock.json`, `pnpm-lock.yaml`, `uv.lock`) alongside the digests of the manifest inputs that produced them.

Used by `rwv doctor` (`managed-file-drift`, `derived-state-stale`) and by `rwv materialize` (`--regenerate-drifted` / `--adopt-drifted`) to distinguish files last written by `rwv` from files modified out-of-band.

## `.rwv-op` — owner op-state record

Written at the **initiating workspace** (the owner) when a `rwv sync` or `rwv sync-to` operation starts, and cleared by the same verbs on success or precondition refusal. It is the sole copy of mutable op state: it records the op's parameters and which phase it reached, so a run that stopped mid-way can be finished with `rwv sync --continue` / `rwv sync-to --continue` or undone with `rwv abort`. Preserved on phase failure for exactly that reason.

Its presence is what those two verbs mean by "an operation is in progress" — `rwv abort` with no `.rwv-op` present reports `no operation in progress`. See [resume or abort a mid-op sync](../how-to/resume-or-abort-mid-op-sync.md) for the operator path.

## `.rwv-op-lease` — thin lease pointer

Written at every **other workspace the op mutates** (never at the owner workspace), and cleared when the owning op completes or is aborted. It gives mutex semantics — a second op against the same workspace is refused while the lease is held — and points back at the owner's `.rwv-op` so `--continue` and `rwv abort` can find the full record from either end.

Both files are written and removed only by the verbs above. Nothing else should parse or edit them: to learn whether an operation is in flight, read `rwv status --json` rather than the files. Their field-level schemas are an implementation detail of rwv's resume logic and are documented with the rest of the internals, out of this reference.

## Directory layout

repoweave uses a flat, provenance-based layout for repos: `{registry}/{owner}/{repo}/`. Project directories live under `projects/{name}/`, and `.workweaves/` lives as a sibling of the weave root by default (`<parent-of-primary>/.workweaves/`).

```
~/
├── work/                                     # weave root (primary weave)
│   ├── github/
│   │   ├── chatly/
│   │   │   ├── server/                       # member repo (clone)
│   │   │   ├── web/                          # member repo
│   │   │   └── protocol/                     # member repo
│   │   └── socketio/
│   │       └── engine.io/                    # member repo (fork)
│   │
│   ├── projects/
│   │   ├── web-app/                          # project repo
│   │   │   ├── rwv.toml
│   │   │   ├── rwv.lock
│   │   │   ├── package.json                  # generated workspace file
│   │   │   ├── package-lock.json             # ecosystem lock file
│   │   │   └── docs/
│   │   └── mobile-app/
│   │       ├── rwv.toml
│   │       ├── rwv.lock
│   │       └── docs/
│   │
│   ├── package.json -> projects/web-app/package.json       # symlink to active project
│   ├── package-lock.json -> projects/web-app/package-lock.json
│   ├── go.work -> projects/web-app/go.work
│   ├── go.work.sum -> projects/web-app/go.work.sum
│   ├── node_modules/                         # tool state, in no repo
│   ├── .venv/                                # tool state, in no repo
│   └── .rwv-active                           # "web-app"
│
└── .workweaves/                              # sibling container for workweaves
    └── web-app--payments/                    # one workweave
        ├── github/chatly/server/             # worktree (role: owned)
        ├── github/chatly/web/                # worktree (role: owned)
        ├── github/socketio/engine.io/        # worktree (role: fork)
        ├── github/other/reference-lib/       # worktree, read-only (role: reference)
        ├── projects/web-app/                 # worktree
        └── .rwv-workweave                    # primary: ~/work, parent: ~/work
```

The `.workweaves/` subtree is created and destroyed by `rwv workweave <project> create` and `rwv workweave <project> delete` (or `rwv sync-to --retire`, which lands the work and deletes in one step); `create --from <source>` is how an existing workweave is duplicated. The layout is shown so the paths in error messages and `rwv status` output are legible — not as a structure to assemble, relocate, or copy by hand.

The first path segment is a **registry** — a short name for where the repo lives. Built-in registries: `github` → `github.com`, `gitlab` → `gitlab.com`, `bitbucket` → `bitbucket.org`, plus `local`, which names no host at all. The set is not configurable. A URL on any other host still resolves: the segment becomes the URL's own host, or `local` for a `file://` URL — the same segment name the `local` registry uses, though reached by that host-derivation rule rather than by looking the registry up by name.

`local` is reachable only as an `rwv add --new` creation address (`rwv add local --new --param root=<dir> --param owner=<owner> --param repo=<repo>`): it never matches an existing URL or shorthand, so it cannot be the registry an ordinary `rwv add <url>` resolves through. Creating through it is two-sided — rwv creates a bare repository under `<dir>` as well as the member checkout — because there is no forge behind `local` to have created one already. See [reference/cli](./cli.md) for the full `--new` invocation and [roles](./roles.md) for how a created member's role is recorded.

- **Repos are regular clones** at the weave root. `cd github/chatly/server && git status` works; no bare repos. (A workweave's checkouts are git worktrees — see the `.workweaves/` tree above.)
- **Ecosystem files are symlinked** at the weave root to the active project's directory. Real files live in `projects/<name>/`.
- **Ecosystem lock files are committable** alongside `rwv.toml` in the project directory.
- **Project repos** have `rwv.toml` and `rwv.lock`. They don't contain importable code — build tools never look at `projects/`.
- **Tool state directories** (`node_modules/`, `.venv/`, `target/`) are at the weave root, outside every repo, regenerated by the ecosystem install commands.

### Project paths

Project paths default to `projects/{name}/`. If names collide across organizations, `rwv fetch` errors and suggests a scoped form: `projects/{owner}/{name}/` or `projects/{registry}/{owner}/{name}/`.

`rwv` commands that take a project path require the path *as created* — if the project lives at `projects/chatly/web-app/`, use `chatly/web-app`, not `web-app`.

### Names, and the characters they exclude

Because workweave directory names, `-w <project>--<name>` addresses, and ephemeral branch names all render a `(project, workweave)` pair as a single flat token (`chatly+web-app--payments`), `rwv` excludes separator characters from its own labels:

| Entity | Excluded characters / patterns | Why & doctor finding |
|---|---|---|
| **Project name** (`projects/<name>/`) | `--`, leading/trailing `-`, `+` | `--` joins `<project>` and `<workweave>`; `/` in a scoped project path (`chatly/web-app`) is encoded as `+`. Reported by `rwv doctor` as [`unnameable-project`](doctor-findings.md#unnameable-project). |
| **Workweave name** | `--`, leading/trailing `-`, `/` (`+` is allowed) | Kept single-segment so `<project>--<workweave>` is unambiguous on sight and valid as a flat git branch name. Reported under [`workweave-tree-integrity`](doctor-findings.md#workweave-tree-integrity). |
| **Member repo name** (`<registry>/<owner>/<repo>`) | *None beyond standard path rules* | Member repos containing `--` or `+` work unchanged; the exclusions apply only to project and workweave labels. |

## Related

- [reference/cli](./cli.md) — verbs that read/write these files
- [reference/roles](./roles.md) — role definitions for `rwv.toml`
- [reference/glossary](./glossary.md) — terminology
- [lock-as-derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is output-only
- [workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — `.rwv-workweave` shape
