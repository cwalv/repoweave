# Branch model

repoweave has three places where a git branch can exist, and they exist for three distinct reasons. Conflating them — treating a canonical store's tracking branch like an ad-hoc feature branch, or treating a workweave's ephemeral branch like a lineage record — is the source of every multi-repo branching footgun.

| Layer | Where it lives on disk | What the branch is | Who depends on it |
|---|---|---|---|
| **L1 — Project repo** | `projects/<project>/` | A **channel** — it carries its own `rwv.lock` | [pyramid-of-stability](./pyramid-of-stability.md); `rwv push`'s canonical-branch gate |
| **L2 — Canonical member** | `<weave>/<repo_path>/` | A **tracking declaration** — what to advance toward | `rwv update`; `version = "<branch>"` in `rwv.toml` |
| **L3 — Workweave checkout** | `.workweaves/<project>--<workweave>/<repo_path>/` | A **disjoint commit target** (`<project>--<workweave>`) | git's one-worktree-per-branch rule; [clone-topology](./clone-topology.md) Invariant I3 |

Everything below relock — `rwv sync`, `rwv sync-to`, `rwv lock`, workweave retirement, `workweave delete`'s unmerged-commits check, `rwv workweave log`, and `rwv status`'s relation column — operates purely on commit SHAs and DAG ancestry, never on branch names.

## L1 — Project repo (`projects/<project>/`): a branch is a channel

The project repository's branch is the only branch in the weave that *carries* composition state. As [pyramid-of-stability](./pyramid-of-stability.md) states: a channel is a branch in the project repo, which carries an `rwv.lock`, which pins a set of 40-character member commit SHAs. Switching the project repository's branch (`git -C projects/web-app checkout stable && rwv fetch --detach-checkouts`) switches which set of member revisions the weave resolves to.

Because publishing a project lock updates the shared channel, `rwv push` requires the project repository in the primary weave to be attached to its remote default branch (`origin/HEAD`), and `rwv doctor` verifies project-repo branch attachment alongside member repositories.

## L2 — Canonical member clone (`<weave>/<repo_path>/`): a branch is a tracking declaration

Each member repository's `version = "<branch>"` field in `rwv.toml` names a branch on the remote:

> The manifest (`rwv.toml`) declares what to **track**; the lock (`rwv.lock`) records where you **are**.

`rwv update` resolves `version` on the remote (`origin/<branch>`) and fast-forwards the canonical clone's local counterpart branch (`<branch>`) to that tip. `rwv fetch` realigns an existing canonical clone by fast-forwarding that same local counterpart branch to the SHA pinned in `rwv.lock` (unless `--detach-checkouts` is passed).

Because `rwv update`, `rwv fetch`, and `rwv sync-to` all expect the canonical member clone to sit on the local counterpart of its declared `version` branch, checking out an ad-hoc feature branch directly inside the canonical member store (`git -C github/chatly/server checkout -b feat/foo`) works against the model:

1. `rwv doctor` reports [`branch-discipline`](../../reference/doctor-findings.md#branch-discipline) (`canonical-wrong-branch`).
2. `rwv update` and `rwv fetch` refuse to move a checkout that is attached to a branch other than `version`.
3. Running `rwv lock --commit && rwv push` while a canonical clone is on `feat/foo` pins unmerged feature-branch SHAs into `main`'s `rwv.lock`.

Canonical member clones stay attached to their declared tracking branch; isolated feature work happens in workweaves.

## L3 — Workweave checkout: a branch is a disjoint commit target

When `rwv workweave <project> create <name>` creates a workweave at `../.workweaves/<project>--<name>/`, every member checkout is a linked git worktree attached to a flat **ephemeral branch** named `<project>--<name>` (with `role = "reference"` tracked regular files set read-only).

The ephemeral branch exists for two mechanical reasons:

1. **Git requires a worktree on a branch to have its own branch.** By default, `git worktree add` refuses to check out a branch (such as `main`) that is already checked out in the canonical clone or another worktree.
2. **Disjoint branch names make DAG queries sound ([clone-topology](./clone-topology.md) Invariant I3).** When every workspace owns a distinct branch ref in the shared canonical object store, "is this workweave's tip an ancestor of its parent's tip?" has an unambiguous answer, and `rwv sync-to` can fast-forward the parent's branch without ref collisions.

Workweave branch names are creation-time commit targets, not lineage records. Parentage is recorded in `.rwv-workweave` (`parent`), not parsed out of the branch name. When a workweave is deleted (`rwv workweave <project> delete <name>`) or retired (`rwv sync-to --retire`), `rwv` deletes its `<project>--<name>` ephemeral branches after verifying that their commits are ancestor-merged into the parent workspace.

## Recommended feature and PR workflows

Because canonical clones stay on their tracked branch (`main`) and workweaves hold isolated `<project>--<name>` branches, multi-repo feature development follows one of two paths depending on whether your team pushes directly to `main` or gates changes through forge pull requests.

### Direct-to-main workflow (`rwv sync-to --retire` + `rwv push`)

When you can land directly onto the tracked branch:

```bash
# 1. Create an isolated workweave
rwv workweave web-app create feat-auth
cd ../.workweaves/web-app--feat-auth

# 2. Edit and commit across member repos (on web-app--feat-auth branches)
git -C github/chatly/server commit -am "Add token verifier"
git -C github/chatly/client commit -am "Send bearer token"

# 3. Land into the primary weave (fast-forwards main, auto-relocks and commits rwv.lock, retires workweave)
rwv sync-to --retire
cd ../../weave

# 4. Push owned/fork repos + project repo
rwv push
```

### Pull-request workflow (push from workweave, `rwv update` after merge)

When `main` is branch-protected and changes must land through pull requests:

```bash
# 1. Create an isolated workweave and commit across member repos
rwv workweave web-app create feat-auth
cd ../.workweaves/web-app--feat-auth
git -C github/chatly/server commit -am "Add token verifier"

# 2. Push the workweave's HEAD to a remote feature branch and open PRs
git -C github/chatly/server push -u origin HEAD:refs/heads/feat/auth
gh -R chatly/server pr create --head feat/auth --base main

# 3. Once the PRs merge into main on the remote, advance the primary weave
cd ../../weave
rwv update
rwv push

# 4. Delete the workweave (pass --discard-unmerged-commits if PRs were squash/rebase-merged)
rwv workweave web-app delete feat-auth --discard-unmerged-commits
```

Note the final step: if the forge merged your PR via **squash-merge** or **rebase-merge**, the remote `main` branch contains a new commit SHA rather than your local workweave commit SHA, so git's DAG ancestry check (`is_ancestor`) sees the local workweave commit as unmerged and `rwv workweave delete` requires `--discard-unmerged-commits`.

## Related joints

- [clone-topology](./clone-topology.md) — Invariant I3 (branch ownership per workspace) and canonical store topology
- [pyramid-of-stability](./pyramid-of-stability.md) — how project-repo branches act as stability channels carrying `rwv.lock`
- [workweave-hierarchy](./workweave-hierarchy.md) — parent-child relationships and flat `<project>--<workweave>` branch naming
- [workweave-lifecycle](./workweave-lifecycle.md) — creation, landing, retirement, and deletion preconditions
- [lock-as-derived](./lock-as-derived.md) — why `rwv.lock` records resolved commit SHAs rather than branch names
