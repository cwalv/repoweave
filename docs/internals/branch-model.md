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
was re-verified against commit `37548fd8ee092178663061e20a1695794ecd7b5a`
(2026-07-25) — the commit at which the implementation of this model finished
landing. A `[V]` marker means "true at `37548fd`".

**This document has changed status.** It was written as a design against a
tree that did not implement it. That tree no longer exists. The
implementation landed the type split (§4), the flat ephemeral name (§3.5),
the receipt registry (R2), the warrants (R3), the store-destroy gate (R4),
the consent flags (§4.4), doctor's missing arms (§4.5, §7.2), and the
one-time migration (§7) — and then **deleted the old surface outright**:
`Vcs::current_ref`,
`Vcs::checkout`, `Vcs::delete_branch`, `Vcs::restore_savepoint`,
`create_worktree`, `push_with_role`, `list_branches_with_prefix`, and
`parse_ephemeral_branch_name` have zero definitions and zero call sites in the
tree. Passages that diagnosed those constructs are therefore **historical**:
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
there are now **zero** `current_ref` call sites anywhere in the tree `[V]`
(the ~18 surviving textual matches are all comments explaining the deletion,
several of them restating what the old collapse meant — `status.rs:160-165`,
`lock.rs:100-104`, `git.rs:1864-1867`, `vcs.rs:2470-2478`).

The broader claim survives the fix that closed it. Where `sync.rs` reads
attachment now, it reads it to *refuse a MOVE when there is no branch to move
onto*, not to make sync branch-aware; sync still never reads *which* branch,
only whether one exists. The observation surface is `Vcs::head_attachment`
(§4.5), and its **18** production call sites `[V]` are: `check.rs` ×4,
`vcs.rs` ×4 (re-observation guards inside the default MOVE/DESTROY bodies),
`sync.rs` ×3, `push.rs` ×2, and one each in `git.rs`, `lock.rs`, `status.rs`,
`fetch.rs`, `update.rs`. Note which two files fell to **zero**:
`workweave.rs` and `add_remove.rs`, the two that used to *derive branch names*
from an observed HEAD, now derive nothing from observation at all (§3.5).

Everything structural is still SHA-and-DAG:

- `sync_one_repo` rebases or fast-forwards *whatever is checked out* onto a
  SHA (`sync.rs:577-628`) `[V]`.
- `ff_advance_repo` — the primitive that "lands" work into a target — obtains
  an `AttachedRef` witness from the target and advances *that* ref to CWD's
  tip; a detached or unborn target is a refusal
  (`sync.rs:5401-5497`, the three-armed `head_attachment` match at
  `:5420-5449`, the MOVE at `:5493-5495` → `git.rs:1251-1258`) `[V]`. This is
  the one place `sync.rs` names a branch — a precondition, not identity
  tracking; §4.6(1) is the argument that produced it, and it is now enforced
  by the type rather than by the author remembering.
- Retire's convergence check compares each repo's HEAD for **exact SHA
  equality** between CWD and target, not an ancestor check
  (`retire_workweave_after_sync_to`, `sync.rs:4315-4402`, the `!=` at
  `:4353`) `[V]`; delete's unmerged gate walks `is_ancestor` in the resolved
  canonical store (`collect_diverged_paths`, `workweave.rs:1989-2091`,
  `is_ancestor` at `:2054`) `[V]`; `rwv workweave log` computes
  `unique_commits`/`unique_diff` from `head_revision` with no `is_ancestor`
  call at all (`workweave_log`, `workweave.rs:3126-3328` →
  `git.rs:1828-1857`) `[V]`. Three different DAG queries — all three read
  only SHAs, never a branch name.
- `rwv.lock` records a resolved revision, never a branch
  (`lock.rs:134-162`, `manifest.rs:1083-1089`) `[V]`; the lock entry type is
  `ResolvedRevisionId` (`manifest.rs:1126`) `[V]`.

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
(`push.rs:177-208`; the three arms at `:191-208`) `[V]`. Two of this
document's proposals are visible in that one function: the gate no longer
consults `default_branch`'s fabricated `"main"` (§4.2, §6.2), and the
non-repo case surfaces `NotARepo` rather than "detached HEAD" (§4.5).

The asymmetry this section used to name — **the one repo whose branch identity
gates publishing is the one repo whose branch identity nothing verifies** — is
**closed**. `scan_repos_on_disk` still walks registry directories only
(`workspace.rs:261-309`) `[V]`, but doctor's branch-discipline pass no longer
uses it as its walker: `workweave_checkouts` explicitly appends
`<workweave>/projects/<project>/` (`check.rs:2796`) and the canonical pass
iterates `workweave_index::projects_on_disk` alongside the manifest members,
with a dedicated scope arm so project-repo findings survive a project-scoped
run (`check.rs:3980-3982`) `[V]`. `git checkout --detach` in the project repo
now produces a finding, pinned by
`tests/branch_discipline_test.rs:706 detached_project_repo_is_reported`, with
`:738 attached_project_repo_is_clean` as its non-vacuity pair `[V]`. This is
§5.1's scope hole, shipped.

### 1.3 L2 — the canonical member store (`<weave>/<repo_path>`): a branch is a tracking declaration

A member repo's `version:` field in `rwv.yaml` is typed `RefName`
(`manifest.rs:446`) `[V]` and is **branch-only by design**. This is a settled
decision, restated here so it need not be looked up:

> The manifest declares what to TRACK; the lock records where you ARE.

A tag or SHA in `version:` conflates the two layers — genuine pinning needs a
differently-named field. `rwv update` means "advance to the tip of the branch
you declared", and that verb is meaningless without a per-repo branch name.
Counting every read of a `version` field — the manifest's `version:` or its
lock-entry echo — `check.rs` has 8 sites, `fetch.rs` 12, `sync.rs` 5,
`push.rs` 4, `lock.rs` 3, `update.rs` 5 `[V]`. (Of `sync.rs`'s 5, only
`sync.rs:1261` reads the manifest; the other four read lock entries.) The
counts rose where this model added a *typed* read: `push.rs`, `fetch.rs`, and
`update.rs` each now parse the declaration at their seam —
`TrackingRef::parse(RawRefName::new(entry.version.as_str()))` at `push.rs:397`,
`fetch.rs:755`, `update.rs:664` `[V]` — because `manifest.rs`'s field is still
typed `RefName` and migrating it is separate work (§4.2).

Note precisely what `version:` names: a branch **on the remote**. `rwv update`
resolves it through `Vcs::resolve_branch_on_remote`, which explicitly refuses
a bare-branch fallback so callers "don't silently advance to the local branch
tip" (`vcs.rs:1793-1809`) `[V]`. That trait method's doc comment also claims
`upstream/<branch>` for `Role::Fork`; the shipped `GitVcs` impl still does not
implement that half — every role resolves to `origin`, `let _ = role; // all …
use origin` at `git.rs:773-782` and again in `push_ref` at `git.rs:2135` `[V]`
— drift already on record from a separate audit and out of this
document's scope to fix. The manifest's `version:` and any local branch of
the same name are different objects in different namespaces that happen to
share a spelling; `TrackingRef::local_counterpart` (`vcs.rs:881-883`) is now
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
   `git.rs:1957` (adopt an existing ref) and `:1966` (author one, `-b`)
   `[V]`. The old three-site `create_worktree` this section used to count is
   deleted; `create_worktree_on` (`vcs.rs:2647-2663`) is no longer an
   uncalled sibling but the production path, called from `workweave.rs:1129`,
   `sync.rs:1282`, and `add_remove.rs:59` `[V]`.

`docs/explanation/joints/workweave-hierarchy.md:189-208` gives exactly this —
and *only* this — as the justification for the ephemeral naming scheme `[V]`:
"Because primary's `main` and a workweave's `myproj--hotfix/main` are
different branch names, no two workweaves compete for the same named branch."
(The joint's example uses `myproj--hotfix/main` `[V]`; earlier drafts of this
document quoted a different `<project>--<workweave>/main` spelling by
mistake.) **That joint has not been updated for §3.5**: it still presents the
three-part `<project>--<workweave>/<segment>` shape as what rwv mints, at
`:192` and `:204`, which the shipped code no longer does — see §6 item 7.

