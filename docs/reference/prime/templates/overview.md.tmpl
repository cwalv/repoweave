# repoweave: orientation

> CWD is not inside a weave or workweave. No per-workspace details are available — this is not an error.

## Concepts

**Weave** — a directory that weaves multiple repository *threads* into a single workspace *fabric*. Contains repositories cloned under `{registry}/{owner}/{repo}/` and projects under `projects/{name}/`. Ecosystem workspace files and symlinks are ephemeral (regenerated on `rwv activate`); repos and projects are the persistent state. Analogous to a `go.work` or Cargo `[workspace]`, with lock-based reproducibility and multi-ecosystem support.

**Workweave** — an ephemeral, isolated derivative of a weave (the multi-repo equivalent of `git worktree`). Each worktree-materialized repo (`owned`, `fork`, `dependency`) gets a worktree on its own ephemeral branch; `role: reference` repos are materialized as a symlink to the canonical weave-root clone and are shared read-only across all workweaves. Ecosystem files and tool state (`node_modules/`, `.venv/`, `target/`) are per-workweave. Use for feature work, PR review, or per-agent isolation without disturbing the primary weave. Created with `rwv workweave PROJECT create NAME`; deleted with `rwv workweave PROJECT delete NAME`.

**Lock & sync** — every project owns an `rwv.lock` that pins each repo to an exact revision (tag name when HEAD is tagged, SHA otherwise). The lock is *load-bearing*, not a passive snapshot: `rwv sync <source>` aligns the CWD workspace with `<source>`'s committed lock. It is direction-neutral — from primary, `rwv sync <workweave>` brings a workweave's work home; from `.workweaves/<project>--<workweave>/`, `rwv sync primary` catches the workweave up. Both sides must satisfy `rwv doctor --locked` first (bypass lock-freshness with `--allow-stale-lock`; discard local commits with `--discard-local-commits`); `rwv abort` rolls back via savepoint refs under `refs/rwv/pre-op/`. `sha256sum rwv.lock` is the project fingerprint — the multi-repo equivalent of `git rev-parse HEAD` on a monorepo.

## Common pitfalls

- Do not confuse a *weave* (the primary workspace root) with a *workweave* (a worktree-based isolated copy). They share an object store, but branches, locks, and tool state diverge.
- Do not `cd` into arbitrary paths before repo-scoped commands; `rwv` infers project and workspace from CWD. Use `rwv resolve` if you need the effective workspace root for scripting.
- Do not edit ecosystem workspace files (`package.json`, `go.work`, `Cargo.toml`) at the weave directory by hand — they are symlinks to generated files in `projects/{name}/` and get clobbered by the next `rwv activate`. Edit `rwv.yaml` instead.
- Do not run `rwv lock` with uncommitted changes — it errors by design (the lock would record HEAD, not your working tree). Commit first, or pass `--dirty` if you accept the divergence.
- Do not assume `rwv sync` has a one-true direction. The verb is direction-neutral; `<source>` is whichever workspace's committed lock you want to align against.
- `rwv prime` without `--no-suppress` is intentionally silent outside a weave; absence of output is not an error.

## Typical flow

Reproduce a project, work in isolation, land the result back in primary:

```
rwv fetch <owner>/<project>             # clone project repo + every repo it lists
rwv activate <project>                  # generate ecosystem workspace files; set active project
rwv workweave <project> create <name>   # spin up an isolated workspace for the feature/agent
# ... edit, test, commit across repos in .workweaves/<project>--<name>/ ...
rwv lock                                # snapshot revisions to rwv.lock
git -C projects/<project> commit -am 'lock: <name>'   # commit the lock in the project repo
cd <primary> && rwv sync <name>         # land the workweave's lock back in primary
rwv doctor                              # convention audit (orphans, stale locks, drift)
```

## Essential commands

Run `rwv --help` for the full command reference. Workspace and project are inferred from CWD unless overridden with `--project`.

| Command | Description |
|---------|-------------|
| `rwv` | Show workspace context |
| `rwv prime [--no-suppress]` | Emit structured context; `--no-suppress` always emits, even outside a weave |
| `rwv resolve` | Print effective workspace root path (handy for scripting: `cd $(rwv resolve)`) |
| `rwv fetch SOURCE [--frozen]` | Clone a project and every repo it lists; align to its `rwv.lock`; activate |
| `rwv activate PROJECT` | Set active project; (re)generate ecosystem workspace files and symlinks |
| `rwv init PROJECT [--provider REG/OWNER]` | Create a new project directory with empty `rwv.yaml` |
| `rwv add URL [--role ROLE\|--new]` | Clone and register a repo; `--new` initializes a brand-new repo at the canonical path |
| `rwv remove PATH [--delete]` | Unregister a repo; `--delete` also removes the clone |
| `rwv lock [--dirty]` | Snapshot repo revisions to the project's `rwv.lock`. Pure git SHA snapshot — no integration hooks fire; run `rwv activate` afterward if membership changed |
| `rwv workweave PROJECT create NAME` | Spin up a worktree-based isolated workspace |
| `rwv workweave PROJECT delete NAME` | Tear down a workweave (worktrees + ephemeral branches) |
| `rwv workweave PROJECT list` | List workweaves for a project |

### Sync family — when to use which

These four commands are easy to confuse — they cooperate around the lock-authoritative model.

| Command | When to use |
|---------|-------------|
| `rwv sync <source> [--strategy ff\|rebase] [--allow-stale-lock] [--discard-local-commits]` | Align CWD workspace with `<source>`'s committed `rwv.lock`. Default `ff`; use `rebase` when both sides advanced |
| `rwv abort` | Restore CWD workspace to its pre-sync state via savepoint refs; runs VCS-native abort for an in-progress rebase |
| `rwv status [--json]` | Show per-repo branch, tip, lock SHA, and relation (`ok`/`ahead`/`behind`/`diverged`/`no-lock`) without changing anything |
| `rwv doctor --locked` | Zero exit iff every repo's tip matches its lock entry — the precondition `rwv sync` enforces. Scriptable |
| `rwv doctor` | Full convention audit (orphans, dangling refs, stale locks, workweave drift, integration checks) |

## Agent integration surfaces

- **Structured output:** `rwv status --json`, `rwv doctor --json`, `rwv sync --json`. `sync` and `doctor` records carry `kind`; `status` has a single record shape and carries no `kind` (discriminate on `relation` instead). `path` + `absolute_path` identifiers are present on every `status` and `sync` record; in `doctor` they are per-kind, not universal — e.g. `weave-root-identity-conflict` carries `root` and `workweave-tree-integrity` carries `workweave_dir` instead. `docs/reference/schemas/<verb>.json` is the authoritative shape per verb.
- **Per-verb reflection:** `rwv explain <verb>` returns a markdown bundle (purpose, invocation, output description with JSON Schema). Use when scripting against an unfamiliar verb.
- **Schemas:** committed at `docs/reference/schemas/<verb>.json`. Each `--json` output embeds a `$schema` URL pointing here.
