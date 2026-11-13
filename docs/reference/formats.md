# File formats

Reference for the on-disk files repoweave reads and writes.

## `rwv.toml` — project manifest

The source of truth for which repos belong to a project. Committed in the project repo at `projects/<name>/rwv.toml`. The one file on this page you author by hand; rwv writes the rest.

```toml
history = "linear"

[repositories."github/chatly/server"]
type = "git"
url = "https://github.com/chatly/server.git"
branch = "main"
role = "owned"

[repositories."github/chatly/web"]
type = "git"
url = "https://github.com/chatly/web.git"
branch = "main"
role = "owned"

[repositories."github/chatly/protocol"]
type = "git"
url = "https://github.com/chatly/protocol.git"
branch = "main"
role = "owned"
history = "merge"

[repositories."github/socketio/engine.io"]
type = "git"
url = "https://github.com/chatly/engine.io.git"
branch = "main"
role = "fork"

[integrations.npm-workspaces]
enabled = false

[integrations.pnpm-workspaces]
enabled = true
```

Repo paths contain `/`, so they are quoted table keys. A project with no repos yet is a bare `[repositories]`; a file with no `repositories` key declares no members.

A verb that changes `rwv.toml` (`rwv member add`, `rwv member remove`, the `rwv member set-*` verbs, `rwv branch set-history`) edits only the entries and keys it sets, adds or removes. Table order, comments, blank lines, key order inside the tables it does not touch, and line endings stay as you wrote them (a file whose lines end in both ways is written in the ending most of its lines have). A changed value keeps its trailing comment. A new member's table goes at its sorted place when the repository tables already stand in sorted order, and otherwise after the last of them; a new key in an existing table goes at its usual place when that table's keys stand in the usual order (`type`, `url`, `branch`, `role`, `recurse-submodules`, `history`), and otherwise at its end. An entry you wrote as an inline table or with dotted keys keeps that form.

### Moving from `rwv.yaml`

Earlier versions read a YAML manifest at `projects/<name>/rwv.yaml`. A project directory still holding one is reported by `rwv doctor` as `legacy-manifest-format` and does not load; rewrite the file as `rwv.toml` in the shape above, then delete the old one.

There is no `--fix` arm. The manifest is yours — the comments and the key order in it are things you wrote, and neither has a mechanical translation between the two formats. A conversion that guessed at them would hand back a file you would have to audit line by line anyway.

`rwv.toml` accepts only the top-level key `history` and three top-level tables: `repositories`, `integrations`, and `workweave` (unknown top-level keys are rejected). Project identity comes from the `projects/<name>/` directory path rather than a `[project]` table, and the legacy `[lock]` table is retired because `rwv.lock` always records 40-character commit SHAs.

### `repositories` entries

Keyed by canonical local path (`<registry>/<owner>/<repo>`). Each entry:

