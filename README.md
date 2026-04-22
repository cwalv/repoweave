# repoweave (`rwv`)

**Monorepo ergonomics for independent repositories.**

You already use `go.work`, Cargo workspaces, pnpm workspaces, or uv workspaces?  
repoweave builds directly on top of them — at the repo layer.

### Why it exists

Your code lives across multiple independent git repositories. Changing a shared internal library forces the version-bump / publish / update dance. Full monorepos eliminate that but require vendoring everything.

repoweave gives you the two biggest practical wins of a monorepo while keeping every repo sovereign:

- **No version-bump dance** during development — imports resolve locally across repos, tests run end-to-end, you commit and `rwv lock`. Repos that publish externally still tag and publish at release time; the rest never need semver at all.
- **Ephemeral isolated workspaces** — spin up clean copies for agents, PRs, or parallel work via git worktrees on ephemeral branches

### The weave metaphor

“Weave” comes from weaving fabric: independent **threads** (your git repositories) are interwoven into a single, usable **fabric** — a unified workspace. The threads keep their identity and history; they simply work better together.

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

That single command clones the project repo, reads its `rwv.yaml` manifest, clones every listed repository to its canonical path (`github/chatly/server/`, etc.), generates ecosystem workspace files (`package.json`, `go.work`, `Cargo.toml`, ...), and runs install commands. You are ready to work.

```bash
# edit across repos freely — cross-package imports resolve locally
cd github/chatly/server
# ... make changes ...
npm test --workspaces             # from weave directory — tests span all repos

rwv lock                          # snapshot repo SHAs into rwv.lock
cd projects/web-app
git add rwv.lock && git commit -m “lock: update after payment feature”
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
| `rwv check --locked` | Alias for `rwv doctor --locked` — zero exit iff every repo tip matches its lock entry |
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

- [tutorial.md](docs/tutorial.md) — walkthrough of common workflows (fetch, add/remove, lock, workweave, agent isolation, CI)
- [concepts.md](docs/concepts.md) — why repoweave exists, design rationale, prior art, design decisions
- [releasing.md](docs/releasing.md) — locking, releasing, per-ecosystem constraint checking and publishing
- [reference.md](docs/reference.md) — directory layout, weaves and workweaves, projects, roles, lock files, commands
- [integrations.md](docs/integrations.md) — built-in ecosystem integrations (npm, pnpm, Go, uv, Cargo, gita, VS Code, static-files)
- [adjacent-tools.md](docs/adjacent-tools.md) — mise, direnv, devcontainers, CI multi-repo checkout

### License

[MIT](LICENSE)