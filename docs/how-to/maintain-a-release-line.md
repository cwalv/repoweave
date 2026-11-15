# Maintain a release line

Ship fixes to a released version while `main` moves on. A release line is a project branch, here `stable`, recorded `declared`, whose entries name a branch of their own for each member that must not follow `main`. Every step below is an ordinary verb used in the ordinary way; rwv has no separate notion of a line.

The commands run in the weave from the [tutorial](../tutorial.md), at `~/work`: project `web-app`, members `protocol`, `server` and `web`, on `main`.

## Cut the line

```bash
rwv branch create stable --declared --set-new-branch github/chatly/server=v1 --set-new-branch github/chatly/protocol=protov1
rwv push
```

One commit on `stable` declares both branches in `rwv.toml`, and each member's branch is created at its pin. `web` is left on `main`'s declared branch and follows `main` by design. `--set-branch <member>=<branch>` declares a branch that already exists, `--pin <member>=<rev>` starts a new one at an older commit, and `--at <rev>` starts `stable` itself from an older project commit such as a release tag. The cut carries `main`'s `history` setting; `rwv branch set-history merge` changes it for `stable` if hotfixes should land by a merge commit.

`rwv push` publishes the member branches, the record that says `stable` is `declared`, and the project branch. It also reports each member entry the cut changed as `member-changed`: a declared entry is published with the branch, so those reports are informational.

## Switch between lines

```bash
rwv switch main
rwv switch stable
```

`rwv switch` puts the project repo on the branch and aligns every member to it. It runs bare on every machine once `stable`'s record is published: a clone reopens the line without being told its mode.

## Land a hotfix

```bash
rwv workweave create hotfix --new-branch hotfix1 --target stable
cd ../.workweaves/web-app--hotfix
```

The workweave starts at `stable`'s tip and pins, and `rwv workweave create` reports where it lands: `hotfix1 lands into each branch stable declares`, so `v1` for `server`. Fix, commit, and land:

```bash
cd github/chatly/server
echo "fixed" > hotfix.txt && git add -A && git commit -m "server: fix the welcome message"
cd ../../..
rwv land --retire
cd ~/work
```

`rwv land` fast-forwards `v1` and `stable`, and `--retire` deletes the workweave, and the last line names where to continue. Publishing is a separate step; `--branch` publishes a branch that is not the one checked out:

```bash
rwv push --branch stable
```

`rwv push` publishes the members' branches before the project branch, so the line never names a commit nobody can fetch.

To review the fix as a pull request first, record the lock in the workweave and publish it before landing: `rwv lock --commit`, then `rwv push` (a push before the lock pins what the member now holds is refused with `pin unpinned`, which prints that same `rwv lock --commit`) publishes `hotfix1` for `server` (a member pull request into `v1`) and for the project repo (a project pull request into `stable`). A landing then needs only `rwv land --retire` and `rwv push --branch stable`, or the forge's merge followed by `rwv pull`.

A member the line leaves on `main` is not part of the hotfix, and a landing names every change it makes to the target's member entries. To change one, declare a branch for it in the hotfix workweave first (`rwv member set-branch github/chatly/web webv1 --new`) and land with `rwv land --allow-member-change github/chatly/web`.

## Port a fix between lines

```bash
rwv switch main
rwv workweave create port --new-branch port-fix --target main
cd ../.workweaves/web-app--port
rwv cherry-pick stable
rwv land --retire
cd ~/work
rwv push
```

Here `stable` names the project commit that landed the hotfix. `rwv cherry-pick <rev>` copies one project commit onto the feature: each member's change, each dependency's pin, and the project change with its relock. Port several commits oldest first, one `rwv cherry-pick` each; `-m <n>` names the parent of a merge commit. A change to a member entry, or to `history`, is not ported: the pick reports it, and the change is made where the line should take it and named at landing. `rwv abort` and `rwv cherry-pick --continue` end a pick that stopped.

## Take a newer member into the line

To bring `main`'s newer `server` into `v1`, switch back to the line and merge `main` in a workweave on it:

```bash
rwv switch stable
```

Then:

```bash
rwv fetch
rwv workweave create bump --new-branch bump-server --target stable
cd ../.workweaves/web-app--bump
git -C github/chatly/server merge --no-ff origin/main -m "server: take main"
rwv lock --commit
rwv land --retire
cd ~/work
rwv push --branch stable
```

The merge's first parent is the line's, so the landing passes the lineage rule. A range that holds a merge is never rebased: it lands by fast-forward or by a merge commit whatever `stable`'s `history` setting says.

## Retire the line

From `main`, a line that has shipped its last fix is deleted by the deletion rule, which keeps a branch whose published copy still holds commits `main` does not:

```bash
rwv branch delete stable --target main
```

The line is kept and listed (`open-head`, exit status `3`), and the listing prints the next step. To give the line up unlanded, record it as a feature, which makes it an ordinary branch, then delete it with its published copy. The delete keeps its tip as a backup first (`rwv backup list`):

```bash
rwv branch set-mode stable feature --target main
rwv branch delete stable --discard-unlanded --delete-remote-unlanded projects/web-app
```

The line's member branches, `v1` and `protov1`, stay: once `stable` is a feature they are ordinary branches, and a delete never touches a branch a live line still declares.

## Related

- [Create a feature workweave](./create-feature-workweave.md) and [bring workweave work home](./bring-workweave-work-home.md) — the workweave and landing verbs used throughout
- [Push a cross-repo feature](./push-cross-repo-feature.md) — what `rwv push` withholds and why
- [Recover from a sync conflict](./recover-from-sync-conflict.md) — when a landing or a pick stops
- [Release a package](./release-a-package.md) — publishing tags and registry releases from a line
