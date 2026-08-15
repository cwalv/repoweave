# The repoweave branch model

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

**Status:** canonical for maintainers editing `src/`. This document decides
what a branch is for in repoweave, states the single rule that governs every
ref-touching operation, and specifies the type split that makes the rule hold
by construction rather than by review.

**Audience:** someone changing rwv's ref-handling code. Every claim is
restated in full. The published contracts this document backs are
[`../explanation/joints/clone-topology.md`](../explanation/joints/clone-topology.md)
(I3), [`../explanation/joints/workweave-hierarchy.md`](../explanation/joints/workweave-hierarchy.md)
(the ephemeral naming scheme), and
[`../reference/explain/doctor.md`](../reference/explain/doctor.md) (which
cites this document by section number for every branch-discipline finding).

**Verification key.** Claims carry a marker:

| Marker | Meaning |
|---|---|
| `[V]` | Verified by reading the source at the citation base below. |
| `[S]` | Verified empirically against a built `rwv` binary. Several `[S]` symptoms below were *reproduced before this model shipped* and no longer reproduce; each such passage says so and names the test that now holds the inverse. |
| `[R]` | Read from source or docs during the branch-behaviour investigation; not re-verified here. |
| `[?]` | Flagged, unverified by anyone. |

Code is cited as `file:line` against this repo. Docs are cited by path.

**Citation base and convention.** Every `file:line` citation in this document
was re-verified against commit `bd443d7bbd5c026414c807a932841cc2a9ecd79b`
(2026-07-26). A `[V]` marker means "true at `bd443d7`". The implementation of
this model finished landing earlier, at `37548fd` (2026-07-25); that is still
the commit the "Decided here" and "Decided in revision" claims in §10 shipped
at, and it is named there rather than here, because a citation base and a
ship point are different facts and conflating them is what let the previous
base drift unnoticed.

**This document has changed status.** It was written as a design against a
tree that did not implement it. That tree no longer exists. The
implementation landed the type split (§4), the flat ephemeral name (§3.5),
the receipt registry (R2), the warrants (R3), the store-destroy gate (R4),
the consent flags (§4.4), doctor's missing arms (§4.5, §7.2), and the
one-time migration (§7) — and then **deleted the old surface outright**:
`Vcs::current_ref`,
`Vcs::checkout`, `Vcs::delete_branch`, `Vcs::restore_savepoint`,
`create_worktree`, `push_with_role`, `list_branches_with_prefix`,
`parse_ephemeral_branch_name`, and — since, by a separate fix —
`Vcs::default_branch` have zero definitions and zero call sites in the tree
(three `rwv add` sites still bind a *local* named `default_branch`, from an
observation rather than a guess — §6.2).
Passages that diagnosed those constructs are therefore **historical**:
they describe what the code did before this model landed, and why that was the
problem. They are kept, in the past tense, because the diagnosis is the reason
the model exists — but none of them describes the present. Where a passage is
historical it says so, and where the fix is pinned by a test the test is named,
because a named test is the only citation that survives a refactor.

**If this document and the tree diverge again, re-run the full sweep and
update this paragraph's commit hash — do not patch individual citations one
at a time as drift is noticed.** Piecemeal patching is exactly how the
`110615f` base went stale invisibly. Where a line number would add nothing
over the construct's name (a long function body, a deleted construct, a trait
method whose signature is the load-bearing fact), the citation names the
construct instead of pinning a range that the next refactor will silently
invalidate.

---

## 1. What a branch is for, per layer

repoweave has three places a branch can exist, and they exist for three
different reasons. Conflating them is the root of every problem this document
addresses.

### 1.1 The engine below relock has no branch model at all

The sharpest single fact about the pre-branch-model code: `current_ref`
appeared **zero times** in `src/sync.rs`. That was the diagnosis, and it is
now history twice over. First commit `62af89f`, landed ahead of this model
and independently of it, added the detached-target refusal §4.6(1) argues
for, giving `sync.rs` its first three `current_ref` reads. Then this model's
implementation **deleted `current_ref` from the trait entirely**:
there are now **zero** `current_ref` call sites anywhere in the tree `[V]`.
Twelve textual matches survive: five comments explaining the deletion
(`git.rs:1841`, `:2907`, `vcs.rs:1468`, `:2586`, `sync.rs:3017`) and seven
string literals inside `src/bin/generate-explain.rs`, which are the
stale-symbol gate's own fixtures — a fixture quoting a deleted method is not
a use of it `[V]`. The count was ~18 at the previous citation base; the
comment sweep that removed the design-document section pointers took the
`status.rs` and `lock.rs` restatements with it.

The broader claim survives the fix that closed it. Where `sync.rs` reads
attachment now, it reads it to *refuse a MOVE when there is no branch to move
onto*, not to make sync branch-aware; sync still never reads *which* branch,
only whether one exists. The observation surface is `Vcs::head_attachment`
(§4.5), and its **19** production call sites `[V]` are: `check.rs` ×4,
`vcs.rs` ×4 (re-observation guards inside the default MOVE/DESTROY bodies),
`sync.rs` ×3, `push.rs` ×2, and one each in `git.rs`, `lock.rs`, `status.rs`,
`fetch.rs`, `update.rs`, `add_remove.rs`. `workweave.rs` — which used to
*derive an ephemeral branch name* from an observed HEAD — is at **zero** and
derives nothing from observation at all (§3.5). `add_remove.rs` reached zero
too and has since come back to one, for a different purpose: `rwv add --new`
reads `head_attachment` to learn what branch `git init` actually chose, and
writes that into `version:` (`add_remove.rs:693-703`) `[V]`. That is an
observation feeding a *tracking declaration*, not an ephemeral name — §6.2's
`"main"` fabrication replaced by a real read.

Everything structural is still SHA-and-DAG:

- `sync_one_repo` rebases or fast-forwards *whatever is checked out* onto a
  SHA (`sync.rs:579-630`) `[V]`.
- `ff_advance_repo` — the primitive that "lands" work into a target — obtains
  an `AttachedRef` witness from the target and advances *that* ref to CWD's
  tip; a detached or unborn target is a refusal
  (`sync.rs:5458-5554`, the three-armed `head_attachment` match at
  `:5477-5506`, the MOVE at `:5550-5552` → `git.rs:1227-1234`) `[V]`. This is
  the one place `sync.rs` names a branch — a precondition, not identity
  tracking; §4.6(1) is the argument that produced it, and it is now enforced
  by the type rather than by the author remembering.
- Retire's convergence check compares each repo's HEAD for **exact SHA
  equality** between CWD and target, not an ancestor check
  (`retire_workweave_after_sync_to`, `sync.rs:4366-4453`, the `!=` at
  `:4404`) `[V]`; delete's unmerged gate walks `is_ancestor` in the resolved
  canonical store (`collect_diverged_paths`, `workweave.rs:1989-2092`,
  `is_ancestor` at `:2055`) `[V]`; `rwv workweave log` computes
  `unique_commits`/`unique_diff` from `head_revision` with no `is_ancestor`
  call at all (`workweave_log`, `workweave.rs:3120-3322` →
  `git.rs:1805-1834`) `[V]`. Three different DAG queries — all three read
  only SHAs, never a branch name.
- `rwv.lock` records a resolved revision, never a branch
  (`lock.rs:128-156`, `manifest.rs:1193-1199`) `[V]`; the lock entry type is
  `ResolvedRevisionId` (`manifest.rs:1236`) `[V]`.

So a branch in repoweave is never load-bearing for *identity*. Identity is
the SHA. A branch is load-bearing for exactly three things, one per layer.

### 1.2 L1 — the project repo (`projects/<project>/`): a branch is a channel

The project repo's branch is the only branch in the system that *carries*
something. `docs/explanation/joints/pyramid-of-stability.md:95-96` states it
directly: "a channel is a branch is a `rwv.lock` is a set of SHAs", and
`:110`: "Each project-repo branch carries its own lock" `[V]`. Switching
the project repo's branch switches which set of member SHAs the weave
resolves to. That is a semantic operation with no SHA-level equivalent.

Consequently `rwv push` treats the project repo's branch as a hard gate, and
that gate is now written in this model's types: it reads
`Vcs::remote_default_branch` for the canonical branch and `head_attachment`
for what the checkout is on, then refuses on `Unborn`, refuses on `Detached`,
and refuses a mismatch via the named predicate `AttachedRef::is_named`
(`push.rs:178-210`; the three arms at `:195-210`) `[V]`. It also refuses when
`origin/HEAD` is unset, naming `git remote set-head origin -a` (`:181-187`)
`[V]`. Two of this document's proposals are visible in that one function: the
gate no longer consults a fabricated `"main"` (§4.2, §6.2), and the non-repo
case surfaces `NotARepo` rather than "detached HEAD" (§4.5).

The asymmetry this section used to name — **the one repo whose branch identity
gates publishing is the one repo whose branch identity nothing verifies** — is
**closed**. `scan_repos_on_disk` still walks registry directories only
(`workspace.rs:335-383`) `[V]`, but doctor's branch-discipline pass no longer
uses it as its walker: `workweave_checkouts` explicitly appends
`<workweave>/projects/<project>/` (`check.rs:3280`) and the canonical pass
iterates `workweave_index::projects_on_disk` alongside the manifest members,
with a dedicated scope arm so project-repo findings survive a project-scoped
run (`check.rs:4573-4575`) `[V]`. `git checkout --detach` in the project repo
now produces a finding, pinned by
`tests/branch_discipline_test.rs:729 detached_project_repo_is_reported`, with
`:761 attached_project_repo_is_clean` as its non-vacuity pair `[V]`. This is
§5.1's scope hole, shipped.

### 1.3 L2 — the canonical member store (`<weave>/<repo_path>`): a branch is a tracking declaration

A member repo's `version:` field in `rwv.toml` is typed `RefName`
(`manifest.rs:573`) `[V]` and is **branch-only by design**. This is a settled
decision, restated here so it need not be looked up:

> The manifest declares what to TRACK; the lock records where you ARE.

A tag or SHA in `version:` conflates the two layers — genuine pinning needs a
differently-named field. `rwv update` means "advance to the tip of the branch
you declared", and that verb is meaningless without a per-repo branch name.
Counting every read of a `version` field — the manifest's `version:` or its
lock-entry echo — `check.rs` has 8 sites, `fetch.rs` 12, `sync.rs` 5,
`push.rs` 4, `lock.rs` 3, `update.rs` 5 `[V]`. (Of `sync.rs`'s 5, only
`sync.rs:1265` reads the manifest; the other four read lock entries.) The
counts rose where this model added a *typed* read: `push.rs`, `fetch.rs`, and
`update.rs` each now parse the declaration at their seam —
`TrackingRef::parse(RawRefName::new(entry.version.as_str()))` at `push.rs:395`,
`fetch.rs:754`, `update.rs:678` `[V]` — because `manifest.rs`'s field is still
typed `RefName` and migrating it is separate work (§4.2).

Note precisely what `version:` names: a branch **on the remote**. `rwv update`
resolves it through `Vcs::resolve_branch_on_remote`, which explicitly refuses
a bare-branch fallback so callers "don't silently advance to the local branch
tip" (`vcs.rs:1895-1911`) `[V]`. That trait method's doc comment also claims
`upstream/<branch>` for `Role::Fork`; the shipped `GitVcs` impl still does not
implement that half — every role resolves to `origin`, `let _ = role; // all …
use origin` at `git.rs:813-822` and again in `push_ref` at `git.rs:2111` `[V]`
— drift already on record from a separate audit and out of this
document's scope to fix. The manifest's `version:` and any local branch of
the same name are different objects in different namespaces that happen to
share a spelling; `TrackingRef::local_counterpart` (`vcs.rs:876-878`) is now
the one named function where that projection is made, per §4.2.

### 1.4 L3 — the workweave checkout: a branch is a mechanical commit target

A workweave checkout's ephemeral branch exists because **git requires it**,
not because it means anything. Two facts:

1. A worktree must be attached to some writable ref to commit onto.
2. Git refuses to give two worktrees one branch **by default**. Verified:
   `git worktree add ../wt2 main` → `fatal: 'main' is already used by
   worktree at …`, git 2.43.0 `[V]`. This is default behaviour, not an
   impossibility — `git worktree add --force ../wt2 main` succeeds on the
   same git version and yields two worktrees both on `main` `[V]`, and
   `git symbolic-ref HEAD` does the same with no flag. rwv itself never
   passes `--force`, and there are now exactly **two** `worktree add`
   invocations in the tree, both inside `materialize_worktree_on_ref`:
   `git.rs:1934` (adopt an existing ref) and `:1943` (author one, `-b`)
   `[V]`. The old three-site `create_worktree` this section used to count is
   deleted; `create_worktree_on` (`vcs.rs:2763-2779`) is no longer an
   uncalled sibling but the production path — and it now has exactly **one**
   production call site, `workweave.rs:1111`, inside
   `birth_ephemeral_worktree` `[V]`. The three verbs that birth a ref all
   route through that chokepoint rather than calling the trait method
   themselves: `workweave create` (`workweave.rs:1503`, `:1628`), `sync`'s
   materialize-missing-repo path (`sync.rs:1271`), and `rwv add`
   (`add_remove.rs:95`) `[V]`. Sync reached the trait method directly at the
   previous citation base; it was routed through the chokepoint since, so the
   four-state `(receipt, ref)` classification runs for it too.

`docs/explanation/joints/workweave-hierarchy.md:187-206` gives exactly this —
and *only* this — as the justification for the ephemeral naming scheme `[V]`:
"Because primary's `main` and a workweave's `myproj--hotfix/main` are
different branch names, no two workweaves compete for the same named branch."
(The joint's example uses `myproj--hotfix/main` `[V]`; earlier drafts of this
document quoted a different `<project>--<workweave>/main` spelling by
mistake.) **That joint has not been updated for §3.5**: it still presents the
three-part `<project>--<workweave>/<segment>` shape as what rwv mints, at
`:190` and `:202`, which the shipped code no longer does — see §6 item 7.

