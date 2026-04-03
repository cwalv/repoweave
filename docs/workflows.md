# Workflows

A walkthrough of common repoweave workflows. These are written as idealized examples — some commands may not exist yet. Where something feels awkward or missing, that's a signal worth investigating.

## Starting from scratch

You just installed repoweave. You have nothing on disk.

```bash
cargo install repoweave
mkdir ~/work && cd ~/work
rwv fetch chatly/web-app
```

What happens:

1. Creates `projects/web-app/` by cloning `https://github.com/chatly/web-app.git`
2. Reads `projects/web-app/rwv.yaml` to get the repo list
3. Clones each repo to its canonical path: `github/chatly/server/`, `github/chatly/web/`, `github/chatly/protocol/`
4. Runs `rwv activate web-app` automatically:
   - Generates ecosystem files in `projects/web-app/` (package.json, go.work, etc.)
   - Creates symlinks at root pointing to them
   - Runs `npm install` / `uv sync` / etc.

Result:

```
~/work/
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
cd ~/work
rwv fetch chatly/mobile-app
```

What happens:

1. Clones `projects/mobile-app/` from GitHub
2. Reads `projects/mobile-app/rwv.yaml`
3. Clones repos that aren't already on disk (e.g., `github/chatly/mobile/`, `github/nickel-io/push-sdk/`)
4. Repos already on disk (`github/chatly/server/`, `github/chatly/protocol/`) are left alone — no re-clone
5. Does NOT activate mobile-app — web-app is still the active project

```
~/work/
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
4. Runs install commands
5. Updates `.rwv-active` to "mobile-app"

```
~/work/
├── package.json -> projects/mobile-app/package.json   # switched
├── package-lock.json -> projects/mobile-app/package-lock.json
├── .rwv-active                       # "mobile-app"
└── ...
```

Now `npm test --workspaces` runs mobile-app's packages. `github/chatly/web/` is still on disk but not in the ecosystem workspace — it's not in mobile-app's `rwv.yaml`.

Switch back:

```bash
rwv activate web-app
```

Symlinks swap back. `npm install` runs (reconciles `node_modules/` with web-app's dependency tree). Fast if nothing changed.

**Open question:** `node_modules/` at root is shared across activations. Switching projects means `npm install` has to reconcile. This is the same cost as pre-workspace reporoot. Is it acceptable? Or should each project's `node_modules/` be separate? (That would mean node_modules inside the project directory, or namespaced somehow.)

## Adding a repo to a project

You need a new dependency:

```bash
cd ~/work
rwv add https://github.com/example/some-lib.git --role dependency
```

What happens:

1. Clones to `github/example/some-lib/`
2. Adds entry to the active project's `rwv.yaml` (web-app)
3. Regenerates ecosystem files in `projects/web-app/` (adds some-lib to package.json workspaces)
4. Symlinks already point to projects/web-app/, so root sees the update
5. Runs `npm install` to pick up the new package

```bash
# Verify it's there
cat projects/web-app/rwv.yaml | grep some-lib
npm ls @example/some-lib
```

The other project (mobile-app) is unaffected — `some-lib` isn't in its `rwv.yaml`.

To add it to mobile-app too:

```bash
rwv activate mobile-app
rwv add github/example/some-lib --role dependency
# No clone needed — already on disk. Just adds to rwv.yaml and regenerates.
```

**Open question:** `rwv add` with a local path (already-cloned repo) vs a URL. The first `add` uses a URL and clones. The second uses a path because the repo is already there. Should both forms work? The manifest entry needs a URL for `rwv fetch` on other machines. Maybe `rwv add <path>` infers the URL from `git remote get-url origin`?

## Removing a repo

```bash
rwv remove github/example/some-lib
```

What happens:

1. Removes entry from the active project's `rwv.yaml`
2. Regenerates ecosystem files
3. Runs install commands
4. Clone stays on disk (other projects might use it)

To also delete the clone:

```bash
rwv remove github/example/some-lib --delete
# Checks no other project references it, then removes the directory
```

## Locking versions

After a day of work, you want to pin the current state:

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

Lock all projects at once (useful when shared repos changed):

```bash
rwv lock-all
cd projects/web-app && git add rwv.lock && git commit -m "lock: update server"
cd ../mobile-app && git add rwv.lock && git commit -m "lock: update server"
```

**Open question:** `rwv lock-all` updates multiple project repos. Should it also commit? Or is that too opinionated? A `--commit` flag? Or a separate `rwv commit` that commits lock file changes in all project repos?

## Creating a weave for a feature branch

You're working on a feature that spans server and protocol. You want to keep main undisturbed.

```bash
rwv weave web-app payments
```

What happens:

1. Creates `~/work--payments/` as a sibling directory
2. For each repo in web-app's manifest, runs `git worktree add` with an ephemeral branch:
   - `github/chatly/server/` → worktree on `payments/main`
   - `github/chatly/web/` → worktree on `payments/feature-A` (tracks whatever branch the primary had)
   - `github/chatly/protocol/` → worktree on `payments/main`
3. Creates a worktree for the project repo too:
   - `projects/web-app/` → worktree on `payments/main`
4. Generates ecosystem files in the weave's `projects/web-app/`
5. Creates symlinks at `~/work--payments/` root
6. Runs `npm install`

```
~/work/                               # primary — undisturbed
~/work--payments/                     # weave
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

