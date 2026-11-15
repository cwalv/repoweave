# Tutorial

A first session with repoweave: join a project, take in a teammate's work, change two repos in a workweave, land the change, publish it, and recover when a landing stops. One path, no choice points. For task-shaped recipes, see the [how-to guides](./how-to/index.md).

> **Note:** `chatly/web-app` is a fictional organization used across these docs. To run every command on your machine against disposable local repositories, run the setup block below first. It redirects `HOME`, so your own git configuration stays untouched, and it points `https://github.com/chatly/` at local bare repositories.

<details>
<summary>Run this tutorial locally with disposable repositories</summary>

```bash
export SANDBOX=$(mktemp -d) && export HOME=$SANDBOX/home && mkdir -p $HOME
git config --global user.name Tutorial && git config --global user.email tutorial@example.com
git config --global init.defaultBranch main && git config --global protocol.file.allow always
git config --global url."file://$SANDBOX/remotes/chatly/".insteadOf https://github.com/chatly/
for repo in protocol server web web-app; do git init -q --bare $SANDBOX/remotes/chatly/$repo.git; done
for repo in protocol server web; do git clone -q https://github.com/chatly/$repo.git $SANDBOX/work-$repo 2>/dev/null; git -C $SANDBOX/work-$repo commit -q --allow-empty -m "initial $repo"; git -C $SANDBOX/work-$repo push -q origin main; done
mkdir $SANDBOX/seed && cd $SANDBOX/seed && rwv init web-app
rwv member set-url projects/web-app https://github.com/chatly/web-app.git
for repo in protocol server web; do rwv member add https://github.com/chatly/$repo.git; done
rwv push && cd $SANDBOX
```

The commands below run unchanged against it. The repositories hold no build files, so skip the build command in step 3.

</details>

## 1. Clone the project

You have a fresh machine and the project already exists:

```bash
mkdir ~/work && cd ~/work
rwv clone github/chatly/web-app
```

`rwv clone` clones `projects/web-app/` from `https://github.com/chatly/web-app.git`, reads its `rwv.toml` for the member list, clones each member to its canonical path (`github/chatly/server/`, `github/chatly/web/`, `github/chatly/protocol/`) at the commit `rwv.lock` pins, activates `web-app`, and runs the integration install hooks.

```
~/work/
├── github/chatly/server/             # ordinary clone
├── github/chatly/web/                # ordinary clone
├── github/chatly/protocol/           # ordinary clone
├── projects/web-app/                 # project repo
│   ├── rwv.toml                      # which repos, with what roles
│   ├── rwv.lock                      # the commit pinned for each
│   └── web-app.code-workspace        # the editor workspace, regenerated from rwv.toml
└── .rwv-active                       # "web-app"
```

The exit status says whether the state reproduced: `0` means every member sits at its pin, and `3` means a member is unpinned or its pinned commit is unreachable. `rwv clone` never moves a pin and writes no `rwv.lock`. A repository that holds no `rwv.toml` is refused as not a project and left untouched; the refusal prints `rwv clone <source> --init-manifest`, which writes a skeleton `rwv.toml`, commits it, and clones as usual.

`rwv status` shows each member's branch, its tip, the commit the lock pins, and how the two relate:

```bash
rwv status
```

The project repo is on `main`, and `rwv status` says how `main` is used on its `record:` line: `main: declared`. A **declared** branch is one whose members follow the branches `rwv.toml` names for them. The other mode is a **feature**, a branch that lands into a target; the next steps use one.

## 2. Take in the team's work

While you were setting up, a teammate landed a change and published it. `rwv pull` brings it in:

```bash
rwv pull
```

`rwv pull` fetches, fast-forwards the project branch to its upstream, and then aligns every member to the pins the pulled lock holds. It reports what it took in. A project branch that diverged from its upstream is refused unless `--merge-upstream` merges it or `--reset-to-upstream` moves it to its upstream. `rwv fetch` only downloads: it moves no branch, checkout, or pin.

## 3. Change two repos in a workweave

Work happens in a **workweave**: an isolated copy of the whole workspace, with its own checkouts, its own `node_modules`, `.venv` and `target`, and its own branch. Your primary weave stays as it is.

```bash
rwv workweave create welcome-message
cd ../.workweaves/web-app--welcome-message
```

The workweave sits beside the weave root, at `<parent>/.workweaves/<project>--<name>/`. Its project repo is on a feature branch named `web-app--welcome-message` that lands into `main`, and each `owned` or `fork` member is on a branch of the same name. Commit in each member as you always do:

```bash
cd github/chatly/protocol
echo "welcome: string" > welcome.txt && git add -A && git commit -m "protocol: add the welcome field"
cd ../server
echo "serve the welcome" > welcome.txt && git add -A && git commit -m "server: add the welcome message"
cd ../../..
```

Ecosystem workspace wiring means a change in `protocol` is visible to `server` without a publish step: `cargo test --workspace`, `npm test --workspaces` or `go test ./...` from the workweave root builds the local clones.

