# Clone topology

repoweave's correctness story has tiers. The
[pyramid-of-stability](./pyramid-of-stability.md) joint defines *which set
of revisions* a project resolves to. The [shared-refs-drift](./shared-refs-drift.md)
and [sync-semantics](./sync-semantics.md) joints describe how those
revisions move and reconcile between workspaces. All of that machinery
assumes a property nothing in those joints states explicitly: every
constituent repo on disk is the *right physical artifact* — one canonical
store per repo, with every workweave checkout linked into it. If that
assumption fails, the higher tiers are operating on incoherent input.

This joint defines that bottom tier — the **clone topology** — and
states the invariants that make the rest of the pyramid sound. It is the
normative spec the `rwv doctor` topology checks enforce.

## Where this sits in the stability stack

Three tiers, bottom-up:

| Tier | What's "stable" at this tier | Joint |
|---|---|---|
| **2 — Revisions** | Each manifest repo on disk is at the SHA recorded in `rwv.lock`. | [pyramid-of-stability](./pyramid-of-stability.md) |
| **1 — Refs** | No silent drift between sibling worktrees on the same branch. | [shared-refs-drift](./shared-refs-drift.md) |
| **0 — Artifacts** | Every constituent repo is one canonical store with worktree-linked checkouts. | this joint |

Tier 2 cannot be true unless tier 0 is true: "the SHA in `rwv.lock`" is
only meaningful as a name for an object in *the* object DAG of that repo.
If two physically separate clones of the same repo exist, the SHAs they
hand out are ambient — they may name structurally identical commits, but
the merged-check (`rwv` asking "is this commit an ancestor of that one?")
runs in one DAG at a time and silently answers `no` across DAGs.
Likewise tier 1: drift classification compares blob content against the
object store, so the answer depends on *which* object store the comparison
runs against.

Every higher-tier check `rwv doctor` runs today operates in the
revision/content layer. None of them notice when the underlying artifact
is wrong. This joint defines what "right" means so a topology check has
a spec to enforce.

## The invariants

Three invariants, stated in VCS-neutral vocabulary. The next section
maps each to its git realization.

### I1 — Single canonical store per manifest repo

Each manifest repo named in any project's `rwv.yaml` has exactly one
**canonical store** in the weave: a fully-materialized clone at
`<weave>/<repo_path>`. That clone holds the object DAG and the refs for
the repo. Nothing under `<weave>/.workweaves/` is a standalone clone of
any manifest repo.

Operationally: when `rwv add` materializes a new manifest repo, the
canonical store always lands at primary's `<weave>/<repo_path>`, even
when the verb runs from inside a workweave. Workweaves do not host
their own clones.

### I2 — Workweave checkouts are linked into the canonical store

Each workweave's on-disk view of a manifest repo is a **linked
workspace** — a workspace whose object DAG and refs come from the
canonical store, not from a separate copy. Every commit reachable in
the canonical store is reachable from the workweave checkout, and
vice versa.

Operationally: a workweave's `<workweave>/<repo_path>/` is created by
asking the canonical store to add a workspace pointed at the workweave
directory. The two share one object DAG.

### I3 — Branches are owned by exactly one workspace

A repo's checked-out branch is owned by exactly one workspace. The
canonical store sits on a non-ephemeral branch (the manifest's tracking
branch, e.g. `main`). Every workweave checkout sits on an **ephemeral
branch** named `<project>--<workweave>/<segment>` — a branch name
visible only to that workweave.

Operationally: the ephemeral naming scheme is what makes two
workweaves both "on main" non-contradictory — they are on disjoint
branch names that each fork from `main`. A branch named for a
workweave is checked out only in that workweave.

The branch-ownership invariant is the soul of why the higher-tier
checks are sound. The merged-check that gates delete/retire — "is the
source's tip an ancestor of the target's tip?" — runs in one ref namespace
at a time. If two workspaces both held the literal branch `main`, "is
ancestor" would be asking about a single ref that disagrees with itself
across workspaces. The ephemeral-branch convention makes the question
well-defined.

## Why these are tier-0 invariants, not implementation details

The three invariants are load-bearing for every check above them:

- **Merged-check soundness (delete / retire).** `rwv sync-to --retire`
  refuses to delete a workweave whose unique commits aren't reachable
  from the parent — an `is_ancestor` query against the parent's tip.
  The query is sound only when both refs live in the same object DAG.
  Without I1+I2, `is_ancestor` can return `false` for two refs that
  *would* be ancestor-related if their objects were in the same DAG —
  the answer reflects the topology, not the history. A merged-check
  green-lights deletion of work the operator wanted; a red flag refuses
  to delete work that is in fact merged. Both failure modes are silent.
- **Sync convergence.** `rwv sync` and `rwv sync-to` push refs and
  materialize working trees by reaching across the parent edge of the
  workweave tree (see [workweave-hierarchy](./workweave-hierarchy.md)).
  The "push a ref into the parent" primitive uses git's local-to-local
  fetch — which collapses to a ref update when the source and target
  share an object DAG, and to an object transfer plus a ref update when
  they don't. The "fast" path is the only one with stable behavior under
  concurrent writers; the "transfer" path is a parallel clone that
  re-introduces I1's two-DAG problem on every push.