The normative invariant that enforces it is I3, in
`docs/explanation/joints/clone-topology.md:82-114` ("tier-0" is stated at
`:116` and in the tier table at `:19-25`, not inside I3's own text). It has
a dedicated scanner set — now **three** passes, not two:
`scan_workweave_repo_branches` (`check.rs:3066-3179`),
`scan_canonical_stores` (`:3361-3524`), and `scan_dangling_receipts`
(`:3587-3637`), entered from `scan_branch_discipline` (`:3695-3730`) `[V]`.
The `BranchDisciplineKind` violation taxonomy is now **twelve** sub-kinds
(`check.rs:708-942`; it was six when this document was written, and five
before that), with a `--fix` and a **38**-test suite
(`tests/branch_discipline_test.rs`; it was 15, and 20 before that — all counts
`[V]`). The scanner/taxonomy/test-suite claim is sourced from the code, not
from `clone-topology.md` itself, which states only that I3 is a tier-0 spec.
But read I3's stated purpose carefully:

> "The merged-check that gates delete/retire — 'is the source's tip an
> ancestor of the target's tip?' — runs in one ref namespace at a time … The
> ephemeral-branch convention makes the question well-defined."
> (`clone-topology.md:96-102`) `[V]`

That is a **disjointness** requirement. It is satisfied by *any* scheme in
which no two workspaces hold the same ref name. It is not a claim that
workweave branch names carry meaning. The code agrees: `workweave.rs:2296-2305`
states defensively that "Branch names are creation-time namespaces, **NOT
lineage records**", and `status.rs:87-98` restates it — "Parent identity comes
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
at `vcs.rs:259-278`, still a bare newtype with a `pub fn new(impl
Into<String>)` constructor and no validation `[V]` — was used for the
manifest's `version:`, for minted ephemeral names, for whatever `current_ref`
returned, for the deletion argument, and for the `default_branch` fallback.
Nothing in the type system distinguished them, so mixing them was invisible.

What changed: notions (1)–(4) are now `TrackingRef`, `EphemeralRefName` /
`OwnedRef`, `AttachedRef`, and `HeadAttachment::Detached(DetachedHead)`
respectively (§4.2), and **three of the five `RefName` sites listed above no
longer exist** — `current_ref` and `delete_branch` were deleted, and ephemeral
names are minted by `EphemeralRefName::mint` `[V]`. `RefName` itself survives,
still unvalidated, but only on the surface this model did not reach:
`manifest.rs:446`'s `version:` field (parsed into a `TrackingRef` at each
consumer's seam rather than at the field, §1.3), `default_branch`
(`vcs.rs:1855`), `tag_at_head` (`:1845`), the `BranchAlreadyExists` error
payload (`:302`), and the two prune predicates (`:2182`, `:2202`) `[V]`.
Migrating the manifest field is the one piece of §4.2's table left undone.

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
  (`vcs.rs:79-81`) `[V]`. The call site could not express "move the branch
  I'm on", so it silently expressed "stop being on a branch".
  **Fixed.** `Vcs::checkout` is deleted; `fetch` now goes through
  `realign_present_clone` (`fetch.rs:718-810`), which reads `head_attachment`
  and either advances the attached counterpart, moves an already-detached
  HEAD, or refuses naming `--detach-checkouts`; `update` mirrors it in
  `advance_checkout` (`update.rs:606-714`) `[V]`. Pinned by
  `tests/fetch_in_place_test.rs:335
  in_place_fetch_fast_forwards_the_counterpart_and_stays_attached`, which
  asserts the checkout is still on `main` *and* that `main` itself moved —
  the exact assertion §4.7 says the suite lacked `[V]`.
  Half of the reporting defect survives: `rwv update` now counts SHA deltas
  rather than non-`Err` outcomes (`update.rs:271-310`, with an in-code comment
  citing §6.2), but its `branch` JSON field still echoes `entry.version`
  verbatim (`update.rs:272`) `[V]`.

- **`rwv doctor` had no arm for the state its own verbs produced most often.**
  The canonical-store scan reported only on `Ok(Some(branch))`; `Ok(None)` —
  detached — matched no arm and produced **no finding**, while `rwv push`
  hard-refused on that same state `[V]` `[S]`.
  **Fixed.** `scan_canonical_stores` matches `HeadAttachment` exhaustively and
  its `Detached` arm always emits `BranchDisciplineKind::CanonicalDetached`
  (`check.rs:3411-3436`), carrying the SHA, the tracking counterpart, and
  whether a reattach is provable `[V]`. Pinned by
  `tests/branch_discipline_test.rs:569 detached_canonical_is_reported`.
  Doctor's remediation advice was also wrong for the case rwv produced: it
  said `git switch -c <prefix>/main`, which errors with "already exists"
  `[S]`. **Also fixed** — `reattach_advice` (`check.rs:4759-4772`) emits
  `git switch <name>` when a receipt names an existing ref and reserves the
  `-c` spelling for when none does `[V]`.

- **`create_worktree` silently force-deleted a colliding branch.** On
  "already exists" it ran `git branch -D` and retried. Verified destroying a
  pre-existing `my-app--feat2/main` carrying a unique commit; reflog wiped,
  commit dangling, **no `--force` needed and nothing printed** `[S]`.
  **Fixed by deletion.** `create_worktree` no longer exists; its successor
  `materialize_worktree_on_ref` (`git.rs:1931-1968`) classifies before acting
  and **adopts** a pre-existing ref (`:1950-1958`) rather than destroying it,
  returning `None` so the caller knows it did not author it `[V]`. There is
  now exactly **one** `branch -D` in `git.rs`, inside `destroy_local_ref`
  (`:2073-2077`), reachable only behind `OwnedRef` + `DeletionWarrant` `[V]`.

- **`rwv doctor --fix` deleted hand-made branches.**
  `parse_ephemeral_branch_name` claimed any `<a>--<b>/<c>` name; hand-made
  `my--feature/wip` and `notes--todo/scratch` were deleted by a plain
  `rwv doctor --fix` `[S]`. Nothing recorded which refs rwv actually created —
  the workweave marker was `{primary, project, parent}` and nothing else.
  **Fixed.** `parse_ephemeral_branch_name` is deleted; ownership comes from
  the receipt registry, and `fix_stale_ephemeral_branches` re-resolves every
  candidate through `RecordedRefs::for_store`, refusing with "rwv holds no
  ownership receipt for it (branch-model.md R2)" when there is none
  (`check.rs:4026-4121`, refusal at `:4071-4077`) `[V]`. Pinned by
  `tests/branch_discipline_test.rs:1005 handmade_lookalike_branch_survives_doctor_fix`
  and `:1057 flat_lookalike_branch_survives_doctor_fix`. The marker is still
  `{primary, project, parent}` (`workspace.rs:983-987`) `[V]` — deliberately:
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
  point elsewhere (`sync.rs:5401-5497`) `[V]`. This is the chain §4.6(1)
  derives the type split from. Pinned by
  `sync.rs:5602 ff_advance_repo_refuses_to_land_onto_a_detached_target`,
  `:5578 ff_advance_repo_lands_on_the_branch_the_target_is_attached_to`, and
  the compile-fail probe
  `tests/branch_model_compile_fail_test.rs:532 a_witness_cannot_point_a_move_at_a_different_repo`.

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
  `EphemeralRefName::mint(project, workweave)` (`vcs.rs:969-971`), total and
  two-argument, called from `workweave.rs:1454`, `sync.rs:1275`, and
  `add_remove.rs:91` `[V]`. `sync.rs:1245-1260` now carries a comment
  explicitly retracting the false "mirrors create_workweave's naming" claim
  `[V]`. Recursion, the `detached-<12sha>` fallback, and the 12-char
  truncation are all gone; nesting cannot occur because there is no third
  component to nest.

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
  **Fixed.** `observe_head` (`git.rs:1863-1906`) tests `is_repo` first and
  returns `VcsError::NotARepo` before reading HEAD (`:1868-1870`); an
  unreadable ref database is `CommandFailed`, not a state `[V]`. §4.5 is the
  argument, and the four-state collapse no longer has a value to live in.

- **`prune_dropped_repo` is blocked by rwv's own refs while they exist.**
  *(This one is unchanged, by design.)* It refuses if any local branch lacks
  an `origin/` counterpart (`sync.rs:1440-1468`) `[V]` — which every
  rwv-authored ephemeral branch does by construction. A store that *currently
  holds* an rwv-authored ephemeral branch cannot be pruned; a clean
  `workweave delete` removes the ephemeral branches, after which the predicate
  passes. The refusal's full message is "dropped from lock but clone has
  local-only commits; push them and re-run, or remove manually"
  (`sync.rs:1470-1473`) `[V]` — the second remedy works. This refusal was also
  the only thing standing between a live workweave's git backing and
  `remove_dir_all`; that is no longer true, because R4 now gates the destroy
  independently (`check_store_unclaimed` at `sync.rs:1479`, before the
  `remove_dir_all` at `:1480-1481`) `[V]`. See the `prune_dropped_repo` row
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
(`tests/destructive_ops_audit_test.rs:289-323`) now records `count: 1` and
justifies `destroy_local_ref` and nothing else — naming, in its own text, that
`create_worktree`'s force-delete-and-retry (whose "deletes a STALE branch"
claim was measured FALSE) and `delete_branch` were both deleted, and that "the
force-delete of a ref rwv holds no receipt for is now unreachable because the
code that could do it does not exist" `[V]`. The one surviving site is
`git.rs:2075`, behind `OwnedRef` (R2) and `DeletionWarrant` (R3).

The `"checkout"` entry has moved the same way. It is now at `:239-287` with
`count: 4`, and opens by recording that "the bare `checkout()` that used to
head this list is DELETED" `[V]`. Its clauses no longer consider only
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
R4-gated: `prune_dropped_repo` (`check_store_unclaimed` at `sync.rs:1479`,
destroy at `:1480-1481`) `[V]`, `rwv remove --delete` (`refuse_claimed_store`
at `add_remove.rs:452`, destroy at `:477`) `[V]`, and `workweave delete`
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
  `undo_ref_births`, `workweave.rs:714`, via `DeletionWarrant::unmoved`
  `vcs.rs:1688`) `[V]`.
- **Merged** — the ref's tip is an ancestor of a named baseline (the recorded
  parent workweave's tip, or the primary weave's tip). This is what
  `workweave delete`'s diverged-paths check computes via `is_ancestor`
  (`collect_diverged_paths`, `workweave.rs:1989-2091`) `[V]`; `sync-to
  --retire` runs it too, by calling into the same delete path after its own
  separate, exact-SHA-equality convergence precondition (`run_retire`,
  `sync.rs:4181-4200` → `retire_workweave_after_sync_to`, `:4315-4402`) `[V]`.
  **Shipped**: `DeletionWarrant::merged` (`vcs.rs:1698`) is the constructor,
  and `retire_recorded_refs` (`workweave.rs:2418-2500`) establishes one per
  recorded ref against `baseline_tips_in_store` (`:2282-2298`) before any
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
  (`commit_replay_exclusion_migration`, `check.rs:1825-1943`) `[R]` — commits
  onto the project repo's current branch. Ref write: the branch advances.
  HEAD's symbolic target is unchanged. → **MOVE**. Legal, no new consent.
- **`rwv abort`'s savepoint restore** (`abort_one_repo`, `sync.rs:4918-4946`,
  the `verified_restore_savepoint` call at `:4938-4945` → `git.rs:2224-2234`,
  reset at `:2231`) `[V]` — resets the current branch to
  `refs/rwv/pre-op/<id>`. → **MOVE**, usually a rewind — and its
  `DiscardWarrant` (§3.2) is trivially held: the savepoint the warrant
  requires is the very ref being restored to, and invoking `abort` is the
  named consent. Note it *already* implemented the attributability
  discipline R3 generalizes: `verified_restore_savepoint` classifies the
  tip and refuses on `ForeignTip` (`vcs.rs:546-594`, `:2107-2150`) `[V]`.
- **A hypothetical `rwv workweave rename`** — would change a ref's name, which
  is a DESTROY of the old name plus a birth of the new. → needs a receipt for
  the old name. Derivable without amending anything.
- **`git commit` run by the operator inside a workweave** — not an rwv
  operation. Out of scope; the rule governs rwv's writes.

### 3.5 The deletion: drop `<segment>` — **shipped**

Ephemeral branch names are **`{project}--{workweave}`**. Flat. No third
component. `EphemeralRefName::mint(&ProjectName, &WorkweaveName)`
(`vcs.rs:969-971`) is total, takes two arguments, and is the only minter
`[V]`.

This was justified by evidence, not taste:

- The docs justify the naming scheme **solely** by git's
  one-worktree-per-branch constraint
  (`workweave-hierarchy.md:189-208`, `clone-topology.md:96-102`) `[V]`, which
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
(`git.rs:2163-2178`) — a change made *because* git's `*` stops at `/` and so a
glob would silently omit a leftover ref `[V]`; and doctor's scoping now keys
on the workweave *directory* basename via `branch_discipline_in_scope`
(`check.rs:3936-3990`), never on a path segment of a branch name `[V]`. The
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
(`cli.rs:461-469`) supplies an arbitrary path, so two `--dir`-placed
workweaves with the same *name* but different paths each passed their own
existence check `[V]`; the registry insert is a silent last-writer-wins
(`record_workweave`, `workweave_index.rs:434-444`) `[V]`, deliberately, for
placement entries. The `<segment>` used to disambiguate the minted names
accidentally; flattening removed that. So this model added a **uniqueness
check at create**, and it shipped: `workweave create` now consults
`workweave_index::lookup_raw` before the directory-existence check and bails
naming the ref both workweaves would mint (`workweave.rs:1234-1257`, lookup at
`:1242`; the existence check follows at `:1259`) `[V]`. Pinned by
`tests/branch_model_lifecycle_test.rs:369
create_refuses_a_name_the_index_already_records`. (The residual
`--`-in-a-name ambiguity is Q12, still open — see §9.)

### 3.6 The mid-operation precondition on detached MOVEs

`HeadAttachment::Detached` collapses two different situations: "rwv
detached this HEAD at a lock SHA" and "the operator is mid-operation" — a
`git bisect`, a `rebase -i` stopped at an `edit`. Yanking HEAD out from
under the second is a consentless loss of operator state: the same
collapse-of-distinct-states sin §4.5 abolishes for `Ok(None)`, reappearing
inside the `Detached` variant.

