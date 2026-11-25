# repoweave (`rwv`)

**Monorepo ergonomics without the monorepo, built for parallel agents.**

If your project spans more than one repo — or even a single `owned` member repo paired with a `projects/<name>/` coordination envelope, read-only `reference` repos for study, and isolated parallel agent workweaves — you've probably felt at least one of these: a setup README that drifts away from what people actually clone; cross-cutting scripts, manifests, and decision records with no obvious home; a publish-and-reinstall cycle every time a shared library changes; no way to say *exactly* this set of revisions was running in production on a given day; two coding agents colliding in the same checkout or explaining the repository topology every new session.

repoweave is a coordination layer that addresses those without merging the repos. A committable manifest lists which repos belong to the project — whether ten polyrepo services or one `owned` member repo alongside `reference` dependencies. A single lock pins every revision for reproducibility. One command (`rwv clone <url>`) clones the project and every repo it lists. Every change is made in a **workweave** — an isolated copy of the whole workspace on its own branch — and brought home with `rwv land --retire`, which composes the work onto its target, records every repo's new revision in one project commit, and deletes the workweave; a landing that stops on a conflict is resumed with `--continue` or taken back whole with `rwv abort`. The repos themselves stay independent.

**Start where you are:** [Shared workspaces for polyrepo teams](docs/explanation/lenses/workspace.md) · [Monorepo speed without migration](docs/explanation/lenses/monorepo.md) · [Parallel sandboxes for agents](docs/explanation/lenses/agent.md)

### Why it exists

A monorepo eliminates most of the coordination pain — at the cost of vendoring every repo into one tree and giving up per-repo ownership. repoweave gives you the coordination wins while leaving each repo sovereign. ([Alternatives comparison](docs/comparison.md) — when git submodules, gita, or a monorepo actually wins.)

