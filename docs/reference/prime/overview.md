# repoweave: orientation

CWD is not inside a weave or workweave. Below is a compact overview of how `rwv` workspaces are structured so you can orient quickly once you enter one.

## Core concepts

- **Weave**: A top-level workspace directory containing `projects/<name>/` (each project's git repo) alongside canonical member clones at `<registry>/<owner>/<repo>/` (e.g., `github/org/service/`).
  - `projects/<name>/rwv.toml` is the hand-authored project manifest declaring member repositories (`type`, `url`, `branch`, `role`) and optional `[integrations.<name>]` settings.
  - `projects/<name>/rwv.lock` is the derived revision record pinning every member repo to a 40-hex commit SHA.
  - Ecosystem workspace files (`Cargo.toml`, `package.json`, `go.work`, `pyproject.toml`, `<name>.code-workspace`) live in `projects/<name>/`. **Authoring verbs** (`rwv member add`, `rwv member remove`, `rwv member set-role`, `rwv update --remote`) and `rwv doctor --fix` author or regenerate the rwv-owned regions of those files; **`rwv activate`** never authors or overwrites file content — it switches `.rwv-active`, surfaces weave-root symlinks (`Cargo.toml` -> `projects/<active>/Cargo.toml`, etc.), and runs install hooks. **`rwv materialize`** settles attested generated-file drift, cleans up disabled integration artifacts, repairs missing weave-root symlinks, and runs install hooks for the project this checkout already presents.
  - Each member repo has a `role` (`owned`, `fork`, `dependency`, `reference`): `owned` and `fork` are read-write and included in `rwv push`; `dependency` is included in ecosystem workspace files and workweave branches but skipped by `rwv push`; `reference` is excluded from ecosystem workspace files and checked out with read-only tracked regular files (write bits removed).

- **Workweave**: An isolated parallel workspace at `.workweaves/<project>--<name>/` (sibling of the weave root by default) for a single task or agent.
  - Every member repo (plus `projects/<project>/`) is materialized as a `git worktree` of the canonical weave-root clone attached to the ephemeral branch `<project>--<name>` (with `role = "reference"` tracked regular files set read-only). `rwv workweave create` also runs the integration install hooks in it (`--no-materialize` skips them); inside a workweave the authoring verbs run no hooks, and `rwv materialize` does.
  - Carries a `.rwv-workweave` marker recording `{primary, project, parent}` (mutually exclusive with `.rwv-active`, which is primary-only).

- **Lock & sync**: `rwv.lock` pins every member repo to a commit SHA (never a tag or branch name).
  - `rwv lock` records local HEAD SHAs over their pins in `rwv.lock`, moving a pin forward only: a member with no pin keeps none, and one behind or diverged from its pin is refused, `--repo` included, and `rwv member pin` moves it by name; `rwv member add`, `rwv member pin` and `rwv update --remote` give a member its first pin (`rwv lock --commit` commits `rwv.lock`, with `rwv.toml` when it has uncommitted changes; `rwv push` refuses while either is uncommitted); `rwv update --remote` fetches from remotes, fast-forwards checkouts, and records the advanced tips in `rwv.lock`; `rwv update` clones the members missing on disk and aligns the rest to `rwv.lock` without advancing pins or writing the lock; `rwv clone` brings up a project at the commits its `rwv.lock` pins; `rwv fetch` downloads and moves no branch, checkout or lock; `rwv pull` fetches, fast-forwards the project branch to its upstream, then runs `rwv update`.
  - `rwv sync` takes the target's new work into a feature (`--from <branch>` merges another weave branch in instead), refusing before anything moves where a member's work is built on another lineage, then relocks and aligns the members; `rwv sync-to` lands CWD's work into its target workspace (auto-relocking CWD at op start when repo tips have moved past `rwv.lock`, rebasing CWD onto target, and fast-forwarding target).

## Common pitfalls

- **Weave-root ecosystem files are symlinks into `projects/<active>/`**: In hybrid files (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.work`), hand-authored keys outside the rwv-owned marker region are preserved across regeneration. Edits inside the rwv-owned region are overwritten when `rwv member add`, `rwv member remove`, `rwv member set-role`, `rwv update --remote`, or `rwv doctor --fix` regenerates the file (`rwv activate` only moves symlinks and never clobbers file content).
- **Don't run `git worktree add` directly**: Always use `rwv workweave create <name>` so all member repos, workweave branches, and `.rwv-workweave` markers stay consistent.
- **A workweave's minted branch is not published**: `rwv push` refuses a project branch named `<project>--<name>` (`name` minted), the name `rwv workweave create` gives a workweave made without a branch of its own. Give the work a branch to publish it.
- **Don't `chmod` or edit `reference` repos in place**: Tracked regular files in `reference` repos carry no write bit. To promote a repo for editing, run `rwv member set-role <path> owned` (or `fork`), which updates `rwv.toml`, regenerates ecosystem files, and restores write bits.

## Typical flow

```sh
# 1. From the primary weave, create an isolated workweave for a task
rwv workweave create <name>
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
| `rwv log [RANGE]` | The VCS's log per repo; `--against W` lists the commits beyond weave branch W's pins (`--diff` for the diff) |
| `rwv doctor` | Check workspace conventions (`--fix` repairs auto-fixable findings) |
| `rwv activate PROJECT` | Switch `.rwv-active`, surface weave-root symlinks, run install hooks |
| `rwv materialize` | Settle drift, clean up disabled integrations, repair symlinks, run hooks |
| `rwv clone SOURCE` | Clone a project and each member at the commit `rwv.lock` pins (exit status 3 when a member is left unpinned) |
| `rwv fetch` | Pruning fetch of every remote of every repo; moves no branch, checkout or lock |
| `rwv pull` | Fetch, fast-forward the project branch to its upstream, then `rwv update` |
| `rwv update` | Clone missing members and align present ones to `rwv.lock`; never advances a pin or writes the lock |
| `rwv update --remote` | Fetch remote branches, fast-forward checkouts, and re-snapshot `rwv.lock` |
| `rwv member add URL` | Clone a repo, register in `rwv.toml`, regenerate integration files, and commit them with the repo's first pin |
| `rwv member pin PATH REV` | Pin a repo at a commit you choose, in either direction |
| `rwv member remove PATH` | Unregister a repo from `rwv.toml` and regenerate integration files |
| `rwv member set-role PATH ROLE` | Change a repo's role (`owned`, `fork`, `dependency`, `reference`) and settle files |
| `rwv lock` | Record current repo HEAD SHAs as `rwv.lock` pins, forward only (`--commit` to commit) |
| `rwv push` | Push member repos then the project repo from the primary weave |
| `rwv workweave create NAME` | Create an isolated workweave on branch `PROJECT--NAME` and run its install hooks |
| `rwv workweave list` | List active workweaves for a project |
| `rwv workweave delete NAME` | Delete a workweave (refuses on uncommitted or unmerged work) |
| `rwv sync` | Take the target's new work into this feature, or `--from BRANCH` to merge a weave branch in (`--continue` to resume) |
| `rwv sync-to [TARGET]` | Land CWD's work into target workspace (`--retire` deletes workweave after) |
| `rwv abort` | Roll an in-progress `sync` or `sync-to` back to its savepoints, from either workspace it touched |
| `rwv backup <list\|drop\|prune>` | List, drop or prune the backups kept before a discard (`--json` on `list`); `drop` and `prune` refuse where a live lock pins a commit only the backup reaches |
| `rwv branch <set-mode\|list>` | Record how a weave branch is used (`feature --target T` or `declared`), or list the weave branches with their records (`--json` on `list`); the verbs that align, lock, land or publish refuse a branch with no record |
| `rwv explain [VERB]` | Print per-verb reference bundle and JSON Schema (or error/finding entry) |

Global addressing flags (placed before the subcommand): `-C <path>` / `--cwd <path>` resolves the workspace as if invoked from `<path>`; `-w <project>--<name>` / `--workweave <project>--<name>` targets a registered workweave by identity from anywhere in the weave.

### Sync family

```
primary weave  <── rwv sync-to [--retire] ──  workweave (.workweaves/<project>--<name>)
               ─── rwv sync ───────────────>
```

- `rwv sync` — take the new work of the branch this feature lands into (its target) into the feature: members first, then the project branch, then the relock. `--strategy rebase` rebases members holding new work; `--from <branch>` merges another weave branch's history in instead.
- `rwv sync-to [target]` — land CWD's state into `target` (rebases CWD onto `target`, auto-relocks, then fast-forwards `target`; `target` defaults to the recorded `parent` inside a workweave). Pass `--retire` to delete the workweave once the landing succeeds.
- `rwv abort` — roll every repo the op touched, in both workspaces, back to its pre-operation savepoint if a `sync` or `sync-to` stops on a conflict or a refusal. Run it from either workspace.

Run `rwv --help` or `rwv <command> --help` for full flag details.

## Agent integration surfaces

Machine-readable `--json` output is supported on `rwv status --json`, `rwv doctor --json`, `rwv sync --json`, `rwv sync-to --json`, `rwv clone --json`, `rwv fetch --json`, `rwv pull --json`, `rwv update --json`, `rwv update --remote --json`, `rwv push --json`, `rwv log --json`, `rwv backup list`, and `rwv branch list` (with `--json`). Each emits one JSON envelope; on the multi-worker verbs (`clone`, `fetch`, `pull`, `update`, `push`, `sync`, `sync-to`) `-j` changes only how many repos run at once:
- `rwv status --json` — per-repo records carry `path` (manifest-relative) and `absolute_path` (fully resolved on disk), plus `lock_pinned_by_name` when an older lock entry holds a ref name instead of a commit SHA; status records are flat and have no `kind` field.
- `rwv sync --json` / `rwv sync-to --json` — one envelope whose `complete` is `true` when the op finished and cleared op-state; `refused` carries each judged line of a refusal with its choices. `sync-to`'s per-repo outcome records carry `kind` (kebab-case discriminant), `path`, and `absolute_path`. Both exit 1 when a checkout the op could not prune is left on disk, even though the op completed.
- `rwv doctor --json` — violations carry a kebab-case `kind` discriminant (and optional `sub_kind`); path fields are per-variant: per-repo variants carry `path` + `absolute_path`, `weave-root-identity-conflict` carries `root`, and `workweave-tree-integrity` carries `workweave_dir`. Integration findings appear on the `issues` array.
- `rwv explain <verb>` — emits a self-contained markdown reference (purpose, invocation, output shape, exit codes, examples, and embedded JSON Schema for `--json` verbs). `rwv explain <token>` looks up any refusal token, doctor finding kind, or VCS error kind. Committed schemas live under `docs/reference/schemas/`.
