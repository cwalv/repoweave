# Workflows

A walkthrough of common repoweave workflows. These are written as idealized examples — some commands may not exist yet. Where something feels awkward or missing, that's a signal worth investigating.

## Starting from scratch

You have nothing on disk. Install repoweave (see [install options](../README.md)) and create a weave:

```bash
mkdir ~/weaveroot && cd ~/weaveroot
rwv fetch chatly/web-app
```

What happens:

1. Creates `projects/web-app/` by cloning `https://github.com/chatly/web-app.git`
2. Reads `projects/web-app/rwv.yaml` to get the repo list
3. Clones each repo to its canonical path: `github/chatly/server/`, `github/chatly/web/`, `github/chatly/protocol/`
4. Runs `rwv activate web-app` automatically:
   - Generates ecosystem files in `projects/web-app/` (package.json, go.work, etc.)
   - Creates symlinks at root pointing to them
5. Runs `rwv lock`, which triggers integration lock hooks (`npm install`, `uv sync`, etc.) and writes `rwv.lock`

Result:

```
~/weaveroot/
├── github/chatly/server/             # clone
├── github/chatly/web/                # clone
├── github/chatly/protocol/           # clone
├── projects/web-app/                 # project repo (clone)
│   ├── rwv.yaml
│   ├── rwv.lock
│   ├── package.json                  # real file
│   ├── package-lock.json             # real file
│   └── docs/
├── package.json -> projects/web-app/package.json
├── package-lock.json -> projects/web-app/package-lock.json
├── node_modules/
├── .rwv-active                       # "web-app"
└── .gitignore
```

You're ready to work:

```bash
cd github/chatly/server
git status
npm test --workspaces   # from root — cross-repo deps resolve
```

## Adding a second project

You also work on the mobile app. Some repos overlap.

```bash
cd ~/weaveroot
rwv fetch chatly/mobile-app
```

What happens:

1. Clones `projects/mobile-app/` from GitHub
2. Reads `projects/mobile-app/rwv.yaml`
3. Clones repos that aren't already on disk (e.g., `github/chatly/mobile/`, `github/nickel-io/push-sdk/`)
4. Repos already on disk (`github/chatly/server/`, `github/chatly/protocol/`) are left alone — no re-clone
5. Does NOT activate mobile-app — web-app is still the active project

```
~/weaveroot/
├── github/chatly/server/             # shared — used by both projects
├── github/chatly/web/                # web-app only
├── github/chatly/protocol/           # shared
├── github/chatly/mobile/             # mobile-app only (just cloned)
├── github/nickel-io/push-sdk/        # mobile-app only (just cloned)
├── projects/
│   ├── web-app/                      # active
│   └── mobile-app/                   # fetched but not active
├── package.json -> projects/web-app/package.json   # still web-app
└── ...
```

The new repos are on disk but not in the ecosystem workspace yet — they're not in web-app's `rwv.yaml`, so `package.json` doesn't list them.

## Switching projects

```bash
rwv activate mobile-app
```

What happens:

