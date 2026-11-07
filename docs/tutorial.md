# Tutorial

A first walkthrough of repoweave: join a project, make a change across repos, lock the state. Single path, no choice points. For task-shaped recipes (workweaves, sync, release, agent handoff), see the [how-to guides](./how-to/index.md).

> **Note:** `chatly/web-app` is a fictional organization used across these docs. To run every command on your machine with disposable local repositories, expand the setup block below first.

<details>
<summary>Run this tutorial locally with disposable bare repos</summary>

```bash
# Create bare "remote" repositories in /tmp/chatly-remotes
mkdir -p /tmp/chatly-remotes && cd /tmp/chatly-remotes
for repo in protocol server web; do
  git init --bare -b main "${repo}.git"
done
git init --bare -b main web-app.git

# Seed the member repos and the web-app project manifest in a temporary bootstrap weave
mkdir -p /tmp/chatly-seed && cd /tmp/chatly-seed
rwv init web-app
git -C projects/web-app remote add origin /tmp/chatly-remotes/web-app.git
for repo in protocol server web; do
  git clone "/tmp/chatly-remotes/${repo}.git" "/tmp/chatly-remotes/work-${repo}"
  git -C "/tmp/chatly-remotes/work-${repo}" commit --allow-empty -m "initial ${repo}"
  git -C "/tmp/chatly-remotes/work-${repo}" push -u origin main
  rwv member add "file:///tmp/chatly-remotes/${repo}.git"
done
git -C projects/web-app add -A && git -C projects/web-app commit -m "initial project"
git -C projects/web-app push -u origin main
rm -rf /tmp/chatly-seed
```

When running the steps below against this bootstrap, substitute `file:///tmp/chatly-remotes/web-app.git` for `https://github.com/chatly/web-app.git` (or `chatly/web-app`) in `rwv clone`, and `local/chatly-remotes/server` for `github/chatly/server`. The bootstrap repos hold no build files, so skip the build command in Step 3.

</details>

## 1. Clone the project

You have a fresh machine. Someone else already created the project. You want to reproduce their environment:

```bash
mkdir ~/work && cd ~/work
rwv clone chatly/web-app
```

What happens:

1. Clones `projects/web-app/` from `https://github.com/chatly/web-app.git`.
2. Reads `projects/web-app/rwv.toml` to get the repo list.
3. Clones each repo to its canonical path: `github/chatly/server/`, `github/chatly/web/`, `github/chatly/protocol/`, and checks out the commit recorded in `rwv.lock`.
4. Activates `web-app` — updates `~/work/.rwv-active`, verifies integration files in `projects/web-app/`, surfaces symlinks to `~/work/`, and runs integration install hooks.

The result:

```
~/work/
├── github/chatly/server/             # regular clone
├── github/chatly/web/                # regular clone
├── github/chatly/protocol/           # regular clone
├── projects/web-app/                 # project repo (clone)
│   ├── .gitattributes
│   ├── rwv.toml
│   ├── rwv.lock
│   └── web-app.code-workspace        # generated workspace file
├── web-app.code-workspace -> projects/web-app/web-app.code-workspace   # symlink
└── .rwv-active                       # "web-app"
```

`rwv clone` places each member at the commit `rwv.lock` pins and lists any member it cannot place — one the lock has no entry for, or whose pinned commit no ref in the clone reaches. Such a member is cloned at the tip of its declared branch, and `rwv clone` exits with status 3 instead of 0. It never advances a pin to an upstream branch tip, and it writes no `rwv.lock`; `rwv member pin` or `rwv update --remote` gives a member the lock does not cover its first pin. To advance existing repos to their remote branch tips: `rwv update --remote`.

## 2. Get the latest

```bash
rwv update --remote
```

**`rwv update --remote` is the form that gets the latest.** It advances each member repo to the branch HEAD configured in `rwv.toml` (`branch = "main"`) on the remote, and re-snapshots `rwv.lock`. Equivalent in spirit to `cargo update` or `npm update` — pull the new world, re-pin.

Compare:

| Verb | What it does | Mutates lock? |
|---|---|---|
| `rwv update` | Clone members missing on disk, align each present member to its pinned SHA | No — never advances a pin, and a member the lock does not cover is left where it is |
| `rwv update --remote` | Advance to tracked branch HEADs, record the advanced tips in the lock | Yes |
| `rwv lock` | Record current local HEADs over their pins, no network | Yes |