The normative invariant that enforces it is I3, in
`docs/explanation/joints/clone-topology.md:82-114` ("tier-0" is stated at
`:116` and in the tier table at `:19-25`, not inside I3's own text). It has
a dedicated scanner set — now **three** passes, not two:
`scan_workweave_repo_branches` (`check.rs:3567-3678`),
`scan_canonical_stores` (`:3859-4034`), and `scan_dangling_receipts`
(`:4096-4230`), entered from `scan_branch_discipline` (`:4288-4323`) `[V]`.
The `BranchDisciplineKind` violation taxonomy is now **twelve** sub-kinds
(`check.rs:855-1083`; it was six when this document was written, and five
before that), with a `--fix` and a **43**-test suite
(`tests/branch_discipline_test.rs`; it was 38 at the previous citation base,
20 before that, and 15 before that — all counts `[V]`). The scanner/taxonomy/test-suite claim is sourced from the code, not
from `clone-topology.md` itself, which states only that I3 is a tier-0 spec.
But read I3's stated purpose carefully:

> "The merged-check that gates delete/retire — 'is the source's tip an
> ancestor of the target's tip?' — runs in one ref namespace at a time … The
> ephemeral-branch convention makes the question well-defined."
> (`clone-topology.md:96-102`) `[V]`

That is a **disjointness** requirement. It is satisfied by *any* scheme in
which no two workspaces hold the same ref name. It is not a claim that
workweave branch names carry meaning. The code agrees: `workweave.rs:2305-2309`
states defensively that "Branch names are creation-time namespaces, **NOT
lineage records**", and `status.rs:82-89` restates it — "Parent identity comes
from the workweave's `.rwv-workweave` marker (`parent:`), NOT from the branch
name" — both directing consumers to read parentage from the marker `[V]`.

### 1.5 Summary of the three purposes

| Layer | The branch is | Who depends on it |
|---|---|---|
| L1 project repo | a **channel** — it carries a lock | `pyramid-of-stability.md`; `rwv push`'s gate |
| L2 canonical member | a **tracking declaration** — what to advance toward | `rwv update`; `version:` |
| L3 workweave checkout | a **commit target**, disjoint per workspace | git's one-worktree-per-branch rule; I3's merged-check soundness |

Nothing else in rwv needs a branch. Sync, lock, retire, delete's merged-check,
`workweave log`, and status's relation column are all pure SHA/DAG.

---

## 2. The diagnosis: four notions, one type

**This section is historical.** It describes the surface as it stood before
this model landed, because that surface is the reason for everything that
follows. Each numbered notion now has its own type, and the paragraph after
the list records what became of the single type they shared.

The surface carried **four unreconciled notions of "the member branch"**:

1. **`version:`** — declared in the manifest, names a branch on a remote.
2. **The ephemeral branch** — `{project}--{workweave}/{segment}`, minted by
   rwv, three different ways (below).
3. **Whatever is currently checked out** — read via `current_ref`.
4. **Nothing** — detached, which two first-class verbs produced by default.

Every **mutating** verb operated on (3) or (4). Every **checking** verb
assumed (1) or (2).

All four were the same Rust type. `pub struct RefName(String)` — still present
at `vcs.rs:252-271`, still a bare newtype with a `pub fn new(impl
Into<String>)` constructor and no validation `[V]` — was used for the
manifest's `version:`, for minted ephemeral names, for whatever `current_ref`
returned, for the deletion argument, and for the `default_branch` fallback.
Nothing in the type system distinguished them, so mixing them was invisible.

What changed: notions (1)–(4) are now `TrackingRef`, `EphemeralRefName` /
`OwnedRef`, `AttachedRef`, and `HeadAttachment::Detached(DetachedHead)`
respectively (§4.2), and **four of the five `RefName` sites listed above no
longer exist** — `current_ref` and `delete_branch` were deleted, ephemeral
names are minted by `EphemeralRefName::mint`, and `default_branch` — the
method the fallback lived in — has been deleted from `GitVcs` and from the
trait `[V]`. `RefName` itself survives, still unvalidated, but only on the
surface this model did not reach: `manifest.rs:573`'s `version:` field
(parsed into a `TrackingRef` at each consumer's seam rather than at the
field, §1.3), `tag_at_head` (`vcs.rs:1947`), the `BranchAlreadyExists` error
payload (`:295`), and the two prune predicates (`:2298`, `:2318`) `[V]`.
`rwv add` still *writes* the field through `RefName::new`
(`add_remove.rs:308`, `:410`, `:694-695`) `[V]`, but the value it wraps is
now an observed `RemoteDefaultBranch` or an observed `HeadAttachment` rather
than a fabrication — §6.2. Migrating the manifest field is the one piece of
§4.2's table left undone.

### 2.1 What the conflation produced, concretely

Each of these was a verified consequence of the single-type design, not a
separate bug. **Every `[S]` symptom in this list except the last has been
fixed by this model**, and each entry now names both the reproduction and the
test that holds the inverse — the symptom is what made the design necessary;
the test is what keeps it from coming back.

- **`fetch` and `update` detached every repo they touched**, including repos
  already at the target SHA `[S]`. Both called `Vcs::checkout`, whose only
  available shape was `git checkout <sha>` because
  `ResolvedRevisionId::as_str()` is always a canonical SHA
  (`vcs.rs:76-78`) `[V]`. The call site could not express "move the branch
  I'm on", so it silently expressed "stop being on a branch".
  **Fixed.** `Vcs::checkout` is deleted; `fetch` now goes through
  `realign_present_clone` (`fetch.rs:717-809`), which reads `head_attachment`
  and either advances the attached counterpart, moves an already-detached
  HEAD, or refuses naming `--detach-checkouts`; `update` mirrors it in
  `advance_checkout` (`update.rs:620-728`) `[V]`. Pinned by
  `tests/fetch_in_place_test.rs:335
  in_place_fetch_fast_forwards_the_counterpart_and_stays_attached`, which
  asserts the checkout is still on `main` *and* that `main` itself moved —
  the exact assertion §4.7 says the suite lacked `[V]`.
  Half of the reporting defect survives: `rwv update` now counts SHA deltas
  rather than non-`Err` outcomes (`update.rs:277-279`, with an in-code comment
  stating why: "a repo that was already at the target was visited successfully
  but advanced nothing"), but its `branch` JSON field still echoes
  `entry.version` verbatim (`update.rs:272`) `[V]`.

- **`rwv doctor` had no arm for the state its own verbs produced most often.**
  The canonical-store scan reported only on `Ok(Some(branch))`; `Ok(None)` —
  detached — matched no arm and produced **no finding**, while `rwv push`
  hard-refused on that same state `[V]` `[S]`.
  **Fixed.** `scan_canonical_stores` matches `HeadAttachment` exhaustively and
  its `Detached` arm always emits `BranchDisciplineKind::CanonicalDetached`
  (`check.rs:3908-3933`), carrying the SHA, the tracking counterpart, and
  whether a reattach is provable `[V]`. Pinned by
  `tests/branch_discipline_test.rs:592 detached_canonical_is_reported`.
  Doctor's remediation advice was also wrong for the case rwv produced: it
  said `git switch -c <prefix>/main`, which errors with "already exists"
  `[S]`. **Also fixed** — `reattach_advice` (`check.rs:5477-5490`) emits
  `git switch <name>` when a receipt names an existing ref and reserves the
  `-c` spelling for when none does `[V]`.

- **`create_worktree` silently force-deleted a colliding branch.** On
  "already exists" it ran `git branch -D` and retried. Verified destroying a
  pre-existing `my-app--feat2/main` carrying a unique commit; reflog wiped,
  commit dangling, **no `--force` needed and nothing printed** `[S]`.
  **Fixed by deletion.** `create_worktree` no longer exists; its successor
  `materialize_worktree_on_ref` (`git.rs:1908-1945`) classifies before acting
  and **adopts** a pre-existing ref (`:1927-1935`) rather than destroying it,
  returning `None` so the caller knows it did not author it `[V]`. There is
  now exactly **one** `branch -D` in `git.rs`, inside `destroy_local_ref`
  (`:2049-2053`), reachable only behind `OwnedRef` + `DeletionWarrant` `[V]`.

- **`rwv doctor --fix` deleted hand-made branches.**
  `parse_ephemeral_branch_name` claimed any `<a>--<b>/<c>` name; hand-made
  `my--feature/wip` and `notes--todo/scratch` were deleted by a plain
  `rwv doctor --fix` `[S]`. Nothing recorded which refs rwv actually created —
  the workweave marker was `{primary, project, parent}` and nothing else.
  **Fixed.** `parse_ephemeral_branch_name` is deleted; ownership comes from
  the receipt registry, and `fix_stale_ephemeral_branches` re-resolves every
  candidate through `RecordedRefs::for_store`, refusing with "rwv holds no
  ownership receipt for it (branch-model.md R2)" when there is none
  (`check.rs:4619-4713`, refusal at `:4664-4669`) `[V]`. Pinned by
  `tests/branch_discipline_test.rs:1028 handmade_lookalike_branch_survives_doctor_fix`
  and `:1080 flat_lookalike_branch_survives_doctor_fix`. The marker is still
  `{primary, project, parent}` (`workspace.rs:1079-1083`) `[V]` — deliberately:
  §6 item 5 explains why the receipts live in the index instead.

- **`sync-to` used to "land" onto a detached target, and then delete destroyed
  the only ref.** Verified end-to-end: with primary's member detached,
  `rwv sync-to primary` printed `ff-advanced to 95fbf86f` /
  `sync-to complete: target fast-forwarded to CWD's tip` while `main` stayed
  at `288294d`; the follow-on `workweave delete` force-deleted the only
  ref, leaving `95fbf86` referenced by nothing. Every step reported success
  `[S]`. Commit `62af89f` first blocked this with a runtime check. **This
  model then replaced the check with the type**:
  `ff_advance_repo` obtains an `AttachedRef` witness from the target and the
  MOVE derives its repo from the witness, so there is no path argument left to
  point elsewhere (`sync.rs:5458-5554`) `[V]`. This is the chain §4.6(1)
  derives the type split from. Pinned by
  `sync.rs:5659 ff_advance_repo_refuses_to_land_onto_a_detached_target`,
  `:5635 ff_advance_repo_lands_on_the_branch_the_target_is_attached_to`, and
  the compile-fail probe
  `tests/branch_model_compile_fail_test.rs:551 a_witness_cannot_point_a_move_at_a_different_repo`.

- **The ephemeral name was derived three incompatible ways.**
  `workweave.rs` used the *fork source's* `current_ref` (recursive when
  forking a workweave), with a `detached-<12sha>` fallback; `add_remove.rs`
  used the *canonical's* `current_ref` in an inlined copy with its own
  truncation; `sync.rs` used the *manifest's `version:`* while its comment
  claimed it "mirror[s] create_workweave's naming" `[V]`. The recursive
  derivation produced unbounded nesting — `p--gc/p--child/p--feat/main` — with
  glob and parser both working at depth 9-180 and failing at ~181 with a raw
  `fatal: cannot lock ref` `[S]`.
  **Fixed by deletion of the question** (§3.5). There is one derivation left,
  `EphemeralRefName::mint(project, workweave)` (`vcs.rs:962-964`), total and
  two-argument, called from `workweave.rs:1439`, `sync.rs:1276`, and
  `add_remove.rs:94` `[V]`. `sync.rs:1261-1264` now carries a comment stating
  that the manifest's tracking branch is the START POINT and that the NAME
  comes from `mint`, which is what the retracted "mirrors create_workweave's
  naming" claim used to obscure `[V]`. Recursion, the `detached-<12sha>`
  fallback, and the 12-char truncation are all gone; nesting cannot occur
  because there is no third component to nest — and since `a79ee35` neither
  component can contain a `--` either (§9, Q12).

- **`rwv push` reported "detached HEAD" for a directory that is not a git
  repo** `[S]`, because `current_ref` mapped every failure to `Ok(None)`:

  ```rust
  // deleted — this is what the collapse looked like
  fn current_ref(&self, repo: &Path) -> Result<Option<RefName>, VcsError> {
      match Self::run(&["symbolic-ref", "--short", "HEAD"], repo) {
          Ok(name) => Ok(Some(RefName::new(name))),
          Err(_) => Ok(None), // detached HEAD
      }
  }
  ```
  **Fixed.** `observe_head` (`git.rs:1840-1883`) tests `is_repo` first and
  returns `VcsError::NotARepo` before reading HEAD (`:1845-1847`); an
  unreadable ref database is `CommandFailed`, not a state `[V]`. §4.5 is the
  argument, and the four-state collapse no longer has a value to live in.

- **`prune_dropped_repo` is blocked by rwv's own refs while they exist.**
  *(This one is unchanged, by design.)* It refuses if any local branch lacks
  an `origin/` counterpart (`sync.rs:1478-1519`) `[V]` — which every
  rwv-authored ephemeral branch does by construction. A store that *currently
  holds* an rwv-authored ephemeral branch cannot be pruned; a clean
  `workweave delete` removes the ephemeral branches, after which the predicate
  passes. The refusal's full message is "dropped from lock but clone has
  local-only commits; push them and re-run, or remove manually"
  (`sync.rs:1521-1524`) `[V]` — the second remedy works. This refusal was also
  the only thing standing between a live workweave's git backing and
  `remove_dir_all`; that is no longer true, because R4 now gates the destroy
  independently (`check_store_unclaimed` at `sync.rs:1530`, before the
  `remove_dir_all` at `:1531-1532`) `[V]`. See the `prune_dropped_repo` row
  in §5.

### 2.2 The shipped justification was false in premise — and is now true

The destructive-operations audit test carries a written justification for
every `branch -D` and `checkout` invocation in `git.rs`. When this document
was written it justified **two legacy** `branch -D` sites like this:

> "(1) create_worktree retry: deletes a **stale** ephemeral branch
> (project--workweave/branch namespace) **left by a previous failed create**.
> (2) delete_branch: only called with ephemeral-prefix branch names from
> delete_workweave, **behind its refusals**."

Both clauses were false. Nothing checked staleness, and delete's refusals
inspected one HEAD per repo while the delete enumerated a whole prefix. That
was the point of this section: the invariants were written down and audited,
and **the audit was of a claim, not of a check**.

**Both sites are now deleted, and the entry says so.** The `-D` entry
(`tests/destructive_ops_audit_test.rs:171-197`) records `count: 1` and
justifies `destroy_local_ref` and nothing else — naming, in its own text, that
`create_worktree`'s force-delete-and-retry (whose "deletes a STALE branch"
claim was measured FALSE) and `delete_branch` were both deleted, and that "the
force-delete of a ref rwv holds no receipt for is now unreachable because the
code that could do it does not exist" `[V]`. The one surviving site is
`git.rs:2051`, behind `OwnedRef` (R2) and `DeletionWarrant` (R3).

The `"checkout"` entry has moved the same way. It is at `:248-297` with
`count: 4`, and opens by recording that "the bare `checkout()` that used to
head this list is DELETED" `[V]`. (Both spans are corrected here: the previous
citation base pinned this pair at `:289-323` and `:239-287`, which are the
`"--hard"` entry and its neighbour — the entries were named correctly and
located wrongly, and no sweep before this one checked the line against the
`pattern:` it claimed.) Its clauses no longer consider only
*working-tree clobber*: they cover `refresh_working_tree_to_head_if_safe`,
`set_detached_head`, `attach_head_to`, and `clone_attached_at` — and the two
attachment-changing ones are reachable only behind `DetachConsent` /
`ReattachConsent`, which now have real minting callers in the CLI dispatch
(§4.4). The blind spot behind the entire detach story is closed at the type
level, not by a comment.

This is what the section originally asked for. The change was never "add a
check"; it was making the shipped claims true, and the audit test is where
that shows up as a diff.

---

## 3. The rule

One rule, stated as a decidable predicate over operations, not as a list of
verbs. A verb-list is what the previous trigger-model document used, and when
the code shipped only some of the listed verbs nobody could tell — and nobody
could derive the answer for a verb the list omitted.

### 3.1 The ref HEAD names

Define, for a workspace `W`:

> **`ref(W)`** = the branch HEAD symbolically points at, when HEAD is
> symbolic; otherwise HEAD itself.

### 3.2 The classification

Every rwv operation that writes to `refs/heads/`, to `HEAD`, or to a ref
store as a whole is exactly one of four kinds. The kind is decidable by
inspecting the writes the operation performs:

| Kind | Predicate | Consent required |
|---|---|---|
| **MOVE** | Changes the *value* of `ref(W)` and nothing about `HEAD`'s symbolic-ness or target. | The verb's own preconditions. A MOVE that is not a fast-forward (a rewind) additionally requires a savepoint under `refs/rwv/pre-op/*` and a `DiscardWarrant` (§4.4). A MOVE of an already-detached HEAD is subject to the mid-operation precondition (§3.6). |
| **ATTACH** | Changes whether `HEAD` is symbolic, or which branch it points at. | Birth, or a named override. |
| **DESTROY** | Removes a ref under `refs/heads/`. | An ownership receipt *and* a warrant. |
| **DESTROY-STORE** | Removes an entire ref store and its object database (`remove_dir_all` of a repo). | R4: no live worktree registered against the store, every receipt keyed to the store retracted, and the verb's own named preconditions. |

DESTROY-STORE exists because a store-level destroy deletes every ref and
every object at once, so no ref-level rule can gate it (Q11 first recorded
this) — and therefore no §5 row may rely on ref-level reasoning to permit
one. rwv performs store-level destroys in three places, and all three are now
R4-gated: `prune_dropped_repo` (`check_store_unclaimed` at `sync.rs:1530`,
destroy at `:1531-1532`) `[V]`, `rwv remove --delete` (`refuse_claimed_store`
at `add_remove.rs:493`, destroy at `:518`) `[V]`, and `workweave delete`
(`workweave.rs:2848-2851`, call at `:2850`, reached only after
`retire_recorded_refs` has run the store's receipts dry) `[V]`.

Anything else — reading refs, writing `refs/rwv/*`, moving remote-tracking
refs — is not a branch-model operation and this rule does not govern it.

### 3.3 The four consent rules, stated

**R1 — rwv moves refs; it does not change attachments.**

Every verb may MOVE the ref a checkout is already on, subject to that verb's
existing preconditions. Creating an attachment where there was none — at
`rwv workweave create`, `rwv add`, `clone` — is **birth** and is fine. A
birth attaches at the revision the verb is materializing — the lock revision
for `fetch`, the resolved start point for `workweave create` and `add` — and
**a birth is never followed by a MOVE to reach the intended revision.**
**Changing which ref a checkout is attached to, including attached →
detached and detached → attached, requires a named override.**

**R2 — rwv may only DESTROY a ref it recorded creating.**

Ownership is by **record**, not by name shape. A ref is rwv's to delete iff
rwv holds a persisted receipt for that exact name in that exact store. A ref
that merely *looks* like rwv's is not rwv's.

**R3 — a DESTROY additionally requires a warrant.**

The receipt says "this is mine". The warrant says "and it is safe to lose
now". Three warrants, and no others:

- **Unmoved** — the ref's tip is exactly the tip rwv recorded. Nothing has
  happened to it since rwv created it. (This was written for
  `create_worktree`'s retry, which no longer exists — §4.6(3). Its shipped
  consumer is workweave-create rollback, which deletes only a ref this create
  authored and only if nothing has landed on it since:
  `undo_ref_births`, `workweave.rs:679`, via `DeletionWarrant::unmoved`
  `vcs.rs:1678`) `[V]`.
- **Merged** — the ref's tip is an ancestor of a named baseline (the recorded
  parent workweave's tip, or the primary weave's tip). This is what
  `workweave delete`'s diverged-paths check computes via `is_ancestor`
  (`collect_diverged_paths`, `workweave.rs:1989-2092`) `[V]`; `sync-to
  --retire` runs it too, by calling into the same delete path after its own
  separate, exact-SHA-equality convergence precondition (`run_retire`,
  `sync.rs:4232-4251` → `retire_workweave_after_sync_to`, `:4366-4453`) `[V]`.
  **Shipped**: `DeletionWarrant::merged` (`vcs.rs:1688`) is the constructor,
  and `retire_recorded_refs` (`workweave.rs:2421-2503`) establishes one per
  recorded ref against `baseline_tips_in_store` (`:2286-2302`) before any
  delete `[V]`.
- **OperatorDiscarded** — the operator passed the named override that consents
  to this specific loss.

**R4 — a DESTROY-STORE requires the store to be unclaimed.**

No live worktree may be registered against the store, and every receipt
keyed to the store must first have been retracted — each retraction via its
own DESTROY, with receipt and warrant — before the store itself may be
destroyed. The verb's own named preconditions (dirty state, unpushed work)
apply on top. A DESTROY-STORE never substitutes for the per-ref DESTROY
discipline; it is what becomes legal after that discipline has run dry.

### 3.4 Deriving answers from the rule

The rule is a decision procedure, not a table. For any verb, present or
future:

1. Enumerate the ref writes the verb performs.
2. Classify each as MOVE / ATTACH / DESTROY by §3.2.
3. Check the verb holds the consent §3.3 requires for that kind.

Worked examples for verbs *not* in the consequences table below, to show the
procedure is total:

- **`rwv doctor --fix`'s `.gitattributes` auto-commit**
  (`commit_replay_exclusion_migration`, `check.rs:1994-2112`) `[R]` — commits
  onto the project repo's current branch. Ref write: the branch advances.
  HEAD's symbolic target is unchanged. → **MOVE**. Legal, no new consent.
- **`rwv abort`'s savepoint restore** (`abort_one_repo`, `sync.rs:4975-5003`,
  the `verified_restore_savepoint` call at `:4995-5002` → `git.rs:2200-2210`,
  reset at `:2207`) `[V]` — resets the current branch to
  `refs/rwv/pre-op/<id>`. → **MOVE**, usually a rewind — and its
  `DiscardWarrant` (§3.2) is trivially held: the savepoint the warrant
  requires is the very ref being restored to, and invoking `abort` is the
  named consent. Note it *already* implemented the attributability
  discipline R3 generalizes: `verified_restore_savepoint` classifies the
  tip and refuses on `ForeignTip` (`vcs.rs:539-587`, `:2223-2266`) `[V]`.
- **A hypothetical `rwv workweave rename`** — would change a ref's name, which
  is a DESTROY of the old name plus a birth of the new. → needs a receipt for
  the old name. Derivable without amending anything.
- **`git commit` run by the operator inside a workweave** — not an rwv
  operation. Out of scope; the rule governs rwv's writes.

### 3.5 The deletion: drop `<segment>` — **shipped**

Ephemeral branch names are **`{project}--{workweave}`**. Flat. No third
component. `EphemeralRefName::mint(&ProjectName, &WorkweaveName)`
(`vcs.rs:962-964`) is total, takes two arguments, and is the only minter
`[V]`.

This was justified by evidence, not taste:

- The docs justify the naming scheme **solely** by git's
  one-worktree-per-branch constraint
  (`workweave-hierarchy.md:187-206`, `clone-topology.md:96-102`) `[V]`, which
  the `{project}--{workweave}` prefix alone satisfies.
- **No consumer read the segment.** Doctor validated the prefix only and said
  so deliberately in its own doc comment; delete globbed the prefix; doctor's
  staleness check keyed on the first path segment. `sync`, `sync-to`, `push`,
  the merged-check, `workweave log` and status's relation column all refused
  to read it `[V]`.

None of those three consumer citations resolves any more, and that is the
point: the doc comment that said "prefix only" is gone because there is no
segment left to exclude; `git.rs`'s `"{prefix}/*"` glob is gone, replaced by
`list_branch_names_with_prefix` filtering `starts_with` in Rust
(`git.rs:2139-2154`) — a change made *because* git's `*` stops at `/` and so a
glob would silently omit a leftover ref `[V]`; and doctor's scoping now keys
on the workweave *directory* basename via `branch_discipline_in_scope`
(`check.rs:4529-4583`), never on a path segment of a branch name `[V]`. The
"load-bearing in the wrong direction" coupling §8.4 complains about therefore
runs the other way now: a project is derived from a directory name, and no
directory name is derived from a branch name.

It dissolved four problems with no new policy: the three-way derivation
disagreement has nothing left to derive; unbounded nesting cannot occur;
adoption can no longer leave a branch name that names a dead parent, making
"names are not lineage" structurally true rather than defended by comment;
and `detached-<sha>`, tag-shaped and `release/1.x` segments stop making the
namespace non-flat.

Collision safety: two repos in one workweave live in *different object
stores*, so one name per store is enough. Two workweaves in one project did
**not** have different names by construction: the only gate at create was a
directory-existence check keyed on the *directory path*, and `--dir`
(`cli.rs:466-474`) supplies an arbitrary path, so two `--dir`-placed
workweaves with the same *name* but different paths each passed their own
existence check `[V]`; the registry insert is a silent last-writer-wins
(`record_workweave`, `workweave_index.rs:454-464`) `[V]`, deliberately, for
placement entries. The `<segment>` used to disambiguate the minted names
accidentally; flattening removed that. So this model added a **uniqueness
check at create**, and it shipped: `workweave create` now consults
`workweave_index::lookup_raw` before the directory-existence check and bails
naming the ref both workweaves would mint (`workweave.rs:1215-1243`, lookup at
`:1225`; the existence check follows at `:1245`) `[V]`. Pinned by
`tests/branch_model_lifecycle_test.rs:359
create_refuses_a_name_the_index_already_records`. (The residual
`--`-in-a-name ambiguity **is closed** — `ProjectName` and `WorkweaveName`
now reject `--` at construction; see §9, Q12.)

### 3.6 The mid-operation precondition on detached MOVEs

`HeadAttachment::Detached` collapses two different situations: "rwv
detached this HEAD at a lock SHA" and "the operator is mid-operation" — a
`git bisect`, a `rebase -i` stopped at an `edit`. Yanking HEAD out from
under the second is a consentless loss of operator state: the same
collapse-of-distinct-states sin §4.5 abolishes for `Ok(None)`, reappearing
inside the `Detached` variant.

So: a MOVE of an already-detached HEAD refuses when the repo is
mid-operation, naming the operation. **This is shipped end to end.** The
detection is `GitVcs::mid_op_state` (`git.rs:533-558`), which checks
`rebase-apply` / `rebase-merge` / `MERGE_HEAD` / `CHERRY_PICK_HEAD` /
`BISECT_LOG` `[V]`; its own doc comment states this section's argument in its
own words, naming the precondition rather than the section — "a bisect …
has no conflict-resume path, so it never appears in `Vcs::mid_op` — but it is
operator state living in HEAD's *position* … That is the state the
detached-MOVE precondition exists to see" (`git.rs:527-532`) `[V]`. It named
`§3.6` by number at the previous citation base; the number was dropped when
the tree stopped citing design documents from comments, and the invariant is
what stayed. `advance_detached_head` —
the MOVE primitive this section specifies — refuses via a dedicated
`mid_operation` trait method wired straight to `mid_op_state`, not through the
older `mid_op`/`ConflictOp` path (`vcs.rs:2725-2748`, refusal at
`:2730-2735`; `mid_operation` impl `git.rs:1904-1906`) `[V]`.

And the wiring this section said was missing has landed: **`fetch` and
`update` now call `advance_detached_head`** (`fetch.rs:735`,
`update.rs:636`) `[V]`, so an already-detached repo they touch is a MOVE
subject to this precondition rather than an unconditional `checkout <sha>`.
The earlier statement that they "call neither one, zero times" is no longer
true.

One asymmetry remains, unchanged and worth keeping visible: `sync.rs` still
consults only the older `mid_op`, a `ConflictOp`-returning wrapper over
`mid_op_state` whose `match` has no bisect arm and folds it into `None`
(`git.rs:1408-1415`) `[V]`. So a bisect is seen by the MOVE primitive and not
by sync's own preflight. Whether that gap matters is Q13.