So: a MOVE of an already-detached HEAD refuses when the repo is
mid-operation, naming the operation. **This is shipped end to end.** The
detection is `GitVcs::mid_op_state` (`git.rs:493-518`), which checks
`rebase-apply` / `rebase-merge` / `MERGE_HEAD` / `CHERRY_PICK_HEAD` /
`BISECT_LOG` `[V]`; its own doc comment names this section by number ("it is
operator state living in HEAD's position … that is the state the
detached-MOVE precondition (§3.6) exists to see"). `advance_detached_head` —
the MOVE primitive this section specifies — refuses via a dedicated
`mid_operation` trait method wired straight to `mid_op_state`, not through the
older `mid_op`/`ConflictOp` path (`vcs.rs:2609-2632`, refusal at
`:2614-2619`; `mid_operation` impl `git.rs:1927-1929`) `[V]`.

And the wiring this section said was missing has landed: **`fetch` and
`update` now call `advance_detached_head`** (`fetch.rs:736`,
`update.rs:622`) `[V]`, so an already-detached repo they touch is a MOVE
subject to this precondition rather than an unconditional `checkout <sha>`.
The earlier statement that they "call neither one, zero times" is no longer
true.

One asymmetry remains, unchanged and worth keeping visible: `sync.rs` still
consults only the older `mid_op`, a `ConflictOp`-returning wrapper over
`mid_op_state` whose `match` has no bisect arm and folds it into `None`
(`git.rs:1431-1438`) `[V]`. So a bisect is seen by the MOVE primitive and not
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
terms of the new one. `vcs.rs:2470-2478` records the deletion in the trait
itself, and states the rule that governed it: the replacements "were deleted
only once every call site had been restated in terms of this surface — and
that restatement was the audit: a site that could not say which replacement it
meant was a site nobody had classified" `[V]`.

Two things this section specified are **not** shipped, and are called out at
point of use rather than here: `manifest.rs`'s `version:` field is still typed
`RefName` (consumers parse a `TrackingRef` at their own seam, §1.3), and Q6 —
which ref a member repo publishes — is still open, with `PublishRef` holding
the shape of the answer but not the answer (`PublishRef::from_local`,
`vcs.rs:1429`, is the unchosen alternative, `#[allow(dead_code)]` and exercised
only by a unit test) `[V]`.

### 4.1 The precedent, in this same file

repoweave has already solved this exact shape once. A lock scalar and a
resolved commit were the same type; "compare the raw thing against the
resolved thing" was a legal line that was always wrong. The fix was two
types.

`ResolvedRevisionId` (`vcs.rs:16-124`) `[V]`:

- Construction is **path-rooted**: the only public constructors are
  `Vcs::resolve_revision` / `Vcs::head_revision` (which resolve against a real
  repo), `from_canonical` (`:49`, mint with a known SHA), and
  `from_rev_parse_output` (`:70`, mint from raw ref-resolution output,
  **verifying** the canonical form — added since this precedent was first
  written, to give savepoint/pre-abort-ref resolution a real constructor
  instead of the escape hatch the next bullet used to name).
- "There is no public way to mint a `ResolvedRevisionId` from a free string —
  the parse boundary lives in `RawRevisionId`."
- Deliberately **no `Deserialize` impl**. Lock-file scalars deserialize into
  `RawRevisionId`; the only way to obtain a resolved value is resolution.
- The escape hatch this precedent originally needed, `pub(crate)
  from_canonical_unchecked`, is **gone** — deleted, its one caller replaced
  with the verifying `from_rev_parse_output` above (`vcs.rs:55-76`) `[V]`. A
  cleaner outcome than the precedent originally shipped with, not a regression
  in it.

`RawRevisionId` (`vcs.rs:128-197`) `[V]`:

- Wraps the YAML scalar verbatim; at the type level we do not know whether it
  is a tag, a branch, or a SHA.
- "It is intentionally not interchangeable with `ResolvedRevisionId`: there is
  no `PartialEq` between the two, and `RawRevisionId` cannot be fed to
  commit-id operations such as `Vcs::advance_attached_ref`." (The doc comment
  named `Vcs::checkout` when this document quoted it; the implementation
  deleted that method and updated the sentence — `vcs.rs:136` `[V]`.)
- The invariant is enforced by a **`compile_fail` doctest**
  (`vcs.rs:156-164`) `[V]`:

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
(`check.rs:4705-4710`) `[V]`:

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
| `RawRefName` | the parse boundary | `Deserialize`; `RawRefName::new(&str)`; VCS porcelain listings | `vcs.rs:633` |
| `TrackingRef` | (1) `version:`, declared | `TrackingRef::parse(RawRefName)` | `vcs.rs:839` |
| `EphemeralRefName` | (2a) an ephemeral name **requested** | `EphemeralRefName::mint(&ProjectName, &WorkweaveName)` | `vcs.rs:963` |
| `OwnedRef` | (2b) an ephemeral ref **rwv holds a receipt for** | `RefRegistry::record_created(...)` or `RefRegistry::lookup(...)` | `vcs.rs:1066` |
| `AttachedRef` | (3) what a checkout is on | `Vcs::head_attachment(repo)` only | `vcs.rs:1187` |

`TrackingRef::parse` is where the §8.8 decision became executable: it runs the
git `check-ref-format` intersection (`validate_ref_name`, `vcs.rs:745-790`)
and then rejects SHA-shaped (`RefNameError::ShaShaped`, ≥7 all-hex) and
tag-shaped (`RefNameError::TagShaped`, `vN.N…`) input outright
(`vcs.rs:848-862`) `[V]`. Like `ResolvedRevisionId`, it has no `Deserialize`
impl. One caveat this document must not let slide: parsing happens at each
*consumer's* seam, not at manifest load, because `manifest.rs:446` still
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
and `!attached.is_named(&declared.local_counterpart())` (`push.rs:408`) `[V]`
— `AttachedRef::is_named` (`vcs.rs:1206`) being the named predicate that
replaced string equality. The author had to state which projection was meant,
which is exactly what §4.6(2) predicted the error would force.

`RawRefName` keeps `as_str()`: it is the parse boundary, and raw porcelain
output must stay inspectable. Each removal is enforced by a `compile_fail`
probe per §4.7 — four of them, one per type.

The legal conversions are these, and only these. Each is a named function
whose body is the place a policy decision lives. All four shipped with the
signatures below — `parse` at `vcs.rs:848`, `on_remote` at `:866`,
`local_counterpart` at `:881`, `mint` at `:969`, `is_attached_by` at `:1121`,
and `RefRegistry`'s pair at `workweave_index.rs:649` / `:733` `[V]`:

```rust
// Rejects SHA-shaped and tag-shaped values: `version:` is a tracking
// declaration, not a pin. Rejects anything that is not a valid ref name
// component.
impl TrackingRef { pub fn parse(raw: RawRefName) -> Result<Self, RefNameError>; }

// The remote branch `version:` actually names. Already existed in spirit as
// Vcs::resolve_branch_on_remote (vcs.rs:1777-1793) — "in spirit" is load-
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
(`vcs.rs:2567`) takes no path, re-observes via
`head_attachment(witness.repo())` (`:2524`), and `ff_advance_repo` now
*obtains* the witness from the target rather than being handed one
(`sync.rs:5420-5449`) `[V]`. The dodge is a compile error, pinned by
`tests/branch_model_compile_fail_test.rs:532
a_witness_cannot_point_a_move_at_a_different_repo` and by
`tests/branch_model_test.rs:310
advance_attached_ref_refuses_a_witness_for_a_repo_that_became_detached`.
(The witness's validity *window* — what happens when the repo's state changes
between production and consumption — is Q15, §9, still open.)

Below, the remaining `RefName` sites in the trait, and the type each gets.
Four of the six are **not yet converted** — the implementation reached the
branch model's own surface and stopped at the edges, which is honest to
record:

| Site | Type today | Under the split |
|---|---|---|
| `BranchAlreadyExists { branch }` (`vcs.rs:302`) | still `RefName` `[V]` | `RawRefName` — an error reports an observed name |
| `tag_at_head` (`vcs.rs:1845`) | still `Option<RefName>` `[V]` | `Option<RawRefName>` — a tag is not a branch; it never enters the branch model |
| `default_branch` (`vcs.rs:1855`) | still `RefName` `[V]` | `Option<RemoteDefaultBranch>` — see below. **The producer shipped and is now wired**: `Vcs::remote_default_branch` (`vcs.rs:2921`, impl `git.rs:2146-2161`) is what `rwv push`'s gate (`push.rs:179`) and doctor's canonical pass (`check.rs:3237`) call. `default_branch` survives only for `rwv add`'s three `version:` writes — §6.2 |
| `branch_has_remote_counterpart` (`vcs.rs:2182`) | still `&RefName` `[V]` | `&RawRefName` — the prune predicate inspects observed names |
| `count_commits_ahead_of_remote` (`vcs.rs:2202`) | still `&RefName` `[V]` | `&RawRefName` — same predicate |
| `list_local_branches` (`vcs.rs:2217`) | still `Vec<RefName>` `[V]` | `Vec<RawRefName>` — **shipped as a sibling rather than a conversion**: `list_local_branch_names` (`vcs.rs:2940`, impl `git.rs:2180-2194`) returns `Vec<RawRefName>` and is what the branch model's listing path uses |

**`RemoteDefaultBranch`** is the remote's own declaration of its primary
branch — the target of `refs/remotes/origin/HEAD`. It is none of §2's four
notions, and it is **not a fifth kind**: rwv never writes it, so it sits
outside the MOVE/ATTACH/DESTROY classification entirely — it is a
read-only *input* to the L1 publish gate (`push.rs:177-208`). It gets its
own type rather than reusing `RemoteRef` because provenance differs: a
`RemoteRef` is the projection of a *declared* `TrackingRef`; a
`RemoteDefaultBranch` is *observed* remote state. **Shipped in full**
(`vcs.rs:1340-1396`) `[V]`: its sole producer is
`Vcs::remote_default_branch(repo) -> Result<Option<RemoteDefaultBranch>, VcsError>`
(`vcs.rs:2921`, `GitVcs` impl `git.rs:2146-2161`) `[V]`, which returns `None`
when `origin/HEAD` is unset or malformed — **no fallback**, exactly as
specified — and `RemoteDefaultBranch::local_counterpart()` (`vcs.rs:1386-1388`)
`[V]` exists. **The publish gate is now wired to it** (`push.rs:179`), so a
weave with no `origin/HEAD` refuses instead of being told its branch is
`"main"` — §4.5's make-the-collapse-unrepresentable move, applied to this
value. What has **not** shipped is the rest of the wiring: `default_branch`
still fabricates `"main"` on any failure and on a malformed symref
(`git.rs:908-922`, `.strip_prefix(…).unwrap_or(FALLBACK)` at `:913-918`) `[V]`,
and `rwv add`'s three `version:`-writing sites still call it — §6.2.

**`BornRef`** is proof of authorship: `create_worktree_on` returns one iff
*this call created* the ref (an adopted pre-existing ref yields none), and
its only consumer is rollback. §6.1's fix depends on exactly this: rollback
deletes only refs it holds a `BornRef` for, so it can no longer delete a
branch the create merely adopted. The receipt itself is written *before*
the birth by `RefRegistry::record_created` (above), so `BornRef` carries no
registry duty — it separates "authored" from "adopted", nothing more.
**Shipped**: `create_worktree_on` (`vcs.rs:2647-2663`) returns
`Option<BornRef>` from the adopt/author classification in
`materialize_worktree_on_ref` (`git.rs:1950-1958`), and `workweave.rs:1129`
turns it into a `RefBirth::{Authored, Adopted}` that rollback keys on
(`undo_ref_births`, `workweave.rs:691-770`) `[V]`.

The honest inventory, then: **five core types** (`RawRefName`,
`TrackingRef`, `EphemeralRefName`, `OwnedRef`, `AttachedRef`), **six
supporting types** (`RemoteRef` `vcs.rs:897`, `LocalRefName` `:928`,
`DetachedHead` `:1275`, `UnbornRef` `:1248`, `BornRef` `:1311`,
`RemoteDefaultBranch` `:1361`), one wrapper whose *policy* is deferred
(`PublishRef` `:1408`, §4.3, Q6), the consent tokens and warrants of §4.4, and
the `RefRegistry` (`workweave_index.rs:595`) — all present `[V]`. Not "five
types replace `RefName`" — five notions become five types, and the rest of the
trait surface is typed to match.

Implementation added four types this section did not anticipate, each earning
its place: `HeadObservation` (`vcs.rs:1458`, the VCS-specific half of
`head_attachment`), `LegacyEphemeralRefName` (`:1017`, so §7.1 arm 1 can name
the pre-flat shape without re-admitting a parser), `RefNameError` (`:660`), and
`SavepointRef` (`:1597`, the `DiscardWarrant`'s payload) `[V]`.

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
`advance_if_fast_forward` (`vcs.rs`, impl `git.rs:1251-1258`) and
`hard_reset` remain on the trait as the mechanical primitives the typed MOVEs
call underneath. `advance_if_fast_forward` still takes a bare `&Path`, which
is why §4.6(1)'s argument is about the *caller's* obligation rather than
about that method's signature; `hard_reset` now has **zero** call sites in
`sync.rs` — `rewind_project_repo` (`sync.rs:4433-4470`) goes through
`reset_attached_ref` with a `DiscardWarrant` instead `[V]`.

The replacement, shipped (`vcs.rs`, decls at the line numbers noted):

```rust
// ---- observation -------------------------------------------------------
// Replaces current_ref. Total over the four states it used to collapse.
fn head_attachment(&self, repo: &Path) -> Result<HeadAttachment, VcsError>;  // :2483

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
                        to: &ResolvedRevisionId) -> Result<(), VcsError>;   // :2551
