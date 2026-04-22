# Tutorial

A walkthrough of repoweave, from joining a project to releasing. Uses a fictional chat product with repos under `github/chatly/`.

## Joining an existing project

Someone already created the project. You want to reproduce their environment:

```bash
mkdir ~/work && cd ~/work
rwv fetch chatly/web-app
```

What happens:

1. Clones `projects/web-app/` from `https://github.com/chatly/web-app.git`
2. Reads `projects/web-app/rwv.yaml` to get the repo list
3. Clones each repo to its canonical path: `github/chatly/server/`, `github/chatly/web/`, `github/chatly/protocol/`
4. Runs `rwv activate web-app` — generates ecosystem workspace files and symlinks them to the weave directory
5. Writes `.rwv-active` with "web-app"

Result:

```
~/work/
├── github/chatly/server/             # clone
├── github/chatly/web/                # clone
├── github/chatly/protocol/           # clone
├── projects/web-app/                 # project repo (clone)
│   ├── rwv.yaml
│   ├── rwv.lock
│   ├── Cargo.toml                    # generated workspace file
│   └── docs/
├── Cargo.toml -> projects/web-app/Cargo.toml   # symlink
├── .rwv-active                       # "web-app"
└── .gitignore
```

You're ready to work. Ecosystem tools see the workspace files at the weave directory — `cargo test --workspace`, `npm test --workspaces`, `go test ./...` all work across repos.

For exact reproduction (same revisions your colleague had):

```bash
rwv fetch chatly/web-app --locked
```

For CI (errors if lock is stale):

```bash
rwv fetch chatly/web-app --frozen
```

## Day-to-day development

The typical cycle — no special tooling needed:

```bash
cd ~/work

# Pull latest across repos
gita super primary pull            # or: cd into each repo and git pull

# Work on a repo
cd github/chatly/server
git checkout -b feature/new-endpoint
# ... edit, test, commit ...

# Test across repos — workspace wiring resolves cross-repo imports
cd ~/work
cargo test --workspace             # or: npm test --workspaces, go test ./...

# Push your work
cd github/chatly/server
git push origin feature/new-endpoint
```

No workweave needed. No worktree indirection. Just repos, just git.

## Managing repos

### Adding a repo

```bash
rwv add https://github.com/example/some-lib.git --role dependency
```

This clones to `github/example/some-lib/`, adds it to the active project's `rwv.yaml`, and re-runs integrations (e.g., adds the repo to workspace config files). Run the ecosystem install command afterward to pick up the new package.

`rwv add` always takes a URL — the manifest needs it for `rwv fetch` on other machines. If the repo is already on disk, the clone step is a no-op.

To create a brand new repo:

```bash
rwv add github/chatly/auth --new
# git init, URL inferred from path convention, added as role: primary
```

### Adding a reference repo

```bash
rwv add https://github.com/interesting/library.git --role reference
```

Reference repos are visible in the workspace but excluded from build graphs. Use this instead of manual `git clone` so repos are tracked — `rwv doctor` reports untracked repos as orphans.

### Removing a repo

```bash
rwv remove github/example/some-lib
```

Removes from `rwv.yaml` and re-runs integrations. The clone stays on disk (other projects might use it). To also delete the clone:

```bash
rwv remove github/example/some-lib --delete
# Checks no other project references it, then removes the directory
```

## Creating a new project

You have repos on disk and want to create a project that groups them:

```bash
cd ~/work
rwv init web-app --provider github/chatly
```

This creates `projects/web-app/` with an empty `rwv.yaml`, initializes a git repo, sets up the remote, and activates the project. Then add repos:

```bash
rwv add https://github.com/chatly/server.git --role primary
rwv add https://github.com/chatly/web.git --role primary
rwv add https://github.com/chatly/protocol.git --role primary
rwv add https://github.com/socketio/engine.io.git --role fork
```

The `--provider` flag is optional — it uses the registry mapping (`github` → `github.com`) to set up the remote. Without it, no remote is configured.

### Working on the project repo

The project repo is a normal git repo containing `rwv.yaml`, `rwv.lock`, docs, and generated ecosystem files:

```bash
cd ~/work/projects/web-app
vim rwv.yaml                          # edit the manifest
cd ~/work
rwv activate web-app                  # regenerate from updated manifest
```

Cross-repo docs live here too:

```bash
cd projects/web-app/docs
vim architecture.md
git add . && git commit -m "docs: update architecture"
git push
```

## Multiple projects

### Fetching a second project

```bash
rwv fetch chatly/mobile-app
```

Clones the project repo and any repos not already on disk. Does NOT activate — the first project stays active. Shared repos (`server`, `protocol`) are left alone.

### Switching projects

```bash
rwv activate mobile-app
```

Swaps symlinks in the weave directory, regenerates ecosystem files. Tool state (`node_modules/`, `.venv/`, `target/`) needs reconciliation — run the ecosystem install command after switching. This is incremental and fast for small dep diffs.

For large dependency differences, or when you need both projects active simultaneously, use a workweave instead of switching:

