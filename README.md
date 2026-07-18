# repoweave (`rwv`)

**Workspace tooling for a project split across several repos.**

If your project spans more than one repo, you've probably felt at least one of these: a setup README that drifts away from what people actually clone; cross-cutting scripts, manifests, and decision records with no obvious home; a publish-and-reinstall cycle every time a shared library changes; no way to say *exactly* this set of revisions was running in production on a given day; explaining the repository topology to an AI agent every new session.

repoweave is a coordination layer that addresses those without merging the repos. A committable manifest lists which repos belong to the project. A single lock pins every revision for reproducibility. One command (`rwv fetch <url>`) clones the project and every repo it lists. Workweaves give you isolated parallel cross-repo work — or an agent sandbox — without disturbing the primary. The repos themselves stay independent.

### Why it exists

A monorepo eliminates most of the coordination pain — at the cost of vendoring every repo into one tree and giving up per-repo ownership. repoweave gives you the coordination wins while leaving each repo sovereign. ([Alternatives comparison](docs/comparison.md) — when git submodules, gita, or a monorepo actually wins.)

- **A committable manifest and lock.** `projects/<name>/rwv.yaml` lists the repos; `rwv.lock` pins every revision. `sha256sum rwv.lock` is the multi-repo equivalent of `git rev-parse HEAD`.
- **One command reproduces the workspace.** `rwv fetch <url>` clones the project, every repo it lists, generates ecosystem workspace files where they apply, and runs install commands. For toolchain pins, env activation, or full OS-level reproduction, drop a `.mise.toml` / `.envrc` / `devcontainer.json` into the project repo (they're cross-cutting artifacts) — `rwv fetch` carries them with everything else. See [adjacent-tools](docs/adjacent-tools.md).
- **A natural home for cross-cutting artifacts that don't quite fit anywhere else.** Operational scripts, k8s manifests, ADRs, demos, release notes, devcontainer configs, `.mise.toml` toolchain pins, Nix flakes — they live in the project repo without contaminating any single library's history, and they come along on every `rwv fetch`.
- **Isolated parallel work via workweaves.** `git worktree` extended across N repos, with per-workweave `node_modules` / `.venv` / `target`. Use for feature branches, PR review, or agent sandboxes — the primary weave stays undisturbed.
- **A bounded surface for automation.** `rwv prime` advertises the workspace; `rwv explain <verb>` returns a markdown bundle with the verb's JSON Schema embedded; roles act as a machine-readable allow-list (`reference` and `dependency` are read-only); workweaves isolate the blast radius. Together they give an agent harness everything it needs to drive the workspace without scraping help text.

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
cd .workweaves/web-app--payments
# independent branches, node_modules, .venv — weave is undisturbed
```

### Commands

| Command | Description |
|---|---|
| `rwv` | Show current context (weave, project, workweave, repos) |
| `rwv fetch <source>` | Clone a project and all its repos; align repos to `rwv.lock`; activate and install. `--frozen` errors if the lock is stale (CI) |
| `rwv init <project>` | Create a new project with empty `rwv.yaml`. Optional `--provider registry/owner` sets up the remote |
| `rwv activate <project>` | Set the active project — generate ecosystem files, symlink to weave directory, run install |
| `rwv add <url>` | Clone a repo, add to `rwv.yaml`, re-run integrations. `--role` sets the role, `--new` for `git init` |
| `rwv remove <path>` | Remove from `rwv.yaml`, re-run integrations. `--delete` removes the clone |
| `rwv lock` | Snapshot repo HEADs into `rwv.lock`. Errors on uncommitted changes (`--dirty` to bypass) |
| `rwv doctor` | Convention enforcement: orphans, dangling refs, stale locks, integration checks. `--locked` for scriptable lock-freshness check |
| `rwv status` | Show per-repo state: branch, tip, lock entry, relation, mid-op state. `--json` for machine-readable output |
| `rwv sync <source>` | Pull: align CWD to another workspace's committed `rwv.lock`. `--strategy ff\|rebase\|merge` (default `ff`); `--allow-stale-lock` to skip lock-freshness check; `--discard-local-commits` to hard-reset project repo to source tip |
| `rwv sync-to [<target>]` | Push: land CWD's commits into target (3-step: rebase CWD against target → auto-relock → FF-advance target). `--strategy rebase\|ff\|merge` (default `rebase`); `--retire` deletes the workweave on success. Bare `rwv sync-to` auto-targets the parent recorded in `.rwv-workweave` |
| `rwv abort` | Restore CWD workspace to its pre-sync state using savepoint refs |
| `rwv push` | Coordinated cross-repo push: manifest repos first, then project repo. `--dry-run` to preview; `--force` for force-push consent; `--role`/`--repo` selectors to limit scope |
| `rwv update` | Advance each repo to its branch HEAD and re-snapshot `rwv.lock` (network bump; analogous to `cargo update`). `--commit` to commit the lock after writing it |
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

### Run agents in parallel

A workweave is a full isolated copy of the workspace — its own branches, its own `node_modules`/`.venv`/`target`, its own lock. Hand one to an agent and it works without touching the primary weave:

```bash
# 1. Create an isolated workspace for the agent
rwv workweave web-app create fix-auth

# 2. Hand the workweave path to the agent
#    The agent CDs into .workweaves/web-app--fix-auth/ and works normally.
#    It can commit, run tests, and rwv lock — primary is undisturbed.

# 3. Review the diff in the workweave before landing
git -C .workweaves/web-app--fix-auth/projects/web-app log --oneline main..
git -C .workweaves/web-app--fix-auth/github/chatly/server diff HEAD~1

# 4. Land the work and retire the workweave
cd .workweaves/web-app--fix-auth
rwv sync-to --retire
```

`rwv sync-to --retire` runs a three-step landing: rebases the workweave's commits onto the parent's current tip, re-snapshots the lock, then fast-forwards the parent to the new tip. The workweave is deleted on success. Every landing is abortable: savepoints at `refs/rwv/pre-op/<id>` let `rwv abort` roll both workspaces back to their exact pre-op state.

Multiple agents can work in parallel — one workweave each. The first landing is a clean fast-forward; subsequent ones absorb the primary's new state via `rwv sync primary --strategy rebase` before calling `rwv sync-to --retire`.

- [How to hand a task to an agent](docs/how-to/hand-task-to-agent.md) — discover the workspace, drive verbs via `rwv explain`, selector grammar
- [Bring workweave work home](docs/how-to/bring-workweave-work-home.md) — sync-to semantics, `--retire`, and conflict resolution
- [Workweave lifecycle](docs/explanation/joints/workweave-lifecycle.md) — create → work → sync-to --retire, the retire contract, and deletion
- [Resume or abort a mid-op sync](docs/how-to/resume-or-abort-mid-op-sync.md) — inspect op-state, `--continue`, `rwv abort` detail

The existing "Agent integration" section above covers context injection (`rwv prime` / `AGENTS.md`); this section is the *workflow* half — isolated sandboxes and the transactional landing path.

### Documentation

Full docs at **[cwalv.github.io/repoweave](https://cwalv.github.io/repoweave/)**, or browse the source:

- **New here?** [tutorial.md](docs/tutorial.md) — fetch a project, make changes, lock, workweave
- **Task-shaped recipes:** [how-to/](docs/how-to/index.md) — add a repo, create a workweave, push a cross-repo feature, hand off to an agent, release a package
- **Command & format lookup:** [reference/cli.md](docs/reference/cli.md) — all verbs, flags, file formats, roles, glossary, and integrations
- **The "why":** [explanation/lenses/](docs/explanation/lenses/workspace.md) — workspace, monorepo, and agent lenses; plus [explanation/](docs/explanation/index.md) for joints and cross-cutting concepts
- **Contributing:** [CONTRIBUTING.md](CONTRIBUTING.md) — how to report bugs, request integrations, and ask design questions
- **Adjacent tools:** [adjacent-tools.md](docs/adjacent-tools.md) — mise, direnv, devcontainers, CI multi-repo checkout

### License

[MIT](LICENSE)
