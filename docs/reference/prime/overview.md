# repoweave: orientation

CWD is not inside a weave or workweave. Below is a compact overview of how `rwv` workspaces are structured so you can orient quickly once you enter one.

## Core concepts

- **Weave**: A top-level workspace directory containing `projects/<name>/` (each project's git repo) alongside canonical member clones at `<registry>/<owner>/<repo>/` (e.g., `github/org/service/`).
  - `projects/<name>/rwv.toml` is the hand-authored project manifest declaring member repositories (`type`, `url`, `version`, `role`) and optional `[integrations.<name>]` settings.
  - `projects/<name>/rwv.lock` is the derived revision snapshot pinning every member repo to a 40-hex commit SHA.
  - Ecosystem workspace files (`Cargo.toml`, `package.json`, `go.work`, `pyproject.toml`, `<name>.code-workspace`) live in `projects/<name>/`. **Authoring verbs** (`rwv add`, `rwv remove`, `rwv role`, `rwv update`) and `rwv doctor --fix` author or regenerate the rwv-owned regions of those files; **`rwv activate`** never authors or overwrites file content — it switches `.rwv-active`, surfaces weave-root symlinks (`Cargo.toml` -> `projects/<active>/Cargo.toml`, etc.), and runs install hooks. **`rwv materialize`** settles attested generated-file drift, cleans up disabled integration artifacts, repairs missing weave-root symlinks, and runs install hooks for the project this checkout already presents.
  - Each member repo has a `role` (`owned`, `fork`, `dependency`, `reference`): `owned` and `fork` are read-write and included in `rwv push`; `dependency` is included in ecosystem workspace files and workweave branches but skipped by `rwv push`; `reference` is excluded from ecosystem workspace files and checked out with read-only tracked regular files (write bits removed).

- **Workweave**: An isolated parallel workspace at `.workweaves/<project>--<name>/` (sibling of the weave root by default) for a single task or agent.
  - Every member repo (plus `projects/<project>/`) is materialized as a `git worktree` of the canonical weave-root clone attached to the ephemeral branch `<project>--<name>` (with `role = "reference"` tracked regular files set read-only). `rwv workweave <project> create` also runs the integration install hooks in it (`--no-materialize` skips them); inside a workweave the authoring verbs run no hooks, and `rwv materialize` does.
  - Carries a `.rwv-workweave` marker recording `{primary, project, parent}` (mutually exclusive with `.rwv-active`, which is primary-only).

- **Lock & sync**: `rwv.lock` pins every member repo to a commit SHA (never a tag or branch name).
  - `rwv lock` snapshots local HEAD SHAs into `rwv.lock` (`rwv lock --commit` commits `rwv.lock`, with `rwv.toml` when it has uncommitted changes; `rwv push` refuses while either is uncommitted); `rwv update` fetches from remotes, fast-forwards checkouts, and updates `rwv.lock`; `rwv fetch` aligns checkouts to `rwv.lock` without advancing pins.
  - `rwv sync <source>` pulls another workspace's committed state into CWD and regenerates CWD's `rwv.lock` after replay completes; `rwv sync-to` lands CWD's work into its target workspace (auto-relocking CWD at op start when repo tips have moved past `rwv.lock`, rebasing CWD onto target, and fast-forwarding target).

## Common pitfalls

- **Weave-root ecosystem files are symlinks into `projects/<active>/`**: In hybrid files (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.work`), hand-authored keys outside the rwv-owned marker region are preserved across regeneration. Edits inside the rwv-owned region are overwritten when `rwv add`, `rwv remove`, `rwv role`, `rwv update`, or `rwv doctor --fix` regenerates the file (`rwv activate` only moves symlinks and never clobbers file content).
- **Don't run `git worktree add` directly**: Always use `rwv workweave <project> create <name>` so all member repos, ephemeral branches, ownership receipts, and `.rwv-workweave` markers stay consistent.
- **Don't run `rwv push` from inside a workweave**: `rwv push` refuses from a workweave (`push-from-workweave`) so ephemeral `<project>--<name>` branches do not leak to shared remotes. Land into the primary weave first with `rwv sync-to` (or `rwv sync-to --retire`), then run `rwv push` from primary.
- **Don't `chmod` or edit `reference` repos in place**: Tracked regular files in `reference` repos carry no write bit. To promote a repo for editing, run `rwv role <path> owned` (or `fork`), which updates `rwv.toml`, regenerates ecosystem files, and restores write bits.

## Typical flow

```sh
# 1. From the primary weave, create an isolated workweave for a task
rwv workweave <project> create <name>
cd ../.workweaves/<project>--<name>

# 2. Edit and commit normally across member repos
git -C github/org/repo-a commit -am "feat: ..."
git -C github/org/repo-b commit -am "feat: ..."

# 3. Land work back into the parent weave and retire the workweave
#    (sync-to auto-relocks at op start; use `rwv lock --commit` for standalone checkpoints)
rwv sync-to --retire

# 4. From the primary weave, publish to remotes
cd <primary>
rwv push
```

## Essential commands

| Command | Description |
|---------|-------------|
| `rwv` | Show workspace context (active project, repos, workweaves) |
| `rwv status` | Per-repo branch, working-tree, and lock alignment status |
| `rwv doctor` | Check workspace conventions (`--fix` repairs auto-fixable findings) |
| `rwv activate PROJECT` | Switch `.rwv-active`, surface weave-root symlinks, run install hooks |
| `rwv materialize` | Settle drift, clean up disabled integrations, repair symlinks, run hooks |
| `rwv fetch [SOURCE]` | Bootstrap a project or align present/missing repos to `rwv.lock` |
| `rwv update` | Fetch remote branches, fast-forward checkouts, and re-snapshot `rwv.lock` |
| `rwv add URL` | Clone a repo, register in `rwv.toml`, and regenerate integration files |
| `rwv remove PATH` | Unregister a repo from `rwv.toml` and regenerate integration files |
| `rwv role PATH ROLE` | Change a repo's role (`owned`, `fork`, `dependency`, `reference`) and settle files |
| `rwv lock` | Snapshot current repo HEAD SHAs into `rwv.lock` (`--commit` to commit) |
| `rwv push` | Push member repos then the project repo from the primary weave |
| `rwv workweave PROJECT create NAME` | Create an isolated workweave on branch `PROJECT--NAME` and run its install hooks |
| `rwv workweave PROJECT list` | List active workweaves for a project |
| `rwv workweave PROJECT log` | Show unique commits or `--diff` in a workweave vs its parent |
| `rwv workweave PROJECT delete NAME` | Delete a workweave (refuses on uncommitted or unmerged work) |
| `rwv sync <SOURCE>` | Pull another workspace's committed state into CWD (`--continue` to resume) |
| `rwv sync-to [TARGET]` | Land CWD's work into target workspace (`--retire` deletes workweave after) |
| `rwv abort` | Roll an in-progress `sync` or `sync-to` back to its savepoints, from either workspace it touched |
| `rwv explain [VERB]` | Print per-verb reference bundle and JSON Schema (or error/finding entry) |

Global addressing flags (placed before the subcommand): `-C <path>` / `--cwd <path>` resolves the workspace as if invoked from `<path>`; `-w <project>--<name>` / `--workweave <project>--<name>` targets a registered workweave by identity from anywhere in the weave.

### Sync family

```
primary weave  <── rwv sync-to [--retire] ──  workweave (.workweaves/<project>--<name>)
               ─── rwv sync primary ───────>
```

- `rwv sync <source>` — pull `source`'s committed state into CWD (`<source>` is required, e.g. `rwv sync primary`; it does not default to `primary`).
- `rwv sync-to [target]` — land CWD's state into `target` (rebases CWD onto `target`, auto-relocks, then fast-forwards `target`; `target` defaults to the recorded `parent` inside a workweave). Pass `--retire` to delete the workweave once the landing succeeds.
- `rwv abort` — roll every repo the op touched, in both workspaces, back to its pre-operation savepoint if a `sync` or `sync-to` stops on a conflict or a refusal. Run it from either workspace.

Run `rwv --help` or `rwv <command> --help` for full flag details.

## Agent integration surfaces

Machine-readable `--json` output is supported on `rwv status --json`, `rwv doctor --json`, `rwv sync --json`, `rwv sync-to --json`, `rwv fetch --json`, `rwv update --json`, `rwv push --json`, and `rwv workweave <project> log` (with `--json`). Multi-worker verbs (`fetch`, `update`, `push`, `sync`, `sync-to`) emit a single JSON envelope under `-j 1` and self-describing NDJSON lines when `-j > 1`:
- `rwv status --json` — per-repo records carry `path` (manifest-relative) and `absolute_path` (fully resolved on disk), plus `lock_pinned_by_name` when an older lock entry holds a ref name instead of a commit SHA; status records are flat and have no `kind` field.
- `rwv sync --json` / `rwv sync-to --json` — per-repo outcome records carry `kind` (kebab-case discriminant), `path`, and `absolute_path`; the envelope's `complete` is `true` when the op finished and cleared op-state. Both exit 1 when a checkout replay could not prune is left on disk, even though the op completed.
- `rwv doctor --json` — violations carry a kebab-case `kind` discriminant (and optional `sub_kind`); path fields are per-variant: per-repo variants carry `path` + `absolute_path`, `weave-root-identity-conflict` carries `root`, and `workweave-tree-integrity` carries `workweave_dir`. Integration findings appear on the `issues` array.
- `rwv explain <verb>` — emits a self-contained markdown reference (purpose, invocation, output shape, exit codes, examples, and embedded JSON Schema for `--json` verbs). `rwv explain <token>` looks up any refusal token, doctor finding kind, or VCS error kind. Committed schemas live under `docs/reference/schemas/`.
