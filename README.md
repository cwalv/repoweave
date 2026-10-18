# repoweave (`rwv`)

**Workspace tooling for a project split across several repos.**

If your project spans more than one repo, you've probably felt at least one of these: a setup README that drifts away from what people actually clone; cross-cutting scripts, manifests, and decision records with no obvious home; a publish-and-reinstall cycle every time a shared library changes; no way to say *exactly* this set of revisions was running in production on a given day; explaining the repository topology to an AI agent every new session.

repoweave is a coordination layer that addresses those without merging the repos. A committable manifest lists which repos belong to the project. A single lock pins every revision for reproducibility. One command (`rwv fetch <url>`) clones the project and every repo it lists. Workweaves give you isolated parallel cross-repo work — or an agent sandbox — without disturbing the primary. The repos themselves stay independent.

### Why it exists

A monorepo eliminates most of the coordination pain — at the cost of vendoring every repo into one tree and giving up per-repo ownership. repoweave gives you the coordination wins while leaving each repo sovereign. ([Alternatives comparison](docs/comparison.md) — when git submodules, gita, or a monorepo actually wins.)

- **A committable manifest and lock.** `projects/<name>/rwv.toml` lists the repos; `rwv.lock` pins every revision. `sha256sum rwv.lock` is the multi-repo equivalent of `git rev-parse HEAD`.
- **One command reproduces the workspace.** `rwv fetch <url>` clones the project and every repo it lists, surfaces ecosystem workspace files at the weave root where they apply, and runs install commands. For toolchain pins, env activation, or full OS-level reproduction, drop a `.mise.toml` / `.envrc` / `devcontainer.json` into the project repo (they're cross-cutting artifacts) — `rwv fetch` carries them with everything else. See [adjacent-tools](docs/adjacent-tools.md).
- **A natural home for cross-cutting artifacts that don't quite fit anywhere else.** Operational scripts, k8s manifests, ADRs, demos, release notes, devcontainer configs, `.mise.toml` toolchain pins, Nix flakes — they live in the project repo without contaminating any single library's history, and they come along on every `rwv fetch`.
- **Isolated parallel work via workweaves.** `git worktree` extended across N repos, with per-workweave `node_modules` / `.venv` / `target` and automatic copy-on-write file sharing where the filesystem supports reflinks. Use for feature branches, PR review, or agent sandboxes — the primary weave stays undisturbed.
- **A bounded surface for automation.** `rwv prime` advertises the workspace; `rwv explain <verb>` returns a markdown bundle with the verb's JSON Schema embedded; `rwv setup claude` wires `SessionStart`/`PreCompact` context hooks and `WorktreeCreate`/`WorktreeRemove` lifecycle hooks so `claude --worktree` provisions a full multi-repo workweave; roles surface repo change-resistance as machine-readable metadata (`reference` repos are physically read-only — their tracked files carry no write bits; `dependency` repos are writable but conventionally off-limits and excluded from `rwv push` by default). Together they give an agent harness everything it needs to drive the workspace without scraping help text.
- **Built-in failure recovery and drift repair.** Every `rwv sync` and `rwv sync-to` records savepoint refs under `refs/rwv/pre-op/<id>` so `rwv abort` can restore every touched repo to its exact pre-operation state; `rwv doctor` detects and repairs workspace drift (`--fix`); destructive overrides require explicit, narrowly scoped flags (`--discard-local-commits`, `--allow-stale-lock`).
- **Near-zero exit cost.** Member repos stay ordinary git clones at `<registry>/<owner>/<repo>/` with untouched history, while `rwv.lock` and ecosystem workspace files live in `projects/<name>/` and surface at the weave root via symlinks. Deleting `projects/`, `.rwv-active`, and the root symlinks leaves plain standalone git clones behind — you can walk away from `rwv` at any time without migrating your repositories back out.

Where your repos share a language (Rust + Rust, TS + TS, Go + Go, ...), the generated workspace files mean cross-repo imports resolve locally — a change in a shared library is immediately visible to its consumer with no publish step. Internal-only repos (typical in proprietary projects) can drop semver maintenance entirely; for repos that publish externally, the dance amortizes to external-release cadence rather than firing on every dev iteration. See the [monorepo lens](docs/explanation/lenses/monorepo.md) for the full cadence story.

### Status

repoweave is in **active alpha** (`0.x`): core workflows are heavily tested, and minor releases may refine CLI flags or on-disk formats as the design settles. When an upgrade introduces a format change, `rwv doctor` reports a named finding and [upgrade-rwv](docs/how-to/upgrade-rwv.md) covers migrating existing workspaces (`rwv doctor --fix` handles mechanical migrations).

### Install

**Prerequisites:** `git` on `$PATH`. On Windows, enable **Developer Mode** (or run from an elevated shell) so `rwv` can create workspace symlinks.

**Quick install** (Linux/macOS — detects platform, installs to `~/.local/bin`):

```bash
curl -fsSL https://cwalv.github.io/repoweave/install.sh | sh
```

**Pre-built binaries** — download from [GitHub Releases](https://github.com/cwalv/repoweave/releases) (Linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64).

**From source** via Cargo:

```bash
cargo install repoweave
```

**Via pip/uvx:**

```bash
pip install repoweave    # or: uvx repoweave
```

### Uninstall

Remove the binary:

```sh
rm ~/.local/bin/rwv
```

### Quick start

```bash
mkdir my-workspace && cd my-workspace
rwv fetch chatly/web-app          # clone project + all its repos, activate, install
```

That single command clones the project repo, reads its `rwv.toml` manifest, clones every listed repo to its canonical path (`github/chatly/server/`, etc.), activates the project (surfacing ecosystem workspace files like `package.json`, `go.work`, `Cargo.toml` at the weave root via symlinks), and runs install hooks. You are ready to work.

```bash
# edit across repos freely — cross-package imports resolve locally
cd github/chatly/server
# ... make changes ...
npm test --workspaces             # from weave root — tests span all repos

rwv lock                          # snapshot repo SHAs into rwv.lock
cd projects/web-app
git add rwv.lock && git commit -m "lock: update after payment feature"
```

Create an isolated working copy when you need parallel work, PR review, or agent isolation:

```bash
rwv workweave web-app create payments    # creates isolated working copy with git worktrees
cd ../.workweaves/web-app--payments
# independent branches, node_modules, .venv — weave is undisturbed
```

(Workweaves live at `<parent>/.workweaves/<project>--<name>/`, a sibling of the weave root by default — so `../.workweaves/...` from the weave root.)

### Commands

| Command | Description |
|---|---|
| `-C <path>`, `--cwd <path>` | Global: resolve the workspace as if invoked from `<path>` (precedes the subcommand) |
| `-w <project>--<name>`, `--workweave <project>--<name>` | Global: address a workweave by name from anywhere (precedes the subcommand) |
| `rwv` | Show current context (weave, project, workweave, repos) |
| `rwv fetch <source>` | Clone a project and all its repos; align repos to `rwv.lock`; activate and install. `--frozen` errors if the lock is incomplete (CI). With no `<source>`, re-clone missing members of the active project (`--project` names another); a weave with exactly one project and none active gets it activated |
| `rwv init <project>` | Create a new project with empty `rwv.toml`. Optional `--provider registry/owner` sets up the remote |
| `rwv activate <project>` | Set the active project — update `.rwv-active`, verify integration files, surface symlinks at the weave root, and run install hooks (`--no-materialize` to skip hooks) |
| `rwv materialize` | Run integration install hooks, clean up disabled-integration artifacts, and settle drifted attested files (`--regenerate-drifted`, `--adopt-drifted`, `--remove-undeclared-links`) without switching projects |
| `rwv add <url>` | Clone a repo, add to `rwv.toml`, re-run integrations. `<url>` is a URL (`file://` for a local remote) or registry shorthand; `--adopt <path>` registers a clone already under the weave root; `--role` sets the role, `--new` for `git init` |
| `rwv remove <path>` | Remove from `rwv.toml`, re-run integrations. `--delete` removes this workspace's checkout |
| `rwv role <repo> <role>` | Change a repo's role in `rwv.toml`, update ecosystem workspace files, and settle file permissions |
| `rwv lock` | Snapshot repo HEADs into `rwv.lock`. Errors on uncommitted changes (`--dirty` to bypass). `--commit` commits the lock, with `rwv.toml` when it has uncommitted changes (`-m` sets the message) |
| `rwv doctor` | Convention enforcement: orphans, dangling refs, stale locks, integration checks. `--locked` for scriptable lock-freshness check |
| `rwv status` | Show per-repo state: branch, tip, lock entry, relation, mid-op state. `--json` for machine-readable output |
| `rwv sync <source>` | Pull: align CWD to another workspace's committed `rwv.lock`. `--strategy ff\|rebase` (default `ff`); `--allow-stale-lock` to skip lock-freshness check; `--discard-local-commits` to hard-reset project repo to source tip |
| `rwv sync-to [<target>]` | Push: land CWD's commits into target (3-step: rebase CWD against target → auto-relock → FF-advance target). `--strategy rebase\|ff` (default `rebase`); `--retire` deletes the workweave on success. Bare `rwv sync-to` auto-targets the parent recorded in `.rwv-workweave` |
| `rwv abort` | Roll an in-flight `sync` or `sync-to` back to its savepoints in every workspace it touched; run it from any of them |
| `rwv push` | Coordinated cross-repo push: member repos first, then project repo. `--dry-run` to preview; `--force` for force-push consent; `--role`/`--repo` selectors to limit scope |
| `rwv update` | Advance each repo to its branch HEAD and re-snapshot `rwv.lock` (network bump; analogous to `cargo update`). `--commit` commits the lock and the regenerated files, with `rwv.toml` when it has uncommitted changes |
| `rwv workweave <project> create <name>` | Create an isolated working copy (worktrees on ephemeral branches) and run its install hooks (`--no-materialize` skips them) |
| `rwv workweave <project> set-container <path>` | Set the directory new workweaves for `project` are created under by default |
| `rwv workweave <project> delete <name>` | Delete a workweave (remove worktrees, clean up ephemeral branches) |
| `rwv workweave <project> list` | List workweaves for a project |
| `rwv workweave <project> log` | Show this workweave's unique commits (or `--diff` for the diff) versus its recorded parent, per repo. `--json` for machine-readable output |
| `rwv resolve` | Print the weave root path (useful for scripting: `cd $(rwv resolve)`) |
| `rwv prime` | Print structured workspace context for agent system prompts |
| `rwv explain [<verb>]` | Print a markdown reflection bundle for `<verb>` (purpose, invocation, output, JSON Schema, exit codes, examples). Omit `<verb>` to list explainable verbs |
| `rwv setup claude` | Register Claude Code hooks (`SessionStart` + `PreCompact` for `rwv prime`, `WorktreeCreate` + `WorktreeRemove` for multi-repo workweaves) |
| `rwv completions <shell>` | Generate shell completions (bash, zsh, fish, etc.) |

Any `rwv-<verb>` executable on `$PATH` is dispatched as `rwv <verb>` with the resolved workspace environment — see [write a plugin](docs/how-to/write-a-plugin.md) and the [plugin protocol reference](docs/reference/plugin-protocol.md).

### Shell completions

Generate completions for your shell and source them:

```bash
rwv completions bash > ~/.local/share/bash-completion/completions/rwv
rwv completions zsh  > ~/.zfunc/_rwv
rwv completions fish > ~/.config/fish/completions/rwv.fish
```

### Working with AI agents

repoweave injects workspace context into AI coding agents and gives each agent an isolated multi-repo sandbox (a workweave) with a transactional landing path.

**Claude Code** — register `rwv prime` (`SessionStart` + `PreCompact`) and workweave lifecycle hooks (`WorktreeCreate` + `WorktreeRemove`) in `~/.claude/settings.json`:

```bash
rwv setup claude
```

Idempotent and safe to re-run. With those hooks registered, every session starts with current workspace context, and `claude --worktree <name>` automatically creates and cleans up a full multi-repo workweave rather than a single-repo git worktree.

**Explicit workweave handoff (any agent harness):**

A workweave is a full isolated copy of the workspace — its own branches, its own `node_modules`/`.venv`/`target`, its own lock. Hand one to an agent and it works without touching the primary weave:

```bash
# 1. Create an isolated workspace for the agent
rwv workweave web-app create fix-auth

# 2. Hand the workweave path to the agent
#    The agent CDs into ../.workweaves/web-app--fix-auth/ and works normally.
#    It can commit and run tests across repos — primary is undisturbed.

# 3. Review the commits and diff in the workweave before landing
rwv -w web-app--fix-auth workweave web-app log
rwv -w web-app--fix-auth workweave web-app log --diff

# 4. Land the work and retire the workweave
cd ../.workweaves/web-app--fix-auth
rwv sync-to --retire
```

`rwv sync-to --retire` runs a three-step landing: rebases the workweave's commits onto the parent's current tip, auto-relocks the workweave, then fast-forwards the parent to the new tip and deletes the workweave on success. Every landing is abortable: savepoints at `refs/rwv/pre-op/<id>` let `rwv abort` roll both workspaces back to their exact pre-op state.

Multiple agents can work in parallel — one workweave each. The first landing is a clean fast-forward; subsequent ones rebase onto the primary's new state automatically during `rwv sync-to --retire` (or explicitly via `rwv sync primary --strategy rebase` if you want to run tests against the combined state before landing).

- [How to hand a task to an agent](docs/how-to/hand-task-to-agent.md) — discover the workspace, drive verbs via `rwv explain`, selector grammar
- [Bring workweave work home](docs/how-to/bring-workweave-work-home.md) — sync-to semantics, `--retire`, and conflict resolution
- [Workweave lifecycle](docs/explanation/joints/workweave-lifecycle.md) — create → work → sync-to --retire, the retire contract, and deletion
- [Resume or abort a mid-op sync](docs/how-to/resume-or-abort-mid-op-sync.md) — inspect op-state, `--continue`, `rwv abort` detail

### Documentation

Full docs at **[cwalv.github.io/repoweave](https://cwalv.github.io/repoweave/)**, or browse the source:

- **New here?** [tutorial.md](docs/tutorial.md) — fetch a project, make changes, lock, workweave
- **Task-shaped recipes:** [how-to/index.md](docs/how-to/index.md) — add a repo, create a workweave, push a cross-repo feature, hand off to an agent, release a package
- **Command & format lookup:** [reference/cli.md](docs/reference/cli.md) — all verbs, flags, file formats, roles, glossary, and integrations
- **The "why":** [explanation/lenses/workspace.md](docs/explanation/lenses/workspace.md) — workspace, monorepo, and agent lenses; plus [explanation/index.md](docs/explanation/index.md) for joints and cross-cutting concepts
- **Contributing:** [CONTRIBUTING.md](CONTRIBUTING.md) — how to report bugs, request integrations, and ask design questions
- **Adjacent tools:** [adjacent-tools.md](docs/adjacent-tools.md) — mise, direnv, devcontainers, CI multi-repo checkout

### License

[MIT](LICENSE)