---

## 4. The type split that enforces it

**Status: shipped.** This section was written as a proposal, and the tense
below has been corrected to match what landed. Everything it specifies exists:
the five core types, the six supporting types, the `Vcs` trait surface (§4.3),
the consent tokens and warrants (§4.4), the `HeadAttachment` split (§4.5), and
the mid-operation refusal (§3.6). The strangler sequencing this section once
described — new surface added alongside the old, called by nothing — is over:
the old surface was **removed**, and every production verb was restated in
terms of the new one. `vcs.rs:2586-2594` records the deletion in the trait
itself, and states the rule that governed it: the replacements "were deleted
only once every call site had been restated in terms of this surface — and
that restatement was the audit: a site that could not say which replacement it
meant was a site nobody had classified" `[V]`.

Two things this section specified are **not** shipped, and are called out at
point of use rather than here: `manifest.rs`'s `version:` field is still typed
`RefName` (consumers parse a `TrackingRef` at their own seam, §1.3), and Q6 —
which ref a member repo publishes — is still open, with `PublishRef` holding
the shape of the answer but not the answer (`PublishRef::from_local`,
`vcs.rs:1419`, is the unchosen alternative, `#[allow(dead_code)]` and exercised
only by a unit test) `[V]`.

### 4.1 The precedent, in this same file

repoweave has already solved this exact shape once. A lock scalar and a
resolved commit were the same type; "compare the raw thing against the
resolved thing" was a legal line that was always wrong. The fix was two
types.

`ResolvedRevisionId` (`vcs.rs:16-121`) `[V]`:

- Construction is **path-rooted**: the only public constructors are
  `Vcs::resolve_revision` / `Vcs::head_revision` (which resolve against a real
  repo), `from_canonical` (`:46`, mint with a known SHA), and
  `from_rev_parse_output` (`:67`, mint from raw ref-resolution output,
  **verifying** the canonical form — added since this precedent was first
  written, to give savepoint/pre-abort-ref resolution a real constructor
  instead of the escape hatch the next bullet used to name).
- "There is no public way to mint a `ResolvedRevisionId` from a free string —
  the parse boundary lives in `RawRevisionId`."
- Deliberately **no `Deserialize` impl**. Lock-file scalars deserialize into
  `RawRevisionId`; the only way to obtain a resolved value is resolution.
- The escape hatch this precedent originally needed, `pub(crate)
  from_canonical_unchecked`, is **gone** — deleted, its one caller replaced
  with the verifying `from_rev_parse_output` above (`vcs.rs:52-73`) `[V]`. A
  cleaner outcome than the precedent originally shipped with, not a regression
  in it.

`RawRevisionId` (`vcs.rs:125-190`) `[V]`:

- Wraps the YAML scalar verbatim; at the type level we do not know whether it
  is a tag, a branch, or a SHA.
- "It is intentionally not interchangeable with `ResolvedRevisionId`: there is
  no `PartialEq` between the two, and `RawRevisionId` cannot be fed to
  commit-id operations such as `Vcs::advance_attached_ref`." (The doc comment
  named `Vcs::checkout` when this document quoted it; the implementation
  deleted that method and updated the sentence — `vcs.rs:133` `[V]`.)
- The invariant is enforced by a **`compile_fail` doctest**
  (`vcs.rs:149-157`) `[V]`:

  ```rust
  /// ```compile_fail
  /// use repoweave::vcs::{RawRevisionId, ResolvedRevisionId};
  /// let raw = RawRevisionId::new("v1.0.0");
  /// let resolved = ResolvedRevisionId::from_canonical(
  ///     "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  ///     Some("v1.0.0".to_string()),
  /// );
  /// let _ = raw == resolved; // E0308: mismatched types
  /// ```
  ```

  (The shipped doctest's own comment names `E0308`, not `E0277` — a
  cross-type `==` is a type mismatch, not an unsatisfied trait bound; this
  document originally miscited the error code; a separate audit fixed the
  doctest's comment, and this correction just catches this document up. The
  comment's current wording is `// E0308: expected RawRevisionId, found
  ResolvedRevisionId` `[V]`.)

And the codebase records the payoff at the consumer site
(`check.rs:5423-5428`) `[V]`:

> "The lock entries are pulled from `input.resolved_locks` (built by the
> caller via `LockFile::resolve_versions`), so equality is purely a
> canonical-SHA comparison — the raw-vs-resolved confusion that produced the
> historical B3/B6 bugs is now a **compile-time impossibility**."

That is the target. Not "add a check". Make the wrong line not compile.

### 4.2 The types — **all shipped**

Five **core** types cover `RefName`'s four notions plus the parse
boundary — the same structure the revision split has (one parse boundary,
one resolved value), scaled to a domain with more roles. They do not cover
the trait's full `RefName` surface by themselves; the supporting types
later in this section complete it, and the honest inventory is stated at
the section's end.

| Type | Notion | Sole way to obtain one | Shipped at |
|---|---|---|---|
| `RawRefName` | the parse boundary | `Deserialize`; `RawRefName::new(&str)`; VCS porcelain listings | `vcs.rs:625` |
| `TrackingRef` | (1) `version:`, declared | `TrackingRef::parse(RawRefName)` | `vcs.rs:834` |
| `EphemeralRefName` | (2a) an ephemeral name **requested** | `EphemeralRefName::mint(&ProjectName, &WorkweaveName)` | `vcs.rs:956` |
| `OwnedRef` | (2b) an ephemeral ref **rwv holds a receipt for** | `RefRegistry::record_created(...)` or `RefRegistry::lookup(...)` | `vcs.rs:1058` |
| `AttachedRef` | (3) what a checkout is on | `Vcs::head_attachment(repo)` only | `vcs.rs:1178` |

`TrackingRef::parse` is where the §8.8 decision became executable: it runs the
git `check-ref-format` intersection (`validate_ref_name`, `vcs.rs:740-786`)
and then rejects SHA-shaped (`RefNameError::ShaShaped`, ≥7 all-hex) and
tag-shaped (`RefNameError::TagShaped`, `vN.N…`) input outright
(`vcs.rs:843-857`) `[V]`. Like `ResolvedRevisionId`, it has no `Deserialize`
impl. One caveat this document must not let slide: parsing happens at each
*consumer's* seam, not at manifest load, because `manifest.rs:573` still
carries `RefName` (§1.3, §2).

Notion (4) — "nothing" — is not a type. It is the absence of an `AttachedRef`,
and it is expressed by the enum in §4.5.

**Cross-type comparison is not implemented, in any direction.** No
`PartialEq<TrackingRef> for AttachedRef`, no `From<AttachedRef> for OwnedRef`,
no `Into<RawRefName>` for anything that would launder a value back to the
parse boundary. And critically: **`AttachedRef`, `TrackingRef`, and
`OwnedRef` expose no `as_str()`** — `Display` only. `RemoteDefaultBranch`
turned out to need the same treatment and got it `[V]`. This was
load-bearing, not stylistic: both comparison sites in `push.rs` were written
with `.as_str()` on both sides, so if these types carried `as_str()` both
lines would have compiled verbatim after the split and §4.6(2)'s error would
never have fired. An author who hit the cross-type error could have silenced
it by appending `.as_str()` — green build, no reviewer signal.

**What actually happened at those two sites**: neither survives. They were
rewritten as `!project_attached.is_named(&project_canonical)` (`push.rs:204`)
and `!attached.is_named(&declared.local_counterpart())` (`push.rs:406`) `[V]`
— `AttachedRef::is_named` (`vcs.rs:1197`) being the named predicate that
replaced string equality. The author had to state which projection was meant,
which is exactly what §4.6(2) predicted the error would force.

`RawRefName` keeps `as_str()`: it is the parse boundary, and raw porcelain
output must stay inspectable. Each removal is enforced by a `compile_fail`
probe per §4.7 — four of them, one per type.

The legal conversions are these, and only these. Each is a named function
whose body is the place a policy decision lives. All four shipped with the
signatures below — `parse` at `vcs.rs:843`, `on_remote` at `:861`,
`local_counterpart` at `:876`, `mint` at `:962`, `is_attached_by` at `:1113`,
and `RefRegistry`'s pair at `workweave_index.rs:665` / `:749` `[V]`:

```rust
// Rejects SHA-shaped and tag-shaped values: `version:` is a tracking
// declaration, not a pin. Rejects anything that is not a valid ref name
// component.
impl TrackingRef { pub fn parse(raw: RawRefName) -> Result<Self, RefNameError>; }

// The remote branch `version:` actually names. Already existed in spirit as
// Vcs::resolve_branch_on_remote (vcs.rs:1879-1895) — "in spirit" is load-
// bearing: its Role::Fork -> upstream claim is itself stale (§1.3).
impl TrackingRef { pub fn on_remote(&self, role: Role) -> RemoteRef; }

// "The local branch of the same name." NOT an identity — a projection across
// namespaces, and the doc comment is where the assumption is stated.
impl TrackingRef { pub fn local_counterpart(&self) -> LocalRefName; }

// Total. No third input, no failure, no `current_ref` read.
impl EphemeralRefName { pub fn mint(p: &ProjectName, w: &WorkweaveName) -> Self; }

// Intent -> receipt -> birth. record_created durably persists the receipt
// FIRST and returns the OwnedRef; create_worktree_on (§4.3) then performs
// the birth against it. Receipts are keyed by (canonical store, ref name)
// and always written BEFORE the ref they describe (§7.1): a crash leaves a
// dangling receipt (benign; doctor retracts a receipt whose ref never
// appeared), never an unreceipted ref (permanently disowned under R2).
impl RefRegistry {
    pub fn record_created(&mut self, store: &RepoPath, name: EphemeralRefName,
                          created_at: ResolvedRevisionId) -> OwnedRef;
    pub fn lookup(&self, store: &RepoPath, name: &RawRefName) -> Option<OwnedRef>;
}

// Named predicate, not an operator. Returns bool; does not yield a witness.
impl OwnedRef { pub fn is_attached_by(&self, a: &AttachedRef) -> bool; }
```

`AttachedRef` has **no** conversion out. It is a *witness*: a value whose
existence proves that, at the moment it was produced, **that repo's** HEAD
was symbolic. Unlike `ResolvedRevisionId` — which refines an immutable
value — an `AttachedRef` observes mutable per-repo state, so the
proposition it carries is bound to a place and can expire. It therefore
**carries its provenance**: `AttachedRef { repo: RepoPath, name: … }`, and
every operation that consumes the witness derives its target repo *from
the witness* rather than taking an independent `&Path`. Without that
binding, a witness obtained from one repo could authorize a MOVE on
another — exactly the cross-repo pass that used to be available at
`ff_advance_repo(target_repo, cwd_repo, cwd_tip)`, where the cwd repo is
always attached inside a workweave and the target may be detached.
`DetachedHead` carries its repo for the same reason. **Shipped, and closed at
the type level**: `advance_attached_ref(&AttachedRef, &ResolvedRevisionId)`
(`vcs.rs:2683`) takes no path, re-observes via
`head_attachment(witness.repo())` (`:2640`), and `ff_advance_repo` now
*obtains* the witness from the target rather than being handed one
(`sync.rs:5477-5506`) `[V]`. The dodge is a compile error, pinned by
`tests/branch_model_compile_fail_test.rs:551
a_witness_cannot_point_a_move_at_a_different_repo` and by
`tests/branch_model_test.rs:310
advance_attached_ref_refuses_a_witness_for_a_repo_that_became_detached`.
(The witness's validity *window* — what happens when the repo's state changes
between production and consumption — is Q15, §9, still open.)

Below, the remaining `RefName` sites in the trait, and the type each gets.
Four of the six are **not yet converted** — the implementation reached the
branch model's own surface and stopped at the edges, which is honest to
record. Two rows have since closed:

| Site | Type today | Under the split |
|---|---|---|
| `BranchAlreadyExists { branch }` (`vcs.rs:295`) | still `RefName` `[V]` | `RawRefName` — an error reports an observed name |
| `tag_at_head` (`vcs.rs:1947`) | still `Option<RefName>` `[V]` | `Option<RawRefName>` — a tag is not a branch; it never enters the branch model |
| `default_branch` | **deleted from `GitVcs` and from the trait** `[V]` | `Option<RemoteDefaultBranch>` — see below. Resolved not by retyping the method but by removing it, so the fabricating shape cannot be reintroduced by a future implementor. `Vcs::remote_default_branch` (`vcs.rs:3037`, impl `git.rs:2122-2137`) is now the only producer, and `rwv push`'s gate (`push.rs:179`), doctor's canonical pass (`check.rs:3736`), and two of `rwv add`'s three `version:` writes (`add_remove.rs:295-296`, `:394`) all read it. The third, `rwv add --new`, has no origin to read and resolves from `head_attachment` instead (`:693-703`) `[V]` — §6.2 |
| `branch_has_remote_counterpart` (`vcs.rs:2298`) | still `&RefName` `[V]` | `&RawRefName` — the prune predicate inspects observed names |
| `count_commits_ahead_of_remote` (`vcs.rs:2318`) | still `&RefName` `[V]` | `&RawRefName` — same predicate |
| `list_local_branches` | **deleted from `GitVcs` and from the trait** `[V]` | `Vec<RawRefName>` — resolved the same way as `default_branch`: not by retyping the qualified listing but by removing it, so a caller cannot reach a qualified name to hand-strip. `list_local_branch_names` (`vcs.rs:3205`, impl `git.rs:2259-2275`) is now the only local-branch listing, and its production caller, `prune_dropped_repo` (`sync.rs:1425`), takes the `RawRefName`s it returns and wraps each bare into a `RefName` — no `refs/heads/` prefix left to strip. Pinned by `tests/refs_heads_hand_strip_test.rs`, which fails if any file outside `git.rs` hand-strips that prefix |

**`RemoteDefaultBranch`** is the remote's own declaration of its primary
branch — the target of `refs/remotes/origin/HEAD`. It is none of §2's four
notions, and it is **not a fifth kind**: rwv never writes it, so it sits
outside the MOVE/ATTACH/DESTROY classification entirely — it is a
read-only *input* to the L1 publish gate (`push.rs:177-208`). It gets its
own type rather than reusing `RemoteRef` because provenance differs: a
`RemoteRef` is the projection of a *declared* `TrackingRef`; a
`RemoteDefaultBranch` is *observed* remote state. **Shipped in full**
(`vcs.rs:1331-1386`) `[V]`: its sole producer is
`Vcs::remote_default_branch(repo) -> Result<Option<RemoteDefaultBranch>, VcsError>`
(`vcs.rs:3037`, `GitVcs` impl `git.rs:2122-2137`) `[V]`, which returns `None`
when `origin/HEAD` is unset or malformed — **no fallback**, exactly as
specified — and `RemoteDefaultBranch::local_counterpart()` (`vcs.rs:1376-1378`)
`[V]` exists. **The publish gate is now wired to it** (`push.rs:179`), so a
weave with no `origin/HEAD` refuses instead of being told its branch is
`"main"` — §4.5's make-the-collapse-unrepresentable move, applied to this
value. **The rest of the wiring has since shipped too**, which is what this
paragraph used to list as outstanding: `default_branch` — the method that
fabricated `"main"` on any failure and on a malformed symref — no longer
exists, and `rwv add`'s three `version:`-writing sites read
`remote_default_branch` (refusing by name when it is `None`) or, on the
`--new` path that has no origin, `head_attachment` `[V]`. §6.2.

**`BornRef`** is proof of authorship: `create_worktree_on` returns one iff
*this call created* the ref (an adopted pre-existing ref yields none), and
its only consumer is rollback. §6.1's fix depends on exactly this: rollback
deletes only refs it holds a `BornRef` for, so it can no longer delete a
branch the create merely adopted. The receipt itself is written *before*
the birth by `RefRegistry::record_created` (above), so `BornRef` carries no
registry duty — it separates "authored" from "adopted", nothing more.
**Shipped**: `create_worktree_on` (`vcs.rs:2763-2779`) returns
`Option<BornRef>` from the adopt/author classification in
`materialize_worktree_on_ref` (`git.rs:1927-1935`), and `workweave.rs:1111`
turns it into a `RefBirth::{Authored, Adopted}` that rollback keys on
(`undo_ref_births`, `workweave.rs:656-735`) `[V]`.

The honest inventory, then: **five core types** (`RawRefName`,
`TrackingRef`, `EphemeralRefName`, `OwnedRef`, `AttachedRef`), **six
supporting types** (`RemoteRef` `vcs.rs:892`, `LocalRefName` `:923`,
`DetachedHead` `:1266`, `UnbornRef` `:1239`, `BornRef` `:1302`,
`RemoteDefaultBranch` `:1351`), one wrapper whose *policy* is deferred
(`PublishRef` `:1399`, §4.3, Q6), the consent tokens and warrants of §4.4, and
the `RefRegistry` (`workweave_index.rs:611`) — all present `[V]`. Not "five
types replace `RefName`" — five notions become five types, and the rest of the
trait surface is typed to match.

Implementation added four types this section did not anticipate, each earning
its place: `HeadObservation` (`vcs.rs:1448`, the VCS-specific half of
`head_attachment`), `LegacyEphemeralRefName` (`:1010`, so §7.1 arm 1 can name
the pre-flat shape without re-admitting a parser), `RefNameError` (`:652`), and
`SavepointRef` (`:1587`, the `DiscardWarrant`'s payload) `[V]`.

### 4.3 The `Vcs` signatures

The surface this model replaced — **all of it deleted; no line number is
given because none of these exists** `[V]`:

```rust
fn current_ref(&self, repo: &Path) -> Result<Option<RefName>, VcsError>;
fn checkout(&self, repo: &Path, revision: &ResolvedRevisionId) -> Result<(), VcsError>;
fn delete_branch(&self, repo: &Path, branch: &RefName) -> Result<(), VcsError>;
fn push_with_role(&self, repo: &Path, role: Role, force: bool) -> Result<(), VcsError>;
fn create_worktree(&self, repo: &Path, dest: &Path, branch_name: &RefName,
                   start_point: &ResolvedRevisionId) -> Result<(), VcsError>;
fn list_branches_with_prefix(&self, repo: &Path, prefix: &RefName)
    -> Result<Vec<RefName>, VcsError>;
fn restore_savepoint(&self, ...) -> Result<(), VcsError>;
```

Two neighbours survive because they are not branch-model operations:
`advance_if_fast_forward` (`vcs.rs`, impl `git.rs:1227-1234`) and
`hard_reset` remain on the trait as the mechanical primitives the typed MOVEs
call underneath. `advance_if_fast_forward` still takes a bare `&Path`, which
is why §4.6(1)'s argument is about the *caller's* obligation rather than
about that method's signature; `hard_reset` now has **zero** call sites in
`sync.rs` — `rewind_project_repo` (`sync.rs:4484-4521`) goes through
`reset_attached_ref` with a `DiscardWarrant` instead `[V]`.

The replacement, shipped (`vcs.rs`, decls at the line numbers noted):

```rust
// ---- observation -------------------------------------------------------
// Replaces current_ref. Total over the four states it used to collapse.
fn head_attachment(&self, repo: &Path) -> Result<HeadAttachment, VcsError>;  // :2615

// ---- MOVE --------------------------------------------------------------
// The AttachedRef parameter is a *witness*: proof the caller established
// attachment on the repo the witness was produced from. The target repo is
// derived FROM the witness — there is no independent &Path parameter, so a
// witness obtained from one repo cannot be used to move another: that pass
// is a type error at the call site and a provenance check in the impl.
// What is guaranteed: the MOVE lands on the repo whose attachment was
// actually observed. What is NOT guaranteed: that the attachment still
// holds at consumption time — the impl re-verifies and errors on a stale
// witness (the wider validity-window question is Q15, §9).
fn advance_attached_ref(&self, on: &AttachedRef,
                        to: &ResolvedRevisionId) -> Result<(), VcsError>;   // :2683
fn reset_attached_ref(&self, on: &AttachedRef,
                      to: &ResolvedRevisionId, warrant: DiscardWarrant)
    -> Result<(), VcsError>;                                                // :2698
// Moving a HEAD that is already detached is a MOVE, not an ATTACH —
// subject to the mid-operation precondition (§3.6). DetachedHead carries
// its repo, like AttachedRef.
fn advance_detached_head(&self, was: &DetachedHead,
                         to: &ResolvedRevisionId) -> Result<(), VcsError>;  // :2725

// ---- ATTACH ------------------------------------------------------------
// Birth: no consent token, because there was no prior attachment to lose.
// Takes the receipt (already persisted by RefRegistry::record_created,
// §4.2 — receipt-first ordering); the store, name, and start point all
// come from it. Returns Some(BornRef) iff this call authored the ref,
// None when it adopted a pre-existing one (§6.1's rollback keys on this).
fn create_worktree_on(&self, owned: &OwnedRef, dest: &Path)
    -> Result<Option<BornRef>, VcsError>;                                   // :2763