`rwv fetch` downloads every remote's new commits and moves no branch, checkout, or lock. When a teammate pushes a new `rwv.lock`, `rwv pull` takes in the project branch and then runs `rwv update`, so your members land on their new pins.

If you forget which to run: `update --remote` to get the latest, `update` to reproduce a pinned state, `lock` to checkpoint local state.

## 3. Make a change

You can edit any repo right where it lives — `github/chatly/server/` is a regular git clone checked out on the branch declared in `rwv.toml` (`main`):

```bash
cd github/chatly/server
# ... edit code ...
git add -A
git commit -m "server: add welcome message"
```

> **Tip — isolated feature branches:** Keep canonical clones in the primary weave on their tracked branch (`branch` in `rwv.toml`). When you want an isolated branch across one or more repos, create a workweave (`rwv workweave create welcome-message`) and land it back with `rwv sync-to --retire` — see [create a feature workweave](./how-to/create-feature-workweave.md).

Ecosystem workspace wiring means a change in `github/chatly/protocol` is immediately visible to `github/chatly/server` without a publish step:

```bash
cd ~/work
cargo test --workspace             # or: npm test --workspaces, go test ./...
```

The cross-repo build resolves imports to the local clones, not to a registry. Edit, test, iterate — no version dance.

## 4. Lock and push the state

To checkpoint the cross-repo git state into `projects/web-app/rwv.lock`:

```bash
cd ~/work
rwv lock --commit
```

`rwv lock` reads the current 40-character commit SHA from each member repo's `HEAD` and writes `projects/web-app/rwv.lock`, moving each pin forward only. `--commit` commits it in `projects/web-app/` in the same step, together with `rwv.toml` when that has uncommitted changes; without `--commit`, run `rwv lock` and then `git -C projects/web-app commit`. Every lock entry records the 40-character commit SHA `HEAD` points to — never a branch or tag name.

Now push the member repo commits and the updated project lock together:

```bash
rwv push
```

`rwv push` pushes every `owned` and `fork` member repo first, then the branch's record, and only pushes `projects/web-app/` after all member pushes succeed — so no teammate ever pulls a `rwv.lock` referencing an unpushed member commit. It refuses before pushing anything while `projects/web-app/` has uncommitted changes, or while a member repo holds a commit `rwv.lock` does not pin.

`sha256sum projects/web-app/rwv.lock` is the project fingerprint: two machines with the same checksum pin the same commit in every member repo, which is the state a fresh `rwv clone` reproduces. A pin is not a checkout — a member committed past its pin shows as `ahead` in `rwv status` until `rwv lock` records it.

Anyone else can now reproduce your exact state:

```bash
mkdir ~/work-ci && cd ~/work-ci
rwv clone chatly/web-app
```

The exit status tells a CI job whether the state reproduced: `0` means every member sits at its pin, and `3` means a member is unpinned (`rwv.lock` has no entry for it) or its pinned commit is unreachable. A CI job also runs `rwv doctor --kind tool-missing` after it: a missing ecosystem tool is only a warning to `rwv clone`, and this check exits non-zero for it. A checkout that does not stand at its pin is reported by `rwv doctor` as [`pin`](reference/doctor-findings.md#pin).

## Where to go next

You've seen the day-to-day path: clone, update, edit, lock. Common follow-ups have their own how-tos:

- **Cross-repo branches and isolation:** [create a feature workweave](./how-to/create-feature-workweave.md)
- **Review someone else's PR locally:** [review a PR with a workweave](./how-to/review-pr-with-workweave.md)
- **Add a repo, including brownfield adoption:** [add a repo](./how-to/add-a-repo.md)
- **Release a package:** [release a package](./how-to/release-a-package.md)
- **Coordinated push across repos:** [push a cross-repo feature](./how-to/push-cross-repo-feature.md)
- **Switch the active project:** [switch projects](./how-to/switch-projects.md)
- **Hand a task to an agent:** [hand task to agent](./how-to/hand-task-to-agent.md)

For the model behind the tool, start with the [introduction](./introduction.md) and the three lenses ([workspace](./explanation/lenses/workspace.md), [monorepo](./explanation/lenses/monorepo.md), [agent](./explanation/lenses/agent.md)). For lookup-shaped material, see [reference/cli](./reference/cli.md).
