# Tutorial

A first walkthrough of repoweave: join a project, make a change across repos, lock the state. Single path, no choice points. For task-shaped recipes (workweaves, sync, release, agent handoff), see the [how-to guides](./how-to/index.md).

This tutorial uses a fictional chat product with repos under `github/chatly/`.

## 1. Fetch the project

You have a fresh machine. Someone else already created the project. You want to reproduce their environment:

```bash
mkdir ~/work && cd ~/work
rwv fetch chatly/web-app
```

What happens:

1. Clones `projects/web-app/` from `https://github.com/chatly/web-app.git`.
2. Reads `projects/web-app/rwv.yaml` to get the repo list.
3. Clones each repo to its canonical path: `github/chatly/server/`, `github/chatly/web/`, `github/chatly/protocol/`.
4. Activates `web-app` — generates ecosystem workspace files in `projects/web-app/` and symlinks them to `~/work/`.
5. Writes `~/work/.rwv-active` containing `web-app`.

The result:

```
~/work/
├── github/chatly/server/             # regular clone
├── github/chatly/web/                # regular clone
├── github/chatly/protocol/           # regular clone
├── projects/web-app/                 # project repo (clone)
│   ├── rwv.yaml
│   ├── rwv.lock
│   └── Cargo.toml                    # generated workspace file
├── Cargo.toml -> projects/web-app/Cargo.toml   # symlink
├── .rwv-active                       # "web-app"
└── .gitignore
```

`rwv fetch` reads `rwv.lock` and aligns clones to it — it does *not* advance to upstream branch tips. The lock is read-only by default. To advance: `rwv update`.

## 2. Get the latest

```bash
rwv update
```

**`rwv update` is the verb that gets the latest.** It advances each manifest repo to the branch HEAD on the remote, and re-snapshots `rwv.lock`. Equivalent in spirit to `cargo update` or `npm update` — pull the new world, re-pin.

Compare:

| Verb | What it does | Mutates lock? |
|---|---|---|
| `rwv fetch` | Read `rwv.lock`, align clones to it | No |
| `rwv update` | Advance to branch HEADs, re-snapshot lock | Yes |
| `rwv lock` | Snapshot current local state, no network | Yes |

If you forget which to run: `update` to get the latest, `fetch` to reproduce a state, `lock` to checkpoint local state.

## 3. Make a change

You can edit any repo right where it lives — `github/chatly/server/` is a regular git clone:

```bash
cd github/chatly/server
git checkout -b feat/welcome-message
# ... edit code ...
git commit -am "server: add welcome message"
```

Ecosystem workspace wiring means a change in `github/chatly/protocol` is immediately visible to `github/chatly/server` without a publish step:

```bash
cd ~/work
cargo test --workspace             # or: npm test --workspaces, go test ./...
```

The cross-repo build resolves imports to the local worktrees, not to a registry. Edit, test, iterate — no version dance.

When you're done, push the branch:

```bash
cd github/chatly/server
git push origin feat/welcome-message
```

## 4. Lock the state

To checkpoint the entire cross-repo state (every repo's tip, plus ecosystem lock files):

```bash
cd ~/work
rwv lock
```

`rwv lock` reads HEAD from each repo and writes `projects/web-app/rwv.lock`. If a tag exists at HEAD, the lock records the tag name; otherwise the revision ID. The lock file is committable in the project repo:

```bash
cd projects/web-app
git add rwv.lock
git commit -m "lock: welcome-message feature"
git push
```

`sha256sum rwv.lock` is the project fingerprint — two machines with the same checksum have identical source for every repo in the project.

Anyone else can now reproduce your exact state:

```bash
rwv fetch chatly/web-app --frozen
```

`--frozen` errors if the lock is stale (suitable for CI). Without `--frozen`, `rwv fetch` aligns each repo to the SHA recorded in `rwv.lock` and proceeds; staleness is reported by `rwv doctor`.

## Where to go next

You've seen the day-to-day path: fetch, update, edit, lock. Common follow-ups have their own how-tos:

- **Cross-repo branches and isolation:** [create a feature workweave](./how-to/create-feature-workweave.md)
- **Review someone else's PR locally:** [review a PR with a workweave](./how-to/review-pr-with-workweave.md)
- **Add a repo, including brownfield adoption:** [add a repo](./how-to/add-a-repo.md)
- **Release a package:** [release a package](./how-to/release-a-package.md)
- **Coordinated push across repos:** [push a cross-repo feature](./how-to/push-cross-repo-feature.md)
- **Switch the active project:** [switch projects](./how-to/switch-projects.md)
- **Hand a task to an agent:** [hand task to agent](./how-to/hand-task-to-agent.md)

For the model behind the tool, start with the [introduction](./introduction.md) and the three lenses ([workspace](./explanation/lenses/workspace.md), [monorepo](./explanation/lenses/monorepo.md), [agent](./explanation/lenses/agent.md)). For lookup-shaped material, see [reference/cli](./reference/cli.md).