fn reset_attached_ref(&self, on: &AttachedRef,
                      to: &ResolvedRevisionId, warrant: DiscardWarrant)
    -> Result<(), VcsError>;                                                // :2566
// Moving a HEAD that is already detached is a MOVE, not an ATTACH —
// subject to the mid-operation precondition (§3.6). DetachedHead carries
// its repo, like AttachedRef.
fn advance_detached_head(&self, was: &DetachedHead,
                         to: &ResolvedRevisionId) -> Result<(), VcsError>;  // :2593

// ---- ATTACH ------------------------------------------------------------
// Birth: no consent token, because there was no prior attachment to lose.
// Takes the receipt (already persisted by RefRegistry::record_created,
// §4.2 — receipt-first ordering); the store, name, and start point all
// come from it. Returns Some(BornRef) iff this call authored the ref,
// None when it adopted a pre-existing one (§6.1's rollback keys on this).
fn create_worktree_on(&self, owned: &OwnedRef, dest: &Path)
    -> Result<Option<BornRef>, VcsError>;                                   // :2631
// Post-birth attachment change. Both take a consent token minted from the
// corresponding named flag (see §4.4 for where the tokens live).
fn detach_head(&self, from: &AttachedRef,
               to: &ResolvedRevisionId, consent: DetachConsent)
    -> Result<(), VcsError>;                                                // :2699
fn reattach_head(&self, from: HeadAttachment,
                 to: &LocalRefName, consent: ReattachConsent)
    -> Result<(), VcsError>;                                                // :2717

// ---- DESTROY -----------------------------------------------------------
// Receipt (OwnedRef) + warrant. No overload takes a name.
fn delete_owned_ref(&self, repo: &Path, branch: &OwnedRef,
                    warrant: DeletionWarrant) -> Result<(), VcsError>;      // :2766

// ---- publish -----------------------------------------------------------
// The ref is now a parameter. Policy leaves the VCS impl (see §9, Q6).
// PublishRef is an opaque wrapper whose ONLY constructor lives in push.rs,
// at the single decision site §4.6(2) creates; Q6 decides what that
// constructor accepts (the attached ref, the tracking counterpart, or both
// under a rule). Until Q6 closes, the constructor is the one place the
// open question is visible — a deferred decision with a producer, not a
// placeholder without one.
fn push_ref(&self, repo: &Path, role: Role, r: &PublishRef, force: bool)
    -> Result<(), VcsError>;                                                // :2895

// ---- listing -----------------------------------------------------------
// Returns raw observed names. Report-only by type: a RawRefName is not an
// OwnedRef, so nothing here can be deleted without a registry lookup.
// Named list_branch_names_with_prefix, to leave no spelling that resolves
// to the deleted method.
fn list_branch_names_with_prefix(&self, repo: &Path, prefix: &str)
    -> Result<Vec<RawRefName>, VcsError>;                                   // :2915
```

Implementation added several more members in the same shape, each because a
verb needed to say something the list above could not: `verify_attachment`
(`:2523`), `resolve_local_branch_tip` (`:2542`), `materialize_worktree_on_ref`
(`:2675`, the VCS-specific half of `create_worktree_on`), `clone_attached_at`
(`:2699`, birth-at-the-lock-revision for `fetch`), `destroy_local_ref`
(`:2796`), `rename_owned_ref` / `rename_local_ref` (`:2822`, `:2852`, §7.1 arm
1), `adopt_detached_checkout` (`:2874`, §7.1 arms 3/5), and `birth_ref_at_head`
(`:2901`) `[V]`.

**`Vcs::checkout` and `Vcs::delete_branch` were removed, not deprecated — and
that has now happened.** So were `current_ref`, `restore_savepoint`,
`create_worktree`, `push_with_role`, and `list_branches_with_prefix`. Every one
of their call sites had to state which replacement it meant, and that
restatement was the audit. The trait says so itself, at `vcs.rs:2470-2478`:
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
(`cli::consent`, `cli.rs:532-688` — `DetachConsent` at `:585`,
`ReattachConsent` at `:616`), and the warrants live in `vcs.rs`
(`DiscardWarrant` `:1630`, `DeletionWarrant` `:1667` with the private
`WarrantKind` at `:1671`) `[V]`.

The "no minting caller yet" gap this section used to carry is closed, and
closed harder than this section asked for. The ask was that one module be the
only place a token is constructed. The private-field idiom delivered that
against the *tuple literal* — but a token has two construction routes, and
`from_flag` was the other one. While dispatch lived in `main.rs` it had to
stay open: a `[[bin]]` target is a separate crate from the `[lib]`, so the
narrowest visibility that admits it is `pub`, and a `pub fn` returning the
token is a second door standing open to every module of the library. Moving
dispatch into `cli::dispatch` is what let the visibility come down.
`from_flag` is now `pub(in crate::cli)` (`cli.rs:606`, `:629`, `:656`,
`:684`), the mints are at `cli/dispatch.rs:309`, `:623`, `:820`, `:825` and
`:1019`, and a `from_flag` call written into `vcs.rs` — the module this
section names as the one that must only ever *receive* a token — is now
`E0624`, not a code-review finding `[V]`. The module header states both seals
and why dispatch had to move (`cli.rs:532-575`).

`tests/consent_minting_audit_test.rs`, the static call-site allowlist that
stood in for the compiler, **no longer exists** — deleted, on the grounds
that an invariant checked by construction should not also carry a tripwire.
What it pinned is now either a compile error or documented where a reader
will meet it. The "not `pub`" half is pinned by error code like the rest, by
`the_flag_mint_is_not_reachable_from_outside_the_cli_module`
(`tests/branch_model_compile_fail_test.rs:435`) `[V]`; the in-crate half of
`pub(in crate::cli)` is not observable from an out-of-crate probe, and the
harness does not claim it is. `granted()` — the unconditional mint, which
checks nothing — is `#[cfg(test)]` on all three tokens that still have one
(`cli.rs:593-594`, `:621-622`, `:645-646`), so it is absent from the library
the binary and the integration tests link against; `AdoptDetachedConsent`
lost its `granted()` outright, because no fixture needed one `[V]`.

Two consent types shipped beyond this list, both needed by §7: 
`DiscardUnmergedConsent` (`cli.rs:640`, the `workweave delete` override R3's
`OperatorDiscarded` warrant is minted from) and `AdoptDetachedConsent`
(`cli.rs:675`, §7.1 arms 3/5). A third, `DiscardLocalCommitsConsent`
(`vcs.rs:1578`), deliberately lives in `vcs.rs` rather than `cli::consent`,
because `sync --continue` must re-mint it from the persisted owner record
rather than from a flag on the resuming invocation `[V]`. That exception now
states its cost rather than only its reason (`vcs.rs:1562-1576`): the layer
holding both spellings of the flag is `sync.rs`, which is a *sibling* of
`vcs.rs`, not a descendant — and `pub(in path)` requires an ancestor, so no
visibility tier names it. `pub(crate)` is the tightest seal the language
offers here, and it admits every module of the crate. The one production mint
is `sync::rewind_project_repo` (`sync.rs:4444`), which is instructed to
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
`VerifiedRestoreOutcome` (`vcs.rs:546-594`) is **not** the shape being
copied here. It is only ever a *return* type (`vcs.rs:2144`,
`git.rs:1348-1428`, `sync.rs:4924`) `[V]` — the fencing it describes lives
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
declared on `doctor` (`cli.rs:186-191`), minted at `cli/dispatch.rs:820`,
threaded into `run_check` (`check.rs:6042`), and consumed at `check.rs:6589-6613`,
where the `--fix` path calls `fix_detached_canonicals` **only** when the
consent is present. `fix_detached_canonicals` (`check.rs:4466-4531`)
re-observes `head_attachment`, requires both halves of §7.2's condition, and
calls `Vcs::reattach_head` with the token `[V]`. The report path is unchanged.
Pinned by `tests/branch_discipline_test.rs:634
detached_canonical_reattaches_only_with_consent`, which is a non-vacuity pair:
with the flag it reattaches, without it the store stays detached.

`--detach-checkouts` (`cli.rs`, `DetachConsent` `cli.rs:585`) is consumed by
`fetch`'s and `update`'s realign paths as §5's table says. A third flag,
`--adopt-detached-checkouts` (`cli.rs:192-199`, `AdoptDetachedConsent` `:675`),
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
`head_attachment`, `vcs.rs:2490`/`:2499` → `git.rs:1863-1906`, word-for-word
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
`VcsError` variants: `VcsError::NotARepo(PathBuf)` (`vcs.rs:298`) for "this
directory is not a repo", and `VcsError::CommandFailed { args, repo, stderr }`
(`vcs.rs:346-351`) for "the ref database is unreadable" `[V]`. So:

```rust
fn head_attachment(&self, repo: &Path) -> Result<HeadAttachment, VcsError>;
```

is total, and every caller's `match` is exhaustive. Four states become three
enum variants plus two typed errors, and `Ok(None)` — the value that meant all
four — does not exist.

**Shipped, and not quite where this paragraph originally said it would land.**
`head_revision` (`git.rs:784-816`) no longer inlines the "ambiguous argument"
catch and a `symbolic-ref --short HEAD` re-run; it delegates the unborn
classification to `self.head_attachment(repo)` and only *renders* the result
(`git.rs:794-797`) `[V]`. And the real detector, `observe_head`
(`git.rs:1863-1906`), deliberately does **not** use `--short`: its own doc
comment explains that `--short` answers `heads/main` instead of `main` when a
same-named tag exists, which does not round-trip through `refs/heads/<name>`
— a correctness fix beyond what this paragraph asked for, not just a move
`[V]`. Unborn detection: `symbolic-ref HEAD` succeeds, then `rev-parse
--verify HEAD^{commit}` fails (`git.rs:1878-1889`) `[V]`.

**Direct consequences of Q9's answer — all four are now shipped.** Three were
listed as future work when this section was written; the fourth was already
done:

- `rwv push` against a non-repo reports `NotARepo`, because the non-repo case
  never reaches the detached branch of the match. `observe_head` checks
  `is_repo` first and returns `NotARepo` before anything else
  (`git.rs:1868-1870`) `[V]`.
- `rwv doctor`'s canonical scan gained its missing arm mechanically: the
  `Ok(None)` that matched nothing is a `match` the compiler forces to cover
  `Detached`. `check.rs` now imports `HeadAttachment` in three places
  (`:3075`, `:3368`, `:4176`) and reads it at four call sites (`:3112`,
  `:3384`, `:4216`, `:4499`); the `Detached` arm is `check.rs:3411-3436` `[V]`.
  Note which arm is now the silent one: `Unborn` (`:3410`), deliberately, and
  reported separately as `UnbornCheckout` by the workweave pass. See §6 item 2.
