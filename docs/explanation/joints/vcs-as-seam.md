# VCS as seam

repoweave operates on repositories. Today every repository it touches is
a git repository, but that is a current-state observation, not a
foundational assumption. The internal design treats the version control
system as a seam: a single layer where VCS-specific knowledge lives, and
everywhere else in rwv core uses the abstraction. This joint states the
principle and walks through four worked examples — each one a closed
refactor that pulled VCS-specific behavior across the seam, behind the
Vcs trait.

This joint is the inner-boundary counterpart to
[verb-vs-composition](./verb-vs-composition.md), which is the
outer-boundary principle. The two joints together form a symmetric
design contract: what belongs in rwv core (outer), what belongs in the
Vcs impl (inner).

## The principle

> Anywhere rwv core is about to use a VCS-specific name, text,
> mechanism, or file convention, the `Vcs` trait owns the abstraction;
> the VCS-specific impl handles the details.

"VCS-specific" includes:

- Command names (`git push`, `git rebase`, `hg pull`, `jj rebase`).
- Remote conventions (`origin`, `upstream`).
- File conventions (`.gitattributes`, `.gitignore`, `.hgrc`).
- Configuration mechanisms (`merge=rwv-ours`, `[core] sparseCheckout`).
- Error message text and recovery instructions ("git rebase --continue",
  "hg resolve --mark").