// Post-birth attachment change. Both take a consent token minted from the
// corresponding named flag (see §4.4 for where the tokens live).
fn detach_head(&self, from: &AttachedRef,
               to: &ResolvedRevisionId, consent: DetachConsent)
    -> Result<(), VcsError>;                                                // :2831
fn reattach_head(&self, from: HeadAttachment,
                 to: &LocalRefName, consent: ReattachConsent)
    -> Result<(), VcsError>;                                                // :2849

// ---- DESTROY -----------------------------------------------------------
// Receipt (OwnedRef) + warrant. No overload takes a name.
fn delete_owned_ref(&self, repo: &Path, branch: &OwnedRef,
                    warrant: DeletionWarrant) -> Result<(), VcsError>;      // :2898

// ---- publish -----------------------------------------------------------
// The ref is now a parameter. Policy leaves the VCS impl (see §9, Q6).
// PublishRef is an opaque wrapper whose ONLY constructor lives in push.rs,
// at the single decision site §4.6(2) creates; Q6 decides what that
// constructor accepts (the attached ref, the tracking counterpart, or both
// under a rule). Until Q6 closes, the constructor is the one place the
// open question is visible — a deferred decision with a producer, not a
// placeholder without one.
fn push_ref(&self, repo: &Path, role: Role, r: &PublishRef, force: bool)
    -> Result<(), VcsError>;                                                // :3027

// ---- listing -----------------------------------------------------------
// Returns raw observed names. Report-only by type: a RawRefName is not an
// OwnedRef, so nothing here can be deleted without a registry lookup.
// Named list_branch_names_with_prefix, to leave no spelling that resolves
// to the deleted method.
fn list_branch_names_with_prefix(&self, repo: &Path, prefix: &str)
    -> Result<Vec<RawRefName>, VcsError>;                                   // :3047
```

Implementation added several more members in the same shape, each because a
verb needed to say something the list above could not: `verify_attachment`
(`:2639`), `resolve_local_branch_tip` (`:2658`), `materialize_worktree_on_ref`
(`:2791`, the VCS-specific half of `create_worktree_on`), `clone_attached_at`
(`:2815`, birth-at-the-lock-revision for `fetch`), `destroy_local_ref`
(`:2912`), `rename_owned_ref` / `rename_local_ref` (`:2938`, `:2968`, §7.1 arm
1), `adopt_detached_checkout` (`:2990`, §7.1 arms 3/5), and `birth_ref_at_head`
(`:3017`) `[V]`.

**`Vcs::checkout` and `Vcs::delete_branch` were removed, not deprecated — and
that has now happened.** So were `current_ref`, `restore_savepoint`,
`create_worktree`, `push_with_role`, and `list_branches_with_prefix`. Every one
of their call sites had to state which replacement it meant, and that
restatement was the audit. The trait says so itself, at `vcs.rs:2586-2594`:
they "were deleted only once every call site had been restated in terms of
this surface — and that restatement was the audit: a site that could not say
which replacement it meant was a site nobody had classified" `[V]`.

The narrow case this section originally singled out came out as predicted:
`restore_savepoint` was an unverified `reset --hard` on the public trait with
zero callers, superseded by `verified_restore_savepoint`, and it went in the
same pass. The three it contrasted against — `checkout`, `delete_branch`,
`current_ref`, then at 3, 5, and 15 call sites — went too, once each of those
sites had an answer.

### 4.4 The consent and warrant tokens — **shipped**

Homes, as specified: the two ATTACH consents live in the CLI flag module
(`cli::consent`, `src/cli/consent.rs` — `DetachConsent` at `:50`,
`ReattachConsent` at `:81`), and the warrants live in `vcs.rs`
(`DiscardWarrant` `:1620`, `DeletionWarrant` `:1657` with the private
`WarrantKind` at `:1661`) `[V]`.

The "no minting caller yet" gap this section used to carry is closed, and
closed harder than this section asked for. The ask was that one module be the
only place a token is constructed. The private-field idiom delivered that
against the *tuple literal* — but a token has two construction routes, and
`from_flag` was the other one. While dispatch lived in `main.rs` it had to
stay open: a `[[bin]]` target is a separate crate from the `[lib]`, so the
narrowest visibility that admits it is `pub`, and a `pub fn` returning the
token is a second door standing open to every module of the library. Moving
dispatch into `cli::dispatch` is what let the visibility come down.
`from_flag` is now `pub(in crate::cli)` (`src/cli/consent.rs:71`, `:94`,
`:121`, `:149`), the mints are at `cli/dispatch.rs:310`, `:624`, `:817`,
`:821` and `:1015`, and a `from_flag` call written into `vcs.rs` — the module
this section names as the one that must only ever *receive* a token — is now
`E0624`, not a code-review finding `[V]`. The module header states both seals
and why dispatch had to move (`src/cli/consent.rs:1-41`).

**The home is forced by Rust's privacy rules, not chosen.** This placement
reads as a layering violation and has been re-opened as one: seven production
domain modules (`vcs`, `fetch`, `update`, `activate`, `workweave`, `check`,
`sync`) import from `crate::cli`, which is a cycle in the reference graph and
a domain module depending on the presentation layer. The enumeration below is
recorded here so the next reader meets a closed design space instead of
re-deriving it. Three requirements are in play:

- **A.** A domain module must not be able to construct a token.
- **B.** The flag-parsing module (`cli::dispatch`) must be able to.
- **C.** A domain module must not import from `crate::cli`.

A private field is visible in the declaring module *and its descendants*, so
for a token declared in module `M` the set of modules that can write the tuple
literal is exactly `{M, descendants of M}`. Requirement B forces the *minting
function* to be visible from `cli::dispatch`, and the only `pub(in P)`
spellings that exist are those where `P` is an ancestor of `M`. So if a seal
tighter than `pub(crate)` is to be available at all, `M` must be an ancestor
of `cli::dispatch` — leaving `crate::cli`, `crate::cli::dispatch`, or `crate`.
`crate` is an ancestor of every domain module, which breaks A. **A mint sealed
tighter than `pub(crate)` therefore requires the token to live in the
`crate::cli` subtree.** A, B and C are jointly unsatisfiable within one crate;
any two of the three are available.

Compiled rather than argued. Four standalone probes, `rustc --edition 2021
--crate-type lib`, each reduced to the one edge it measures:

```rust
// Shape B — token at crate root, mint a sibling: `pub(crate) fn from_flag`.
pub mod workweave {
    pub fn forge_by_literal() -> crate::consent::DetachConsent {
        crate::consent::DetachConsent(())               // E0603 — refused
    }
}
pub mod workweave {                                     // literal removed
    pub fn forge_by_mint() -> Option<crate::consent::DetachConsent> {
        crate::consent::DetachConsent::from_flag(true)  // COMPILES
    }
}

// Shape A — token under crate::cli, as shipped: `pub(in crate::cli) fn from_flag`.
pub mod workweave {
    pub fn forge_by_mint() -> Option<crate::cli::consent::DetachConsent> {
        crate::cli::consent::DetachConsent::from_flag(true)  // E0624 — refused
    }
}
pub mod cli { pub mod dispatch {                        // the positive control
    pub fn mint() -> Option<super::consent::DetachConsent> {
        super::consent::DetachConsent::from_flag(true)       // COMPILES
    }
} }
```

The shape-B literal and the shape-B mint are separate probes because a single
file aborts at the first error and never reports on the second, which would
leave the `COMPILES` claim untested. Shape A's dispatch arm is the control
that keeps `E0624` from being read as "the mint is reachable from nowhere":
without it, a token no module can construct satisfies A and C by failing B.

The literal seal is identical in both shapes — the codes differ only by
vantage. An in-crate sibling writing the tuple literal gets `E0603` (the
constructor is private); the four out-of-crate probes in
`tests/branch_model_compile_fail_test.rs` get `E0423` for the same seal, since
from outside the crate the constructor is not in scope to be named at all.
**The entire difference between the shipped design and a domain-level home is
one compile error: `E0624` becomes a silent success.** That is the whole
trade, and it is the trade `DiscardLocalCommitsConsent` took under duress —
which is why the paragraph below records its `pub(crate)` as the tightest seal
the language offers there rather than as a preference.

**The compile-fail suite cannot see that regression.**
`tests/branch_model_compile_fail_test.rs` probes from an *external* crate,
where `pub(crate)` and `pub(in crate::cli)` are indistinguishable — both are
simply "not `pub`". The four tuple-literal probes and
`the_flag_mint_is_not_reachable_from_outside_the_cli_module` all stay green
through a move to a crate-root `consent` while the in-crate guarantee vanishes
silently. Any change that loosens the mint must budget for a new in-crate
probe, because the existing suite will not report the loss.

**The two exits, and what triggers each.** Neither is wrong on the merits;
both are unpaid-for today, and each is written down with its trigger so the
day it becomes right is recognisable.

- **Hoist to a crate-root `consent`; widen the mint to `pub(crate)`.**
  *Trigger: a second in-crate frontend is real* — an `api` or `daemon` module
  that must mint legitimately and cannot be placed under `crate::cli`. Today
  nothing presses: plugins are external executables discovered on `PATH` with
  no library linkage, so a plugin needing a consented op runs `rwv` and passes
  the flag exactly as an operator does. The visibility change is one line and
  the rest is import rewrites, but it lands together with the in-crate probe
  the paragraph above requires, because the mechanism can no longer tell a
  second frontend from a domain module inventing consent — both are "some
  module in this crate". `DiscardLocalCommitsConsent` stops being an exception
  on that day and becomes the general case.
- **Domain-declared marker traits, frontend-declared proofs.** The domain
  names a capability (`pub trait DetachProof`) and each frontend's token
  implements it, so the concrete type is never spoken downstream and A, B and
  C are all satisfied at once. *Trigger: a third frontend* — the first point
  at which two tokens must satisfy one domain requirement without either
  weakening its own seal. The costs are paid up front and are not small.
  `Vcs` is consumed as `&dyn Vcs`, so the parameters must be `&dyn Proof`
  rather than `impl Proof` or the trait stops being dyn-compatible, which
  turns a `Copy` zero-sized token into a reference with a lifetime and forces
  the per-worker copy that `Copy` exists for to be re-established through the
  borrow. Worse, a `pub` trait is implementable by anyone: `struct Yes; impl
  DetachProof for Yes {}` inside `workweave.rs` is two lines and compiles.
  Sealing the trait needs a private supertrait in the domain, which the CLI
  then cannot implement — the same wall as above. So this exit refuses forgery
  at neither the literal nor the mint, and substitutes a reviewable `impl`
  plus a source scan in the style of `tests/destructive_ops_audit_test.rs`.

`tests/consent_minting_audit_test.rs`, the static call-site allowlist that
stood in for the compiler, **no longer exists** — deleted, on the grounds
that an invariant checked by construction should not also carry a tripwire.
What it pinned is now either a compile error or documented where a reader
will meet it. The "not `pub`" half is pinned by error code like the rest, by
`the_flag_mint_is_not_reachable_from_outside_the_cli_module`
(`tests/branch_model_compile_fail_test.rs:437`) `[V]`; the in-crate half of
`pub(in crate::cli)` is not observable from an out-of-crate probe, and the
harness does not claim it is. `granted()` — the unconditional mint, which
checks nothing — is `#[cfg(test)]` on three of this section's four tokens
(`src/cli/consent.rs:58-59`, `:86-87`, `:110-111`), so it is absent from the library
the binary and the integration tests link against; `AdoptDetachedConsent`
lost its `granted()` outright, because no fixture needed one `[V]`.

Two consent types shipped beyond this list, both needed by §7: 
`DiscardUnmergedConsent` (`src/cli/consent.rs:105`, the `workweave delete`
override R3's `OperatorDiscarded` warrant is minted from) and
`AdoptDetachedConsent` (`src/cli/consent.rs:140`, §7.1 arms 3/5). A third, `DiscardLocalCommitsConsent`
(`vcs.rs:1568`), deliberately lives in `vcs.rs` rather than `cli::consent`,
because `sync --continue` must re-mint it from the persisted owner record
rather than from a flag on the resuming invocation `[V]`. That exception now
states its cost rather than only its reason (`vcs.rs:1555-1566`): the layer
holding both spellings of the flag is `sync.rs`, which is a *sibling* of
`vcs.rs`, not a descendant — and `pub(in path)` requires an ancestor, so no
visibility tier names it. `pub(crate)` is the tightest seal the language
offers here, and it admits every module of the crate. The one production mint
is `sync::rewind_project_repo` (`sync.rs:4495`), which is instructed to
thread the token rather than let a second mint appear `[V]`.

```rust
/// Proof that the operator consented to leaving a checkout on no branch.
/// Minted from `--detach-checkouts`. Defined in the CLI layer's flag
/// module — the only place that can construct it, via the private-field
/// idiom — and passed down; `vcs.rs` takes it as an opaque parameter.
/// (It cannot be "defined in vcs.rs but minted only by the CLI": within
/// one crate a `pub fn` constructor is callable from anywhere, and a
/// sealed type with no constructor is mintable nowhere. Home in the flag
/// module is what makes the minting story compile.)
pub struct DetachConsent(());

/// Proof that the operator consented to moving a checkout from one branch
/// to another. Same home, same idiom. Minted from `--reattach-checkouts`.
pub struct ReattachConsent(());

/// Proof that a rewinding MOVE (non-fast-forward) may proceed: a savepoint
/// under refs/rwv/pre-op/* has been written AND the operator passed the
/// verb's named override (e.g. --discard-local-commits). Constructed only
/// by the savepoint-writing path, so a rewind without a savepoint is
/// unrepresentable. This is the token §3.2's MOVE row requires.
pub struct DiscardWarrant { savepoint: SavepointRef }

/// Why this ref is safe to destroy now. An opaque struct over a PRIVATE
/// enum: a `pub enum`'s variant constructors cannot be made private in
/// Rust, so the §3.3 warrants are minted exclusively by `pub fn` checkers
/// that RUN the check they certify.
pub struct DeletionWarrant(WarrantKind);            // field private
enum WarrantKind {
    Unmoved { recorded_tip: ResolvedRevisionId },
    Merged { baseline: ResolvedRevisionId },
    OperatorDiscarded,
}
impl DeletionWarrant {
    /// Some(_) iff the ref's current tip equals the receipt's recorded tip.
    pub fn unmoved(vcs: &dyn Vcs, r: &OwnedRef) -> Option<Self>;
    /// Some(_) iff the ref's tip is an ancestor of `baseline`.
    pub fn merged(vcs: &dyn Vcs, r: &OwnedRef,
                  baseline: &ResolvedRevisionId) -> Option<Self>;
    /// The operator passed --discard-unmerged-commits.
    pub fn operator_discarded(consent: DiscardUnmergedConsent) -> Self;
}
```

A note on direction, replacing a miscited precedent from an earlier draft:
`VerifiedRestoreOutcome` (`vcs.rs:539-587`) is **not** the shape being
copied here. It is only ever a *return* type (`vcs.rs:2260`,
`git.rs:1324-1405`, `sync.rs:4981`) `[V]` — the fencing it describes lives
*inside* `verified_restore_savepoint`, and nothing consumes it as
authorization. `DeletionWarrant` deliberately **inverts** that direction:
it is caller-supplied proof, because the destroy site (`delete_owned_ref`)
and the check sites (the registry, the merged-check) are different code.
That inversion is exactly why constructibility is load-bearing here when
it is not there — and why the private-enum shape above is a requirement,
not a style choice.

Flag naming follows the house rule: **escape hatches are named for the
precondition they waive, never a bare `--force`**. `--detach-checkouts` and
`--reattach-checkouts` name two categorically different consequences —
losing the name your commits hang off, versus moving which name they hang off
— and so are two flags, not one `--change-attachment`. This is the same split
already decided for `workweave delete --force` → `--discard-uncommitted` +
`--discard-unmerged-commits`.

**Where each flag attaches to a verb — decided, and built as decided.**
`--detach-checkouts` names its verbs directly in §5's table (`fetch`,
`update`). `--reattach-checkouts` did not appear in §5 at all, and a reader
implementing it from §5 alone had no verb to attach it to. Resolved by
derivation, not invention: §7.2's `Detached` arm is the only reattach site
this whole model specifies, so it is the only verb the flag *can* attach to.
`--reattach-checkouts` is a flag on `rwv doctor`, gating whether `--fix`
*performs* the §7.2 Detached-arm reattach; without it, `--fix` keeps doing
exactly what §7.2 already specifies — reporting the detached canonical with
the correct `git switch` spelling. This is strictly additive to §7.2: the
report path is unchanged, and only the mutating path gains the named consent
R1 requires. It decides none of Q6, Q7, Q10, Q12–Q15.

**Verified against what shipped**, since a recorded decision that the code
contradicted would be worse than no decision: `--reattach-checkouts` is
declared on `doctor` (`cli.rs:186-191`), minted at `cli/dispatch.rs:817`,
threaded into `run_check` (`check.rs:6858`), and consumed at `check.rs:7492-7516`,
where the `--fix` path calls `fix_detached_canonicals` **only** when the
consent is present. `fix_detached_canonicals` (`check.rs:5118-5183`)
re-observes `head_attachment`, requires both halves of §7.2's condition, and
calls `Vcs::reattach_head` with the token `[V]`. The report path is unchanged.
Pinned by `tests/branch_discipline_test.rs:657
detached_canonical_reattaches_only_with_consent`, which is a non-vacuity pair:
with the flag it reattaches, without it the store stays detached.

`--detach-checkouts` (`cli.rs`, `DetachConsent` `src/cli/consent.rs:50`) is consumed by
`fetch`'s and `update`'s realign paths as §5's table says. A third flag,
`--adopt-detached-checkouts` (`cli.rs:192-199`, `AdoptDetachedConsent`
`src/cli/consent.rs:140`),
gates §7.1's arms 3 and 5 — it is named for what it does to a legacy branch's
tip rather than to an attachment, which is why it is not a spelling of
`--reattach-checkouts` `[V]`.

### 4.5 Q9: "no current branch" is not one state

The survey's Q9 asks whether "no current branch" is one state or four.
`current_ref`'s `Err(_) => Ok(None) // detached HEAD` collapsed four distinct
conditions into a single `None`, and the verified consequence was `rwv push`
reporting "is on a detached HEAD" for a `projects/<name>/` that is not a git
repo at all `[S]`.

The fix was not a check. It was making the collapsed states unrepresentable
(**shipped**: this section's `HeadAttachment` enum, `head_attachment`, and
the `NotARepo`/`CommandFailed` typing below are `observe_head` +
`head_attachment`, `vcs.rs:2606`/`:2615` → `git.rs:1840-1883`, word-for-word
the design this section specifies — down to the impl's own doc comment
independently landing on "where the question is actually asked", the same
phrase two paragraphs below `[V]`):

```rust
/// What HEAD is, in a workspace that is known to be a repo.
pub enum HeadAttachment {
    /// HEAD is symbolic and the branch has at least one commit.
    Attached(AttachedRef),
    /// HEAD is symbolic but the branch has no commits yet. Carries the
    /// branch name, because `git symbolic-ref --short HEAD` succeeds here.
    /// Distinct payload type, deliberately: an UnbornRef is NOT an
    /// AttachedRef, so it cannot be passed to advance_attached_ref. MOVE
    /// semantics on an unborn HEAD are undefined (git's ff-merge fails
    /// while `reset` would stamp the branch), so the model makes the call
    /// unrepresentable: a MOVE on an unborn HEAD is an error, not a state.
    Unborn(UnbornRef),
    /// HEAD is not symbolic. Carries the commit, so callers that want to
    /// MOVE a detached HEAD have their witness.
    Detached(DetachedHead),
}
```

The two remaining conditions are **errors, not states**, and both already have
`VcsError` variants: `VcsError::NotARepo(PathBuf)` (`vcs.rs:291`) for "this
directory is not a repo", and `VcsError::CommandFailed { args, repo, stderr }`
(`vcs.rs:339-344`) for "the ref database is unreadable" `[V]`. So:

```rust
fn head_attachment(&self, repo: &Path) -> Result<HeadAttachment, VcsError>;
```

is total, and every caller's `match` is exhaustive. Four states become three
enum variants plus two typed errors, and `Ok(None)` — the value that meant all
four — does not exist.

**Shipped, and not quite where this paragraph originally said it would land.**
`head_revision` (`git.rs:824-856`) no longer inlines the "ambiguous argument"
catch and a `symbolic-ref --short HEAD` re-run; it delegates the unborn
classification to `self.head_attachment(repo)` and only *renders* the result
(`git.rs:834-837`) `[V]`. And the real detector, `observe_head`
(`git.rs:1840-1883`), deliberately does **not** use `--short`: its own doc
comment explains that `--short` answers `heads/main` instead of `main` when a
same-named tag exists, which does not round-trip through `refs/heads/<name>`
— a correctness fix beyond what this paragraph asked for, not just a move
`[V]`. Unborn detection: `symbolic-ref HEAD` succeeds, then `rev-parse
--verify HEAD^{commit}` fails (`git.rs:1855-1866`) `[V]`.