```bash
rwv workweave mobile-app create dev
cd .workweaves/dev
# independent tool state, no reconciliation needed
```

## Workweaves

Workweaves are worktree-based derivatives of the weave, created on demand for isolation. Each workweave has its own branches, ecosystem files, and tool state. The weave is undisturbed.

```bash
rwv workweave web-app create payments
```

Creates `.workweaves/payments/` with a git worktree for each repo on an ephemeral branch, plus ecosystem workspace files:

```
.workweaves/payments/
├── github/chatly/server/             # worktree, on payments/main
├── github/chatly/web/                # worktree, on payments/feature-A
├── github/chatly/protocol/           # worktree, on payments/main
├── projects/web-app/                 # worktree
├── Cargo.toml -> projects/web-app/Cargo.toml
└── .rwv-active                       # "web-app"
```

Work in the workweave like you would in the weave — `cargo test --workspace`, `git commit`, `git push` all work. Changes don't affect the weave.

### Use cases

**Feature branch** spanning multiple repos:

```bash
rwv workweave web-app create payments
cd .workweaves/payments/github/chatly/server
# ... make changes across server and protocol, test, commit ...
```

**PR review** without disrupting your work:

```bash
rwv workweave web-app create review-pr-42
cd .workweaves/review-pr-42/github/chatly/server
git fetch origin pull/42/head:pr-42 && git checkout pr-42
cargo test --workspace
rwv workweave web-app delete review-pr-42    # clean up when done
```

**Agent isolation** — each agent gets its own workweave:

```bash
rwv workweave web-app create agent-task-99
# agent works in .workweaves/agent-task-99/
# ... agent runs rwv lock + commits project repo when done ...

# bring completed work home:
cd ~/work                             # primary weave
rwv sync agent-task-99
rwv workweave web-app delete agent-task-99
```

**Parallel projects** — work on two projects without switching:

```bash
# web-app is active in the weave
rwv workweave mobile-app create dev
cd .workweaves/dev
# mobile-app has its own tool state here
```

### Bringing workweave work home

`rwv sync` is a direction-neutral primitive that aligns one workspace with another's committed `rwv.lock`. Both workspaces must be locked — tip must match `rwv.lock` across all repos — before `sync` will run.

**Typical flow** — work finishes in the workweave, land it in primary:

```bash
# in workweave: capture the cross-repo state
cd .workweaves/payments
(commit work across repos)
rwv lock
git -C projects/web-app commit -am "lock: payments feature"

# from primary: bring the workweave's work home
cd ~/work
rwv sync payments              # fast-forwards primary's repos to workweave's lock
```

**Catching a workweave up to primary** — when primary has advanced (upstream merge, another workweave's sync):

```bash
# in primary, after whatever landed
rwv lock
git -C projects/web-app commit -am "lock: upstream merge"

# in the workweave that needs to catch up
cd .workweaves/payments
rwv sync primary               # same verb, opposite direction
```

**When fast-forward won't apply** — workweave and primary have both advanced independently:

```bash
# replay workweave's commits onto primary's tip
rwv sync primary --strategy rebase

# or create a merge commit joining both sides
rwv sync primary --strategy merge
```

**Conflict handling** — if rebase or merge hits a conflict, fix it in the affected repo, then:

```bash
rwv sync primary --strategy rebase   # re-run resumes; already-advanced repos are no-ops
```

To give up entirely and restore every repo to its pre-sync state:

```bash
rwv abort
```

### Cleanup

```bash
rwv workweave web-app delete payments
```

Removes worktrees, cleans up ephemeral branches, deletes the directory.

## Checking project health

### Lock freshness

Verify that every repo in a workspace is at its locked revision — a precondition for `rwv sync`:

```bash
rwv check --locked
```

Zero exit means all repos match their `rwv.lock` entries. Nonzero exit shows what drifted:

```
github/chatly/server: tip abc1234 ≠ lock e1f2a3b  ← run rwv lock to update
github/chatly/web:    ok
```

### Per-repo status

```bash
rwv status
```

Shows each repo's branch, tip, lock entry, and relation:

```
github/chatly/server   main   abc1234   lock: e1f2a3b   [2 commits ahead]
github/chatly/web      main   e1f2a3b   lock: e1f2a3b   [ok]
github/chatly/protocol main   7a3b2c1   lock: 7a3b2c1   [ok]
```

Add `--json` for machine-readable output.

### Full convention audit

```bash
rwv doctor
```

Reports:

```
web-app:
  ✓ 4 repos on disk, 4 in manifest
  ✓ rwv.lock matches HEAD revisions
  ⚠ github/chatly/web: 3 commits ahead of locked revision

mobile-app:
  ✓ 4 repos on disk, 4 in manifest
  ✗ rwv.lock stale: github/chatly/server HEAD differs

orphans:
  ⚠ github/example/old-experiment/ not in any project

workweaves:
  .workweaves/payments: web-app (3 repos)
  .workweaves/agent-task-99: web-app (3 repos, stale — 7 days old)
```