- `rwv lock`'s detached-HEAD warning says which of unborn / detached it saw
  instead of inferring from `.ok().flatten()`: it matches all three arms
  (`lock.rs:119-125`, warning at `:145-153`), takes the SHA from the
  `DetachedHead` witness rather than from `version`, deliberately says nothing
  on `Unborn` (deferring to `head_revision`'s named refusal), and turns the
  read error into a hard refusal naming the repo rather than silence `[V]`.
- Doctor's remediation string stopped being wrong: with a registry lookup it
  knows whether the recorded ref exists, so `reattach_advice`
  (`check.rs:4759-4772`) emits `git switch <branch>` when it does and reserves
  `git switch -c <branch>` for when it does not `[V]`. The receipt reaches it
  as a `recorded_ref: Option<String>` on the finding (`check.rs:734`, `:753`,
  `:770`).

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
cwd_tip)` — which takes no path at all (`sync.rs:5420-5449`, MOVE at
`:5493-5495`) `[V]`. The function's own doc comment states the rule:
`target_repo` is where the witness is *obtained*; it is never handed to the
MOVE.

So the dodge this paragraph named — obtain a witness from the *cwd* repo,
always attached inside a workweave, and use it while operating on the target
— is a compile error, not a review finding:
`tests/branch_model_compile_fail_test.rs:532
a_witness_cannot_point_a_move_at_a_different_repo` asserts `E0061` on
`advance_attached_ref(cwd_witness, target_repo, to)` `[V]`. The runtime
refusals are pinned by `sync.rs:5599
ff_advance_repo_refuses_to_land_onto_a_detached_target` and `:5575
ff_advance_repo_lands_on_the_branch_the_target_is_attached_to`, and the
end-to-end behaviour by `tests/sync_to_test.rs:775
sync_to_advances_the_target_branch_not_just_head`.

This is Q4's answer, obtained for free: **sync-to advances the ref the target
is attached to, and refuses when the target is detached.**

One honest residual: `ff_advance_repo` itself still takes
`(target_repo: &Path, cwd_repo: &Path, cwd_tip: &ResolvedRevisionId)`
(`sync.rs:5401-5405`) `[V]`. A caller can hand it two arbitrary paths; what
cannot be routed around is the *obtaining* of the witness inside. The
compiler enforces the MOVE's target, not the function's argument order.

**(2) Comparing a declared branch with an observed one.** The member gate
(now `push.rs:361-421`) used to read:

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
(`push.rs:408`) and `!project_attached.is_named(&project_canonical)`
(`push.rs:204`) `[V]`. The policy itself is unchanged and deliberately so —
the member gate still only warns and pushes anyway (`push.rs:408-414`), which
is Q6, still open. What changed is that the assumption is now written down at
the line that depends on it. `PublishRef::from_attached` (`push.rs:215`,
`:420`) is the single decision site the split created; the alternative
answer, `PublishRef::from_local`, exists unused (`vcs.rs:1429`) so that
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
successor `materialize_worktree_on_ref` (`git.rs:1931-1968`) classifies first
and **adopts** the colliding ref (`:1950-1958`), returning `None` so the caller
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

**Shipped as `retire_recorded_refs`** (`workweave.rs:2418-2500`): it ranges
over `RefRegistry::lookup` hits (`:2429`), requires a `DeletionWarrant::merged`
or `operator_discarded` for each (`:2439-2441`), refuses by name when neither
holds (`:2464-2468`), retracts the receipt only after the ref is gone
(`:2447`), and **reports** everything else in the namespace via
`list_branch_names_with_prefix` (`:2482-2497`) `[V]`. `RefRegistry::lookup`
returns `None` for a hand-made branch, so `my--feature/wip` and
`dependabot--npm/lodash` stopped being rwv's property; the compile-fail probes
at `tests/branch_model_compile_fail_test.rs:330`, `:346`, and `:360` hold the
three routes (a listed name, a requested name, a receipt without a warrant)
shut. `parse_ephemeral_branch_name` — the function that made name shape into
ownership — was deleted outright, exactly as this paragraph proposed; its
successor `looks_like_a_pre_flat_ref` (`check.rs:2761-2771`) returns a `bool`
and feeds one report-only finding, never a delete `[V]`. The same argument
applied to `doctor --fix`'s safe-class deletions, which now re-resolve through
the registry (`fix_stale_ephemeral_branches`, `check.rs:4026-4121`) `[V]`.

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

**Shipped.** `fetch`'s `realign_present_clone` (`fetch.rs:718-810`) and
`update`'s `advance_checkout` (`update.rs:606-714`) both match
`head_attachment` and take one of four exits: no-op when already at the pin,
`advance_detached_head` for an already-detached HEAD (a MOVE, §3.6),
`advance_attached_ref` for a fast-forward of the tracking counterpart, or a
refusal naming `--detach-checkouts` — which routes to `detach_head` with a
real `DetachConsent`. `Unborn` is its own refusal `[V]`. The compile-fail
probe `tests/branch_model_compile_fail_test.rs:415` holds the
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
one call to `mint` (`workweave.rs:1454`, `add_remove.rs:91`, `sync.rs:1275`)
`[V]`. Two compile-fail probes hold it: `:378` (arity) and `:396` (no observed
input can be an argument).

### 4.7 Enforcing the invariant the way the precedent does

Ship the split with `compile_fail` enforcement on the type-level docs,
mirroring `vcs.rs:156-164` `[V]` — one per illegal comparison, one per illegal
construction, and one for the `as_str()`-laundered comparison
(`branch.as_str() != entry.version.as_str()`), which must fail with `E0599`
once those types are `Display`-only. `compile_fail` is the only form of this
enforcement that survives a refactor, because it fails in CI when someone
re-adds the `PartialEq` or the `From` impl to "make it easier".

**Shipped, in two layers.** Seven `compile_fail` doctests live on the types
themselves — `vcs.rs:156` (`RawRevisionId`, the §4.1 precedent), `:818` and
`:831` (`TrackingRef`: cross-type `==`, and `as_str` laundering), `:1165` and
`:1180` (`AttachedRef`: field forgery, and `as_str`), plus
`workweave_index.rs:567` (a `RawRefName` cannot be `record_created`) and
`:586` (a `WorkweaveIndex` cannot be struct-literal-forged around
`RefRegistry`) `[V]`.

Above them sits a dedicated harness, `tests/branch_model_compile_fail_test.rs`
(561 lines), which shells out to `rustc` against the built rlib and asserts
the *specific* error code: **22 probes plus one sanity check** that a legal
snippet still compiles — the check that keeps the other 22 from passing
vacuously `[V]`. The probes cover cross-type comparison (`:140`), the four
`as_str()` removals (`:160`, `:178`, `:217`, `:231`), laundering (`:196`),
witness/receipt/warrant forgery (`:251`, `:266`, `:285`), a MOVE on an
`UnbornRef` (`:309`), the three deletion routes (`:330`, `:346`, `:360`),
`mint`'s arity and its refusal of observed input (`:378`, `:396`),
consent-required detach (`:415`), the unreachability of the consent *mint*
from outside `crate::cli` (`:435` — the twenty-second, added when §4.4's
second construction route was sealed), three consent tuple-literal forgeries
(`:472`, `:486`, `:500`), the warrant argument on a rewind (`:514`), and the
cross-repo witness (`:532`).

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
| `workweave create` | mints `{project}--{workweave}`, attaches worktree | birth | Legal, unchanged in spirit. Name lost `<segment>`. Name uniqueness is checked against the workweave index before the directory check (`workweave.rs:1234-1257`) `[V]`; the receipt is written before the ref, and `birth_ephemeral_worktree` (`:1047-1150`) classifies the four `(receipt, ref)` states rather than force-deleting on collision (`:1073-1122`) `[V]`. Test: `tests/branch_model_lifecycle_test.rs:369`. |
| `sync` (ff / rebase) | advances the ref the CWD checkout is on (`apply_strategy`, `sync.rs:720-765`) `[V]` | MOVE | Legal, unchanged. |
| `sync --discard-local-commits` | rewinds the current ref | MOVE | **Now typed.** `hard_reset` has zero call sites in `sync.rs`; `rewind_project_repo` (`sync.rs:4433-4470`) mints a `DiscardWarrant` from the savepoint (`:4434`, `:4444`) and calls `reset_attached_ref` (`:4451`) or `advance_detached_head` (`:4461`), bailing on `Unborn` `[V]`. The savepoint-plus-named-loss shape §3.2 requires of every rewinding MOVE is now a property of the kind, not of this verb. |
| `sync-to` (landing) | advances the *target's* ref (`ff_advance_repo`, `sync.rs:5401-5497`) `[V]` | MOVE | **Shipped.** `62af89f` added the runtime refusal; this model replaced it with the witness (`:5420-5449`), closing the "landed onto nothing, then deleted the only ref" chain by construction — see §4.6(1) for the one residual (the function's own argument list). |
| `abort` | resets the current ref to the savepoint (`sync.rs:4918-4946` → `git.rs:2224-2234`) `[V]` | MOVE | Legal, unchanged. Already verified attributability (`git.rs:1348-1428`) `[V]`. |
| `fetch` (present clone) | `realign_present_clone` (`fetch.rs:718-810`) `[V]` | ATTACH | **Shipped as specified.** On the tracking counterpart: no-op when equal (`:783-785`), `advance_attached_ref` when an advance (`:790-793`), refuse-or-`detach_head` on a non-fast-forward (`:795-809`). Attached to any *other* ref: refuses, naming `--detach-checkouts` (`:763-777`, §5.3). Already-detached repos stay detached via `advance_detached_head` (`:736`), subject to §3.6. `Unborn` is its own refusal (`:738-746`). |
| `fetch` (absent clone) | `clone_attached_at` (`fetch.rs:948-954`) `[V]` | birth | **Shipped as specified**, and by a better route than the row predicted: rather than clone-then-align, the birth is a single call that attaches at the lock revision (R1's birth-target rule), so bootstrapping a weave from a lock behind origin performs no MOVE and needs no consent. `clone_with_role` survives only for the additive path where the lock has no entry (`fetch.rs:964`) `[V]`. |
| `update` (canonical, on a branch) | `advance_checkout` (`update.rs:606-714`) `[V]` | ATTACH | **Shipped.** Fast-forwards the attached ref when it is the tracking counterpart (`:673`, `:695-697`, §5.3); refuses a non-fast-forward naming the two exits — reconcile yourself per §8.7, or `--detach-checkouts` (`:700-714`). |
| `update` (inside a workweave) | the workweave arm of `advance_checkout` (`update.rs:641-657`) `[V]` | ATTACH | **Q8 answered and shipped:** advances the ephemeral ref when that is a fast-forward; refuses otherwise and points at `rwv sync` — deliberately *without* offering `--detach-checkouts`, since detaching a workweave checkout has no meaning R1 would sanction. No longer a detach. |
| `lock` | none (reads HEAD) | — | **Shipped.** Matches all three `HeadAttachment` arms (`lock.rs:119-125`), warns from the `DetachedHead` witness (`:145-153`), and refuses by name on an unreadable ref database instead of falling silent `[V]`. |
| `push` (project repo) | none (reads) | — | **Shipped.** The gate survives; the non-repo case reports `NotARepo` (§4.5), the canonical branch comes from `remote_default_branch` rather than a fabricated `"main"` (`push.rs:179`), and the mismatch test is `AttachedRef::is_named` (`:204`) `[V]`. |
| `push` (member repo) | none (reads) | — | **Shipped.** The publish ref is an explicit `&PublishRef` argument to `Vcs::push_ref` (`vcs.rs:2911`, impl `git.rs:2128-2143`) instead of an implicit `current_ref` read inside the impl `[V]`. Test: `git.rs:3053 push_ref_publishes_the_ref_it_was_given_not_the_one_head_is_on`. **Q6 stays open** — the split relocated the decision, it did not make it, and `PublishRef::from_attached` is the shipped choice. |
| `workweave delete` / `sync-to --retire` | `retire_recorded_refs` (`workweave.rs:2418-2500`) `[V]` | DESTROY | **Shipped.** Deletes recorded refs with a `Merged` (or `OperatorDiscarded`) warrant (`:2439-2441`); **reports** everything else in the namespace (`:2482-2497`). The set/singleton mismatch is gone because both the check and the deletion range over the recorded set. Tests: `tests/branch_model_lifecycle_test.rs:161`, `:237`, `:313`. |
| `doctor --fix` (stale ephemerals) | `fix_stale_ephemeral_branches` (`check.rs:4026-4121`) `[V]` | DESTROY | **Shipped.** Recorded refs only, re-resolved through the registry and re-warranted at fix time (`:4064-4098`); hand-made look-alikes survive. Tests: `tests/branch_discipline_test.rs:1005`, `:1057`. |
| `prune_dropped_repo` | removes the worktree; on `Checkout::Primary`, removes the entire store (`sync.rs:1386-1483`) `[V]` | DESTROY-STORE | **Unchanged at the ref level, gated at the store level.** The local-only refusal (`sync.rs:1440-1468`) is unchanged, and recorded rwv refs were deliberately *not* excluded from it: **unblocking prune is not a payoff of R2.** What changed is that the refusal is no longer the only thing standing between a live workweave's backing and `remove_dir_all` — `check_store_unclaimed` (`sync.rs:1313`, called at `:1479`) implements R4 directly. Tests: `sync.rs:5677`, `:5688`, `:5716`, `:5751`, `:5803`, `:5817`, `:5847`, `:5888`. |
| `remove --delete` | `remove_dir_all` on the whole store (`add_remove.rs:477`) `[V]` | DESTROY-STORE | **Shipped.** `refuse_claimed_store` (`add_remove.rs:452`, fn at `:486`) refuses while any live worktree is registered against the store or any receipt for it stands — across all projects on disk — and refuses rather than guesses when a registration is unreadable. It runs *before* the manifest write, so a refused destroy leaves the manifest as it found it. The verb-level named-precondition set (dirty state, unpushed work) is still separate work — **Q11, narrowed** — see §9. |
| `add` (inside a workweave) | mints an ephemeral name (`add_remove.rs:91`) `[V]` | birth | **Shipped.** Uses `EphemeralRefName::mint`; the inlined derivation and its private truncation are deleted (`add_remove.rs:44-48` records the removal). Emits a receipt first (`:90`), so `workweave delete` visits it. |
| `doctor` I3 scan | none | — | **Shipped.** Attachment is checked against the **receipt** (`OwnedRef::is_attached_by`, used at `check.rs:3130` and `:3390`), never against a name shape; detached is a finding at the canonical too (§4.5); scope extends to `projects/<project>/` (§5.1). |

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
  directory, so `scan_repos_on_disk` (`workspace.rs:261-309`) was not the
  right walker — `workweave_checkouts` enumerates the project directory
  separately (`check.rs:2796`), the canonical pass iterates
  `workweave_index::projects_on_disk`, and a dedicated arm in
  `branch_discipline_in_scope` (`check.rs:3980-3982`) keeps project-repo
  findings inside a project-scoped run `[V]`. Tests:
  `tests/branch_discipline_test.rs:706` and `:738`.
- **Delete's project-repo arm stopped being conditional.** It used to be
  nested inside *both* `dot_git.is_file()` and the `Ok`/`else` of
  `remove_worktree`, while the member-repo prefix-delete loop ran
  unconditionally and `remove_dir_all` ran regardless. Now the
  `dot_git.is_file()` block contains only `remove_worktree` and
  `worktree_prune` (`workweave.rs:2807-2831`), and `retire_recorded_refs`
  runs for the project repo unconditionally outside both (`:2837-2845`); the
  member arm also falls through to the ref pass when `remove_worktree` fails
  (`:2782-2790`) `[V]`. Under R2 both arms are the same operation over the
  same receipt set, and the asymmetry has nowhere left to live.

### 5.2 The reference-repo carve-out survives unchanged

A `role: reference` repo is materialized as a **symlink** to the canonical
store, has no per-workweave checkout, and therefore has no ephemeral branch
(`clone-topology.md:104-114`) `[V]`. `rwv doctor`'s I3 scan still skips it by
`classify_checkout(&abs) == CheckoutKind::ReferenceAlias`, and the skip is now
structurally ahead of every branch read rather than one line ahead of one: the
checkout enumerator itself retains only non-alias directories
(`workweave_checkouts`, `check.rs:2797`), with a second independent skip in
`scan_clone_topology` (`:2616`) `[V]`. The carve-out is tested at
`tests/branch_discipline_test.rs:1419
symlinked_reference_does_not_fire_shared_branch`, `:1458
worktree_reference_on_ephemeral_branch_flows_through_normally`, and `:1491
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
  `main`" (`sync-semantics.md:63-70`) `[V]`. Under this model it gets a
  receipt like any other worktree repo, and nothing keys on `role`.