1. Generates ecosystem files in `projects/mobile-app/` (or updates them)
2. Removes old symlinks at root (web-app's package.json, etc.)
3. Creates new symlinks pointing to `projects/mobile-app/`
4. Updates `.rwv-active` to "mobile-app"

```
~/weaveroot/
├── package.json -> projects/mobile-app/package.json   # switched
├── package-lock.json -> projects/mobile-app/package-lock.json
├── .rwv-active                       # "mobile-app"
└── ...
```

Now `npm test --workspaces` runs mobile-app's packages. `github/chatly/web/` is still on disk but not in the ecosystem workspace — it's not in mobile-app's `rwv.yaml`.

Switch back:

```bash
rwv activate web-app
npm install   # reconcile node_modules/ with web-app's dependency tree; fast if nothing changed
```

`rwv activate` only swaps the symlinks and regenerates config files. Run `npm install` (or `rwv lock`) separately to reconcile tool state.

Tool state (`node_modules/`, `.venv/`, `target/`) is shared across activations. Switching projects means the ecosystem tool has to reconcile — `npm install` is incremental and fast for small dep diffs. For large dependency differences or when you need both projects active simultaneously, create a workweave for the second project instead of switching — each workweave has its own tool state.

## Adding a repo to a project

You need a new dependency:

```bash
cd ~/weaveroot
rwv add https://github.com/example/some-lib.git --role dependency
```

What happens:

1. Clones to `github/example/some-lib/`
2. Adds entry to the active project's `rwv.yaml` (web-app)
3. Re-runs integrations (e.g., adds the new repo to workspace config files like `Cargo.toml`, `go.work`, `package.json`)
4. Symlinks already point to projects/web-app/, so root sees the update

Run the ecosystem install command (or `rwv lock`) afterward to pick up the new package.

```bash
# Verify it's there
cat projects/web-app/rwv.yaml | grep some-lib
npm install
npm ls @example/some-lib
```

The other project (mobile-app) is unaffected — `some-lib` isn't in its `rwv.yaml`.

To add it to mobile-app too:

```bash
rwv activate mobile-app
rwv add https://github.com/example/some-lib.git --role dependency
# Clone is a no-op — already on disk. Just adds to rwv.yaml and regenerates.
```

`rwv add` always takes a URL. The manifest needs the URL for `rwv fetch` on other machines. If the repo is already cloned at the canonical path, the clone step is a fast no-op.

To create a brand new repo that doesn't exist yet:

```bash
rwv add github/chatly/auth --new
# git init at the canonical path
# URL inferred from path convention: https://github.com/chatly/auth.git
# Added to rwv.yaml with role: primary (the default for --new)
```

## Removing a repo

```bash
rwv remove github/example/some-lib
```

What happens:

1. Removes entry from the active project's `rwv.yaml`
2. Re-runs integrations
3. Clone stays on disk (other projects might use it; `rwv check` will report it as an orphan if no project references it)

Run the ecosystem install command (or `rwv lock`) afterward to reconcile tool state.

To also delete the clone:

```bash
rwv remove github/example/some-lib --delete
# Checks no other project references it, then removes the directory
```

To find and clean up orphaned clones that no project references:

```bash
rwv check   # reports orphans
```

## Locking versions

When you're ready to capture the current state for release or reproducibility:

```bash
rwv lock
```

Reads HEAD SHA from each repo in the active project, writes `projects/web-app/rwv.lock`:

```bash
cd projects/web-app
git diff rwv.lock   # see what changed
git add rwv.lock && git commit -m "lock: update after payment feature"
git push
```

To lock multiple projects, activate and lock each explicitly:

```bash
rwv activate web-app
rwv lock
rwv activate mobile-app
rwv lock
```

Then commit in each project repo:

```bash
cd projects/web-app && git add rwv.lock && git commit -m "lock: update server"
cd ../mobile-app && git add rwv.lock && git commit -m "lock: update server"
```

## Creating a workweave for isolation

Workweaves are worktree-based derivatives of the weave, created on demand. Use cases:

- **Feature branch** — work on a cross-repo feature without disturbing main
- **Parallel projects** — only one project can be active in the weave; a workweave lets you work on a second project with its own tool state
- **PR review** — check out a PR in isolation, run tests, delete when done
- **Agent isolation** — each agent gets its own branches and ecosystem files

A feature that spans server and protocol — you want to keep main undisturbed:

```bash
rwv workweave web-app payments
```

What happens:

1. Creates `.workweaves/payments/` under the weaveroot
2. For each repo in web-app's manifest, runs `git worktree add` with an ephemeral branch:
   - `github/chatly/server/` → worktree on `payments/main`
   - `github/chatly/web/` → worktree on `payments/feature-A` (tracks whatever branch the weave had)
   - `github/chatly/protocol/` → worktree on `payments/main`
3. Creates a worktree for the project repo too:
   - `projects/web-app/` → worktree on `payments/main`
4. Generates ecosystem files in the workweave's `projects/web-app/`
5. Creates symlinks at `.workweaves/payments/` root

```
~/weaveroot/                               # weave — undisturbed
.workweaves/payments/                 # workweave
├── github/chatly/server/             # worktree, on payments/main
├── github/chatly/web/                # worktree, on payments/feature-A
├── github/chatly/protocol/           # worktree, on payments/main
├── projects/web-app/                 # worktree, on payments/main
│   ├── rwv.yaml
│   ├── rwv.lock
│   └── package.json
├── package.json -> projects/web-app/package.json
├── node_modules/                     # isolated
└── .rwv-active                       # "web-app"
```

Work in the workweave:

```bash
cd .workweaves/payments
cd github/chatly/server
git checkout -b feature/payments      # or: already on payments/main, start working
# ... make changes, commit ...
cd ../protocol
# ... make changes, commit ...
npm test --workspaces                 # from workweave root — isolated deps
```

Meanwhile, the weave is untouched:

```bash
cd ~/weaveroot
git -C github/chatly/server status    # still on main, clean
npm test --workspaces                 # weave's deps, weave's branches
```

## Creating a workweave for PR review

A colleague opened a PR against server. You want to review it without disrupting your work.

```bash
rwv workweave web-app review-pr-42
cd .workweaves/review-pr-42/github/chatly/server
git fetch origin pull/42/head:pr-42
git checkout pr-42
npm test --workspaces
# read code, run tests, leave comments
```

When done:

```bash
rwv workweave web-app review-pr-42 --delete
```

Worktrees removed, ephemeral branches cleaned up, directory deleted.

## Agent isolation

An orchestrator spins up a workweave for an agent:

```bash
rwv workweave web-app agent-task-99
# Launch agent with CWD at .workweaves/agent-task-99/github/chatly/server/
```

The agent works in full isolation — its own branches, its own `node_modules/`, its own ecosystem resolution. It can commit, push, run tests without affecting anything.

When the agent finishes:

```bash
# Review what the agent did
cd .workweaves/agent-task-99/github/chatly/server
git log --oneline agent-task-99/main..HEAD

# Merge into main if good
cd ~/weaveroot/github/chatly/server
git merge agent-task-99/main

# Clean up
rwv workweave web-app agent-task-99 --delete
```

## Checking project health

```bash
rwv check
```

Output:

```
web-app:
  ✓ 4 repos on disk, 4 in manifest
  ✓ rwv.lock matches HEAD SHAs
  ✓ package.json up to date
  ⚠ github/chatly/web: 3 commits ahead of locked SHA

mobile-app:
  ✓ 4 repos on disk, 4 in manifest
  ✗ rwv.lock stale: github/chatly/server HEAD differs
  ✓ package.json up to date

orphans:
  ⚠ github/example/old-experiment/ not in any project

workweaves:
  .workweaves/payments: web-app (3 repos)
  .workweaves/agent-task-99: web-app (3 repos, stale — 7 days old)
```

## Day-to-day flow

The typical cycle, no workweaves needed:

```bash
cd ~/weaveroot

# Morning: pull latest
cd github/chatly/server && git pull
cd ../web && git pull
cd ../protocol && git pull
# Or with gita: gita super primary pull

# Work on server
cd ~/weaveroot/github/chatly/server
git checkout -b feature/new-endpoint
# ... edit, test, commit ...

# Test cross-repo
cd ~/weaveroot
npm test --workspaces

# Lock and push
rwv lock
cd projects/web-app
git add rwv.lock && git commit -m "lock: new endpoint"
git push

cd ~/weaveroot/github/chatly/server
git push origin feature/new-endpoint
```

No workweave created. No worktree indirection. Just repos, just git.

## Ecosystem file conflicts when switching projects

When switching projects with `rwv activate`, ecosystem files swap instantly (symlinks) but tool state (`node_modules/`, `.venv/`, `target/`) needs reconciliation. Run the ecosystem install command after switching — it's incremental, typically a few seconds for small dep diffs.

For large dependency differences or when reconciliation is too slow, create a workweave for the second project instead (see [Creating a workweave for isolation](#creating-a-workweave-for-isolation)).

## Initializing a new project from existing repos

You already have repos cloned. You want to create a project that groups them.

```bash
cd ~/weaveroot
rwv init web-app --provider github/chatly
# Creates projects/web-app/
# git init
# git remote add origin https://github.com/chatly/web-app.git
# Creates empty rwv.yaml

rwv add https://github.com/chatly/server.git --role primary
rwv add https://github.com/chatly/web.git --role primary
rwv add https://github.com/chatly/protocol.git --role primary
rwv add https://github.com/socketio/engine.io.git --role fork
```

`rwv init` creates the project and activates it. The `--provider` flag is optional; it uses the registry mapping (`github` → `github.com`) and the project name to set up the remote as a convenience. Without `--provider`, no remote is configured — you add it later when ready.

## Adding a repo for reference

You want to read another team's code alongside yours:

```bash
rwv add https://github.com/interesting/library.git --role reference
```

This clones to the canonical path, adds it to the active project's `rwv.yaml` as a reference repo, and re-runs integrations. Reference repos are visible in the workspace but excluded from build graphs (not listed in workspace config files).

`rwv check` reports repos on disk that aren't in any project's manifest as orphans — prefer `rwv add --role reference` over manual `git clone` so repos are tracked.

## Reproducing a project on a new machine

Your colleague wants to work on web-app:

```bash
mkdir ~/weaveroot && cd ~/weaveroot
rwv fetch chatly/web-app
```

This clones the project repo, reads `rwv.yaml`, clones all listed repos, activates (generates ecosystem files and symlinks), and runs `rwv lock` (which triggers integration lock hooks like `npm install`, `uv sync`, etc. and writes `rwv.lock`). One command from zero to working.

This fetches latest (branch HEAD from `rwv.yaml`) and updates `rwv.lock` with the SHAs that were actually checked out — same as how `npm install` resolves latest and updates `package-lock.json`.

If they want the exact versions you had:

```bash
rwv fetch chatly/web-app --locked
```

This checks out each repo at the revision in `rwv.lock` instead of the branch in `rwv.yaml`. Deterministic reproduction.

For CI, `--frozen` is stricter — errors if the lock file is missing or doesn't match the manifest:

```bash
rwv fetch chatly/web-app --frozen
```

## Working on the project repo itself

The project repo contains `rwv.yaml`, `rwv.lock`, docs, and now ecosystem files. Sometimes you need to edit the manifest directly:

```bash
cd ~/weaveroot/projects/web-app
vim rwv.yaml                          # add a repo, change a role
cd ~/weaveroot
rwv activate web-app                  # regenerate ecosystem files from updated manifest
```

Or edit cross-repo docs:

```bash
cd ~/weaveroot/projects/web-app/docs
vim architecture.md
git add . && git commit -m "docs: update architecture after payments refactor"
git push
```

The project repo is a normal git repo. You commit to it, push it, branch it.

## Releasing: the lock file as handoff

During development, ecosystem workspace wiring (`go.work`, Cargo workspaces, npm workspaces) eliminates the version bump dance — cross-repo imports resolve locally, no publishing needed. At release time, the dance returns: downstream repos need version pins to published upstream artifacts.

The lock file is the handoff point. `rwv lock` captures the exact SHAs that were tested together. Your release process — whatever it is — consumes this state and produces tagged, publishable artifacts.

### The pattern

1. Develop with workspace wiring — no version bumps, just edit and test
2. When ready: `rwv lock` — captures the tested state
3. Release repos in dependency order (leaf nodes first)
4. Update downstream version pins to freshly published versions
5. The lock file records which versions were tested together

The dependency order comes from the ecosystem manifests: `go.mod` imports, `Cargo.toml` path dependencies, `package.json` workspace references. repoweave doesn't need to understand these — the ecosystem tools already do.

### Per-ecosystem recipes

**Go** (e.g., `server` depends on `protocol`):

```bash
# Development: go.work resolves protocol locally, no version pin needed
rwv lock                                        # capture tested state
cd github/chatly/protocol
git tag v1.5.0 && git push origin v1.5.0        # release protocol first
cd ../server
go get github.com/chatly/protocol@v1.5.0        # update go.mod pin
git tag v2.2.0 && git push origin v2.2.0        # release server
```

**Cargo** (e.g., `server` depends on `protocol`):

```bash
rwv lock
cd github/chatly/protocol
cargo publish                                    # publish to crates.io
cd ../server
# update Cargo.toml: protocol = "1.5.0" (or path dep → version dep)
cargo publish
```

**Node** (npm/pnpm workspaces):

```bash
rwv lock
cd github/chatly/shared-types
npm version 1.3.0 && npm publish
cd ../server
npm install @chatly/shared-types@1.3.0           # update package.json pin
npm version 2.1.0 && npm publish
```

**Internal only** (no publishing — the most common case):

```bash
rwv lock
cd projects/web-app
git add rwv.lock && git commit -m "lock: release candidate"
git push
```

The lock file IS the version. `sha256sum rwv.lock` is the project fingerprint. Two machines with the same lock checksum have identical source for every repo.

### Why this matters

In a single-ecosystem project, the dependency graph is visible to the ecosystem tool and the release sequence is obvious. In a multi-ecosystem project — a Go service importing protobufs that are also used by a TypeScript frontend — no single ecosystem tool sees the full picture. The lock file is the only artifact that captures the cross-ecosystem dependency state. It generalizes to any DAG of inter-repo dependencies, regardless of which ecosystems are involved.

## Design decisions (resolved)

1. **Shared tool state across project switches** — acceptable cost. `npm install` after `rwv activate` is incremental. If the cost is too high, create a workweave for the second project — natural escalation from activate to workweave.

2. **`rwv add` takes a URL** — always. The manifest needs URLs for `rwv fetch` on other machines. If the repo is already on disk, clone is a no-op. For new repos that don't exist yet, `rwv add <path> --new` initializes at the canonical path and infers the URL.

3. **No `lock-all`** — dropped. Each project needs `activate` first (to generate ecosystem files), making lock-all a heavyweight operation that's better done explicitly: `rwv activate web-app && rwv lock`, repeat per project. `rwv lock` does not auto-commit — following ecosystem tool convention (npm, cargo, uv none auto-commit). Lock writes `rwv.lock` and runs integration lock hooks.

4. **`rwv init` is explicit** — like `git init`, a one-time setup. Optional `--provider github/chatly` uses registry mapping to set up the remote as a convenience.

5. **`rwv fetch` updates the lock** — fetches at branch HEAD from `rwv.yaml` and updates `rwv.lock` with actual SHAs (like `npm install` updates `package-lock.json`). `--locked` checks out exact revisions from the lock. `--frozen` errors if lock is missing or stale (CI mode). `--latest` ignores the lock entirely.

6. **Workweave location** — workweaves live in `.workweaves/{name}` under the weaveroot. The `.rwv-workweave` marker file records the weave path and project, making the relationship explicit and independent of directory naming.