- **A committable manifest and lock.** `projects/<name>/rwv.toml` lists the repos; `rwv.lock` pins every revision. `sha256sum rwv.lock` is the multi-repo equivalent of `git rev-parse HEAD`.
- **One command reproduces the workspace.** `rwv clone <url>` clones the project and every repo it lists at its pinned commit, surfaces ecosystem workspace files at the weave root where they apply, and runs install commands. For toolchain pins, env activation, or full OS-level reproduction, drop a `.mise.toml` / `.envrc` / `devcontainer.json` into the project repo (they're cross-cutting artifacts) — `rwv clone` carries them with everything else. See [adjacent-tools](docs/adjacent-tools.md).
- **A dedicated `projects/<name>/` envelope for intent and cross-cutting artifacts.** Keeping `rwv.toml`, `rwv.lock`, deliberative design records, issue trackers, `CLAUDE.md`, operational scripts, k8s manifests, devcontainer configs, `.mise.toml` toolchain pins, and Nix flakes in `projects/<name>/` gives them versioned correspondence with your code without polluting any product repository's commit history or public landing page — and they come along on every `rwv clone`.
- **Isolated parallel work via workweaves.** `git worktree` extended across N repos, with per-workweave `node_modules` / `.venv` / `target` and automatic copy-on-write file sharing where the filesystem supports reflinks. Use for feature branches, PR review, hotfixes on a release line, or agent sandboxes — the primary weave stays undisturbed.
- **Landing as one verb.** `rwv land` composes the workweave onto the branch its record lands into as that branch's `history` setting says (`linear`, `merge` or `ff-only`), records every member's tip in one project commit, and fast-forwards the target. `rwv sync` takes the target's newer work into a workweave first when you want to test against it. `rwv pull` takes in a teammate's published work and aligns every repo to the pins it holds; `rwv push` publishes members first and the project last.
- **Branches and release lines without special support.** A weave branch is a **feature** (work that lands into a target) or **declared** (members follow the branches its `rwv.toml` names). A release line is a declared branch; hotfixes are workweaves that land into it. See [maintain a release line](docs/how-to/maintain-a-release-line.md).
- **A bounded surface for automation.** `rwv prime` advertises the workspace; `rwv explain <verb>` returns a markdown bundle with the verb's JSON Schema embedded; `rwv setup claude` wires `SessionStart`/`PreCompact` context hooks and `WorktreeCreate`/`WorktreeRemove` lifecycle hooks so `claude --worktree` provisions a full multi-repo workweave; roles surface repo change-resistance as machine-readable metadata (`reference` repos are physically read-only — their tracked files carry no write bits; `dependency` repos are writable but conventionally off-limits and excluded from `rwv push` by default). Together they give an agent harness everything it needs to drive the workspace without scraping help text.
- **Built-in failure recovery and drift repair.** Every `rwv sync`, `rwv land` and merging `rwv pull` takes savepoints first, so a stop on a conflict ends in `--continue` or in `rwv abort`, which restores every touched repo to its exact pre-operation state and keeps whatever it discards as a backup; `rwv doctor` detects workspace drift and `--fix` repairs what a verb can; destructive overrides are narrowly scoped flags that name what they give up (`--discard-unlanded`, `--reset-to-upstream`).
- **Near-zero exit cost.** Member repos stay ordinary git clones at `<registry>/<owner>/<repo>/` with untouched history, while `rwv.lock` and ecosystem workspace files live in `projects/<name>/` and surface at the weave root via symlinks. Delete each workweave with `rwv workweave delete` first, which removes its worktrees from the clones; then deleting `projects/`, `.rwv-active`, and the root symlinks leaves plain standalone git clones behind — you can walk away from `rwv` at any time without migrating your repositories back out. Two traces stay in the clones: a `reference` member's tracked files have no write bits until you restore them (`chmod -R u+w <member>`, or `attrib -R <member>\* /S` on Windows), and the refs under `refs/rwv/` that keep recovery points remain until you drop them: backups (`rwv backup drop`), and any savepoint an operation stopped mid-way still holds until `rwv abort` or `--continue` ends it.

Where your repos share a language (Rust + Rust, TS + TS, Go + Go, ...), the generated workspace files mean cross-repo imports resolve locally — a change in a shared library is immediately visible to its consumer with no publish step. Internal-only repos (typical in proprietary projects) can drop semver maintenance entirely; for repos that publish externally, the dance amortizes to external-release cadence rather than firing on every dev iteration. See the [monorepo lens](docs/explanation/lenses/monorepo.md) for the full cadence story.

### Status

repoweave is in **active alpha** (`0.x`): core workflows are heavily tested, and minor releases may refine CLI flags or on-disk formats as the design settles. When an upgrade introduces a format change, `rwv doctor` reports a named finding and [upgrade-rwv](docs/how-to/upgrade-rwv.md) covers migrating existing workspaces (`rwv doctor --fix` handles mechanical migrations).

### Install

**Prerequisites:** git 2.43 or newer on `$PATH`; `rwv` refuses to run a repository verb with an older one. On Windows, enable **Developer Mode** (or run from an elevated shell) so `rwv` can create workspace symlinks.

Every channel below installs `rwv` and, beside it, `rwv-each` — the plugin behind [`rwv each`](docs/reference/rwv-each.md).

**Quick install** (Linux/macOS — detects platform, installs to `~/.local/bin`):

```bash
curl -fsSL https://cwalv.github.io/repoweave/install.sh | sh
```

**Homebrew** (macOS/Linux):

```bash
brew install cwalv/tap/repoweave
```

**Pre-built binaries** — download from [GitHub Releases](https://github.com/cwalv/repoweave/releases) (Linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64).

**From source** via Cargo (not published to crates.io; builds from this repo):

```bash
cargo install --git https://github.com/cwalv/repoweave
```

**Via pip/uvx:**

```bash
pip install repoweave    # or: uvx repoweave
```

### Uninstall

Remove the binaries:

```sh
rm ~/.local/bin/rwv ~/.local/bin/rwv-each
```

### Quick start

**Cloning an existing project:**

```bash
mkdir my-workspace && cd my-workspace
rwv clone github/chatly/web-app
```

That single command clones the project repo, reads its `rwv.toml` manifest, clones every listed repo to its canonical path (`github/chatly/server/`, etc.) at the commit `rwv.lock` pins, activates the project (surfacing ecosystem workspace files like `package.json`, `go.work`, `Cargo.toml` at the weave root via symlinks), and runs install hooks. `rwv status` shows each repo against its pin.

**Making a change — in a workweave:**

```bash
rwv workweave create payments
cd ../.workweaves/web-app--payments
git -C github/chatly/server commit --allow-empty -m "server: payments"
rwv land --retire
```

The workweave sits beside the weave root at `<parent>/.workweaves/<project>--<name>/`, on a feature branch that lands into the branch the weave had checked out. `rwv land --retire` composes its commits onto that target, records every repo's new revision in one project commit, fast-forwards the target, and deletes the workweave. Your shell is left in a directory that no longer exists, so `cd` back to the weave root.

**Taking in the team's work and publishing yours:**

```bash
rwv pull
rwv push
```

`rwv pull` fetches, fast-forwards the project branch to its upstream, and aligns every member to the pins the pulled lock holds. `rwv push` publishes the member repos first, then the branch's record, then the project repo, so no teammate pulls a lock that names an unpublished commit. Until then `rwv pull` and `rwv fetch` report a pin the remote does not hold as `pin off remote`.

**Starting a new project from scratch (Day 0)** — see [create a project](docs/how-to/create-a-project.md) for the full walkthrough:

```bash
mkdir my-workspace && cd my-workspace
rwv init tooling
rwv member set-url projects/tooling https://github.com/chatly/tooling.git
rwv member add https://github.com/chatly/server.git
rwv member add https://github.com/chatly/shared-types.git --role dependency
rwv push
```

`rwv init` commits `rwv.toml`; each `rwv member add` clones the repo, commits the entry with the member's first pin, and re-runs the ecosystem integrations.

[The tutorial](docs/tutorial.md) walks this through end to end, including a landing that stops, with disposable local repositories.

### Commands

| Command | Description |
|---|---|
| `-C <path>`, `--cwd <path>` | Global: resolve the workspace as if invoked from `<path>` (precedes the subcommand) |
| `-w <project>--<name>`, `--workweave <project>--<name>` | Global: address a workweave by name from anywhere (precedes the subcommand) |
| `rwv` | Show current context (weave, project, workweave, repos) |
| `rwv clone <source>` | Clone a project and each of its repos at the commit `rwv.lock` pins; activate and install. Writes no lock. A repo the lock has no entry for, or pins at an unreachable commit, is cloned at its branch tip and listed, and the clone exits `3` — the check a CI job runs for a reproducible build |
| `rwv init <project>` | Create a new project with empty `rwv.toml` and commit the files it wrote. Optional `--provider registry/owner` sets up the remote |
| `rwv activate <project>` | Set the active project — update `.rwv-active`, verify integration files, surface symlinks at the weave root, and run install hooks (`--no-materialize` to skip hooks). Moves no checkout: a member the project shares with the active project that holds edits or is not aligned for it refuses the switch, and `--update` aligns first |
| `rwv materialize` | Run integration install hooks, clean up disabled-integration artifacts, and settle drifted attested files (`--regenerate-drifted`, `--adopt-drifted`, `--remove-undeclared-links`) without switching projects |
| `rwv member add <url>` | Clone a repo, add to `rwv.toml`, re-run integrations, and commit the change with the repo's first pin. `<url>` is a URL (`file://` for a local remote) or registry shorthand; `--adopt <path>` registers a clone already under the weave root; `--role` sets the role, `--new` for `git init`, `--no-recurse-submodules` leaves the member's submodules uninitialized in new checkouts |
| `rwv member remove <path>` | Remove from `rwv.toml`, re-run integrations. `--delete` removes this workspace's checkout |
| `rwv member pin <repo> <rev>` | Pin a repo at a commit you choose, in either direction, and align it to the pin. Gives a repo its first pin when `member add` could not (`--commit` commits the lock) |
| `rwv member set-branch <repo> <branch>` | Declare the branch a repo follows and pin it at that branch's tip, committing both (`--new` creates a branch that exists nowhere) |
| `rwv member set-url <repo> <url>` | Set a repo's url; for `projects/<name>`, record the remote the project repo publishes to |
| `rwv member set-role <repo> <role>` | Change a repo's role in `rwv.toml`, update ecosystem workspace files, and settle file permissions |
| `rwv integration set-enabled <name> <true\|false>` | Turn an integration on (authoring its managed files) or off (deleting nothing; `rwv materialize` strips), and commit the change; `rwv integration set <name> <key> <value>` sets another key of its table |
| `rwv member set-recurse-submodules <repo> <true\|false>` | Say whether new checkouts of a repo populate its submodules, and update ecosystem workspace files |
| `rwv lock` | Record repo HEADs as `rwv.lock`'s pins, moving a pin forward only: a repo with no pin keeps none, and one behind or diverged from its pin is refused unless `--repo` names it. Errors on uncommitted changes (`--dirty` to bypass). `--commit` commits the lock, with `rwv.toml` when it has uncommitted changes (`-m` sets the message) |
| `rwv doctor` | Convention enforcement: orphans, dangling refs, pins that disagree with their checkouts, integration checks. `--locked` for scriptable lock-freshness check |
| `rwv status` | Show per-repo state: branch, tip, lock entry, relation, mid-op state. `--json` for machine-readable output |
| `rwv log [<range>] [--against <W>]` | Run the VCS's log in every repo for a range in its own syntax. `--against <W>` lists the commits HEAD holds beyond weave branch `<W>`'s pins, per repo, with `--diff` for the diff; `--json` for machine-readable output |
| `rwv sync` | Take the target's new work into a feature: each member from the branch it lands on, then the project branch, then the relock, each repo integrated as the target's `history` setting says (`--history linear\|merge\|ff-only` for one run where it records none); `--from <branch>` merges another weave branch in; `--reset-to-target <repo>` moves a repo to what it lands on, keeping a backup. Work built on another lineage is refused before anything moves |
| `rwv land` | Land the feature into the branch its record lands into: compose this workspace onto it by the target's `history` setting, record every member's tip in one project commit, fast-forward the target. `--target <T>` lands into another branch for one run, `--history <h>` supplies a setting where the target records none, `--retire` deletes the workweave on success, `--continue` resumes a stopped landing |
| `rwv abort` | Roll an in-flight `sync` or `land` back to its savepoints in every workspace it touched; run it from any of them |
| `rwv branch <create\|set-mode\|set-history\|list\|rename\|delete>` | Create a weave branch, record how one is used (`feature` onto a `--target` branch, or `declared`) or how it accepts landings, list the weave branches with their records, rename one, or delete one by the deletion rule. `init`, `clone` and `workweave create` record the branch they make or leave checked out; `update`, `pull`, `lock`, `sync`, `land` and `push` refuse a branch with no record |
| `rwv switch <W>` | Put the project repo on a recorded weave branch and align the members to it; `--detach <rev>` detaches the workspace at a revision |
| `rwv cherry-pick <project-rev>` | Port one project commit onto this feature: each member's change, each dependency's pin, and the project change with its relock |
| `rwv backup <list\|drop\|prune>` | List the backups rwv keeps before a step that would leave a commit or an edit unpreserved (`workweave delete --discard-*`, and `abort`, which keeps what it resets in one slot per branch and per checkout), drop them by name, or prune those a branch already reaches. A drop that would leave a commit some project's lock pins unreachable is refused |
| `rwv push` | Publish a weave branch from any workspace: member repos first, then the branch's record, then the project branch. `--dry-run` to preview; `--overwrite <repo>` to replace a rewritten branch; `--branch` to push one that is not checked out; `--role`/`--repo` selectors to limit scope |
| `rwv fetch` | Pruning fetch of every remote of the project repo and of each member present on disk. Moves no branch, checkout, or lock; reports which remote-tracking branches moved |
| `rwv pull` | Fetch, fast-forward the project branch to its upstream, then `rwv update`. Reports the members the pulled lock re-pinned |
| `rwv update` | Clone the repos missing on disk and align each present repo to the commit `rwv.lock` pins. Never advances a pin or writes the lock. `--project` names another project; a weave with exactly one project and none active gets it activated |
| `rwv update --remote` | Advance each repo to its branch HEAD and re-snapshot `rwv.lock` (network bump; analogous to `cargo update`). `--commit` commits the lock and the regenerated files, with `rwv.toml` when it has uncommitted changes |
| `rwv workweave create <name>` | Create an isolated working copy (worktrees on ephemeral branches) and run its install hooks (`--no-materialize` skips them). `--dir <path>` places it at exactly that path; `--basedir <path>` places it under that directory for one run |
| `rwv workweave set-default-create-basedir <path>` | Set the directory new workweaves of the project are created under by default |
| `rwv workweave delete <name>` | Delete a workweave (remove worktrees, clean up ephemeral branches) |
| `rwv workweave list` | List the project's workweaves |
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
rwv workweave create fix-auth

# 2. Hand the workweave path to the agent
#    The agent CDs into ../.workweaves/web-app--fix-auth/ and works normally.
#    It can commit and run tests across repos — primary is undisturbed.

# 3. Review the commits and diff in the workweave before landing
rwv -w web-app--fix-auth log --against main
rwv -w web-app--fix-auth log --against main --diff

# 4. Land the work and retire the workweave
cd ../.workweaves/web-app--fix-auth
rwv land --retire
```

`rwv land --retire` composes the workweave's commits onto the branch its record lands into as that branch's `history` setting says (`rwv branch set-history linear` rebases, `merge` makes a merge commit), records the workweave's member tips in the landing's one project commit, then fast-forwards the target and deletes the workweave on success. Every landing is abortable: `rwv abort` rolls both workspaces back to their exact pre-operation state.

Multiple agents can work in parallel — one workweave each. The first landing is a clean fast-forward; later ones are composed onto the target's new state by its `history` setting during `rwv land --retire` (or explicitly via `rwv sync` in a feature workweave if you want to run tests against the combined state before landing).

- [How to hand a task to an agent](docs/how-to/hand-task-to-agent.md) — discover the workspace, drive verbs via `rwv explain`, selector grammar
- [Bring workweave work home](docs/how-to/bring-workweave-work-home.md) — land semantics, `--retire`, and conflict resolution
- [Workweave lifecycle](docs/explanation/joints/workweave-lifecycle.md) — create → work → land --retire, the retire contract, and deletion
- [Resume or abort a mid-op sync](docs/how-to/resume-or-abort-mid-op-sync.md) — inspect the op record, `--continue`, `rwv abort` detail

### Documentation

Full docs at **[cwalv.github.io/repoweave](https://cwalv.github.io/repoweave/)**, or browse the source:

- **New here?** [tutorial.md](docs/tutorial.md) — clone a project, change it in a workweave, land, publish, recover from a stop
- **Task-shaped recipes:** [how-to/index.md](docs/how-to/index.md) — add a repo, create a workweave, push a cross-repo feature, maintain a release line, hand off to an agent, release a package
- **Command & format lookup:** [reference/cli.md](docs/reference/cli.md) — all verbs, flags, file formats, roles, glossary, and integrations
- **The "why":** [explanation/lenses/workspace.md](docs/explanation/lenses/workspace.md) — workspace, monorepo, and agent lenses; plus [explanation/index.md](docs/explanation/index.md) for joints and cross-cutting concepts
- **Contributing:** [CONTRIBUTING.md](CONTRIBUTING.md) — how to report bugs, request integrations, and ask design questions
- **Adjacent tools:** [adjacent-tools.md](docs/adjacent-tools.md) — mise, direnv, devcontainers, CI multi-repo checkout

### License

[MIT](LICENSE)