### 5.3 The version-relatedness guard

**Decided: `fetch` and `update` MOVE only the tracking declaration's local
counterpart. Shipped** — `fetch.rs:763-777` and `update.rs:673` are the two
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
   `EphemeralRefName::mint`'s output as clean (`check.rs:3112-3115`) `[V]`,
   pinned by `tests/branch_discipline_test.rs:272
   healthy_workweave_ephemeral_branch_is_clean`.

2. **Weaves whose members were detached needed a decision.** Detached was the
   *normal* state after any `rwv fetch` or `rwv update` `[S]`, so this was
   not a rare case — it was most weaves. `rwv doctor` gained its missing
   `Detached` arm at the canonical (`check.rs:3411-3436`) `[V]` and a `--fix`
   for it (`fix_detached_canonicals`, `:4466-4531`, gated by
   `--reattach-checkouts`). **The forecast that the `--fix` would be
   honest-but-partial held**: §7.2's reattach condition (the local counterpart
   exists *and* its tip equals HEAD) is checked in exactly those two halves at
   `check.rs:4507-4520`, and it is **false** for the ordinary post-fetch state
   — stale local counterpart, HEAD at the lock SHA. The finding carries a
   `reattachable` flag so the operator can see which population they are in
   (`:3411-3436`) `[V]`. What the forecast could not know: the population
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
   report (`retire_recorded_refs`, `workweave.rs:2482-2497`) `[V]`. Correct,
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
   file sits *inside* the workweave (`workspace.rs:1001`) `[V]`, and
   `workweave delete` still ends in `remove_dir_all` on the directory
   (`workweave.rs:2848-2851`) `[V]` — a marker-homed receipt would die with
   the directory, and §7.2's "recorded as belonging to a deleted workweave"
   arm could never fire. The marker is accordingly still
   `{primary, project, parent}` and nothing else (`workspace.rs:983-987`)
   `[V]`; no receipt field was added to it.
   The shipped shape: `RefReceipt { store, name, created_at }`
   (`workweave_index.rs:179-192`) in a private `receipts` field on
   `WorkweaveIndex` (`:130`), reached through `RefRegistry` (`:595`) —
   `record_created` `:649`, `lookup` `:733`, `retract` `:776`,
   `adopt_legacy` `:678`, `migrate_legacy_index` `:807`. The store key is
   `std::fs::canonicalize` (`:905-907`), so `record_created` fails rather than
   records an unresolvable key. Writes go through one durable path
   (`write_durably` `:327`: fsync file, rename, fsync dir) under an in-process
   RMW guard (`:308-325`) `[V]`.
   Legacy markers and indexes migrate along the path that already existed:
   "Markers written before `parent` was introduced (legacy markers) must be
   migrated with `rwv doctor --fix` before the workweave can be used" is the
   rationale stated on `WorkweaveMarker` itself (`workspace.rs:979-981`)
   `[V]` — the migration logic it names lives in `check.rs`, not
   `workspace.rs`: detection in `scan_for_legacy_workweave_markers`
   (`check.rs:1625-1674`) and the fix in `fix_legacy_workweave_marker`
   (`:1708-1756`) `[V]`. The index half is separate and runs first
   (`registry.migrate_legacy_index()` at `check.rs:6533`; the report-only
   variant is `LegacyWorkweaveIndex` at `:6553`) `[V]`. (Receipt lifecycle
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
   rwv never passes `--force` (`git.rs:1957`, `:1966` are now the only two
   `worktree add` sites, §1.4) `[V]`.
   The drift *class* is real — both citations of "shared-ref advance in a
   sibling worktree" are in `check.rs` (`:65`, `:73-74`), not one in
   `check.rs` and one in `git.rs` as an earlier version of this note had it
   `[V]`, and `sync.rs` repairs it every sync at its actual per-repo call
   sites (`refresh_index_to_head_if_safe` / `refresh_working_tree_to_head_if_safe`,
   `sync.rs:3751-3752`) `[V]` — but its *stated mechanism* cannot occur
   **through default porcelain or through any path rwv takes**. Operator
   `--force` / `symbolic-ref` is therefore the first concrete candidate for
   the untraced path.
   `[?]` **Nobody has traced the actual drift-producing path.** That tracing is
   a prerequisite to rewriting the joint, and it is the one item on this list
   that is not just work but unknown work.

7. **The joints still describe the pre-flat naming scheme — new, found by
   the post-implementation citation sweep.** §3.5's flat name shipped in code and in
   `docs/reference/`, but **five normative or explanatory doc locations still
   present `<project>--<workweave>/<segment>` as what rwv mints**:
   `clone-topology.md:88` (I3's own normative sentence), `:180`, `:212`, and
   `workweave-hierarchy.md:192`, `:204` `[V]`. Four generated reference
   locations still describe the *stacked* form that flattening made
   impossible: `explain/workweave.md:158` and its template, `explain/status.md:109`,
   `schemas/status.json:64` `[V]`. Meanwhile `explain/doctor.md` and
   `schemas/doctor.json` were rewritten for the flat scheme and cite this
   document by section (`doctor.md:65`, `:78`, `:281`, `:308`, `:330`, `:499`,
   `:715`) `[V]`. So the repo's own docs now disagree with each other about
   what an ephemeral branch is called, with I3 — a tier-0 spec — on the wrong
   side. §7.1's "must land in the same release" rule was written about the
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
  (`git.rs:2163-2178`) delegates to `list_local_branch_names` (`:2180-2194`),
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
  (`workweave.rs:562`) records the outcome `birth_ephemeral_worktree` returned
  (`:1130-1145`), and `undo_ref_births` (`:691-770`) skips
  `RefBirth::Adopted` (`:700-710`), requires `DeletionWarrant::unmoved`
  (`:714`), and reads the tip from the birth's own record (`:753-756`) — so
  there is no second HEAD read left to race `[V]`. Tests at
  `workweave.rs:3555-3660`.
- The `WorktreeRemove` hook passed `force: true` unconditionally and
  `.unwrap_or(dir_name)` fabricated a workweave name from a basename with no
  `--` — the one path where dirty *and* diverged workweaves were destroyed
  with no operator confirmation. **The destructive half is fixed**: the call
  now passes `false, None` (`workweave.rs:3485-3491`), and the unmerged case
  is unconstructible because `DiscardUnmergedConsent` only mints at CLI
  dispatch `[V]`. Test: `tests/branch_model_lifecycle_test.rs:509
  claude_worktree_remove_hook_does_not_destroy_uncommitted_work`. **The
  basename fabrication is still live** (`.unwrap_or(dir_name)` at
  `workweave.rs:3466`) `[V]` — it is now a mis-naming rather than a
  mis-destruction, which is why R3 retired the dangerous half and left this
  one standing.

### 6.2 Bugs this model does not touch

These are plain defects with no model dependency. Fix them independently; do
not wait for this design:

- **Still live.** `default_branch` fabricates `"main"` on any failure *and* on
  a malformed symref via `.strip_prefix(...).unwrap_or(FALLBACK)`
  (`git.rs:908-922`, the fallback at `:913-918`) `[V]`, and its return value
  is still written into `rwv.yaml` verbatim as `version:` by three `rwv add`
  call sites (`add_remove.rs:283`, `:369`, `:652`, each reading a
  `default_branch` bound one line above) `[V]`, where `update` then resolves
  `origin/main` and fails forever. §4.2's `RemoteDefaultBranch` makes the
  fabrication unrepresentable *where it is used* — the producer returns `None`
  and the consumer must state a refusal policy — and the publish gate has been
  moved onto it (`push.rs:179`), as has doctor's canonical pass
  (`check.rs:3237`) `[V]`. **These three `add` sites are what is left**, and
  they are the ones that write the bad value into a file rather than merely
  reading it.
- **Half fixed.** `rwv update`'s "advanced N repo(s)" now counts SHA deltas
  rather than non-`Err` outcomes (`update.rs:271-310`, `UpToDate` vs
  `Updated` at `:275-285`, with an in-code comment citing this section) `[V]`.
  Its `branch` JSON field still echoes `entry.version` verbatim
  (`update.rs:272`) `[V]` — harmless now that `update` no longer detaches, but
  still a field that reports the declaration rather than the observation.
- Comment and doc falsehoods on branch-touching paths — **all now resolved**:
  - `sync.rs`'s "mirror create_workweave's naming": **fixed**, and explicitly
    retracted in place. `sync.rs:1245-1260` now records that the comment "used
    to claim it 'mirrors create_workweave's naming' while doing something else
    entirely" and states that one derivation is left `[V]`.
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
    `update.rs:399` calls `activate_intent`, but `lock.rs` still never does —
    the three call sites are `add_remove.rs:159`, `update.rs:399`,
    `check.rs:6844` `[V]`.

---

## 7. Migration — **shipped**

rwv is alpha. **No back-compat shims.** No dual-read of old and new names, no
"accept either shape" fallback, no legacy-tolerant doctor arm that survives
past the cutover. Migration is operator-handled and one-off, and unmigrated
state produces a **migration error** that names the command to run.

The whole of §7.1 landed: `fix_branch_model_migration` (`check.rs:4170-4259`,
with a doc comment at `:4123-4169` that enumerates the arms in this
document's own order), plus `migrate_legacy_ref` (`:4289-4361`),
`adopt_flat_ref` (`:4362-4393`), and `adopt_detached_workweave_checkout`
(`:4394-4443`) `[V]`. Detection is `scan_workweave_repo_branches`
(`:3066-3179`) over `refs_in_workweave_namespace` (`:2812-2842`) and
`legacy_ref_at_tip` (`:3180-3232`). Each arm below names where it lives.

The precedent for the error shape is `cli/dispatch.rs:336-350` `[V]` — it
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
  told to migrate. (The op-state skip is `check.rs:4194-4202`) `[V]`.
- **Write ordering, binding on every arm: the receipt is persisted per
  repo, durably, *before* the ref write it describes.** A crash then
  leaves a dangling receipt (benign — retractable by a later pass), never
  an unreceipted ref (permanently disowned under R2). The migration is
  idempotent: re-running it over its own partial output reaches the same
  end state (arm 2 is what makes this true). Receipt-first is visible at
  every write: `record_created` at `check.rs:4330`, `:4371`, `:4436`, each
  ahead of its ref operation `[V]`. The dangling-receipt half has its own
  scanner, `scan_dangling_receipts` (`check.rs:3587-3637`), pinned by
  `tests/branch_discipline_test.rs:768 dangling_receipt_is_reported_and_retracted`.
- **The pass enumerates refs per store — attached and unattached — not
  attachment states.** The objects R2/R3 govern are branches; a pass keyed
  on `head_attachment` alone silently disowns a commit-bearing legacy
  branch that a fetch left behind. Enumeration covers every
  worktree-materialized repo (skipping `ReferenceAlias` checkouts, §5.2)
  **and the project-repo checkout**, which the member walker does not
  reach (`scan_repos_on_disk`, `workspace.rs:261-309`). The migration's
  enumerator is `workweave_checkouts` (`check.rs:2783-2799`), which appends
  the project-repo path explicitly at `:2796` `[V]`; pinned by
  `tests/branch_discipline_test.rs:2250
  migration_reaches_the_project_repo_checkout`. §5.1 states this for the
  scan; it holds for the migration for the same reason — an implementer
  reusing the member walker leaks one project-repo branch per workweave.

The arms, in order. All seven shipped:

1. **A legacy-shape branch `{project}--{workweave}/*` for *this* workweave
   exists, and HEAD is attached to it** — write the receipt (current tip
   as `created_at`), then rename the ref to `{project}--{workweave}`. The
   common case; fully automatic. The rename is a birth plus a DESTROY of
   the old name; the DESTROY's warrant is `Unmoved` against the tip
   observed one line earlier. Detection `check.rs:3115-3121`
   (`UnmigratedEphemeralBranch`, keyed on `AttachedRef::legacy_name_under`);
   fix `:4217-4225` → `migrate_legacy_ref` `:4289-4361` `[V]`.
   The rename and the scanner/glob/parser cutover **had to land in the same
   release**, because a flat name was previously classified as `SharedBranch`,
   `git.rs`'s `"{prefix}/*"` glob never matched one, and
   `parse_ephemeral_branch_name` required the slash — a half-landed cutover
   would have mis-flagged every healthy repo and orphaned every flat branch at
   delete. **It landed complete, and the polarity is now inverted**: flat is
   the healthy shape (`check.rs:3112-3115`), the glob is gone (`git.rs`
   filters with `starts_with`), and the successor predicate
   `looks_like_a_pre_flat_ref` (`check.rs:2761-2771`) *deliberately refuses to
   match* the flat shape (`:2769`) so it can never claim a healthy ref `[V]`.

2. **A flat new-shape name exists with no receipt** — adopt it: write a
   receipt at the observed tip. Without this arm, a repo the migration
   half-processed (or a crash between receipt and rename) falls into arm 4
   on re-run and is disowned forever. Detection `check.rs:3098-3107`
   (`UnrecordedEphemeralBranch`); fix `:4229-4234` → `adopt_flat_ref`
   `:4362-4393` `[V]`.

3. **HEAD is `Detached(_)` and a legacy-shape branch for *this* workweave
   exists at a different tip** — the post-fetch state §6 item 2 calls
   normal, possibly with operator commits on the branch. Report **both**
   tips. First remediation: reattach to the existing branch (arm 1 then
   applies on re-run). Second: `--adopt-detached-checkouts`, which mints
   flat `{project}--{workweave}` **at HEAD** — i.e. at the lock SHA — and
   **must warn that it strands the legacy branch's tip** whenever that
   branch carries commits HEAD does not. Detection `check.rs:3146-3154`,
   carrying a `LegacyRefAtTip { branch, tip_sha, strands_commits }`
   (`:945-960`) — the stranding warning is a *field*, computed at scan time,
   not a message the fix path has to remember to print; fix `:4237-4251`,
   gated on `AdoptDetachedConsent` (`:4238`) `[V]`.

4. **`Attached(a)`, `a` is anything else** — an operator branch, a shared
   `main`, or a foreign workweave's ephemeral. **Report, do not touch.**
   Under R2 these are not rwv's refs. They become the Q7 population (§9).
   Fix path `check.rs:4254-4255` (`Ok(_) => {}`); detection splits them into
   `ForeignEphemeral` (the registry says another workweave holds it) and
   `SharedBranch` (`:3122-3143`) `[V]`.

5. **`Detached(_)` with no legacy-shape branch for this workweave** —
   report, and offer `--adopt-detached-checkouts` as in arm 3; with no
   competing tip there is nothing to warn about. Same code path as arm 3
   with `legacy_branch: None` (`check.rs:3146`) `[V]`.

6. **`Unborn(_)`** — a repo with no commits. Report; there is nothing to
   attach a receipt to. Detection `check.rs:3155-3158` (`UnbornCheckout`);
   report-only at `:4254-4255` `[V]`. (This state also reaches `rwv lock` and
   produces the "unborn HEAD (no commits yet, on branch '<x>'): make an
   initial commit, then re-run rwv lock" error, rendered by `head_revision`
   (`git.rs:784-806`), which delegates the classification itself to
   `head_attachment` (§4.5) `[V]`.)

7. **Legacy markers and indexes** — the registry field migrates in the same
   pass, alongside the already-existing `parent`-field migration
   (detection `check.rs:1625-1674`, fix `:1708-1756`, called at `:6139`)
   `[V]`, receipt-first like everything else. The index half is a separate
   step that runs *first*, `registry.migrate_legacy_index()` at
   `check.rs:6533` (ordering rationale `:6517-6522`; report-only variant
   `LegacyWorkweaveIndex` at `:6553`) `[V]` — the index must have a receipt
   registry before any arm can write into it.

Without the flag named in arm 3/5, the workweave stays unmigrated and every
rwv verb on it errors with the flag named — except `abort` and `status`,
exempted above. The arms are tested at `tests/branch_discipline_test.rs:1597`,
`:1680`, `:1772`, `:1829`, `:1889`, `:1963`, `:2022`, `:2074`, `:2120`,
`:2187`, `:2250` `[V]`.

### 7.2 The canonical-store pass

Separately, for each canonical store at `<weave>/<repo_path>` — shipped as
`scan_canonical_stores` (`check.rs:3361-3524`), with the arms below in the
same order `[V]`:

- `Attached(a)` — leave it alone. The canonical's attachment is **operator
  state**; `clone-topology.md:226-228` says "rwv does not own the
  canonical store's branch state beyond I3. The canonical store can sit on any
  non-ephemeral branch the operator picked" `[V]`. Implemented as the
  no-receipt fall-through (`check.rs:3408`, "No receipt → arm 1: operator
  state, left alone").
- `Attached(a)` where `a` is a ref recorded as belonging to a **live**
  workweave — an I3 disjointness violation. git forbids the topology anyway,
  so this indicates a directory was moved or copied. Report; no automatic fix.
  Sub-kind `CanonicalHoldsLiveWorkweaveRef` (`check.rs:823`) `[V]`.
- `Attached(a)` where `a` is a ref recorded as belonging to a **deleted**
  workweave — a leak. Report; `--fix` deletes it with a `Merged` warrant if
  one can be established, and refuses otherwise. Sub-kind
  `CanonicalHoldsLeakedRef` (`check.rs:838`) `[V]`. (Reclamation of this class
  belongs to the `rwv gc` family when it lands; its triggers must stay
  **structural** — ancestry, named-ref reachability, counts — and never
  wall-clock.)
- `Detached(_)` — a finding, which is new. **Shipped**: the arm at
  `check.rs:3411-3436` always emits `CanonicalDetached { at_sha, counterpart,
  reattachable }` `[V]`, pinned by `tests/branch_discipline_test.rs:569
  detached_canonical_is_reported`. `--fix`, gated by `--reattach-checkouts`
  (§4.4, gate at `check.rs:6589-6613`), reattaches to the tracking
  declaration's local counterpart when that ref exists as a **local branch**
  and its tip equals HEAD (`fix_detached_canonicals`, `:4466-4531`, both
  halves at `:4507-4520`) — resolved through `resolve_local_branch_tip` so a
  same-named tag cannot answer instead `[V]`. Without the flag, or when the
  condition fails, `--fix` reports with the correct `git switch` spelling
  instead; the report path is unchanged. Stated plainly: that condition fails
  for the ordinary post-fetch state (stale local counterpart, HEAD at the lock
  SHA), so this `--fix` reattaches the minority — and the finding says so
  per-repo via its `reattachable` field rather than leaving the operator to
  discover it. It repairs what it can prove; it does not deliver the
  weave-wide reattachment §6 item 2 might suggest. Pinned by
  `tests/branch_discipline_test.rs:634
  detached_canonical_reattaches_only_with_consent`.

Two arms the implementation added: `Unborn(_)` is silent here (`:3410`) and
reported by the workweave pass instead, and a receipt whose workweave is still
live is skipped rather than reported stale (`:3453-3455`, with a comment
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
`refs/rwv/pre-abort/*` refs that already exist (`vcs.rs:2062`, `:2068`,
`:2073`, `:2095`, `:2105`) `[V]`. Two independent reviewers proposed it.

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
   (`push.rs:383-388`) `[V]`, and `rwv lock`'s detached warning
   (`lock.rs:145-153`) `[V]`. That is a larger change than the one being
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
(`OwnedRef::is_attached_by`, `check.rs:3130`, `:3390`), and the one surviving
name-shaped predicate, `looks_like_a_pre_flat_ref` (`check.rs:2761-2771`),
returns a `bool` that feeds a report and nothing else `[V]`. The
name-to-directory coupling also reversed: `branch_discipline_in_scope`
(`check.rs:3936-3990`) derives a *project* from a directory basename via
`parse_weave_dir_name` (`:3956`), and no directory name is derived from a
branch name anywhere `[V]`. Tests holding the rejection:
`tests/branch_discipline_test.rs:1005 handmade_lookalike_branch_survives_doctor_fix`
and `:1057 flat_lookalike_branch_survives_doctor_fix` — note the second, which
covers a hand-made branch that matches the *new* flat shape exactly, the case
name-shape ownership would get wrong most often now.

### 8.5 Making the branch name authoritative for lineage

**Rejected.** Under `<segment>`-derived-from-fork-source, a nested name
*looks* like a lineage record. Two places in the code state defensively that
it is not — "Branch names are creation-time namespaces, **NOT lineage
records**" (`workweave.rs:2296-2305`, echoed at `status.rs:87-98` and now also
at `check.rs:1756`) `[V]` — and direct consumers to read parentage from the
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

**Q6 — What is a member repo's publish ref? — STILL OPEN.**
`push_with_role` used to run `git push origin <current_ref>`, publishing
whatever branch the checkout happened to be on from inside the VCS impl. `rwv
push` warned when that differed from `entry.version` and then pushed anyway.
So: is the publish ref the attached ref, or the manifest's declared tracking
branch? Is `version:` a *constraint* on publishing or a *default*? And the
sub-question for the project repo: where is its canonical branch recorded?
It used to be invented from `origin/HEAD` with a hardcoded `"main"` fallback,
which is wrong for `rwv init`-created repos, for `master` / `trunk` defaults,
and for every pyramid channel other than the default.
**This is the one place a per-repo branch is genuinely semantic, and this model
does not decide it.** What the model contributed, and has now shipped:
`Vcs::push_ref` takes the ref as a parameter (`vcs.rs:2911`), so the decision
is made at one site in `push.rs` instead of being implicit inside the VCS impl
(§4.6(2)); `PublishRef::from_attached` (`push.rs:215`, `:420`) is the choice
currently made, matching the shipped behaviour exactly, and
`PublishRef::from_local` (`vcs.rs:1429`) sits unused as the other answer so
that changing it is a one-line change at one place `[V]`. The member gate
still warns and pushes anyway (`push.rs:408-414`) `[V]` — the split relocated
the decision, it did not make it. The project repo's half moved further: the
gate now reads `remote_default_branch` and **refuses** when `origin/HEAD` is
unset rather than inventing `"main"` (`push.rs:179`) `[V]`, so the *absence*
is explicit; where a non-default channel's identity should be *recorded* is
still undecided.

**Q7 — Is an operator-created branch inside a workweave legal? — STILL OPEN.**
R2 makes such a branch **safe** — rwv never destroys it — but leaves it
**unaccounted**: it pins objects, gates nothing, and disappears from view once
the workweave directory is gone. The doctor finding stays report-only, with no
`--fix` path — as built: §7.1 arm 4 is `Ok(_) => {}` (`check.rs:4254-4255`), and the
population lands in `SharedBranch` / `ForeignEphemeral` (`:3122-3143`) `[V]`.
The question has a sharp form: if operator branches are legal, who
merged-checks them at delete time and who cleans them up? If they are not
legal, the refusal belongs at checkout time, not at doctor time. (The how-to
that used to *tell* operators to create one is being removed independently;
the policy is untouched by that removal.)

**Q10 — Workweave existence: registry or directory scan? — STILL OPEN, harm
narrowed.** The container scan still exists, now split in two:
`list_workweave_dirs` (`workweave.rs:2942-2974`) assembles the containers and
`doctor_scan_container` (`:2985-3028`) walks them, documented there as "the
ONLY surviving on-disk scan" `[V]`. Workweaves placed with `--dir` (advertised
at `cli.rs:461-469`) are still not enumerated by a directory walk. R2 removed
the *deletion* consequence — an unrecorded ref is never destroyed, and a
recorded one is looked up by receipt rather than by container membership — and
the implementation removed the *misclassification* consequence too:
`live_workweave_names` (`check.rs:2987-3011`) consults the workweave index as
well as the markers, and the canonical pass skips a receipt whose workweave is
live (`:3453-3455`, with a comment naming `--dir` explicitly) `[V]`, so a
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
(`add_remove.rs:452`, fn at `:486`) refuses on any live worktree registration
or any standing receipt, across all projects on disk, and runs before the
manifest write so a refusal is retryable `[V]`. `prune_dropped_repo` gained
the same gate (`check_store_unclaimed`, `sync.rs:1313`, called at `:1479`).
What stays open is exactly what this entry said: the verb-level
named-precondition set (dirty state, unpushed work — the set `workweave
delete` has), which this model scopes but does not specify, and which
`refuse_claimed_store`'s own doc comment defers to this question.

**Q12 — What is the legal grammar for project and workweave names? — STILL
OPEN.** `WorkweaveName::new` (`manifest.rs:154-156`) and `RefName::new`
(`vcs.rs:264-266`) are still bare newtypes with no validation `[V]`. Project
`p` with workweave `x--y`, and project `p--x` with workweave `y`, produce the
same directory name *and* the same branch name. Flattening the segment (§3.5)
reduced the number of `split_once("--")` assumptions in the tree, and R2
downgraded the collision from a **correctness** problem to a **legibility**
one — nothing parses an ephemeral name any more, because ownership comes from
the receipt, and `parse_ephemeral_branch_name` was duly deleted `[V]`. But the
grammar is still unvalidated: `EphemeralRefName::mint` (`vcs.rs:969-971`) is a
bare `format!("{}--{}", …)` that neither rejects `--` in its components nor
uses a non-splittable encoding `[V]`. Note the asymmetry the implementation
introduced — `TrackingRef::parse` validates against git's `check-ref-format`
(`vcs.rs:745-790`) while `mint` validates nothing, so the *declared* name is
now better checked than the *minted* one. Parse-don't-validate on the two
newtypes is still needed.

**Q13 — Is a deliberate detached *position* protected operator state? — STILL
OPEN.** §8.3 decides that *attachment* is operator state rwv must not silently
change. A mid-bisect or mid-rebase-edit *position* is operator state by
the same argument, and it still gets strictly less protection than an
attached HEAD. §3.6 protects exactly the cases `mid_op_state` can detect, and
bisect detection has landed there (`git.rs:493-518`) — but only on the path
that goes through `mid_operation`. `sync.rs`'s own preflight still uses
`mid_op`, whose `match` folds bisect into `None` (`git.rs:1431-1438`) `[V]`,
so the protection is uneven *within* the tree, not just absent from the model.
The general principle — does the model protect positions, or only attachments?
— is undecided, and `refs/bisect/*` is not consulted by anything.

**Q14 — What is a receipt's lifecycle beyond its home? — STILL OPEN.**
§6 item 5 picks the home and the key, and both shipped. Still open: what
invalidates a receipt; whether R4's retraction step on a store destroy is a
per-ref DESTROY needing its own warrant each time or a bulk operation with its
own consent — the shipped `refuse_claimed_store` sidesteps this by *refusing*
while any receipt stands rather than retracting them (`add_remove.rs:486`)
`[V]`, which is the conservative reading and not a decision; what a receipt
pointing into a store that no longer exists means for doctor — partially
answered by `scan_dangling_receipts` (`check.rs:3587-3637`), which reports and
`--fix`-retracts a receipt whose *ref* never appeared, but not one whose
*store* is gone; and whether receipts are ever reclaimed (§7.2 gestures at the
`rwv gc` family — whose triggers must stay structural: ancestry, named-ref
reachability, counts, never wall-clock).

**Q15 — What is the validity window of a witness? — STILL OPEN.**
§4.2 binds `AttachedRef` to its repo, which closes the cross-repo pass, and
that shipped: `advance_attached_ref` re-observes via
`head_attachment(witness.repo())` (`vcs.rs:2524`) and errors on a stale
witness, pinned by `tests/branch_model_test.rs:310
advance_attached_ref_refuses_a_witness_for_a_repo_that_became_detached` `[V]`.
So a witness is re-verified at the moment of consumption. Not settled: whether
a witness is valid across phases within one verb (an earlier phase can detach
a repo whose witness a later phase still holds — the TOCTOU form of the same
defect; `ff_advance_repo` names this question in its own comment at
`sync.rs:5486-5490`), and whether `sync --continue` must verify that the
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
`warn_on_sibling_sync` (`sync.rs:2837-2870`) still gates its warning on
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

**Left open, with the question stated — all still open, none decided by the
implementation:** Q6 (publish ref), Q7 (operator branches in a workweave), Q10
(`--dir` workweave liveness), Q11 (verb-level preconditions for store
destroys — narrowed), Q12 (name grammar), Q13 (detached positions as operator
state), Q14 (receipt lifecycle), Q15 (witness validity window), the
merge-strategy rationale, and the `shared-refs-drift` mechanism. §9 records,
per question, what the implementation changed *around* it without answering it.

**Status of the implementation.** Everything under "Decided here" and "Decided
in revision" has shipped, at repoweave `37548fd`. The old surface —
`current_ref`, `checkout`, `delete_branch`, `restore_savepoint`,
`create_worktree`, `push_with_role`, `list_branches_with_prefix`,
`parse_ephemeral_branch_name` — is deleted rather than deprecated. Three
things this document specified are **not** done, and are stated where they
belong rather than only here: `manifest.rs`'s `version:` field is still
`RefName` (§4.2), `default_branch`'s `"main"` fabrication still reaches
`rwv.yaml` through `rwv add`'s three sites (§6.2), and five joint locations
plus four generated-reference locations still describe the pre-flat branch
name (§6 item 7).

**The mechanism:** one rule with four kinds (MOVE / ATTACH / DESTROY /
DESTROY-STORE), a decision procedure that answers for verbs not yet
written, and a type split of `RefName` (five core types, §4.2's honest
inventory) that turns each violation into a compile error — the same move that
made raw-versus-resolved revision confusion "a compile-time impossibility"
(`check.rs:4705-4710`) `[V]`. The enforcement is 7 `compile_fail` doctests
plus 22 probes in `tests/branch_model_compile_fail_test.rs` (§4.7).

---

## Anchoring

- `docs/explanation/joints/clone-topology.md` — I1/I2/I3; the reference-repo
  carve-out; the `git-common-dir` mapping. I3's ephemeral-branch clause is
  restated by this model as an ownership-by-receipt clause; its *purpose*
  (merged-check soundness via ref disjointness) is unchanged. **Now contradicts
  the shipped code** on the name shape (`:88`, `:180`, `:212`) — §6 item 7.
- `docs/explanation/joints/workweave-hierarchy.md` — where the ephemeral naming
  scheme lives operationally, and the sole recorded justification for it
  (git's one-worktree-per-branch constraint). **Also still pre-flat**
  (`:192`, `:204`) — §6 item 7.
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
  — the downstream consumers of §7. Both were rewritten for this model and now
  cite this document by section number (`doctor.md:65`, `:78`, `:281`, `:308`,
  `:330`, `:499`, `:715`) `[V]`, so §7's arm numbering is load-bearing outside
  this file and should not be renumbered casually.
Every claim that came from the earlier branch-behaviour investigation is
restated in this document; no external hop is required to derive any answer
here.