- **Drift classification.** [shared-refs-drift](./shared-refs-drift.md)'s
  safe-class / live-class classifier asks "is this on-disk blob present
  as a committed object?" The answer depends on which object store is
  consulted. Under I1+I2, "the object store" is unambiguous; without
  them, the classifier picks a store and silently classifies live work
  as safe, or vice versa.

The pattern in all three is the same: every higher-tier check is sound
*within* one object DAG. Multiple physically separate DAGs for the same
manifest repo turn every higher-tier check into a coin toss whose outcome
depends on which DAG the check happened to consult.

## Case study: `fo-a0spgj`

`fo-a0spgj` exposed exactly this failure mode. A repo's canonical store
lived inside a workweave; the slot at `<weave>/<repo_path>` held a
disconnected clone — same URL, separate object DAG; and 20+ worktrees
across the weave tree had been linked into the workweave-held store.
`rwv doctor` reported clean, because every existing check operates above
tier 0. The merged-check vouched for retirements that crossed DAGs; the
sync convergence path silently transferred objects on every push,
masking divergence; drift classification ran against whichever store
the verb reached first.

The corruption was invisible *and* progressive: each subsequent verb
deepened the inconsistency, because no check had a vocabulary for
"the artifact is wrong." This joint provides that vocabulary. The
sibling topology check in `rwv doctor` (landed alongside this spec)
enforces the invariants above directly — its violation kinds key off
the three I1–I3 categories.

## Git mapping

Each invariant maps to a specific git mechanism. The mapping is git's
worked example of the general spec; future Vcs impls
([vcs-as-seam](./vcs-as-seam.md)) will provide their own mappings.

| Spec | Git mechanism |
|---|---|
| Canonical store at `<weave>/<repo_path>` (I1) | A directory containing a `.git/` (or a `.git` file resolving into the canonical store, but not into a workweave). |
| Linked workspace (I2) | A `git worktree`-created checkout whose `.git` file resolves into the canonical store's `git-common-dir`. |
| Ephemeral branch ownership (I3) | A branch named `<project>--<workweave>/<segment>`, present in the canonical store's refs, checked out only in the workweave's worktree. |

The decisive git query is `git rev-parse --git-common-dir`. Run from
inside any workspace, it resolves to the path of the shared object/refs
directory:

- **Canonical store.** `git-common-dir` resolves to `<weave>/<repo_path>/.git`.
- **Workweave checkout under I2.** `git-common-dir` resolves to
  `<weave>/<repo_path>/.git` — the *same* path as the canonical store's.
  Both workspaces share one object DAG.
- **Disconnected clone (violation of I1).** `git-common-dir` resolves to
  some other path — typically `<weave>/.workweaves/<workweave>/<repo_path>/.git`.
  The workspace at `<weave>/<repo_path>` and the canonical store referenced
  by the workweave point at different directories. Two object DAGs exist.

Stating the topology check in terms of `git-common-dir` keeps it
content-addressed: a check that asks "does every workspace pointing
at this `repo_path` resolve `git-common-dir` to `<weave>/<repo_path>/.git`?"
catches both the disconnected-clone case and the inverted-canonical
case (canonical store living inside a workweave) without enumerating
either by name.

The ephemeral branch convention — `<project>--<workweave>/<segment>` —
is the same scheme described in
[workweave-hierarchy](./workweave-hierarchy.md). I3 is the spec; the
hierarchy joint is where the operational story lives.

## What rwv does *not* do

The invariants are about clone topology, not about repo-internal state:

- rwv does not require that every clone of a manifest repo across
  different weaves share an object DAG. The invariants are scoped to
  one weave at a time. Two weaves on the same machine can each hold
  their own canonical store of the same repo; they are independent
  topologies.
- rwv does not own the canonical store's branch state beyond I3. The
  canonical store can sit on any non-ephemeral branch the operator
  picked.
- rwv does not police bare clones the operator made by hand outside
  any `<repo_path>` slot. The invariants govern the manifest-named
  slots; anything else is the operator's territory.

## Anchoring

The topology check that enforces I1–I3 is wired into
`rwv doctor`; see the doctor reference for the violation `kind` and the
`--fix` semantics (where remediation is possible). The `fo-a0spgj`
case study above is the regression scenario the check is gated on.

## Related joints

- [pyramid-of-stability](./pyramid-of-stability.md) — the tier above
  this one; canonical-tip identity assumes tier-0 invariants hold.
- [workweave-hierarchy](./workweave-hierarchy.md) — where the
  ephemeral-branch naming scheme that I3 mandates lives operationally.
- [shared-refs-drift](./shared-refs-drift.md) — the drift classifier
  depends on a single object DAG to be sound.
- [sync-semantics](./sync-semantics.md) — push/replay primitives that
  assume one DAG per repo.
- [vcs-as-seam](./vcs-as-seam.md) — the spec here is VCS-neutral; the
  git mapping above is one impl. New Vcs impls own their own
  canonical-store / linked-workspace / branch-ownership mappings.