Work in the weave:

```bash
cd ~/work--payments
cd github/chatly/server
git checkout -b feature/payments      # or: already on payments/main, start working
# ... make changes, commit ...
cd ../protocol
# ... make changes, commit ...
npm test --workspaces                 # from weave root — isolated deps
```

Meanwhile, the primary is untouched:

```bash
cd ~/work
git -C github/chatly/server status    # still on main, clean
npm test --workspaces                 # primary's deps, primary's branches
```

## Creating a weave for PR review

A colleague opened a PR against server. You want to review it without disrupting your work.

```bash
rwv weave web-app review-pr-42
cd ~/work--review-pr-42/github/chatly/server
git fetch origin pull/42/head:pr-42
git checkout pr-42
npm test --workspaces
# read code, run tests, leave comments
```

When done:

```bash
rwv weave web-app review-pr-42 --delete
```

Worktrees removed, ephemeral branches cleaned up, directory deleted.

## Agent isolation

An orchestrator spins up a weave for an agent:

```bash
rwv weave web-app agent-task-99
# Launch agent with CWD at ~/work--agent-task-99/github/chatly/server/
```

The agent works in full isolation — its own branches, its own `node_modules/`, its own ecosystem resolution. It can commit, push, run tests without affecting anything.

When the agent finishes:

```bash
# Review what the agent did
cd ~/work--agent-task-99/github/chatly/server
git log --oneline agent-task-99/main..HEAD

# Merge into main if good
cd ~/work/github/chatly/server
git merge agent-task-99/main

# Clean up
rwv weave web-app agent-task-99 --delete
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

weaves:
  ~/work--payments: web-app (3 repos)
  ~/work--agent-task-99: web-app (3 repos, stale — 7 days old)
```

## Day-to-day flow

The typical cycle, no weaves needed:

```bash
cd ~/work

# Morning: pull latest
cd github/chatly/server && git pull
cd ../web && git pull
cd ../protocol && git pull
# Or with gita: gita super primary pull

# Work on server
cd ~/work/github/chatly/server
git checkout -b feature/new-endpoint
# ... edit, test, commit ...

# Test cross-repo
cd ~/work
npm test --workspaces

# Lock and push
rwv lock
cd projects/web-app
git add rwv.lock && git commit -m "lock: new endpoint"
git push

cd ~/work/github/chatly/server
git push origin feature/new-endpoint
```

No weave created. No worktree indirection. Just repos, just git.

## Ecosystem file conflicts when switching projects