**Direct consequences of Q9's answer — all four are now shipped.** Three were
listed as future work when this section was written; the fourth was already
done:

- `rwv push` against a non-repo reports `NotARepo`, because the non-repo case
  never reaches the detached branch of the match. `observe_head` checks
  `is_repo` first and returns `NotARepo` before anything else
  (`git.rs:1845-1847`) `[V]`.
- `rwv doctor`'s canonical scan gained its missing arm mechanically: the
  `Ok(None)` that matched nothing is a `match` the compiler forces to cover
  `Detached`. `check.rs` matches on `HeadAttachment` at several sites now,
  each one exhaustive over `Attached` / `Detached` / `Unborn` by construction —
  a call site cannot silently drop `Detached` again the way the old
  `Ok(None)` collapse did, because nothing compiles until every arm is
  written.
  Note which arm is the silent one: `Unborn`, deliberately, and
  reported separately as `UnbornCheckout` by the workweave pass. See §6 item 2.
- `rwv lock`'s detached-HEAD warning says which of unborn / detached it saw
  instead of inferring from `.ok().flatten()`: it matches all three arms
  (`lock.rs:113-119`, warning at `:139-147`), takes the SHA from the
  `DetachedHead` witness rather than from `version`, deliberately says nothing
  on `Unborn` (deferring to `head_revision`'s named refusal), and turns the
  read error into a hard refusal naming the repo rather than silence `[V]`.
- Doctor's remediation string stopped being wrong: with a registry lookup it
  knows whether the recorded ref exists, so `reattach_advice`
  (`check.rs:5477-5490`) emits `git switch <branch>` when it does and reserves
  `git switch -c <branch>` for when it does not `[V]`. The receipt reaches it
  as a `recorded_ref: Option<String>` on the finding (`check.rs:881`, `:900`,
  `:915`).

### 4.6 Wrong lines that became compile errors

Each item below states the line that used to compile, and what the tree does
now. All six landed.

**(1) Landing onto a detached target.** `ff_advance_repo` first gained a
**runtime** refusal — commit `62af89f`, ahead of and independently of this
model, made it read the target's `current_ref` and bail before ever calling
`advance_if_fast_forward`. That closed the #1 data-loss
chain's observable symptom, described and reproduced with real SHAs in §2.1.
**What it did not do is what this section is about**: the check was a value
the author had to remember to write at one call site, not a type the compiler
enforces at every call site. Nothing stopped a *second* caller of
`advance_if_fast_forward` from skipping it, because the method takes a bare
`&Path`:

```rust
// the shape this section objected to — still compiles, and still does not
// require having established an attachment
vcs.advance_if_fast_forward(&target_repo, &source_tip)?;
```

**Shipped, by relocating the obligation rather than the signature.**
`advance_if_fast_forward` still exists with its bare `&Path` (it is the
mechanical primitive underneath). What changed is that `sync.rs` no longer
calls it for a landing: `ff_advance_repo` matches `head_attachment` on the
**target**, binds the `Attached` arm's witness, refuses `Detached` and
`Unborn` by name, and hands the witness to `advance_attached_ref(&on,
cwd_tip)` — which takes no path at all (`sync.rs:5477-5506`, MOVE at
`:5550-5552`) `[V]`. The function's own doc comment states the rule:
`target_repo` is where the witness is *obtained*; it is never handed to the
MOVE.

So the dodge this paragraph named — obtain a witness from the *cwd* repo,
always attached inside a workweave, and use it while operating on the target
— is a compile error, not a review finding:
`tests/branch_model_compile_fail_test.rs:551
a_witness_cannot_point_a_move_at_a_different_repo` asserts `E0061` on
`advance_attached_ref(cwd_witness, target_repo, to)` `[V]`. The runtime
refusals are pinned by `sync.rs:5659
ff_advance_repo_refuses_to_land_onto_a_detached_target` and `:5635
ff_advance_repo_lands_on_the_branch_the_target_is_attached_to`, and the
end-to-end behaviour by `tests/sync_to_test.rs:775
sync_to_advances_the_target_branch_not_just_head`.

This is Q4's answer, obtained for free: **sync-to advances the ref the target
is attached to, and refuses when the target is detached.**

One honest residual: `ff_advance_repo` itself still takes
`(target_repo: &Path, cwd_repo: &Path, cwd_tip: &ResolvedRevisionId)`
(`sync.rs:5458-5462`) `[V]`. A caller can hand it two arbitrary paths; what
cannot be routed around is the *obtaining* of the witness inside. The
compiler enforces the MOVE's target, not the function's argument order.

**(2) Comparing a declared branch with an observed one.** The member gate
(now `push.rs:359-419`) used to read:

```rust
// the shape this section objected to — and the site of push's two
// contradictory policies
if branch.as_str() != entry.version.as_str() {
    eprintln!("rwv push: warning: {} is on branch '{}', manifest declares '{}'", ...);
}
```

Both shipped lines compared through `.as_str()` on both sides — that one, and
its project-repo sibling `project_current.as_str() != project_canonical.as_str()`.
That is why §4.2 removes `as_str()` from `AttachedRef`, `TrackingRef`, and
`OwnedRef` entirely: with it, **both lines would have compiled verbatim after
the split** and this section's error would never have fired. Without it, the
`.as_str()` calls are `E0599` (no such method), and the direct comparison does
not typecheck either:

```
error[E0277]: can't compare `AttachedRef` with `TrackingRef`
   --> src/push.rs
    |
    |     if branch != entry.version {
    |               ^^ no implementation for `AttachedRef == TrackingRef`
    |
    = note: `TrackingRef` names a branch on a remote; `AttachedRef` names a
            local ref. Project with `TrackingRef::local_counterpart()` or
            resolve with `TrackingRef::on_remote()` — the choice is the
            publish-ref decision.
```

The author must write `entry.version.local_counterpart()` and, in doing so,
state the assumption that the local branch of the same name is the tracking
branch's counterpart. That is Q6 becoming visible at one line instead of being
implicit at three.

**That is what the author wrote.** Both sites are now named predicates over
the projection: `!attached.is_named(&declared.local_counterpart())`
(`push.rs:406`) and `!project_attached.is_named(&project_canonical)`
(`push.rs:204`) `[V]`. The policy itself is unchanged and deliberately so —
the member gate still only warns and pushes anyway (`push.rs:406-412`), which
is Q6, still open. What changed is that the assumption is now written down at
the line that depends on it. `PublishRef::from_attached` (`push.rs:213`,
`:418`) is the single decision site the split created; the alternative
answer, `PublishRef::from_local`, exists unused (`vcs.rs:1419`) so that
choosing it later is a one-line change at one place `[V]`.

**(3) Deleting a ref you only recognised.** `create_worktree`'s retry:

```rust
// the shape this section objected to — verified destroying a branch carrying
// a unique commit, no --force, nothing printed [S]
let deleted = Self::run(&["branch", "-D", branch], repo).is_ok();
```

That line no longer exists anywhere in the tree `[V]`. Expressed through the
trait it would have been `delete_branch(repo, &branch)` where `branch:
&EphemeralRefName` (the name this create *requested*):

```
error[E0308]: mismatched types
   --> src/git.rs
    |
    |     self.delete_owned_ref(repo, branch, warrant)?;
    |                                 ^^^^^^ expected `&OwnedRef`,
    |                                        found `&EphemeralRefName`
    |
    = note: an `EphemeralRefName` is a name this operation asked for; an
            `OwnedRef` is a receipt rwv persisted. Obtain one with
            `RefRegistry::lookup`.
```

And even with a receipt, the second argument is still missing:

```
error[E0061]: this method takes 4 arguments but 3 arguments were supplied
    = note: argument #4 of type `DeletionWarrant` is missing
```

The retry could only proceed by looking the name up in the registry *and*
obtaining a warrant from `DeletionWarrant::unmoved` (§4.4), which returns
`Some` only when the tip equals the recorded tip. **The outcome went one step
further than this section asked**: rather than gate the destructive retry, the
successor `materialize_worktree_on_ref` (`git.rs:1908-1945`) classifies first
and **adopts** the colliding ref (`:1927-1935`), returning `None` so the caller
learns it did not author it. The false audit claim — "deletes a **stale**
ephemeral branch left by a previous failed create" — was not made true; it was
**retired**, and the audit entry says so (§2.2) `[V]`.

**(4) Deleting a whole prefix.** `workweave delete`'s cleanup used to read:

```rust
// the shape this section objected to — destroys a prefix-scoped SET while the
// merged-check vouched for a HEAD-scoped SINGLETON
for b in vcs.list_branches_with_prefix(&repo, &prefix)? {
    let _ = vcs.delete_branch(&repo, &b);
}
```

`list_branch_names_with_prefix` returns `Vec<RawRefName>`, so the loop can only
*report*:

```
error[E0308]: mismatched types
    |     let _ = vcs.delete_owned_ref(&repo, &b, warrant);
    |                                          ^^ expected `&OwnedRef`,
    |                                             found `&RawRefName`
```

**Shipped as `retire_recorded_refs`** (`workweave.rs:2421-2503`): it ranges
over `RefRegistry::lookup` hits (`:2432`), requires a `DeletionWarrant::merged`
or `operator_discarded` for each (`:2444-2449`), refuses by name when neither
holds (`:2468-2472`), retracts the receipt only after the ref is gone
(`:2452-2454`), and **reports** everything else in the namespace via
`list_branch_names_with_prefix` (`:2485-2500`) `[V]`. `RefRegistry::lookup`
returns `None` for a hand-made branch, so `my--feature/wip` and
`dependabot--npm/lodash` stopped being rwv's property; the compile-fail probes
at `tests/branch_model_compile_fail_test.rs:332`, `:348`, and `:362` hold the
three routes (a listed name, a requested name, a receipt without a warrant)
shut. `parse_ephemeral_branch_name` — the function that made name shape into
ownership — was deleted outright, exactly as this paragraph proposed; its
successor `looks_like_a_pre_flat_ref` (`check.rs:3218-3228`) returns a `bool`
and feeds one report-only finding, never a delete `[V]`. The same argument
applied to `doctor --fix`'s safe-class deletions, which now re-resolve through
the registry (`fix_stale_ephemeral_branches`, `check.rs:4619-4713`) `[V]`.

**(5) Silently detaching in `fetch` / `update`.** Both used to read:

```rust
// the shape this section objected to — detaches every repo it touches,
// including repos already at the target SHA [S]
if let Err(e) = git.checkout(&dest, &resolved) { ... }
```

```
error[E0599]: no method named `checkout` found for struct `GitVcs`
    |
    = note: `Vcs::checkout` was removed. It could not express the difference
            between moving the ref you are on and abandoning it. Use
            `advance_attached_ref`, `advance_detached_head`, or
            `detach_head` (which requires `DetachConsent`).
```

There is no way to restore the old behaviour without either producing an
`AttachedRef` (and thereby moving the branch instead of abandoning it) or
threading a `DetachConsent` up to a named CLI flag. R1 holds by construction.

**Shipped.** `fetch`'s `realign_present_clone` (`fetch.rs:717-809`) and
`update`'s `advance_checkout` (`update.rs:620-728`) both match
`head_attachment` and take one of four exits: no-op when already at the pin,
`advance_detached_head` for an already-detached HEAD (a MOVE, §3.6),
`advance_attached_ref` for a fast-forward of the tracking counterpart, or a
refusal naming `--detach-checkouts` — which routes to `detach_head` with a
real `DetachConsent`. `Unborn` is its own refusal `[V]`. The compile-fail
probe `tests/branch_model_compile_fail_test.rs:417` holds the
consent-required detach shut.

**(6) Minting an ephemeral name from something observed.** All three
derivation sites built the name from a third component:

```rust
// the shape this section objected to — three different third arguments,
// one of them wrong
let ephemeral_branch = ephemeral_branch_name(project, name, &branch_segment);
```

`EphemeralRefName::mint(&ProjectName, &WorkweaveName)` takes two arguments and
neither can be an `AttachedRef` or a `TrackingRef`:

```
error[E0061]: this function takes 2 arguments but 3 arguments were supplied
```

The disagreement was not resolved; it was deleted. **`ephemeral_branch_name`
did not lose a parameter — the function is gone**, and the inlined copy in
`add_remove.rs` and the `version:`-based variant in `sync.rs` collapsed into
one call to `mint` (`workweave.rs:1439`, `add_remove.rs:94`, `sync.rs:1276`)
`[V]`. Two compile-fail probes hold it: `:380` (arity) and `:398` (no observed
input can be an argument).

### 4.7 Enforcing the invariant the way the precedent does

Ship the split with `compile_fail` enforcement on the type-level docs,
mirroring `vcs.rs:149-157` `[V]` — one per illegal comparison, one per illegal
construction, and one for the `as_str()`-laundered comparison
(`branch.as_str() != entry.version.as_str()`), which must fail with `E0599`
once those types are `Display`-only. `compile_fail` is the only form of this
enforcement that survives a refactor, because it fails in CI when someone
re-adds the `PartialEq` or the `From` impl to "make it easier".

**Shipped, in two layers.** Seven `compile_fail` doctests live on the ref
types themselves — `vcs.rs:149` (`RawRevisionId`, the §4.1 precedent), `:813`
and `:826` (`TrackingRef`: cross-type `==`, and `as_str` laundering), `:1156`
and `:1171` (`AttachedRef`: field forgery, and `as_str`), plus
`workweave_index.rs:583` (a `RawRefName` cannot be `record_created`) and
`:602` (a `WorkweaveIndex` cannot be struct-literal-forged around
`RefRegistry`) `[V]`. Two more now sit on the *name* types `mint` consumes —
`manifest.rs:125` and `:207`, holding that `ProjectName::new` /
`WorkweaveName::new` cannot be treated as infallible `[V]`; those arrived with
Q12's answer (§9) and are the reason `mint`'s totality is now a property
rather than an assertion.

Above them sits a dedicated harness, `tests/branch_model_compile_fail_test.rs`
(580 lines), which shells out to `rustc` against the built rlib and asserts
the *specific* error code: **23 probes plus one sanity check** that a legal
snippet still compiles — the check that keeps the other 23 from passing
vacuously `[V]`. The probes cover cross-type comparison (`:142`), the four
`as_str()` removals (`:162`, `:180`, `:219`, `:233`), laundering (`:198`),
witness/receipt/warrant forgery (`:253`, `:268`, `:287`), a MOVE on an
`UnbornRef` (`:311`), the three deletion routes (`:332`, `:348`, `:362`),
`mint`'s arity and its refusal of observed input (`:380`, `:398`),
consent-required detach (`:417`), the unreachability of the consent *mint*
from outside `crate::cli` (`:437` — added when §4.4's second construction
route was sealed), **four** consent tuple-literal forgeries (`:474`, `:488`,
`:502`, `:516` — the last being `AdoptDetachedConsent`, the twenty-third
probe, added after the previous citation base), the warrant argument on a
rewind (`:533`), and the cross-repo witness (`:551`).

Also add the assertion shape the suite is missing. **Partly done, and the
remainder is worth stating precisely.**
`in_place_fetch_leaves_present_member_at_locked_sha_unmoved`
(`tests/fetch_in_place_test.rs:290-325`) still cannot catch a detach: its
fixture pre-detaches the repo via `materialize_repo_at` (`:169`) and asserts
only `rev-parse HEAD` equality, with no `current_branch` check `[V]`. That
single gap is unchanged. What did change is everything around it: the sibling
this section pointed at as proof the primitive existed —
`in_place_fetch_realigns_a_present_member_and_detaches_its_branch` — no longer
exists, because fetch no longer detaches. It was replaced by its inverse,
`in_place_fetch_fast_forwards_the_counterpart_and_stays_attached` (`:335-368`),
which asserts both that the checkout is still on `main` *and* that `main`
itself moved, and by eleven siblings covering refuse-rewind,
refuse-unrelated-branch, the `--detach-checkouts` waiver, the
already-detached MOVE, and the mid-operation refusal `[V]`. `current_branch`
(`:223`) is the shared helper. So the assertion shape this section called for
is now the file's normal idiom; one older fixture predates it.

---

## 5. Consequences, verb by verb

These are **derived** from §3, not additional policy. Each row shows the ref
writes the verb performs, their classification, and what changed. **Every row
has landed**; the "Outcome" column is now a description of the tree, and each
one names where.

| Verb | Ref writes | Kind | Outcome |
|---|---|---|---|
| `workweave create` | mints `{project}--{workweave}`, attaches worktree | birth | Legal, unchanged in spirit. Name lost `<segment>`. Name uniqueness is checked against the workweave index before the directory check (`workweave.rs:1215-1243`) `[V]`; the receipt is written before the ref, and `birth_ephemeral_worktree` (`:1029-1132`) classifies the four `(receipt, ref)` states rather than force-deleting on collision (`:1055-1104`) `[V]`. Test: `tests/branch_model_lifecycle_test.rs:359`. |
| `sync` (ff / rebase) | advances the ref the CWD checkout is on (`apply_strategy`, `sync.rs:722-778`) `[V]` | MOVE | Legal, unchanged. |
| `sync --discard-local-commits` | rewinds the current ref | MOVE | **Now typed.** `hard_reset` has zero call sites in `sync.rs`; `rewind_project_repo` (`sync.rs:4484-4521`) mints a `DiscardWarrant` from the savepoint (`:4485`, `:4495`) and calls `reset_attached_ref` (`:4502`) or `advance_detached_head` (`:4512`), bailing on `Unborn` `[V]`. The savepoint-plus-named-loss shape §3.2 requires of every rewinding MOVE is now a property of the kind, not of this verb. |
| `sync-to` (landing) | advances the *target's* ref (`ff_advance_repo`, `sync.rs:5458-5554`) `[V]` | MOVE | **Shipped.** `62af89f` added the runtime refusal; this model replaced it with the witness (`:5477-5506`), closing the "landed onto nothing, then deleted the only ref" chain by construction — see §4.6(1) for the one residual (the function's own argument list). |
| `abort` | resets the current ref to the savepoint (`sync.rs:4975-5003` → `git.rs:2200-2210`) `[V]` | MOVE | Legal, unchanged. Already verified attributability (`git.rs:1324-1405`) `[V]`. |
| `fetch` (present clone) | `realign_present_clone` (`fetch.rs:717-809`) `[V]` | ATTACH | **Shipped as specified.** On the tracking counterpart: no-op when equal (`:782-784`), `advance_attached_ref` when an advance (`:789-792`), refuse-or-`detach_head` on a non-fast-forward (`:794-808`). Attached to any *other* ref: refuses, naming `--detach-checkouts` (`:763-776`, §5.3). Already-detached repos stay detached via `advance_detached_head` (`:735`), subject to §3.6. `Unborn` is its own refusal (`:737-745`). |
| `fetch` (absent clone) | `clone_attached_at` (`fetch.rs:946-952`) `[V]` | birth | **Shipped as specified**, and by a better route than the row predicted: rather than clone-then-align, the birth is a single call that attaches at the lock revision (R1's birth-target rule), so bootstrapping a weave from a lock behind origin performs no MOVE and needs no consent. `clone_with_role` survives only for the additive path where the lock has no entry (`fetch.rs:962`) `[V]`. |
| `update` (canonical, on a branch) | `advance_checkout` (`update.rs:620-728`) `[V]` | ATTACH | **Shipped.** Fast-forwards the attached ref when it is the tracking counterpart (`:687`, `:709-711`, §5.3); refuses a non-fast-forward naming the two exits — reconcile yourself per §8.7, or `--detach-checkouts` (`:714-728`). |
| `update` (inside a workweave) | the workweave arm of `advance_checkout` (`update.rs:655-671`) `[V]` | ATTACH | **Q8 answered and shipped:** advances the ephemeral ref when that is a fast-forward; refuses otherwise and points at `rwv sync` — deliberately *without* offering `--detach-checkouts`, since detaching a workweave checkout has no meaning R1 would sanction. No longer a detach. |
| `lock` | none (reads HEAD) | — | **Shipped.** Matches all three `HeadAttachment` arms (`lock.rs:113-119`), warns from the `DetachedHead` witness (`:139-147`), and refuses by name on an unreadable ref database instead of falling silent `[V]`. |
| `push` (project repo) | none (reads) | — | **Shipped.** The gate survives; the non-repo case reports `NotARepo` (§4.5), the canonical branch comes from `remote_default_branch` rather than a fabricated `"main"` (`push.rs:179`), and the mismatch test is `AttachedRef::is_named` (`:204`) `[V]`. |
| `push` (member repo) | none (reads) | — | **Shipped.** The publish ref is an explicit `&PublishRef` argument to `Vcs::push_ref` (`vcs.rs:3027`, impl `git.rs:2104-2119`) instead of an implicit `current_ref` read inside the impl `[V]`. Test: `git.rs:3029 push_ref_publishes_the_ref_it_was_given_not_the_one_head_is_on`. **Q6 stays open** — the split relocated the decision, it did not make it, and `PublishRef::from_attached` is the shipped choice. |
| `workweave delete` / `sync-to --retire` | `retire_recorded_refs` (`workweave.rs:2421-2503`) `[V]` | DESTROY | **Shipped.** Deletes recorded refs with a `Merged` (or `OperatorDiscarded`) warrant (`:2444-2449`); **reports** everything else in the namespace (`:2485-2500`). The set/singleton mismatch is gone because both the check and the deletion range over the recorded set. Tests: `tests/branch_model_lifecycle_test.rs:161`, `:231`, `:305`. |
| `doctor --fix` (stale ephemerals) | `fix_stale_ephemeral_branches` (`check.rs:4619-4713`) `[V]` | DESTROY | **Shipped.** Recorded refs only, re-resolved through the registry and re-warranted at fix time (`:4657-4690`); hand-made look-alikes survive. Tests: `tests/branch_discipline_test.rs:1028`, `:1080`. |
| `prune_dropped_repo` | removes the worktree; on `Checkout::Primary`, removes the entire store (`sync.rs:1392-1534`) `[V]` | DESTROY-STORE | **Unchanged at the ref level, gated at the store level.** The local-only refusal (`sync.rs:1478-1519`) is unchanged, and recorded rwv refs were deliberately *not* excluded from it: **unblocking prune is not a payoff of R2.** What changed is that the refusal is no longer the only thing standing between a live workweave's backing and `remove_dir_all` — `check_store_unclaimed` (`sync.rs:1311`, called at `:1530`) implements R4 directly. Tests: `sync.rs:5734`, `:5745`, `:5773`, `:5808`, `:5860`, `:5874`, `:5904`, `:5945`. |
| `remove --delete` | `remove_dir_all` on the whole store (`add_remove.rs:518`) `[V]` | DESTROY-STORE | **Shipped.** `refuse_claimed_store` (`add_remove.rs:493`, fn at `:548`) refuses while any live worktree is registered against the store or any receipt for it stands — across all projects on disk — and refuses rather than guesses when a registration is unreadable. It runs *before* the manifest write, so a refused destroy leaves the manifest as it found it. The verb-level named-precondition set (dirty state, unpushed work) is still separate work — **Q11, narrowed** — see §9. |
| `add` (inside a workweave) | mints an ephemeral name (`add_remove.rs:94`) `[V]` | birth | **Shipped.** Uses `EphemeralRefName::mint`; the inlined derivation and its private truncation are deleted (`add_remove.rs:41-48` records the removal). Emits a receipt first (`:93`), so `workweave delete` visits it. |
| `doctor` I3 scan | none | — | **Shipped.** Attachment is checked against the **receipt** (`OwnedRef::is_attached_by`, used at `check.rs:3632` and `:3887`), never against a name shape; detached is a finding at the canonical too (§4.5); scope extends to `projects/<project>/` (§5.1). |

### 5.1 Q5: is the project repo an instance of the model?

**Decided: yes, for the branch model.** The project repo obeys the same three
kinds with the same types. What makes it special is not its ref discipline but
its *channel semantics* (§1.2) — its branch selects a lock. Channel semantics
raise exactly one branch-model question, which is the publish-ref question, and
that is Q6, still open.

Two things follow immediately, and both shipped:

- **Doctor's scope hole closed.** `projects/<project>/` is in the
  branch-discipline scan. `git checkout --detach` there used to yield zero
  findings while the same action on a member yielded a `Detached` violation
  `[S]`; it now yields a finding. The scan is by workspace, not by registry
  directory, so `scan_repos_on_disk` (`workspace.rs:335-383`) was not the
  right walker — `workweave_checkouts` enumerates the project directory
  separately (`check.rs:3280`), the canonical pass iterates
  `workweave_index::projects_on_disk`, and a dedicated arm in
  `branch_discipline_in_scope` (`check.rs:4573-4575`) keeps project-repo
  findings inside a project-scoped run `[V]`. Tests:
  `tests/branch_discipline_test.rs:729` and `:761`.
- **Delete's project-repo arm stopped being conditional.** It used to be
  nested inside *both* `dot_git.is_file()` and the `Ok`/`else` of
  `remove_worktree`, while the member-repo prefix-delete loop ran
  unconditionally and `remove_dir_all` ran regardless. Now the
  `dot_git.is_file()` block contains only `remove_worktree` and
  `worktree_prune` (`workweave.rs:2808-2831`), and `retire_recorded_refs`
  runs for the project repo unconditionally outside both (`:2837-2845`); the
  member arm also falls through to the ref pass when `remove_worktree` fails
  (`:2783-2791`) `[V]`. Under R2 both arms are the same operation over the
  same receipt set, and the asymmetry has nowhere left to live.

### 5.2 The reference-repo carve-out survives unchanged

A `role: reference` repo is materialized as a **symlink** to the canonical
store, has no per-workweave checkout, and therefore has no ephemeral branch
(`clone-topology.md:104-114`) `[V]`. `rwv doctor`'s I3 scan still skips it by
`classify_checkout(&abs) == CheckoutKind::ReferenceAlias`, and the skip is now
structurally ahead of every branch read rather than one line ahead of one: the
checkout enumerator itself retains only non-alias directories
(`workweave_checkouts`, `check.rs:3281`), with a second independent skip in
`scan_clone_topology` (`:3073`) `[V]`. The carve-out is tested at
`tests/branch_discipline_test.rs:1446
symlinked_reference_does_not_fire_shared_branch`, `:1485
worktree_reference_on_ephemeral_branch_flows_through_normally`, and `:1518
worktree_reference_on_shared_branch_still_fires` `[V]` — the third being the
non-vacuity pair for the first two.

Two properties must be preserved by any implementation of this model, and are:

- Sync excludes `ReferenceAlias` checkouts **by construction**, not by guard —
  every mutating phase gates on `checkout_is_syncable` (true iff the path is
  an existing, non-symlink worktree), so the shared canonical store is
  unreachable from all of them (`sync-semantics.md:55-62`) `[V]`.
- **The exclusion keys on alias-ness, never on role.** A reference repo
  created with `--worktree-references` is a real worktree on its own ephemeral
  branch; "sync only ever moves *that* branch, never the canonical's shared
  `main`" (`sync-semantics.md:64-69`) `[V]`. Under this model it gets a
  receipt like any other worktree repo, and nothing keys on `role`.

### 5.3 The version-relatedness guard

**Decided: `fetch` and `update` MOVE only the tracking declaration's local
counterpart. Shipped** — `fetch.rs:763-776` and `update.rs:687` are the two
guards `[V]`. When a checkout is attached to
`entry.version.local_counterpart()`, the verbs advance it as their rows
state. When it is attached to anything else — an operator's personal
branch — they refuse, naming both the attached ref and the expected
counterpart, with `--detach-checkouts` as the exit. They never relocate a
ref they cannot relate to the layer that justifies the move.

Why this side of the fork: moving an arbitrary attached ref re-enacts the
notion-(1)-versus-(3) conflation §2 diagnoses — the verb's *justification*
comes from the tracking declaration, so its *object* must too. §7.2's
doctor arm already applies exactly this guard; this decision makes the two
verbs that move refs daily agree with it. And under §8.3, attachment is
operator state: even a fast-forward of a personal branch — which strands
no commits, since the old tip remains an ancestor of the new one — silently
changes what the operator's bookmark means. The blast-radius consequence
is recorded in §6 item 3.

---

## 6. What changed and what broke

Six items were forecast. This is the honest blast radius, with what actually
landed — plus a seventh the implementation exposed.

1. **Every existing ephemeral branch name changed.**
   `{project}--{workweave}/{segment}` → `{project}--{workweave}`. Every
   workweave in every weave on every machine is affected. The one-time
   migration in §7 shipped, and the flat name is now the *healthy* shape:
   `scan_workweave_repo_branches` treats an attachment matching
   `EphemeralRefName::mint`'s output as clean (`check.rs:3616-3619`) `[V]`,
   pinned by `tests/branch_discipline_test.rs:295
   healthy_workweave_ephemeral_branch_is_clean`.

2. **Weaves whose members were detached needed a decision.** Detached was the
   *normal* state after any `rwv fetch` or `rwv update` `[S]`, so this was
   not a rare case — it was most weaves. `rwv doctor` gained its missing
   `Detached` arm at the canonical (`check.rs:3908-3933`) `[V]` and a `--fix`
   for it (`fix_detached_canonicals`, `:5118-5183`, gated by
   `--reattach-checkouts`). **The forecast that the `--fix` would be
   honest-but-partial held**: §7.2's reattach condition (the local counterpart
   exists *and* its tip equals HEAD) is checked in exactly those two halves at
   `check.rs:5159-5172`, and it is **false** for the ordinary post-fetch state
   — stale local counterpart, HEAD at the lock SHA. The finding carries a
   `reattachable` flag so the operator can see which population they are in
   (`:3908-3933`) `[V]`. What the forecast could not know: the population
   itself shrinks from here, because `fetch` and `update` no longer *produce*
   the detached state (§5). The one-way ratchet remains — after a later
   `--detach-checkouts`, the counterpart's tip no longer equals HEAD and a
   subsequent `--fix` will not reattach — and nothing ever ratchets a weave
   back toward attached in bulk.