List what will land, per repo, beyond what `main` pins:

```bash
rwv log --against main
```

## 4. Take in newer work, then land

Meanwhile the teammate published again. Pull it into the primary weave, and have `main` say how it accepts landings that need more than a fast-forward; that setting is recorded once, in the primary weave:

```bash
(cd ~/work && rwv pull && rwv branch set-history linear)
```

`linear` replays your commits on top of the target's; `merge` joins them with a merge commit; `ff-only` refuses unless the target's tip is already in your feature. No setting is assumed: a landing that needs one refuses until a branch records it, and `rwv land --history linear` supplies it for one run.

`rwv sync` takes that new work into your feature:

```bash
rwv sync
```

`rwv sync` takes in the branch your feature lands into, each member first, then the project branch, then the relock. Only your workweave changes. Then land it and retire the workweave:

```bash
rwv land --retire
cd ~/work
```

`rwv land` composes the workweave's commits onto the target, records every member's tip in the landing's one project commit, fast-forwards the target, and `--retire` deletes the workweave once everything landed. Where something did not land the workweave is kept or a branch survives, and the exit status is `3`. Your shell is left in a directory that no longer exists, hence the `cd`.

## 5. Publish

Landing moves `main` in your weave only. `rwv push` publishes it:

```bash
rwv push
```

`rwv push` pushes every `owned` and `fork` member first, then the branch's record, and the project repo last, so no teammate pulls an `rwv.lock` that names a commit nobody published.

## 6. When a landing stops

Two workweaves edit the same line of the same file:

```bash
rwv workweave create alpha && rwv workweave create beta
echo alpha > ../.workweaves/web-app--alpha/github/chatly/server/shared.txt && git -C ../.workweaves/web-app--alpha/github/chatly/server add -A && git -C ../.workweaves/web-app--alpha/github/chatly/server commit -qm "server: alpha"
echo beta > ../.workweaves/web-app--beta/github/chatly/server/shared.txt && git -C ../.workweaves/web-app--beta/github/chatly/server add -A && git -C ../.workweaves/web-app--beta/github/chatly/server commit -qm "server: beta"
(cd ../.workweaves/web-app--alpha && rwv land --retire)
```

The second landing meets the first one's change and stops on the conflict:

```bash
cd ../.workweaves/web-app--beta
rwv land
```

`rwv land` keeps the workweave, records the operation, and names the repository and the file. `rwv status` opens with the operation and the two commands that end it. Resolve the file in the named repository, mark it resolved, and run `rwv land --continue`; it resumes where it stopped and skips what already converged. Or take everything back:

```bash
rwv abort
```

`rwv abort` restores every repository either workspace touched to where it stood before the landing began. It keeps anything it discards, your resolutions included, as a backup, so nothing is lost: `rwv backup list` shows them. From the weave root, `rwv workweave delete beta` removes the workweave. Its branch holds a commit that has not landed, so the branch stays, `rwv workweave delete` lists it, and the exit status is `3`; `rwv branch delete web-app--beta --discard-unlanded` deletes it, keeping its tip as a backup first.

## 7. Pins and drift

A pin is where the lock says a member belongs; it is not a checkout. A member committed past its pin shows as `ahead` in `rwv status` until a lock records it, and `rwv land` records it for the workweave it lands. Three verbs move pins:

| Verb | What it does |
|---|---|
| `rwv update` | Clone missing members and align each to its pin. Writes no lock |
| `rwv update --remote` | Advance each member to its branch tip on the remote and record the tips in the lock |
| `rwv member pin <repo> <rev>` | Pin one member at a commit you choose, forward or backward |

`rwv doctor` checks the weave for drift: orphaned or dangling files, a checkout that disagrees with its pin, a missing ecosystem tool, an operation left half-done. `rwv doctor --fix` repairs what a verb can repair, and every finding names its kind in [Doctor findings](./reference/doctor-findings.md).

```bash
rwv doctor
```

`sha256sum projects/web-app/rwv.lock` is the project fingerprint: two machines with the same checksum pin the same commit in every member.

## Where to go next

- **Branches and release lines:** [maintain a release line](./how-to/maintain-a-release-line.md)
- **Cross-repo features and landing:** [create a feature workweave](./how-to/create-feature-workweave.md), [bring workweave work home](./how-to/bring-workweave-work-home.md)
- **Stops:** [recover from a sync conflict](./how-to/recover-from-sync-conflict.md), [resume or abort a mid-op sync](./how-to/resume-or-abort-mid-op-sync.md)
- **Review someone else's PR:** [review a PR with a workweave](./how-to/review-pr-with-workweave.md)
- **Hand a task to an agent:** [hand task to agent](./how-to/hand-task-to-agent.md)

For the model behind the tool, start with the [introduction](./introduction.md) and the three lenses ([workspace](./explanation/lenses/workspace.md), [monorepo](./explanation/lenses/monorepo.md), [agent](./explanation/lenses/agent.md)).
