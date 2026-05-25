# repoweave (`rwv`)

**Workspace tooling for a project split across several repos.**

If your project spans more than one repo, you've probably felt at least one of these: a setup README that drifts away from what people actually clone; cross-cutting scripts, manifests, and decision records with no obvious home; a publish-and-reinstall cycle every time a shared library changes; no way to say *exactly* this set of revisions was running in production on a given day; explaining the repository topology to an AI agent every new session.

repoweave is a coordination layer that addresses those without merging the repos. A committable manifest lists which repos belong to the project. A single lock pins every revision for reproducibility. One command (`rwv fetch <url>`) clones the project and every repo it lists. Workweaves give you isolated parallel cross-repo work — or an agent sandbox — without disturbing the primary. The repos themselves stay independent.

### Why it exists

A monorepo eliminates most of the coordination pain — at the cost of vendoring every repo into one tree and giving up per-repo ownership. repoweave gives you the coordination wins while leaving each repo sovereign:

- **A committable manifest and lock.** `projects/<name>/rwv.yaml` lists the repos; `rwv.lock` pins every revision. `sha256sum rwv.lock` is the multi-repo equivalent of `git rev-parse HEAD`.
- **One command to reproduce the world.** `rwv fetch <url>` clones the project and every repo it lists, generates ecosystem workspace files where they apply, and runs install commands. New machine to working environment in one step.
- **A home for cross-cutting artifacts.** The project repo carries the manifest, lock, and any operational scripts, k8s manifests, demos, or decision records that don't belong to any single library.
- **Isolated parallel work via workweaves.** `git worktree` extended across N repos, with per-workweave `node_modules` / `.venv` / `target`. Use for feature branches, PR review, or agent sandboxes — the primary weave stays undisturbed.
- **Structured agent context.** `rwv prime`, `rwv explain --json`, and role-tagged repos give AI harnesses a machine-readable view of the workspace, with `reference` and `dependency` roles acting as a read-only allow-list.

Where your repos share a language (Rust + Rust, TS + TS, Go + Go, ...), the generated workspace files mean cross-repo imports resolve locally — a change in a shared library is immediately visible to its consumer with no publish step. Internal-only repos (typical in proprietary projects) can drop semver maintenance entirely; for repos that publish externally, the dance amortizes to external-release cadence rather than firing on every dev iteration. See the [monorepo lens](docs/explanation/lenses/monorepo.md) for the full cadence story.

### Install

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

That single command clones the project repo, reads its `rwv.yaml` manifest, clones every listed repo to its canonical path (`github/chatly/server/`, etc.), generates ecosystem workspace files (`package.json`, `go.work`, `Cargo.toml`, ...), and runs install commands. You are ready to work.

```bash
# edit across repos freely — cross-package imports resolve locally
cd github/chatly/server
# ... make changes ...
npm test --workspaces             # from weave directory — tests span all repos

rwv lock                          # snapshot repo SHAs into rwv.lock
cd projects/web-app
git add rwv.lock && git commit -m "lock: update after payment feature"
```

Create an isolated working copy when you need parallel work, PR review, or agent isolation:

```bash
rwv workweave web-app create payments    # creates isolated working copy with git worktrees
cd .workweaves/payments
# independent branches, node_modules, .venv — weave is undisturbed
```

### Commands

| Command | Description |
|---|---|
| `rwv` | Show current context (weave, project, workweave, repos) |
| `rwv fetch <source>` | Clone a project and all its repos; activate and install. `--locked` for exact reproduction, `--frozen` for CI |
| `rwv init <project>` | Create a new project with empty `rwv.yaml`. Optional `--provider registry/owner` sets up the remote |
| `rwv activate <project>` | Set the active project — generate ecosystem files, symlink to weave directory, run install |
| `rwv add <url>` | Clone a repo, add to `rwv.yaml`, re-run integrations. `--role` sets the role, `--new` for `git init` |
| `rwv remove <path>` | Remove from `rwv.yaml`, re-run integrations. `--delete` removes the clone |
| `rwv lock` | Snapshot repo HEADs into `rwv.lock`. Errors on uncommitted changes (`--dirty` to bypass) |
| `rwv doctor` | Convention enforcement: orphans, dangling refs, stale locks, integration checks. `--locked` for scriptable lock-freshness check |
| `rwv status` | Show per-repo state: branch, tip, lock entry, relation, mid-op state. `--json` for machine-readable output |
| `rwv sync <source>` | Align CWD workspace with another workspace's committed `rwv.lock`. `--strategy ff\|rebase\|merge`, `--force` to bypass lock-freshness precondition |
| `rwv abort` | Restore CWD workspace to its pre-sync state using savepoint refs |
| `rwv workweave <project> create <name>` | Create an isolated working copy (worktrees on ephemeral branches) |
| `rwv workweave <project> delete <name>` | Delete a workweave (remove worktrees, clean up ephemeral branches) |
| `rwv workweave <project> list` | List workweaves for a project |
| `rwv resolve` | Print the weave directory path (useful for scripting: `cd $(rwv resolve)`) |
| `rwv prime` | Print structured workspace context for agent system prompts |
| `rwv setup claude` | Register `rwv prime` as a Claude Code hook (SessionStart + PreCompact) |
| `rwv setup agents-md` | Generate `AGENTS.md` at the weave directory for Cursor, Copilot, and other agents |
| `rwv completions <shell>` | Generate shell completions (bash, zsh, fish, etc.) |

### Shell completions

Generate completions for your shell and source them:

```bash
rwv completions bash > ~/.local/share/bash-completion/completions/rwv
rwv completions zsh  > ~/.zfunc/_rwv
rwv completions fish > ~/.config/fish/completions/rwv.fish
```

### Agent integration

repoweave can inject workspace context into AI coding agents so they understand the multi-repo layout, active project, repo roles, and available commands.

**Claude Code** — register `rwv prime` as a hook that fires on session start and pre-compact:

```bash
rwv setup claude
```

**Cursor, Copilot, and other agents** that read `AGENTS.md`:

```bash
rwv setup agents-md
```

Both commands are idempotent and safe to re-run.

### Documentation

Full docs at **[cwalv.github.io/repoweave](https://cwalv.github.io/repoweave/)**, or browse the source:

- **New here?** [tutorial.md](docs/tutorial.md) — fetch a project, make changes, lock, workweave
- **Task-shaped recipes:** [how-to/](docs/how-to/index.md) — add a repo, create a workweave, push a cross-repo feature, hand off to an agent, release a package
- **Command & format lookup:** [reference/cli.md](docs/reference/cli.md) — all verbs, flags, file formats, roles, glossary, and integrations
- **The "why":** [explanation/lenses/](docs/explanation/lenses/workspace.md) — workspace, monorepo, and agent lenses; plus [joints/](docs/explanation/joints/) for cross-cutting concepts
- **Contributing:** [contributing/developing.md](docs/contributing/developing.md) — build from source, the dogfood loop, releasing
- **Adjacent tools:** [adjacent-tools.md](docs/adjacent-tools.md) — mise, direnv, devcontainers, CI multi-repo checkout

### License

[MIT](LICENSE)