3. **`rwv fetch` and `rwv update` became refusable.** Automation that runs
   them across a weave whose members sit on branches will refuse until
   it passes `--detach-checkouts`, or until the target revision is a
   fast-forward. This is the intended consequence of the already-decided
   "changing attachment needs consent" principle, but it is a real behaviour
   change with a real blast radius, and it will surface in CI first.
   A specific sharp edge: **materializing an older lock** (rolling a weave
   back to a prior lock) is not a fast-forward, so it refuses and requires the
   named override. That is correct — you asked for a state your branch cannot
   represent — but it will be someone's first encounter with the flag.
   A second sharp edge, from §5.3: a checkout attached to a personal branch
   (not the tracking counterpart) makes `fetch`/`update` refuse outright
   rather than relocate the branch — the conservative reading of
   "attachment is operator state", at the price of more refusals.
   Both refusals are live and tested (`tests/fetch_in_place_test.rs:380`
   onward) `[V]`, and `docs/reference/cli.md` now documents them.

4. **`workweave delete` stopped force-deleting non-recorded branches under the
   prefix.** Operators who used delete as a cleanup broom get leftovers plus a
   report (`retire_recorded_refs`, `workweave.rs:2485-2500`) `[V]`. Correct,
   but louder. The conservative direction is required by
   `docs/explanation/destructive-operations.md` rules 1-3 — every
   destructive call sits behind a *named* precondition that refuses (rule 1,
   `:14-36`); overrides are narrow and informed (rule 2, `:38-73`); discards
   stay recoverable via `refs/rwv/pre-op/<op-id>` (rule 3, `:75-93`) `[R]`.

5. **The receipt registry's home — decided, and built there.** It lives in
   `projects/<project>/.rwv-workweave-index` in the **primary**
   (`INDEX_FILENAME` `workweave_index.rs:99`, `index_path` `:194-200`) `[V]`,
   keyed by **(canonical store, ref name)** — because the refs outlive the
   workweave directory. It could not live in the `.rwv-workweave` marker: that
   file sits *inside* the workweave (`workspace.rs:1097`) `[V]`, and
   `workweave delete` still ends in `remove_dir_all` on the directory
   (`workweave.rs:2848-2851`) `[V]` — a marker-homed receipt would die with
   the directory, and §7.2's "recorded as belonging to a deleted workweave"
   arm could never fire. The marker is accordingly still
   `{primary, project, parent}` and nothing else (`workspace.rs:1079-1083`)
   `[V]`; no receipt field was added to it.
   The shipped shape: `RefReceipt { store, name, created_at }`
   (`workweave_index.rs:179-192`) in a private `receipts` field on
   `WorkweaveIndex` (`:130`), reached through `RefRegistry` (`:611`) —
   `record_created` `:665`, `lookup` `:749`, `retract` `:793`,
   `adopt_legacy` `:694`, `migrate_legacy_index` `:823`. The store key is
   `std::fs::canonicalize` (`:921-923`), so `record_created` fails rather than
   records an unresolvable key. Writes go through one durable path
   (`durable_file::replace`: fsync file, rename, fsync dir) under an in-process
   RMW guard (`workweave_index.rs:306-317`) `[V]`.
   Legacy markers and indexes migrate along the path that already existed:
   "Markers written before `parent` was introduced (legacy markers) must be
   migrated with `rwv doctor --fix` before the workweave can be used" is the
   rationale stated on `WorkweaveMarker` itself (`workspace.rs:1075-1077`)
   `[V]` — the migration logic it names lives in `check.rs`, not
   `workspace.rs`: detection in `scan_for_legacy_workweave_markers`
   (`check.rs:1825-1874`) and the fix in `fix_legacy_workweave_marker`
   (`:1900-1925`) `[V]`. The index half is separate and runs first
   (`registry.migrate_legacy_index()` at `check.rs:7436`; the report-only
   variant is `LegacyWorkweaveIndex` at `:7459`) `[V]`. (Receipt lifecycle
   beyond the home — invalidation, retraction on store destroy, reclamation —
   is Q14, §9, still open.)

6. **`docs/explanation/joints/shared-refs-drift.md` needs rewriting** if its
   premise is indeed impossible. It states "sibling worktrees on the same
   branch are the normal case (every workweave's repos share the primary's
   branch refs)" (`:46-48`) and "Multiple worktrees can commit on the same
   branch; that is intentional" (`:152-153`) `[V]`. **The implementation
   did not touch this joint**; both quotes are still there, verbatim, at
   those lines. Under
   I3 no two workweave checkouts share a branch, and git refuses the topology
   **by default** (`git worktree add ../wt2 main` → `fatal: 'main' is already
   used by worktree at …`, git 2.43.0) `[V]` — but only by default: `--force`
   succeeds on the same git version and yields two worktrees both on
   `main` `[V]`, and `git symbolic-ref HEAD` does the same with no flag.
   rwv never passes `--force` (`git.rs:1934`, `:1943` are now the only two
   `worktree add` sites, §1.4) `[V]`.
   The drift *class* is real — both citations of "shared-ref advance in a
   sibling worktree" are in `check.rs` (`:65`, `:73-74`), not one in
   `check.rs` and one in `git.rs` as an earlier version of this note had it
   `[V]`, and `sync.rs` repairs it every sync at its actual per-repo call
   sites (`refresh_index_to_head_if_safe` / `refresh_working_tree_to_head_if_safe`,
   `sync.rs:3802-3803`) `[V]` — but its *stated mechanism* cannot occur
   **through default porcelain or through any path rwv takes**. Operator
   `--force` / `symbolic-ref` is therefore the first concrete candidate for
   the untraced path.
   `[?]` **Nobody has traced the actual drift-producing path.** That tracing is
   a prerequisite to rewriting the joint, and it is the one item on this list
   that is not just work but unknown work.

7. **Some joints still describe the pre-flat naming scheme — narrowed since
   the previous citation base.** §3.5's flat name shipped in code and in
   `docs/reference/`, and **`clone-topology.md` has since been brought over**:
   I3's own normative sentence (`:88`), the invariant table (`:180`), and the
   tier-0 rationale (`:212`) all now say `<project>--<workweave>` `[V]`. The
   tier-0 spec is no longer on the wrong side. What is left:
   `workweave-hierarchy.md:190` and `:202`, which still give
   `myproj--hotfix/main` as the example of what rwv mints `[V]`; and three
   generated reference locations describing the *stacked* form that flattening
   made impossible — `explain/workweave.md:162` and its template
   (`explain/templates/workweave.md.tmpl:162`), and the `ParentInfo`
   description carried in both `explain/status.md:109` and
   `schemas/status.json:64` `[V]`. §7.1's
   "must land in the same release" rule was written about the
   scanner/glob/parser cutover and was satisfied there; it was not applied to
   the joints. This is doc work, not code work, and it is not tracked by
   anything in this document other than this item.

### 6.1 Bugs that stopped being reachable

Not part of the blast radius, but worth recording as the payoff. Each of these
was a live defect that the model retired without a targeted fix. All three are
now gone; the first went further than predicted, the third only partly.

- The `list_branches_with_prefix` porcelain parser stripped only `*`, so a
  branch checked out in *another* worktree (git marks it `+ `) yielded
  `RefName("+ p--ww/main")` and the subsequent `branch -D` failed, leaking the
  branch. Under the model the function is report-only, so a mangled entry
  would have been a display defect rather than a leak — but **the defect does
  not survive at all**: the replacement `list_branch_names_with_prefix`
  (`git.rs:2139-2154`) delegates to `list_local_branch_names` (`:2156-2170`),
  which reads `for-each-ref --format=%(refname:lstrip=2)`. There is no
  porcelain to mis-parse `[V]`. The "should still switch to `for-each-ref`"
  recommendation is done. (It also stopped globbing: the filter is a Rust
  `starts_with`, because git's ref `*` stops at `/` and a glob would silently
  omit `<prefix>deep/inner` — a listing that omits a leftover ref reports it
  as absent, which is the same class of bug in the other direction `[V]`.)
- Rollback recorded the *intended* branch unconditionally, so when
  `create_worktree` **adopted** a pre-existing branch, rollback force-deleted a
  branch that create did not author — and it re-read `current_ref` a second
  time, so a HEAD move between reads made rollback delete the wrong branch.
  **Fixed as specified.** `record_ref_attempt(owned, birth)`
  (`workweave.rs:527`) records the outcome `birth_ephemeral_worktree` returned
  (`:1112-1127`), and `undo_ref_births` (`:656-735`) skips
  `RefBirth::Adopted` (`:665-675`), requires `DeletionWarrant::unmoved`
  (`:679`), and reads the tip from the birth's own record (`:718-721`) — so
  there is no second HEAD read left to race `[V]`. Tests at
  `workweave.rs:3549-3654`.
- The `WorktreeRemove` hook passed `force: true` unconditionally and
  `.unwrap_or(dir_name)` fabricated a workweave name from a basename with no
  `--` — the one path where dirty *and* diverged workweaves were destroyed
  with no operator confirmation. **The destructive half is fixed**: the call
  now passes `false, None` (`workweave.rs:3482`), and the unmerged case
  is unconstructible because `DiscardUnmergedConsent` only mints at CLI
  dispatch `[V]`. Test: `tests/branch_model_lifecycle_test.rs:493
  claude_worktree_remove_hook_does_not_destroy_uncommitted_work`. **The
  basename fabrication is still live** (`.unwrap_or(dir_name)` at
  `workweave.rs:3460`) `[V]` — it is now a mis-naming rather than a
  mis-destruction, which is why R3 retired the dangerous half and left this
  one standing.

### 6.2 Bugs this model does not touch

These are plain defects with no model dependency. Fix them independently; do
not wait for this design:

- **Fixed since the previous citation base.** `default_branch` fabricated
  `"main"` on any failure *and* on a malformed symref, and its return value
  was written into `rwv.toml` verbatim as `version:` by three `rwv add` call
  sites, where `update` then resolved `origin/main` and failed forever. The
  method has been **deleted from `GitVcs` and from the `Vcs` trait**, so the
  fabricating shape cannot be reintroduced by a future implementor `[V]`. The
  two `add` sites with an origin to read now consume `remote_default_branch`
  and **refuse**, naming `git remote set-head origin -a`, when it is `None`
  (`add_remove.rs:295-308`, `:397-410`) `[V]`; the `--new` site has no origin
  — `init_repo` creates none — and resolves from `head_attachment` instead,
  which is an observation of the repo's own HEAD rather than a guess
  (`:693-703`) `[V]`. §4.2's `RemoteDefaultBranch` is what made the
  fabrication unrepresentable where it is used; deleting the producer is what
  removed the last three writers. The publish gate (`push.rs:179`) and
  doctor's canonical pass (`check.rs:3736`) read the same producer `[V]`.
- **Half fixed.** `rwv update`'s "advanced N repo(s)" now counts SHA deltas
  rather than non-`Err` outcomes (`update.rs:271-308`, `UpToDate` vs
  `Updated` at `:280-285`, with an in-code comment at `:277-279` stating the
  distinction rather than pointing here) `[V]`.
  Its `branch` JSON field still echoes `entry.version` verbatim
  (`update.rs:272`) `[V]` — harmless now that `update` no longer detaches, but
  still a field that reports the declaration rather than the observation.