| Field | Required | Description |
|---|---|---|
| `type` | yes | VCS type. Currently `git`. |
| `url` | yes | Clone source: a URL (`https://…`, `git@…`, `file://` for a local remote) or registry shorthand (`github/owner/repo`), as `rwv member add` was given it. Every clone resolves shorthand through the built-in registries, so `rwv clone` or `rwv update` on another machine clones the same remote. |
| `branch` | yes | The branch the member tracks. |
| `role` | yes | One of `owned`, `fork`, `dependency`, `reference`. |
| `recurse-submodules` | no | `true` (the default, and not written when rwv writes the file) or `false`. See [Submodules](#submodules). |
| `history` | no | `linear`, `merge` or `ff-only`: how this branch accepts landings of this member, overriding the top-level `history`. See [`history`](#history). |

Any other key in an entry is rejected, naming it. `version` and `submodules`, the keys an earlier release wrote for what `branch` and `recurse-submodules` now say, are rejected too: a project whose entries name one does not load, and `rwv doctor` reports it as [`unparseable-project`](./doctor-findings.md#unparseable-project) / `pre-schema`. `rwv doctor --fix` renames the keys in place and commits only that rename; see [Upgrade rwv](../how-to/upgrade-rwv.md).

`branch` is branch-only by design: the manifest declares which branch to *track*, while `rwv.lock` records the exact commit SHA you *are* on. Specifically:

- **`rwv member add <url>`** populates `branch` from the remote's default branch, and `rwv member add --new` from the branch the new repo is on.
- A `branch` that reads as a commit id (seven or more hex digits) or a release tag (`v2.0`, `v0.3.4-rc1`) fails to parse, naming the value and its line, and `rwv member add` refuses with [`name`](./refusals.md#name) `a revision` rather than record such a branch.
- **`rwv update --remote`** reads `branch` to know which remote branch tip to advance each repo to before recording the advanced tips in `rwv.lock`.
- **`rwv clone`** and **`rwv update`** check a new clone out on the local counterpart of the branch `branch` names, and `rwv update` puts a present checkout there too, moving it off another branch only where every commit it holds stays on some other ref.
- **`rwv clone`**, **`rwv update`** and **`rwv sync`** align checkouts to the 40-character commit SHA in `rwv.lock`. A member that `rwv clone` or `rwv update` clones and the lock has no entry for is checked out at the tip of `branch` instead; neither verb records it, and neither does `rwv lock`: `rwv member pin` and `rwv update --remote` give a member its first pin.

The role for your own code is spelled `role = "owned"`; a manifest spelling it `primary` fails to parse. See [reference/roles — Migrating from `role: primary`](./roles.md#migrating-from-role-primary).

#### Submodules

When rwv creates a checkout of a member — `rwv workweave create`, or a member a workweave lacks created by `rwv update`, `rwv member add`, `rwv sync`, `rwv land` or `rwv doctor --fix` — it checks out each submodule the member records from the copy of that submodule's repository the canonical clone already holds, nested submodules included. It never uses the network for this. The files are cloned or linked from the same submodule in the weave's other checkouts where the filesystem allows, as a member's own files are. A `reference` member's submodule files are read-only like the rest of its files.

The canonical clone gets that copy from the network verbs. When `rwv member add`, `rwv clone` or `rwv update` clones a member or finds it present, it checks out every submodule whose directory in the canonical clone is empty, nested ones included, from its upstream remote, and fetches into a copy it already holds any commit a checkout of the member records that the copy lacks. A copy's own checkout in the canonical clone is not moved by that fetch.

A submodule whose recorded commit the canonical clone does not hold stays empty, and `rwv doctor` reports it as [`uninitialized-submodule`](./doctor-findings.md#uninitialized-submodule) / `unfetched`. `rwv update --repo <path>` run in that workweave fetches it and checks it out.

`recurse-submodules = false` leaves a member's submodules empty in the canonical clone and in every checkout rwv creates, and doctor does not report them. So does git's own `submodule.<name>.update = none`, from `.gitmodules` or the repository's config, for that one submodule.

The repo set is modeled on vcstool's `.repos` format, extended with `role` and optional `integrations` and `workweave` tables.

### `history`

How the branch whose `rwv.toml` this is accepts landings, and how a feature cut from it takes its work in: the top-level `history` for the project branch and every member without its own, a member entry's `history` for that member. `linear` fast-forwards, else rebases onto the receiving branch; `merge` fast-forwards, else makes a merge commit; `ff-only` lands only by fast-forward, the rebase happening when a feature takes the target's work in. Any other value fails to parse, naming it and its line.

There is no default and no role implies one: a project whose `rwv.toml` names none has the setting unset, and no verb writes one implicitly. `rwv branch set-history <h> [--repo <path>]` writes it and `--unset` removes it, committing only that change; `rwv lock --commit` commits a hand edit of it. The setting is content of the branch, so a branch cut from another starts with its setting. Two projects listing one member whose live weave branches record different values for it are reported by `rwv doctor` as [`history-disagreement`](./doctor-findings.md#history-disagreement).

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

Like any manifest parse error, it stops every verb that loads the manifest: `rwv doctor` reports the project as [`unparseable-project`](./doctor-findings.md#unparseable-project), and `rwv status` and `rwv prime` skip it with a warning. A value of the wrong type for a `cargo-workspace`, `go-work`, `static-files` or `vscode-workspace` key the integration does read, and an unknown key nested deeper (inside `[integrations.cargo-workspace.members."<repo>"]`), are reported by `rwv doctor` as [`malformed-settings`](./doctor-findings.md#malformed-settings) instead.

### `workweave` block

Optional. Declares gitignored paths (relative to the weave root) that `rwv workweave create` should carry from the source workspace into each new workweave:

```toml
[workweave]
link = [".env.local", "config/dev.key"]
copy = [".cache/build-seed"]
```

| Field | Required | Description |
|---|---|---|
| `link` | no | Gitignored paths to symlink from the source workspace into the new workweave. |
| `copy` | no | Gitignored paths to deep-copy from the source workspace into the new workweave. |

Both lists default to empty and only apply to gitignored artifacts (secrets, local caches, local overrides). Tracked files inside member repositories do not need to be listed here: `rwv workweave create` provisions every member as a git worktree and automatically shares file storage with the source checkout via copy-on-write reflinks (or hardlinks for read-only `reference` files on Linux) when the filesystem supports them.

## `rwv.lock` — pinned revisions

Written by `rwv lock`, `rwv member add`, `rwv member pin` and `rwv update --remote`. Same repository table as `rwv.toml` but JSON, with `type`, `url` and a `version` that holds each repo's commit id — never a tag or branch name, even when a tag points at HEAD. A name can be moved or deleted after the lock is written; a commit id cannot.

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

An older or hand-edited lock may still carry an entry pinned by a tag or branch name. Verbs that read the lock resolve it to wherever the name points now. `rwv clone`, `rwv update` and `rwv sync` warn, naming the entry; `rwv status` marks it and `rwv doctor` reports it as [`lock-pinned-by-name`](./doctor-findings.md#lock-pinned-by-name), in `--json` too. The next `rwv lock`, or a sync's relock, records the commit id.

`rwv.lock` is derived state. Do not hand-edit; it is recorded by `rwv lock` and the verbs that name a commit, by `rwv sync`, and by `rwv land` Phase 3. See [lock-as-derived](../explanation/joints/lock-as-derived.md).

`sha256sum rwv.lock` is the project fingerprint — two locks with the same checksum pin the same commit of every repo. They are pins, not a description of the checkouts: a working tree whose HEAD has moved past its lock reads the same checksum, and `rwv status` or `rwv doctor --locked` is what compares the two.

Each workspace (primary weave or workweave) owns its own `rwv.lock`. A `rwv lock` from a workweave updates the workweave's lock, not primary's.

### Re-pins by name in lock commits

A lock commit made by a verb that re-pins members by name ends with one `Rwv-Repin` trailer per member whose pin it moves, forward or backward: the member's path as a JSON string, its old pin (`none` where the lock had none), its new pin, both as full commit ids, and the verb:

```
member pin github/chatly/server 0123abcd --commit

Rwv-Repin: "github/chatly/server" 7a3b2c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b 0123abcd4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b member pin
```

The verbs that write it: `rwv member pin --commit`; `rwv member set-branch`, and `--set-branch`, `--set-new-branch` and `--pin` on `rwv branch create` and `rwv workweave create`; `rwv lock --commit` over a hand-edited `branch` of an entry; `rwv update --remote --reset-to-upstream <repo> --commit`, for the member it names. The old pin is the one the commit's first parent's lock holds, so a merge commit's trailer reads against the branch the merge was made on. `rwv lock`, with or without `--repo`, a relock by `rwv update --remote` or `rwv sync`, rwv's own merges, and `rwv member add` write none.

`rwv push` reads it: a merge in the pushed range that left a member's pins unmerged is waived (`pin` `waived`) only by a commit at or after it, the merge's own included, whose trailer re-pins that member backward to a commit reaching neither pin the merge left. A commit made with git carries no trailer and waives nothing; a trailer whose pins are not the ones the commit's parent and the commit itself hold waives nothing and is listed. `rwv member pin <m> <rev> --commit` is the by-name re-pin `pin` `unmerged` offers.

## Branch records

Lives on the `rwv-records` branch of the project repo: an orphan branch with no working tree and no history in common with any weave branch, so it carries no `rwv.toml` and is no weave branch. `rwv branch set-mode` writes it, and so do `rwv init`, `rwv clone` and `rwv workweave create`; every verb that aligns members, locks, lands or publishes reads it. It holds one file per weave branch, in one flat tree, named for the branch with `%` written `%25` and `/` written `%2F` (`feat/x` is the file `feat%2Fx`), so a branch and a branch below its name (`a` and `a/b`) each keep their own file. A file whose name is not one a branch name encodes to (a directory, a raw `/`, any other `%` sequence) is not read as a record, and rwv never changes or removes it. Each commit on it ends with a trailer naming the release that wrote it:

```
records: set feat-x feature

Rwv-Writer: 0.25.0
```

An entry is a small TOML file. A feature names the branch it lands into, `target`, and that branch's mode when the entry was written, `as`; a declared branch names only its mode:

```toml
mode = "feature"
target = "main"
as = "declared"
```

```toml
mode = "declared"
```

When two writers recorded one branch differently and a merge met both, the entry holds only `conflict`, an array of the two values, until `rwv branch set-mode` writes one:

```toml
conflict = [
  { mode = "declared" },
  { mode = "feature", target = "main", as = "declared" },
]
```

An entry with any other shape (an unknown key, a `feature` without `target`, a file that is not TOML) reads as `target` `unreadable` for that branch alone; the other entries read as usual, and `rwv branch set-mode` rewrites it. The writing release is named by the trailer of the commit that last changed the entry.

The branch is read from the records remotes as well as from this repo: each remote-tracking records branch of the project repo's url remote and of every remote a project branch here publishes to is merged into the local one, entry by entry against their merge base. An entry changed on one side takes that side, one changed alike on both takes that value, and one changed differently is stored as a `conflict`. A remote added only to fetch someone's branches is read as nothing. A verb that only reads (`rwv branch list`, `rwv status`, `rwv log`, `rwv fetch`) does the merge in memory and writes nothing; a verb that writes a record, aligns, locks, lands or publishes commits the merge first. Writers move the local branch with a compare-and-swap, so two verbs writing at once lose neither write.

The branch name `rwv-records` and every name below it (`rwv-records/…`) are reserved: no weave branch takes them (`name` `reserved`). Do not write the branch by hand; the next `rwv branch set-mode` converges whatever a hand edit left.

## `.rwv-active` — active project marker

Lives at the weave root. Single line: the active project name.

```
web-app
```

`.rwv-active` is the **single source of truth** for the active project within a workspace. There is no CWD-based override; `cd projects/<name>/` does not switch projects.

Updated by `rwv activate <project>`, by `rwv init`, and by the `rwv clone` or `rwv update` that activates a project when none is. A file that is empty or holds anything but a project name selects nothing: a verb that reads it refuses (`unreadable-active-project`) instead of acting as if no project were selected. Action verbs (`rwv lock`, `rwv member add`, `rwv sync`) read it; pass `--project <name>` for a one-shot override without changing the file. A verb that writes into its project, run from inside `projects/<other>/` while the file names a different project, refuses (`cwd-in-inactive-project`) rather than act on the one the file names.

## `.rwv-workweave` — workweave marker

Lives at every workweave directory root, and is a workweave root's *only* identity file — a workweave never carries `.rwv-active` beside it, and `rwv doctor` reports a root holding both. Records the workweave's identity and parent. The project clone's workweave index keeps the same `parent`, with the workweave's path, so a marker that is lost or edited is written again from the index and the workweave's project checkout being a worktree of that clone; a verb that does not destroy the workweave restores it and says so, and `rwv workweave delete` and `rwv land --retire` refuse instead, naming `rwv doctor --fix`.

Written by `rwv workweave create <name>`, which, run inside a workweave, also duplicates that workweave's state; removed with the workweave by `rwv workweave delete <name>` or `rwv land --retire`.

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

- `rwv land` — lands into the target the feature's record names.
- `rwv land --retire` — lands, then deletes the workweave on success.

See [workweave hierarchy](../explanation/joints/workweave-hierarchy.md) for one-hop semantics.

## `rwv-workweave-index` — workweave registry

Lives in the directory the project clone's worktrees share (git: `.git/` of `<primary>/projects/<project>/`), so the primary and every workweave reach the same one, and a clean of the project's working tree cannot remove it. Machine-local JSON state, written and updated by `rwv workweave create`, `delete`, `set-default-create-basedir`, and `rwv doctor --fix`.

It records the project's default workweave basedir once `set-default-create-basedir` has written one (`<parent-of-primary>/.workweaves` until then), and, for every workweave created for the project, its absolute path and the workspace it was forked from. `-w <project>--<name>` and `rwv workweave list` resolve through the paths; the parents are what restore a workweave's marker whole. Its field-level schema is an implementation detail; read it through `rwv workweave list` and `rwv doctor`, and let `rwv doctor --fix` repair it.

An index that earlier releases kept at `<primary>/projects/<project>/.rwv-workweave-index`, in any shape, is not read: every verb that reads the index refuses it as `legacy-workweave-index`, and `rwv doctor --fix` converts it and moves it into the clone. See [upgrade-rwv — Step 8](../how-to/upgrade-rwv.md#step-8--a-workweave-index-at-the-old-path).

## `rwv-workspace-state` — what the workspace last aligned for

Lives in the project repo's private directory for the checkout (`git rev-parse --absolute-git-dir` in `projects/<project>/`), so the weave root and every workweave each keep their own and none is tracked. It holds the branch the workspace last aligned its members for, or that it aligned them detached, and, for each branch it aligned, that branch's mode and the member entries it aligned them by, and the record it last saw each branch hold. `rwv branch create` forgets what it held for the name it creates, so a branch made under a deleted one's name starts with nothing stored.

`rwv update` writes it when it aligned every member of the project and what it holds changed, and writes nothing otherwise; `rwv switch` writes it when it moves the workspace, `rwv branch rename` carries what it holds for the old name to the new one, and `rwv init`, `rwv clone` and `rwv workweave create` record it for the workspace they make. `rwv branch set-mode` and `rwv branch create` store the record they write, and `rwv sync` stores the record it reads where none is stored. `rwv status` reads it to say whether the project repo has moved to another branch since, and whether the checked-out branch's record changed elsewhere (`target` `record changed`). A file that cannot be read or does not parse reads as absent, and `rwv status`, `rwv update` and `rwv switch` say so: a workspace with no state is `project unaligned`, `rwv branch rename` leaves such a file as it is, and the next `rwv update` or `rwv switch` writes it again. Each workspace keeps its own and only a verb acting on that workspace writes it, so it is published whole, with no claim. Its field-level schema is an implementation detail; read it through `rwv status`.

## `.rwv-owned-digests` — attested generated-file ledger

Lives at `<workspace>/projects/<project>/.rwv-owned-digests` (with a transient `.rwv-owned-digests.lock` sibling while a write is in flight). Machine-local JSON state (excluded via `.git/info/exclude`) that records SHA-256 digests of ecosystem files produced by integration install hooks (such as `Cargo.lock`, `package-lock.json`, `pnpm-lock.yaml`, `uv.lock`) alongside the digests of the manifest inputs that produced them.

Used by `rwv doctor` (`managed-file-drift`, `derived-state-stale`) and by `rwv materialize` (`--regenerate-drifted` / `--adopt-drifted`) to distinguish files last written by `rwv` from files modified out-of-band.

## `.rwv-op` — owner op-state record

Written at the **initiating workspace** (the owner) when a `rwv sync` or `rwv land` operation starts, and cleared by the same verbs on success or precondition refusal. It is the sole copy of mutable op state: it records the op's parameters and which phase it reached, so a run that stopped mid-way can be finished with `rwv sync --continue` / `rwv land --continue` or undone with `rwv abort`. Preserved on phase failure for exactly that reason.

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

The `.workweaves/` subtree is created and destroyed by `rwv workweave create` and `rwv workweave delete` (or `rwv land --retire`, which lands the work and deletes in one step); `create` run inside a workweave is how an existing workweave is duplicated. The layout is shown so the paths in error messages and `rwv status` output are legible — not as a structure to assemble, relocate, or copy by hand.

The first path segment is a **registry** — a short name for where the repo lives. Built-in registries: `github` → `github.com`, `gitlab` → `gitlab.com`, `bitbucket` → `bitbucket.org`, plus `local`, which names no host at all. The set is not configurable. A URL on any other host still resolves: the segment becomes the URL's own host, or `local` for a `file://` URL — the same segment name the `local` registry uses, though reached by that host-derivation rule rather than by looking the registry up by name.

`local` is reachable only as an `rwv member add --new` creation address (`rwv member add local --new --param root=<dir> --param owner=<owner> --param repo=<repo>`): it never matches an existing URL or shorthand, so it cannot be the registry an ordinary `rwv member add <url>` resolves through. Creating through it is two-sided — rwv creates a bare repository under `<dir>` as well as the member checkout — because there is no forge behind `local` to have created one already. See [reference/cli](./cli.md) for the full `--new` invocation and [roles](./roles.md) for how a created member's role is recorded.

- **Repos are regular clones** at the weave root. `cd github/chatly/server && git status` works; no bare repos. (A workweave's checkouts are git worktrees — see the `.workweaves/` tree above.)
- **Ecosystem files are symlinked** at the weave root to the active project's directory. Real files live in `projects/<name>/`.
- **Ecosystem lock files are committable** alongside `rwv.toml` in the project directory.
- **Project repos** have `rwv.toml` and `rwv.lock`. They don't contain importable code — build tools never look at `projects/`.
- **Tool state directories** (`node_modules/`, `.venv/`, `target/`) are at the weave root, outside every repo, regenerated by the ecosystem install commands.

### Project paths

Project paths default to `projects/{name}/`. If names collide across organizations, a project can live at a scoped path instead: `projects/{owner}/{name}/` or `projects/{registry}/{owner}/{name}/`. `rwv clone` refuses with [`project-dir-occupied`](./refusals.md#project-dir-occupied) when the directory for the name is already taken.

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