- In-flight state names ("mid-rebase", "mid-merge", "in-progress
  graft").

Each of these is a place where a single concept manifests differently
per VCS. Centralizing the concept-to-detail mapping in the Vcs impl
keeps rwv core readable: the higher layer says "push this branch on the
remote associated with this role" without needing to know which remote
name the VCS implementation uses for a given role.

Future implementations (jj, hg, sl) would each own their own module; rwv
core consumes the trait, not any one impl.

The backend type is **private to `src/git.rs`**. Outside that module the
git implementation is reachable only as a `Vcs` handle. That is what
makes the principle above a compile-time property rather than a
convention: rwv core cannot name a backend, so every frame that speaks
to a repository has to accept a handle it could be given a different one
of.

This joint states the contract, in the vocabulary a reader who never
opens `src/` can check it in. The module and ownership model underneath
— which type lives where, how wide the trait is, and the call sites that
knowingly bypass it — is `ARCHITECTURE.md` §6.1, at the repo root.

## Why this matters

Two failure modes the seam prevents:

- **Git-only tool by accident.** Without the seam, the easiest place
  for any new feature to put git-specific code is wherever the feature
  is being added. Over time, every module becomes git-aware. The
  switch cost (to support jj or hg) grows without bound, and the
  proposal "add a new VCS" hits a wall composed of dozens of
  unrelated patches.
- **Conceptually duplicated state.** Without the seam, the same
  concept ("the remote for a fork") gets coded in three places that
  drift apart. The push path computes one name; the fetch path
  computes a different one; the dry-run output disagrees with the
  actual run. Single-source-of-truth at the seam prevents this
  category of bug entirely.

The principle is also a code-review tool. If a PR adds a git command,
git-specific name, or git config flag *outside* `src/git.rs`, send it
back: the abstraction belongs in the Vcs trait.

## Worked examples

### (a) `Vcs::resolve_branch_on_remote` + role-accepting remote resolution

**Concept:** "look up branch X on whichever remote a repo of this role
uses." The trait accepts a `Role` so future VCS impls can route
differently per role. The git impl accepts and ignores the role: all
clones use `origin` regardless of role (the `role` parameter is kept
as a signal value — a future VCS impl could route differently).

**Anchor:** commit `1b76456`.

`Vcs::clone_with_role` and `Vcs::resolve_branch_on_remote` both take the
role; the git impl discards it, clones with the remote named `origin`,
and qualifies a branch lookup as `origin/<branch>`.

**Why this is the seam shape.** Before the refactor, the
`origin/<branch>` qualifier was spelled out at several call sites in rwv
core. Exposing role-aware resolution as a trait method means:

- rwv core never spells `origin` directly.
- The remote convention is decided once, in the VCS impl.
- A different VCS impl can choose a different convention (jj's `default`
  / hg's `default-push`) without rwv core caring.

There is no bare-branch fallback in the trait surface: when the role's
conventional remote doesn't have the branch, the trait returns
`VcsError::RevisionNotFound`, not the local branch tip. This prevents
the silent "we advanced to the local working state instead of the
remote target" failure mode.

### (b) `Vcs::conflict_resolution_hint`

**Concept:** the human-readable text we splice into sync's bail
messages explaining how to resume after the user resolves conflicts.
The VCS impl owns git-vocabulary steps (stage the resolution, run the
VCS-native continue for merge/cherry-pick ops). For rebase ops,
rwv has a native `rwv sync --continue` / `rwv sync-to --continue` that
drives the remaining picks; the VCS impl stops at staging and rwv core
appends the `rwv <verb> --continue` line.

**Anchor:** commit `26ba786`.

`Vcs::conflict_resolution_hint` takes a `ConflictOp` — the in-flight op
sync's project-repo path can leave behind, one of `Rebase`, `Merge` or
`CherryPick` — and returns the hint block for it. The git impl is
asymmetric by op: `Merge` and `CherryPick` end with the VCS-native
continue command, `Rebase` stops at staging the resolution.

Every conflict-bail site in sync composes its message from that trait
method, and the handle it calls is the one its frame was given — sync
resolves a backend per repo from the manifest entry's declared VCS and
passes it down, so the hint text comes from whichever backend that repo
declared.

**Why this is the seam shape.** Before the refactor, sync's
`bail!` macros embedded "git add" / "git rebase --continue" text
directly. The text was specific, helpful, and quietly git-only — and
the same phrasing appeared three times in slightly different forms
across the conflict-bail sites. Centralizing the per-op hint text in
the trait method means:

- rwv core never writes "git add" or "git rebase --continue".
- The hint is per-VCS *and* per-op.
- A hg impl returns `hg resolve --mark` text without changing any sync
  bail site.

The seam also enforces the `Rebase` asymmetry at a boundary: the VCS
impl owns git vocabulary, so it must not name rwv verbs (`rwv sync
--continue`). By stopping at staging for `Rebase` and leaving the
`rwv <verb> --continue` line to rwv core, each layer owns exactly the
vocabulary it should know about.

The trait method is small on purpose: it returns a short block, not a
full bail message. Surrounding context — which repo, how to re-run sync,
how to abort — is composed by the verb. The seam carries the
VCS-specific noun phrases, not the entire prose.

### (c) `Vcs::set_replay_exclusion`

**Concept:** configure a repo such that during replay (rebase) any
changes to a specified path are silently overridden by the replay
target's version. For git this is a three-layer mechanism, and the
layers do different jobs:

1. A namespaced `merge=rwv-ours` entry in committed `.gitattributes`
   **assigns** the driver to the path.
2. An inline `-c merge.rwv-ours.driver=true` flag, carried by whatever
   derived-content policy the replay states, **defines** the driver for
   that one git process.
3. A durable `merge.rwv-ours.*` repo-local config **defines** it for
   every later git process in that repo, including ones rwv did not
   start. rwv plants it wherever it establishes or repairs a project
   repo's replay-exclusion setup, so the definition is in place before
   any replay can stop.

Layer 1 is committed and travels with a clone; layers 2 and 3 do not
travel, and that asymmetry is the whole shape of the thing. A
`.gitattributes` line *names* a resolution, it does not carry one —
whoever runs the command has to supply the definition. The namespaced
name `rwv-ours` keeps that supply from colliding with an unrelated
global `merge.ours.driver` config or a third-party `merge=ours` line in
the same repo.

Rebase replays each commit as a 3-way merge, which is why the driver
is still needed even though the `merge` sync strategy was removed.

**Layer 3 is project-repo-only, which sharpens the caveat considerably
outside a project repo.** rwv plants the durable config in the repos it
manages the lifecycle of. Any other repo that adopts the same
declaration for its own derived content gets layers 1 and 2 and no layer
3 — repoweave's own tree is such a repo, declaring `merge=rwv-ours` over
its generated reference pages. In a project repo, "manual git has no
definition" is close to theoretical: a bare `git rebase --continue`
finds the planted config and is genuinely safe. Everywhere else it is
the normal case rather than an edge case, and every merge or rebase run
outside rwv resolves the declared paths as an ordinary textual conflict.
That is noisy, not dangerous, and which side you pick does not matter:
the content is derived, so regenerate afterwards and the repo's own
drift gate judges the result on either route.

**Anchor:** commit `d29bb2f` (initial refactor); the driver rename +
durable config plant landed later.

For git, `set_replay_exclusion` writes layer 1 and only layer 1: it
appends `<path> merge=rwv-ours` to the repo's `.gitattributes`,
idempotently, migrating a legacy `merge=ours` spelling in place when it
finds one. The assignment has to be committed before it takes effect,
which is why a sync will not quietly add it on your behalf — `rwv doctor
--fix` is the path that does.

Layers 2 and 3 do not cover each other, which is why both exist. The
inline flags are stated per call by the replay's derived-content policy,
and they are the only route in a repo that never gets a plant. The
durable plant is the only route for a git process rwv did not start —
bare `git rebase --continue`, the resume path git itself advertises in
conflict output, which inherits no inline flags. They overlap on exactly
one case, a project-repo replay driven by rwv; drop either and it
strands the case the other never covered.

**Used by** [sync-semantics](./sync-semantics.md)'s Phase 1' to keep
`rwv.lock` out of the merge inputs. The lock is regenerated from
manifest tips in Phase 3, so carrying user lock-edits through a rebase
would only produce noise. See
[lock-as-derived](./lock-as-derived.md) for why this is structurally
right.

**Why this is the seam shape.** Before the refactor, sync ran a custom
cherry-pick loop with per-commit lock-exclusion logic inline. Replacing
it with a one-shot `set_replay_exclusion` call — made wherever a project
repo is created, adopted or repaired — and a standard `rebase` call
collapsed dozens of lines of git-aware loop into one trait call plus one
rebase call.

The win is not just shorter code — it is that the exclusion mechanism
is now properly per-VCS. A hg impl can implement `set_replay_exclusion`
via `[merge-patterns]` in `.hgrc`; a jj impl might do something
entirely different. Sync neither knows nor cares.

The companion `replay_exclusion_state` query exists so `rwv doctor` can
detect projects initialised before this path landed and offer to add
the missing entry. The detection logic in core stays VCS-agnostic. It
answers with a state rather than a bool because doctor asks twice — once
to report, once to repair — and a pair of bools let those two answers be
derived independently, which is how a repair came to rewrite and commit
in a project the report had called clean.

### (d) `Vcs::push_ref`

**Concept:** push a named ref on the remote associated with the given
role. For git, this ignores the role (all roles push to `origin` — see
example (a)) and runs `git push origin <ref>`.

**Anchor:** commit `6066ce1`, for a predecessor that took the role but
not the ref. The ref became a parameter when the branch model landed.

The impl owns the mechanism:

- The remote name selection (all roles push to `origin`; the `role`
  parameter is accepted and ignored, kept for signal value and future
  VCS impl flexibility — see example (a)).
- The force-push flag spelling.
- The argument shape `git push <remote> <ref>`.

And what it explicitly does **not** own: the choice of which ref to
publish. The predecessor read the current branch inside the impl, which
put a policy decision — *what does publishing this repo mean?* — inside
the git wrapper, where no caller could see it and a detached HEAD
surfaced as a raw command failure from three layers down. `push_ref`
takes the ref as a `PublishRef`, a value only the push verb can
construct, so the decision is made once at the publish gate and the
refusal for a detached checkout is stated there, in the verb's own
voice. What that gate *should* choose for a detached checkout is still
open; the signature only makes the choice visible.

The push verb does the cross-repo work — walking the manifest, applying
selectors, ordering project-repo last, checking the lock-state
precondition — but never invokes git directly.

**Why this is the seam shape.** Before this method existed, the
push-loop draft constructed `git push` argument strings inline. Per the
verbs design discussion ("the trait captures the per-role push policy
— refs come from the manifest, not from `git push` argument
parsing"), the right shape was a trait method whose contract names the
*intent* ("publish this ref on the role-conventional remote") and
hides the *mechanism* (`git push origin <ref>` or some entirely
different command on another VCS).

Note the trait-level Fork policy is *neutral*: pushing a fork goes to
`origin` just like any other role. The plan-time default scope (owned +
fork; dependency and reference excluded before the loop) lives in the
push verb. The trait stays a thin shell over the VCS surface; the policy
of which roles to include in the push loop lives where the loop lives.
The asymmetry is deliberate: it keeps the trait composable (a future
verb that wants to push only owned repos calls `push_ref` after
filtering out forks) and keeps verb-level policy debuggable — the
default-scope choice is one visible location rather than a rule
distributed across the seam.

## What enforces this

The parts of the principle that can be mechanised are mechanised, and a
reviewer should read a green build as having already answered them.

1. **Naming the backend.** The type is private to `src/git.rs`, so any
   reference from rwv core is `error[E0603]`. There is no way to write a
   new hardcoded backend that compiles.
2. **Minting one.** The one constructor that hands out a git backend is
   `pub`, because the test suite needs a concrete one. That means a verb
   could call it instead of accepting a handle — dispatching correctly
   while remaining impossible to substitute in a test. The `vcs-seam`
   gate refuses it outside `src/vcs.rs`, where every resolver that mints
   a backend with no manifest entry to resolve from documents why git is
   the only answer it can give.
3. **Spawning git from scratch.** The same gate refuses
   `Command::new("git")` outside `src/git.rs`. Both rules stop at the
   first test module, under any name: a `#[cfg(test)]` module may build
   a concrete backend.

`scripts/ci-local.sh` runs the gate. It fails naming the file, the line
and which of the two bypasses it is.

What is left for a human is the part no gate can see — whether a *name*
carries git's vocabulary across the seam:

- **A remote name.** ("origin", "upstream", "fork".) The naming policy
  belongs behind the trait; git uses `origin` for all roles.
- **A `.git*` file convention.** (`.gitattributes`, `.gitignore`,
  `.gitmodules`.) The convention belongs in `src/git.rs`; a caller
  outside it that needs one is in the wrong module.
- **A user-facing git-vocabulary string.** ("rebase", "cherry-pick",
  "merge --continue".) Per-VCS phrasing belongs in a trait method like
  `conflict_resolution_hint`.

Each of the four worked examples above started as a change that
initially put VCS-specific code in rwv core; each one was refactored to
move the behavior across the seam. Every one of them predates the gate,
which is the argument for the gate: a convention that well-informed
authors violated repeatedly is a preference until something refuses it.

## Anchoring

The examples above each cite a closed work item and a landed commit. The
sync codepath that depends on examples (b) and (c) is covered by:

- `tests/e2e_two_workweaves_test.rs` — exercises the `merge=rwv-ours`
  replay-exclusion path end-to-end.
- `tests/e2e_sync_lock_replay_test.rs` — pins the three-layer driver
  mechanism (namespaced name, inline flags, durable config plant) and
  the `rwv sync --continue` / `rwv sync-to --continue` resume path.
- `tests/e2e_sync_abort_test.rs` — covers the conflict-hint text
  surfacing through the bail messages.

The push codepath (example (d)) is covered by:

- `tests/push_test.rs` — direct exercises of the push loop.
- `tests/doc_claims_push_test.rs` — anchors fork-pushes-like-owned,
  dependency/reference excluded from default scope, and
  project-repo-last ordering.

## Related joints

- [verb-vs-composition](./verb-vs-composition.md) — outer-boundary
  counterpart. The two joints together delimit rwv core.
- [verb-vs-vocabulary](./verb-vs-vocabulary.md) — naming half of the
  outer boundary; depends on this joint for the implementation-side
  half.
- [sync-semantics](./sync-semantics.md) — phase model that depends on
  examples (b) and (c).
- [lock-as-derived](./lock-as-derived.md) — the property that
  motivates `set_replay_exclusion` (example (c)) in the first place.
- [clone-topology](./clone-topology.md) — the tier-0 spec is
  VCS-neutral by design (canonical store, linked workspace, branch
  ownership); the git mapping there is the worked example, in the same
  shape as the examples in this joint.