- Comment and doc falsehoods on branch-touching paths — **all now resolved**:
  - `sync.rs`'s "mirror create_workweave's naming": **fixed.** The comment at
    `sync.rs:1261-1264` now states the invariant directly — the manifest's
    tracking branch is the START POINT and only that, while the NAME comes
    from `EphemeralRefName::mint`, the same call `create_workweave` and `rwv
    add` make from the same two inputs `[V]`. It formerly carried an explicit
    retraction of the old claim; that sentence was removed with the rest of
    the tree's what-this-used-to-say prose, which is the house rule
    (`CLAUDE.md`) and not a regression: the invariant it was contrasting
    against is what remains.
  - `cli.md:85`: **audited, and the claim was wrong.** Line 85 is the "No
    `<source>` — in-place repair mode" bullet; it is not branch-touching and
    is not false `[V]`. The real falsehood was one bullet down, in the text
    claiming fetch "leaves it on a detached HEAD"; the implementation
    rewrote that passage (and renamed `--detach-working-branch` to
    `--detach-checkouts`
    throughout `cli.md`) `[V]`. This document carried a live-falsehood claim
    against an innocent line for two sweeps.
  - `fetch.rs`'s "Present repos are untouched": **fixed** — the doc comment
    (now `fetch.rs:249-262`) says the opposite, that present clones realign
    `[V]`.
  - `activate.rs`: **still half-fixed**, unchanged by this model. Its doc
    comment no longer claims `update`/`lock` are unreachable intent verbs, and
    `update.rs:397` calls `activate_intent`, but `lock.rs` still never does.
    The call sites are `add_remove.rs:162` and `update.rs:397` for
    `activate_intent` itself, plus `check.rs:7759`, which calls the
    weave-parameterized `activate_intent_at` so a `--fix` run from inside a
    workweave binds the repair to that workweave rather than to primary
    `[V]`.

---

## 7. Migration — **shipped**

rwv is alpha. **No back-compat shims.** No dual-read of old and new names, no
"accept either shape" fallback, no legacy-tolerant doctor arm that survives
past the cutover. Migration is operator-handled and one-off, and unmigrated
state produces a **migration error** that names the command to run.

The whole of §7.1 landed: `fix_branch_model_migration` (`check.rs:4771-4906`,
with a doc comment at `:4717-4770` that enumerates the arms in this
document's own order), plus `migrate_legacy_ref` (`:4935-5013`),
`adopt_flat_ref` (`:5014-5045`), and `adopt_detached_workweave_checkout`
(`:5046-5095`) `[V]`. Detection is `scan_workweave_repo_branches`
(`:3567-3678`) over `refs_in_workweave_namespace` (`:3296-3326`) and
`legacy_ref_at_tip` (`:3679-3731`). Each arm below names where it lives.

The precedent for the error shape is `cli/dispatch.rs:337-351` `[V]` — it
moved out of `main.rs` with the rest of dispatch, unchanged in shape: rwv
detects a removed flag before clap sees it and exits 2 with the replacements
named —

```
error: `--force` has been removed from `rwv sync` and `rwv sync-to`.

Replace it with the specific override(s) you need:
  --allow-stale-lock        skip the lock-freshness precondition
  --discard-local-commits   discard CWD project commits not in source
                            (recoverable via `rwv abort`)
```

### 7.1 The migration pass

`rwv doctor --fix` gained one migration, run per workweave. Three
pass-level rules first, all three implemented:

- **The recovery verbs stay reachable.** `rwv abort` and `rwv status` are
  exempt from the migration gate, and the migration itself runs only when
  no in-flight operation state exists — an operator who upgrades while a
  sync is stopped mid-rebase resolves or aborts it first, without being
  told to migrate. (The op-state skip is `check.rs:4795-4803`) `[V]`.
- **Write ordering, binding on every arm: the receipt is persisted per
  repo, durably, *before* the ref write it describes.** A crash then
  leaves a dangling receipt (benign — retractable by a later pass), never
  an unreceipted ref (permanently disowned under R2). The migration is
  idempotent: re-running it over its own partial output reaches the same
  end state (arm 2 is what makes this true). Receipt-first is visible at
  every write: `record_created` at `check.rs:4982`, `:5023`, `:5088`, each
  ahead of its ref operation `[V]`. The dangling-receipt half has its own
  scanner, `scan_dangling_receipts` (`check.rs:4096-4230`), pinned by
  `tests/branch_discipline_test.rs:791 dangling_receipt_is_reported_and_retracted`.
- **The pass enumerates refs per store — attached and unattached — not
  attachment states.** The objects R2/R3 govern are branches; a pass keyed
  on `head_attachment` alone silently disowns a commit-bearing legacy
  branch that a fetch left behind. Enumeration covers every
  worktree-materialized repo (skipping `ReferenceAlias` checkouts, §5.2)
  **and the project-repo checkout**, which the member walker does not
  reach (`scan_repos_on_disk`, `workspace.rs:335-383`). The migration's
  enumerator is `workweave_checkouts` (`check.rs:3267-3283`), which appends
  the project-repo path explicitly at `:3280` `[V]`; pinned by
  `tests/branch_discipline_test.rs:2714
  migration_reaches_the_project_repo_checkout`. §5.1 states this for the
  scan; it holds for the migration for the same reason — an implementer
  reusing the member walker leaks one project-repo branch per workweave.

The arms, in order. All seven shipped:

1. **A legacy-shape branch `{project}--{workweave}/*` for *this* workweave
   exists, and HEAD is attached to it** — write the receipt (current tip
   as `created_at`), then rename the ref to `{project}--{workweave}`. The
   common case; fully automatic. The rename is a birth plus a DESTROY of
   the old name; the DESTROY's warrant is `Unmoved` against the tip
   observed one line earlier. Detection `check.rs:3619-3624`
   (`UnmigratedEphemeralBranch`, keyed on `AttachedRef::legacy_name_under`);
   fix `:4864-4872` → `migrate_legacy_ref` `:4935-5013` `[V]`.
   The rename and the scanner/glob/parser cutover **had to land in the same
   release**, because a flat name was previously classified as `SharedBranch`,
   `git.rs`'s `"{prefix}/*"` glob never matched one, and
   `parse_ephemeral_branch_name` required the slash — a half-landed cutover
   would have mis-flagged every healthy repo and orphaned every flat branch at
   delete. **It landed complete, and the polarity is now inverted**: flat is
   the healthy shape (`check.rs:3616-3619`), the glob is gone (`git.rs`
   filters with `starts_with`), and the successor predicate
   `looks_like_a_pre_flat_ref` (`check.rs:3218-3228`) *deliberately refuses to
   match* the flat shape (`:3226`) so it can never claim a healthy ref `[V]`.

2. **A flat new-shape name exists with no receipt** — adopt it: write a
   receipt at the observed tip. Without this arm, a repo the migration
   half-processed (or a crash between receipt and rename) falls into arm 4
   on re-run and is disowned forever. Detection `check.rs:3607-3613`
   (`UnrecordedEphemeralBranch`); fix `:4876-4881` → `adopt_flat_ref`
   `:5014-5045` `[V]`.

3. **HEAD is `Detached(_)` and a legacy-shape branch for *this* workweave
   exists at a different tip** — the post-fetch state §6 item 2 calls
   normal, possibly with operator commits on the branch. Report **both**
   tips. First remediation: reattach to the existing branch (arm 1 then
   applies on re-run). Second: `--adopt-detached-checkouts`, which mints
   flat `{project}--{workweave}` **at HEAD** — i.e. at the lock SHA — and
   **must warn that it strands the legacy branch's tip** whenever that
   branch carries commits HEAD does not. Detection `check.rs:3647-3654`,
   carrying a `LegacyRefAtTip { branch, tip_sha, strands_commits }`
   (`:1085-1097`) — the stranding warning is a *field*, computed at scan time,
   not a message the fix path has to remember to print; fix `:4884-4898`,
   gated on `AdoptDetachedConsent` (`:4885`) `[V]`.

4. **`Attached(a)`, `a` is anything else** — an operator branch, a shared
   `main`, or a foreign workweave's ephemeral. **Report, do not touch.**
   Under R2 these are not rwv's refs. They become the Q7 population (§9).
   Fix path `check.rs:4901-4902` (`Ok(_) => {}`); detection splits them into
   `ForeignEphemeral` (the registry says another workweave holds it) and
   `SharedBranch` (`:3626-3644`) `[V]`.

5. **`Detached(_)` with no legacy-shape branch for this workweave** —
   report, and offer `--adopt-detached-checkouts` as in arm 3; with no
   competing tip there is nothing to warn about. Same code path as arm 3
   with `legacy_branch: None` (`check.rs:3647`) `[V]`.

6. **`Unborn(_)`** — a repo with no commits. Report; there is nothing to
   attach a receipt to. Detection `check.rs:3655-3658` (`UnbornCheckout`);
   report-only at `:4901-4902` `[V]`. (This state also reaches `rwv lock` and
   produces the "unborn HEAD (no commits yet, on branch '<x>'): make an
   initial commit, then re-run rwv lock" error, rendered by `head_revision`
   (`git.rs:824-846`), which delegates the classification itself to
   `head_attachment` (§4.5) `[V]`.)

7. **Legacy markers and indexes** — the registry field migrates in the same
   pass, alongside the already-existing `parent`-field migration
   (detection `check.rs:1825-1874`, fix `:1900-1925`, called at `:6955`)
   `[V]`, receipt-first like everything else. The index half is a separate
   step that runs *first*, `registry.migrate_legacy_index()` at
   `check.rs:7436` (ordering rationale `:7420-7426`; report-only variant
   `LegacyWorkweaveIndex` at `:7459`) `[V]` — the index must have a receipt
   registry before any arm can write into it.

Without the flag named in arm 3/5, the workweave stays unmigrated and every
rwv verb on it errors with the flag named — except `abort` and `status`,
exempted above. The arms are tested at `tests/branch_discipline_test.rs:1624`,
`:1707`, `:1799`, `:1856`, `:1916`, `:1990`, `:2049`, `:2101`, `:2147`,
`:2650`, `:2714` `[V]`.

### 7.2 The canonical-store pass

Separately, for each canonical store at `<weave>/<repo_path>` — shipped as
`scan_canonical_stores` (`check.rs:3859-4034`), with the arms below in the
same order `[V]`:

- `Attached(a)` — leave it alone. The canonical's attachment is **operator
  state**; `clone-topology.md:226-228` says "rwv does not own the
  canonical store's branch state beyond I3. The canonical store can sit on any
  non-ephemeral branch the operator picked" `[V]`. Implemented as the
  no-receipt fall-through (`check.rs:3905`, "No receipt → arm 1: operator
  state, left alone").
- `Attached(a)` where `a` is a ref recorded as belonging to a **live**
  workweave — an I3 disjointness violation. git forbids the topology anyway,
  so this indicates a directory was moved or copied. Report; no automatic fix.
  Sub-kind `CanonicalHoldsLiveWorkweaveRef` (`check.rs:966`) `[V]`.
- `Attached(a)` where `a` is a ref recorded as belonging to a **deleted**
  workweave — a leak. Report; `--fix` deletes it with a `Merged` warrant if
  one can be established, and refuses otherwise. Sub-kind
  `CanonicalHoldsLeakedRef` (`check.rs:981`) `[V]`. (No dedicated
  reclamation verb exists or is planned while inflow stays at its recorded
  floor: doctor's per-class count lines are the instrument, and a class's
  count regrowing past its recorded post-sweep baseline is the structural
  trigger that reopens the verb question — a count against a recorded
  floor, never wall-clock. The shelved verb design is
  `docs/sync-state-space/rwv-gc-verb.md` in the project docs.)
- `Detached(_)` — a finding, which is new. **Shipped**: the arm at
  `check.rs:3908-3933` always emits `CanonicalDetached { at_sha, counterpart,
  reattachable }` `[V]`, pinned by `tests/branch_discipline_test.rs:592
  detached_canonical_is_reported`. `--fix`, gated by `--reattach-checkouts`
  (§4.4, gate at `check.rs:7492-7516`), reattaches to the tracking
  declaration's local counterpart when that ref exists as a **local branch**
  and its tip equals HEAD (`fix_detached_canonicals`, `:5118-5183`, both
  halves at `:5159-5172`) — resolved through `resolve_local_branch_tip` so a
  same-named tag cannot answer instead `[V]`. Without the flag, or when the
  condition fails, `--fix` reports with the correct `git switch` spelling
  instead; the report path is unchanged. Stated plainly: that condition fails
  for the ordinary post-fetch state (stale local counterpart, HEAD at the lock
  SHA), so this `--fix` reattaches the minority — and the finding says so
  per-repo via its `reattachable` field rather than leaving the operator to
  discover it. It repairs what it can prove; it does not deliver the
  weave-wide reattachment §6 item 2 might suggest. Pinned by
  `tests/branch_discipline_test.rs:657
  detached_canonical_reattaches_only_with_consent`.

Two arms the implementation added: `Unborn(_)` is silent here (`:3907`) and
reported by the workweave pass instead, and a receipt whose workweave is still
live is skipped rather than reported stale (`:3945-3952`, with a comment
naming `--dir` placements) — which is Q10's harm neutralized without deciding
Q10 `[V]`.

### 7.3 What the migration deliberately does not do

- It does not attempt to reconstruct which workweave a stray
  `<a>--<b>/<c>` branch belonged to. Name shape is not ownership (R2); that is
  the whole point.
- It does not rename or delete anything in a store it cannot associate with a
  live workweave directory. `--dir`-placed workweaves are invisible to the
  container scan today (Q10, §8), and a migration that deleted their live
  branches on that basis would be the exact failure this model exists to
  prevent.

---

## 8. Rejected alternatives

Recorded with reasons, so a settled question is not re-proposed.

### 8.1 A reserved `refs/rwv/weaves/...` namespace outside `refs/heads/`

**Rejected.** This is the theoretically clean answer to "which refs are
rwv's" — ownership by namespace, by analogy to the `refs/rwv/pre-op/*` and
`refs/rwv/pre-abort/*` refs that already exist (`vcs.rs:2178`, `:2184`,
`:2189`, `:2211`, `:2221`) `[V]`. Two independent reviewers proposed it.

It is rejected because **worktree checkouts need a branch under `refs/heads/`
to commit onto**. Git will not attach a worktree's HEAD to a ref outside
`refs/heads/` and let ordinary `git commit` advance it. Implementing this
would mean either symref gymnastics on every checkout or losing the ability to
run plain `git commit` inside a workweave — a change to the *daily* surface far
larger than the problem being solved. Recording created refs (R2) buys the same
ownership guarantee for the cost of one schema field, and leaves the workweave
a normal git workspace.

### 8.2 Keeping `<segment>`

**Rejected.** Retaining `{project}--{workweave}/{segment}` would have meant
reconciling the three incompatible derivations in `workweave.rs`,
`add_remove.rs`, and `sync.rs` `[V]` rather than deleting the question. The
decisive evidence was that **no consumer read the segment** — doctor validated
the prefix only and its doc comment said this was deliberate; delete globbed
the prefix; sync-to, push, the merged-check, `workweave log` and status all
explicitly refused to read it. A component that nothing reads and three
writers disagree about is not carrying information; it is carrying risk.
Keeping it would also have kept unbounded nesting
(`p--gc/p--child/p--feat/main`, working to depth ~180 and then failing with a
raw `fatal: cannot lock ref`) `[S]`, which is a legibility failure, and
legibility failure is what caused the incident that motivated this design.

None of those three consumer citations resolves any more, because the
implementation removed the question rather than answering it (§3.5). That is
the intended outcome, not drift: a rejected alternative whose supporting
evidence has been deleted stays rejected — re-proposing it would mean
re-introducing the three derivations, the prefix glob, and the parser.

### 8.3 Detached-at-lock-SHA as the correct resting state for members

**Rejected.** This is the survey's Q1 option (b): treat a member checkout as a
pure content-addressed materialization, so detached is *right* and `fetch` /
`update` are correct as shipped.

It is coherent — it is what the engine actually implements today — and it is
rejected for three reasons:

1. It contradicts the settled meaning of `version:`. If the on-disk state is
   content-addressed, "the manifest declares what to TRACK" has no on-disk
   consequence and `rwv update` has nothing to advance.
2. It requires rewriting three shipped surfaces to agree with it: I3's
   canonical clause ("The canonical store sits on a non-ephemeral branch",
   `clone-topology.md:85-86`) `[V]`, `rwv push`'s detached refusal
   (`push.rs:381-386`) `[V]`, and `rwv lock`'s detached warning
   (`lock.rs:139-147`) `[V]`. That is a larger change than the one being
   avoided — and it is now larger still, since doctor's `CanonicalDetached`
   arm (§7.2) would have to be deleted too.
3. It makes every commit an operator makes in a canonical store unreferenced
   by default. The failure mode is silent and the recovery is reflog
   archaeology.

The adopted answer is Q1 option (c): a member's on-disk ref is a **working-set
handle**. Its only structural requirement is I3-disjointness; its attachment is
**operator state that rwv must not silently change**. That is consistent with
`version:` being a tracking declaration and with the lock being the record of
where you are.

### 8.4 Ownership by name shape

**Rejected** — this *was* the status quo, and it is what deleted hand-made
`my--feature/wip` and `notes--todo/scratch` on a plain `rwv doctor --fix` `[S]`.
Any predicate over names claims refs rwv never created. Making the predicate
stricter (requiring a matching live workweave directory, say) only narrows the
class; it does not change the kind of claim being made, and it re-couples
branch names to directory names, which used to be load-bearing in the wrong
direction — a directory basename derived from a branch name `[V]`.

**The status quo is now the model.** Ownership is the receipt
(`OwnedRef::is_attached_by`, `check.rs:3632`, `:3887`), and the one surviving
name-shaped predicate, `looks_like_a_pre_flat_ref` (`check.rs:3218-3228`),
returns a `bool` that feeds a report and nothing else `[V]`. The
name-to-directory coupling also reversed: `branch_discipline_in_scope`
(`check.rs:4529-4583`) derives a *project* from a directory basename via
`parse_weave_dir_name` (`:4549`), and no directory name is derived from a
branch name anywhere `[V]`. Tests holding the rejection:
`tests/branch_discipline_test.rs:1028 handmade_lookalike_branch_survives_doctor_fix`
and `:1080 flat_lookalike_branch_survives_doctor_fix` — note the second, which
covers a hand-made branch that matches the *new* flat shape exactly, the case
name-shape ownership would get wrong most often now.

### 8.5 Making the branch name authoritative for lineage

**Rejected.** Under `<segment>`-derived-from-fork-source, a nested name
*looks* like a lineage record. Two places in the code state defensively that
it is not — "Branch names are creation-time namespaces, **NOT lineage
records**" (`workweave.rs:2306-2307`, echoed at `status.rs:84-85` and now also
at `check.rs:1925`) `[V]` — and direct consumers to read parentage from the
workweave marker. Reversing that would reverse a decision stated twice, and
would make renaming a workweave (which rwv has no operation for) corrupt its
ancestry. §3.5 made the rejection structural rather than defended: with no
third component, there is no nesting left for a name to record.

### 8.6 A pre-push hook, or any enforcement that workweave branches are never published

**Rejected**, already. `rwv push` refuses from a workweave; plain `git push` is
operator discipline. The correct response is to qualify the documentation, not
to add enforcement. Note the live consequence for §9: "workweave branches are
unpublished by construction" **is** operator discipline rather than an
enforced invariant — `sync-semantics.md:511-517` says so in its own
words ("but that's policy, not physics … No pre-push hook backs it … so a
plain `git push` bypasses it entirely") `[V]` — so the "why no merge strategy"
argument at `sync-semantics.md:502-521` rests on discipline too. §9 restates
this as still open only because *rewording the joint* is unstarted work, not
because the premise is in doubt; the implementation did not touch that
joint `[V]`.

### 8.7 Growing branch verbs

**Rejected**, already, twice. `pyramid-of-stability.md:123-141` records that
channels and tiers are *concept*, not verbs (the table at `:128-135`), and
that adding verbs for them was refused (`:137-141`) `[R]`.
`workweave-hierarchy.md:139-140` records that cross-branch cherry-picks and
project-repo branch promotion are refused, with "Use `git cherry-pick`
directly" and "Ordinary git workflow" as the recorded discipline `[V]`.
This model adds no verbs. It changed the types the existing verbs use and
added three named overrides — `--detach-checkouts`, `--reattach-checkouts`,
and `--adopt-detached-checkouts`; the count grew by one from the design,
because §7.1's adopt-at-HEAD consents to stranding a tip rather than to
changing an attachment, and R1's "named for the consequence" rule
(§4.4) forbids spelling those two the same way.

### 8.8 `version:` accepting a tag or a SHA

**Rejected**, already. A tag conflates the declaration layer with the record
layer: `version:` declares what to TRACK, the lock records where you ARE.
Genuine pinning needs a differently-named field, not an overloaded one. This
is why `TrackingRef::parse` **rejects** SHA-shaped and tag-shaped input rather
than accepting and resolving it.

---

## 9. Open questions

Stated precisely. This document does not answer them, and does not paper over
them.

**Q6 — What is a member repo's publish ref? — STILL OPEN in its main clause;
the fabrication sub-clause is closed.**
`push_with_role` used to run `git push origin <current_ref>`, publishing
whatever branch the checkout happened to be on from inside the VCS impl. `rwv
push` warned when that differed from `entry.version` and then pushed anyway.
So: is the publish ref the attached ref, or the manifest's declared tracking
branch? Is `version:` a *constraint* on publishing or a *default*?
**That is the one place a per-repo branch is genuinely semantic, and this
model still does not decide it.** What the model contributed, and has
shipped: `Vcs::push_ref` takes the ref as a parameter (`vcs.rs:3027`), so the
decision is made at one site in `push.rs` instead of being implicit inside
the VCS impl (§4.6(2)); `PublishRef::from_attached` (`push.rs:213`, `:418`)
is the choice currently made, matching the shipped behaviour exactly, and
`PublishRef::from_local` (`vcs.rs:1419`) sits unused as the other answer so
that changing it is a one-line change at one place `[V]`. The member gate
still warns and pushes anyway (`push.rs:406-412`) `[V]` — the split relocated
the decision, it did not make it.

The sub-question this entry attached to the project repo has since **split in
two, and one half is answered.** The half that is answered: the canonical
branch used to be *invented* from `origin/HEAD` with a hardcoded `"main"`
fallback, wrong for `rwv init`-created repos, for `master` / `trunk`
defaults, and for every pyramid channel other than the default. Nothing
invents it any more. `default_branch`, the method that fabricated, has been
deleted from `GitVcs` and from the trait, so no reader and no *writer* can
reach the fallback: the publish gate refuses when `origin/HEAD` is unset
(`push.rs:181-187`), doctor's canonical pass reads the same honest `Option`
(`check.rs:3736`), and `rwv add`'s three `version:`-writing sites either
refuse by name or read the repo's own HEAD (`add_remove.rs:295-308`,
`:397-410`, `:693-703`) `[V]`. A repo whose real default is `master` now
gets `master` recorded. **What remains open is the other half, unchanged:
where a *non-default channel's* identity should be recorded.**
`remote_default_branch` answers "what does the remote call its primary
branch", which is the right question only for the default channel; a
pyramid channel that is deliberately not `origin/HEAD` has no recorded
identity anywhere, and the gate above will refuse it rather than publish it.
Deciding where that belongs is a design call this document still does not
make.

**Q7 — Is an operator-created branch inside a workweave legal? — STILL OPEN.**
R2 makes such a branch **safe** — rwv never destroys it — but leaves it
**unaccounted**: it pins objects, gates nothing, and disappears from view once
the workweave directory is gone. The doctor finding stays report-only, with no
`--fix` path — as built: §7.1 arm 4 is `Ok(_) => {}` (`check.rs:4901-4902`), and the
population lands in `SharedBranch` / `ForeignEphemeral` (`:3626-3644`) `[V]`.
The question has a sharp form: if operator branches are legal, who
merged-checks them at delete time and who cleans them up? If they are not
legal, the refusal belongs at checkout time, not at doctor time. (The how-to
that used to *tell* operators to create one is being removed independently;
the policy is untouched by that removal.)

**Q10 — Workweave existence: registry or directory scan? — STILL OPEN, harm
narrowed.** The container scan still exists, now split in two:
`list_workweave_dirs` (`workweave.rs:2941-2968`) assembles the containers and
`doctor_scan_container` (`:2979-3022`) walks them, documented there as "the
ONLY surviving on-disk scan" `[V]`. Workweaves placed with `--dir` (advertised
at `cli.rs:466-474`) are still not enumerated by a directory walk. R2 removed
the *deletion* consequence — an unrecorded ref is never destroyed, and a
recorded one is looked up by receipt rather than by container membership — and
the implementation removed the *misclassification* consequence too:
`live_workweave_names` (`check.rs:3490-3514`) consults the workweave index as
well as the markers, and the canonical pass skips a receipt whose workweave is
live (`:3951-3953`, with a comment naming `--dir` explicitly) `[V]`, so a
`--dir` workweave's live branches no longer classify as stale. What remains is
the *reporting* hole this question named: which of index and directory is
authoritative for "does this workweave exist" is still undecided, and
registry-versus-scan remains a separate decision with its own blast radius.