A subtle issue: web-app and mobile-app both need `package.json` at root, but with different contents. When you `rwv activate mobile-app`, the symlink swaps, but `node_modules/` was installed for web-app's `package.json`.

```bash
rwv activate web-app
npm install                 # installs web-app deps into node_modules/
# ... work ...
rwv activate mobile-app
npm install                 # has to reconcile node_modules/ for mobile-app's deps
```

This is the same cost as any project switch. `npm install` is incremental — it adds/removes the diff. For large dependency trees this can take 10-30 seconds.

If this is too slow, create a weave for the second project:

```bash
rwv weave mobile-app dev
cd ~/work--dev
# This weave has its own node_modules/, no reconciliation needed
```

This is the natural escalation: start with activate (shared tool state, fast switching for small dep diffs), graduate to weaves when you need full isolation.

## Initializing a new project from existing repos

You already have repos cloned. You want to create a project that groups them.

```bash
cd ~/work
rwv init web-app
# Creates projects/web-app/ with an empty rwv.yaml
# Initializes it as a git repo

rwv add github/chatly/server --role primary
rwv add github/chatly/web --role primary
rwv add github/chatly/protocol --role primary
rwv add github/socketio/engine.io --role fork

# Or, if you want to add by URL (clones if not present):
rwv add https://github.com/chatly/server.git --role primary
```

**Open question:** `rwv init` is a new command — not in the current spec. It creates a project directory and initializes it as a git repo. Is this needed, or can `rwv add` create the project directory on first use? An explicit `init` is clearer.

## What about repos not in any project?

You clone something for a quick look:

```bash
cd ~/work
git clone https://github.com/interesting/library.git github/interesting/library
```

It's on disk at the canonical path. It's not in any project's `rwv.yaml`. It doesn't appear in ecosystem workspace files. `rwv check` reports it as an orphan (informational, not an error).

If you later want it in a project:

```bash
rwv add github/interesting/library --role reference
```

This adds it to the active project's `rwv.yaml` (inferring URL from the clone's origin remote) and regenerates ecosystem files.

## Reproducing a project on a new machine

Your colleague wants to work on web-app:

```bash
cargo install repoweave
mkdir ~/work && cd ~/work
rwv fetch chatly/web-app
```

This clones the project repo, reads `rwv.yaml`, clones all listed repos, activates, generates ecosystem files, runs install. One command from zero to working.

If they want the exact versions you had:

```bash
rwv fetch chatly/web-app --locked
```

This checks out each repo at the SHA in `rwv.lock` instead of the branch in `rwv.yaml`. Deterministic reproduction.

**Open question:** `--locked` flag — is this needed as a separate flag, or should `rwv fetch` always use the lock file when present? The lock file might be stale (committed weeks ago). Using `rwv.yaml` branches by default (latest) is friendlier for getting started; `--locked` is for CI or debugging a specific state.

## Working on the project repo itself

The project repo contains `rwv.yaml`, `rwv.lock`, docs, and now ecosystem files. Sometimes you need to edit the manifest directly:

```bash
cd ~/work/projects/web-app
vim rwv.yaml                          # add a repo, change a role
cd ~/work
rwv activate web-app                  # regenerate ecosystem files from updated manifest
```

Or edit cross-repo docs:

```bash
cd ~/work/projects/web-app/docs
vim architecture.md
git add . && git commit -m "docs: update architecture after payments refactor"
git push
```

The project repo is a normal git repo. You commit to it, push it, branch it.

## Summary of open questions

1. **Shared node_modules across project switches** — acceptable cost, or should tool state be per-project?
2. **rwv add with local path vs URL** — should both work? How does path-based add get the URL for the manifest?
3. **rwv lock-all and committing** — should lock-all also commit? A --commit flag? Separate rwv commit?
4. **rwv init** — needed as an explicit command, or can project creation be implicit on first add?
5. **rwv fetch --locked** — separate flag, or default behavior when lock file exists?
6. **Weave naming** — `~/work--payments/` uses `--` as separator. What if the primary directory name has dashes? Configurable separator?