**Q11 — Who owns canonical-store lifetime? (narrowed) — STILL OPEN, and
narrower than when written.** `rwv remove --delete` used to run
`remove_dir_all` on the whole store with no live-worktree check, no dirty
check, and no unpushed check, and `rwv doctor` reported nothing afterwards
because the manifest entry was removed first `[S]`. It destroys the refdb
every recorded ref lives in. No ref-level rule can defend against that — it is
a store-level operation. **DESTROY-STORE (§3.2) and R4 classify it and state
its consent shape**, and R4 is now enforced: `refuse_claimed_store`
(`add_remove.rs:493`, fn at `:548`) refuses on any live worktree registration
or any standing receipt, across all projects on disk, and runs before the
manifest write so a refusal is retryable `[V]`. `prune_dropped_repo` gained
the same gate (`check_store_unclaimed`, `sync.rs:1311`, called at `:1530`).
What stays open is exactly what this entry said: the verb-level
named-precondition set (dirty state, unpushed work — the set `workweave
delete` has), which this model scopes but does not specify, and which
`refuse_claimed_store`'s own doc comment defers to this question.

**Q12 — What is the legal grammar for project and workweave names? —
ANSWERED, at the parse boundary.** The grammar exists, is stated in the type,
and is enforced at construction and at deserialization.

`ProjectName::new` and `WorkweaveName::new` are **fallible**, returning
`ProjectNameError` / `WorkweaveNameError` (`manifest.rs:172-176`, `:267-271`)
`[V]`. The rules, in the two `validate_*` functions the constructors and the
`Deserialize` impls share (`manifest.rs:163-168`, `:255-263`) `[V]`:

- **Neither may contain `--`, nor start or end with `-`.** `--` is the
  delimiter `EphemeralRefName::mint` joins on, and a leading or trailing `-`
  reconstructs it across the join: `mint("x-", "y")` and `mint("x", "-y")`
  both yield `x---y`. Rejecting all three closes the collision this entry was
  written about — project `p` with workweave `x--y`, and project `p--x` with
  workweave `y`, are no longer both constructible.
- **A workweave name may not contain `/`.** A project name may: nested
  `projects/<owner>/<name>/` derivation is a real, tested feature. The
  asymmetry is load-bearing rather than incidental — a `/` in the *workweave*
  half would make a minted flat name read back as
  `LegacyEphemeralRefName`'s segmented shape for a different, live workweave,
  and doctor's migration arms would misclassify it (`manifest.rs:221-225`
  states exactly this) `[V]`.
- **Both must be usable as ref-name components**, delegated to the same
  `validate_ref_name` (`vcs.rs:740-786`) that `TrackingRef::parse` runs. The
  asymmetry this entry used to name — the *declared* name better checked than
  the *minted* one — is gone: both sides now go through one predicate.

`EphemeralRefName::mint` (`vcs.rs:962-964`) is still a bare
`format!("{}--{}", …)` `[V]`, and that is now correct rather than a gap: its
two arguments are types that cannot carry the delimiter, so `mint`'s totality
is a property of its signature. Two `compile_fail` doctests hold the
constructors fallible (`manifest.rs:125`, `:207`) so that a future caller
cannot quietly treat either as infallible again `[V]`, and the enforcement
reaches persisted state too: both types carry a hand-written `Deserialize`
that runs the same predicate (`manifest.rs:195-201`, `:284-289`) `[V]`, so
neither `LockFile.workweave` (`manifest.rs:1225`, typed `Option<WorkweaveName>`)
nor the `.rwv-workweave` marker's `project:` (`workspace.rs:1081`, typed
`ProjectName`) can smuggle a name past the constructor by being hand-edited
on disk `[V]`.

**What is *not* covered, stated rather than assumed:** `RefName::new`
(`vcs.rs:257-259`) is still a bare unvalidated newtype `[V]`. That is
consistent with the rest of this document rather than a residue of this
question — `RefName` is the un-migrated legacy type (§2, §4.2), it names no
project and no workweave, and migrating `manifest.rs`'s `version:` field off
it is separately tracked as the one piece of §4.2's table left undone.
Q12 asked for the grammar of *project and workweave* names; that grammar is
now decided and enforced.

**Q13 — Is a deliberate detached *position* protected operator state? — STILL
OPEN.** §8.3 decides that *attachment* is operator state rwv must not silently
change. A mid-bisect or mid-rebase-edit *position* is operator state by
the same argument, and it still gets strictly less protection than an
attached HEAD. §3.6 protects exactly the cases `mid_op_state` can detect, and
bisect detection has landed there (`git.rs:533-558`) — but only on the path
that goes through `mid_operation`. `sync.rs`'s own preflight still uses
`mid_op`, whose `match` folds bisect into `None` (`git.rs:1408-1415`) `[V]`,
so the protection is uneven *within* the tree, not just absent from the model.
The general principle — does the model protect positions, or only attachments?
— is undecided, and `refs/bisect/*` is not consulted by anything.

**Q14 — What is a receipt's lifecycle beyond its home? — STILL OPEN.**
§6 item 5 picks the home and the key, and both shipped. Still open: what
invalidates a receipt; whether R4's retraction step on a store destroy is a
per-ref DESTROY needing its own warrant each time or a bulk operation with its
own consent — the shipped `refuse_claimed_store` sidesteps this by *refusing*
while any receipt stands rather than retracting them (`add_remove.rs:548`)
`[V]`, which is the conservative reading and not a decision; what a receipt
pointing into a store that no longer exists means for doctor — partially
answered by `scan_dangling_receipts` (`check.rs:4096-4230`), which reports and
`--fix`-retracts a receipt whose *ref* never appeared, but not one whose
*store* is gone; and whether receipts are ever reclaimed. On that last
question: no dedicated verb exists or is planned while inflow stays at its
recorded floor — doctor's per-class count lines are the instrument, and a
class's count regrowing past its recorded post-sweep baseline is the
structural trigger that reopens the verb question (a count against a
recorded floor; triggers stay structural — ancestry, named-ref
reachability, counts — never wall-clock). The shelved verb design is
`docs/sync-state-space/rwv-gc-verb.md` in the project docs.

**Q15 — What is the validity window of a witness? — STILL OPEN.**
§4.2 binds `AttachedRef` to its repo, which closes the cross-repo pass, and
that shipped: `advance_attached_ref` re-observes via
`head_attachment(witness.repo())` (`vcs.rs:2640`) and errors on a stale
witness, pinned by `tests/branch_model_test.rs:310
advance_attached_ref_refuses_a_witness_for_a_repo_that_became_detached` `[V]`.
So a witness is re-verified at the moment of consumption. Not settled: whether
a witness is valid across phases within one verb (an earlier phase can detach
a repo whose witness a later phase still holds — the TOCTOU form of the same
defect; `ff_advance_repo` states it in its own comment at `sync.rs:5543-5549`,
which records that an attachment changed since the read is a refusal rather
than a landing and that "how wide that window should be stays
open" `[V]`), and whether `sync --continue` must verify that the
attachment it planned against is the attachment it resumes against. R1 makes
re-derive-and-move *legal* on resume; whether it is *wanted* is this question.

**The merge-strategy rationale. — STILL OPEN.**
`sync-semantics.md:502-521` argues that no `merge` strategy is needed because
workweave branches are unpublished — but its own words, re-checked against
the current text, already concede the premise this document doubted: "but
that's **policy, not physics** … No pre-push hook backs it … so a plain
`git push` bypasses it entirely" (`sync-semantics.md:511-517`) `[V]`. So the
joint already agrees with §8.6's conclusion; "by construction" was this
document's gloss, not the joint's own claim, and it is the gloss that needs
dropping, not the joint that needs rewording. There is also a live
counterexample: `rwv sync <workweave>` run *from primary* rebases primary's
published branch onto an unpublished ephemeral tip, unwarned —
`warn_on_sibling_sync` (`sync.rs:2888-2921`) still gates its warning on
`Checkout::Workweave` and so only fires for the workweave-CWD direction `[V]`
— which is precisely the case the joint says cannot arise. That is a
sync-semantics question; this model does not answer it, and the
implementation did not change it.

**The `shared-refs-drift` contradiction.** See §6 item 6. The drift class is
real and repaired every sync; its documented mechanism is impossible for
worktree-materialized repos; and `[?]` the actual drift-producing path has not
been traced by anyone.

---

## 10. Summary

**Decided here:**

- **Q1** — a member's on-disk ref is a **working-set handle** (option c).
  Attachment is operator state; rwv must not silently change it.
- **Q2** — ownership is by **record**, not by name shape.
- **Q3** — `<segment>` is deleted. Ephemeral names are
  `{project}--{workweave}`.
- **Q4** — landing names the ref the target is **attached** to, and refuses
  when the target is detached. Falls out of R1; no new machinery.
- **Q5** — the project repo is an instance of the same model. Doctor's scope
  extends to it. Its channel semantics raise only Q6.
- **Q8** — `rwv update` inside a workweave **advances the ephemeral ref** when
  that is a fast-forward and refuses otherwise. Falls out of R1.
- **Q9** — "no current branch" is not one state. `HeadAttachment` has three
  variants; not-a-repo and unreadable are typed errors. `Ok(None)` ceases to
  exist.

**Decided in revision:**

- **DESTROY-STORE** (§3.2, R4) — store-level destroys are the fourth kind.
  `prune_dropped_repo`, `remove --delete`, and `workweave delete`'s
  `remove_dir_all` are classified, and no ref-level rule ever unblocks one.
- **The version-relatedness guard** (§5.3) — `fetch`/`update` MOVE only the
  tracking declaration's local counterpart; any other attachment refuses.
- **Birth targets** (R1, §5) — a birth attaches at the revision the verb is
  materializing; a birth is never followed by a MOVE.
- **The receipt home** (§6 item 5) — the primary's workweave index, keyed
  by (canonical store, ref name), receipt-written-before-ref (§7.1).
- **`default_branch`** (§4.2) — typed as observed remote state
  (`RemoteDefaultBranch`, `Option`-explicit, no `"main"` fabrication); an
  input to the publish gate, not a fifth notion.
- **Name uniqueness at create** (§3.5) — checked against the workweave
  index, not the container directory.

**Decided since, by the tree rather than by this document:**

- **Q12** (§9) — project and workweave names have a stated grammar, enforced
  at construction *and* at deserialization: no `--`, no leading or trailing
  `-`, no `/` in a workweave name, and both must pass `validate_ref_name`.
  `ProjectName::new` and `WorkweaveName::new` are fallible. `mint`'s totality
  became a property of its argument types instead of an assertion.
- **The `"main"` fabrication** (§4.2, §6.2, and Q6's project-repo
  sub-question) — `default_branch` is deleted from `GitVcs` and from the
  trait. Nothing in the tree invents a canonical branch any more; the gate
  and all three `rwv add` writers read an observation or refuse by name.

**Left open, with the question stated:** Q6 (the publish-ref choice itself,
and where a non-default channel's identity is recorded — its
`"main"`-fabrication clause is closed, see above), Q7 (operator branches in a
workweave), Q10 (`--dir` workweave liveness), Q11 (verb-level preconditions
for store destroys — narrowed), Q13 (detached positions as operator state),
Q14 (receipt lifecycle), Q15 (witness validity window), the merge-strategy
rationale, and the `shared-refs-drift` mechanism. §9 records, per question,
what the tree changed *around* it without answering it.

**Status of the implementation.** Everything under "Decided here" and "Decided
in revision" shipped at repoweave `37548fd`; the two entries under "Decided
since" landed after it and are verified at the citation base above. The old
surface — `current_ref`, `checkout`, `delete_branch`, `restore_savepoint`,
`create_worktree`, `push_with_role`, `list_branches_with_prefix`,
`parse_ephemeral_branch_name` — is deleted rather than deprecated, and
`default_branch` has since joined it. Two things this document specified are
**not** done, and are stated where they belong rather than only here:
`manifest.rs`'s `version:` field is still `RefName` (§4.2), and two joint
locations plus three generated-reference locations still describe the
pre-flat branch name (§6 item 7) — down from five and four, since
`clone-topology.md` was brought over.

**The mechanism:** one rule with four kinds (MOVE / ATTACH / DESTROY /
DESTROY-STORE), a decision procedure that answers for verbs not yet
written, and a type split of `RefName` (five core types, §4.2's honest
inventory) that turns each violation into a compile error — the same move that
made raw-versus-resolved revision confusion "a compile-time impossibility"
(`check.rs:5423-5428`) `[V]`. The enforcement is 7 `compile_fail` doctests on
the ref types, 2 more on the name types `mint` consumes, and 23 probes in
`tests/branch_model_compile_fail_test.rs` (§4.7).

---

## Anchoring

- `docs/explanation/joints/clone-topology.md` — I1/I2/I3; the reference-repo
  carve-out; the `git-common-dir` mapping. I3's ephemeral-branch clause is
  restated by this model as an ownership-by-receipt clause; its *purpose*
  (merged-check soundness via ref disjointness) is unchanged. It used to
  contradict the shipped code on the name shape; **it no longer does** — `:88`,
  `:180`, and `:212` all give the flat `<project>--<workweave>` `[V]`.
- `docs/explanation/joints/workweave-hierarchy.md` — where the ephemeral naming
  scheme lives operationally, and the sole recorded justification for it
  (git's one-worktree-per-branch constraint). **Also still pre-flat**
  (`:190`, `:202`) — §6 item 7.
- `docs/explanation/joints/workweave-lifecycle.md` — creation flags, the retire
  contract, deletion semantics; the `--force` split into named overrides.
- `docs/explanation/joints/sync-semantics.md` — the direction-pair contract,
  the `ReferenceAlias` exclusion-by-construction, and the "why no merge
  strategy" argument flagged in §9.
- `docs/explanation/joints/pyramid-of-stability.md` — a channel is a branch is
  a lock; channels are concept, not verbs.
- `docs/explanation/destructive-operations.md` — rules 1-3 (`:14-36`,
  `:38-73`, `:75-93`), which R2 and R3 mechanize. Note the path: this file is
  **not** under `joints/`, as an earlier revision of this document had it.
- `docs/explanation/joints/shared-refs-drift.md` — needs rewriting; see §6
  item 6. Untouched by the implementation.
- `docs/explanation/joints/vcs-as-seam.md` — not cited above, but the joint
  this model's trait changes land in: `:269-330` records the
  `push_with_role` → `push_ref(…, &PublishRef, …)` change and defers to §4.3
  and Q6 for the policy `[V]`. It is the only joint the implementation
  updated.
- `docs/reference/explain/doctor.md` and `docs/reference/schemas/doctor.json`
  — the downstream consumers of §7. Both were rewritten for this model, and
  both still cite it, but the citation surface has changed shape and is worth
  stating exactly, because it is what "do not renumber" now protects.
  `doctor.md` names this document three times — `:71` (§7.2), `:84` (R2),
  `:185` (R2) — and its template carries the same three
  (`explain/templates/doctor.md.tmpl:71`, `:84`, `:185`) `[V]`.
  `doctor.json` no longer names the file at all, but its generated
  descriptions still cite the **rule labels** — R2 at `:42`, `:79`, `:185`,
  `:376` — and `doctor.md` echoes those plus R3 (`:85`, `:187`) `[V]`. So
  what is load-bearing outside this file is now the **R1-R4 labels** first
  and §7.2 second; the per-arm §7.1 numbering that the earlier generated
  descriptions spelled out is no longer quoted downstream.
  **`src/` has stopped citing this document by section entirely.** Doctor's
  operator-facing strings used to carry `§7.1 arm 7`, `§3.5`, `R2`, `§7.3`,
  `§4.2` and `§7.2`; each now states the rule or the action itself, on the
  grounds that this is an internals document, is absent from `docs/SUMMARY.md`,
  and an operator who meets a pointer to it in `rwv doctor` output has nothing
  to open `[V]`. The only surviving section references under `src/` are two in
  `src/bin/generate-explain.rs` (`:1570`, `:2967`) — prose about the rule and a
  gate fixture, not operator output `[V]`. **Renumber nothing regardless.** The
  numbering is still quoted by the three surfaces above, by
  `docs/explanation/joints/vcs-as-seam.md:307` (§4.3 and Q6) `[V]`, and by this
  document's own cross-references, and a renumber is a cross-surface change
  larger than any single edit to this file.
- `docs/reference/doctor-findings.md` — the published, operator-facing
  findings page keyed by `kind`, added after the previous citation base and
  listed in `docs/SUMMARY.md:55`. It cites **nothing** from this document —
  no section, no rule label `[V]` — which is the point: it is the surface an
  operator is meant to open, and this one is not.
Every claim that came from the earlier branch-behaviour investigation is
restated in this document; no external hop is required to derive any answer
here.
