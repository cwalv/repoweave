# VCS as seam

repoweave operates on repositories. Today every repository it touches is
a git repository, but that is a current-state observation, not a
foundational assumption. The internal design treats the version control
system as a seam: a single layer where VCS-specific knowledge lives, and
everywhere else in rwv core uses the abstraction. This joint states the
principle and walks through four worked examples — each one a closed
refactor that pulled VCS-specific behavior across the seam, behind the
Vcs trait. The first carries a correction as well as a lesson: it is
where the seam once kept a parameter no backend ever read.

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
keeps rwv core readable: the higher layer says "push this branch to the
remote this repo publishes to" without needing to know what that remote
is called.

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
  concept ("the remote a managed clone publishes to") gets coded in
  three places that drift apart. The push path computes one name; the
  fetch path computes a different one; the dry-run output disagrees with
  the actual run. Single-source-of-truth at the seam prevents this
  category of bug entirely.

The principle is also a code-review tool. If a PR adds a git command,
git-specific name, or git config flag *outside* `src/git.rs`, send it
back: the abstraction belongs in the Vcs trait.

## Worked examples

### (a) `Vcs::resolve_branch_on_remote`, and the parameter it no longer takes

**Concept:** "look up branch X on the remote this repo publishes to."
The trait owns the qualifier; rwv core never spells a remote name.

**Anchors:** commits `1b76456` (the move across the seam) and `046aafd`
(what emptied the parameter it introduced).

`Vcs::clone_repo` and `Vcs::resolve_branch_on_remote` are the two
methods that own the convention: the git impl clones with the remote
named `origin` and qualifies a branch lookup as `origin/<branch>`.

**And the convention is not a variant a caller selects.** Naming the
remote arrived as a second clone method beside a `clone_repo` that left
the name to git — which resolves `clone.defaultRemoteName` from the
operator's own config. Three callers stayed on the plain one, so an
operator who had set that key got clones rwv made and then could not
read back: the remote exists under their name, and every later
`remote_url` / `add_remote` / set-head lookup asks for rwv's. A
choice between "named by the backend" and "named by whoever ran the
command" is not optionality either — no caller here wants the second,
and the seam's own rule says the backend decides. So the two methods
are one: `clone_repo` names the remote, and there is no longer a
spelling of "clone" that opts out of the convention.

**Why this is the seam shape.** Before the refactor the
`origin/<branch>` qualifier was spelled out at several call sites in rwv
core, and the fork convention lived on the manifest's `Role` type, as a
method handing back the literal strings `"upstream"` and `"origin"` from
a domain type. Moving both behind trait methods means:

- rwv core never spells `origin` directly.
- The remote convention is decided once, in the VCS impl.
- A different VCS impl can choose a different convention (jj's `default`
  / hg's `default-push`) without rwv core caring.

There is no bare-branch fallback in the trait surface: when the
conventional remote doesn't have the branch, the trait returns
`VcsError::RevisionNotFound`, not the local branch tip. This prevents
the silent "we advanced to the local working state instead of the
remote target" failure mode.

**What this example teaches second, and the reason it is first.** When
those methods were introduced they took the manifest's `Role`, and the
git impl mapped the fork role to a remote named `upstream`. The
parameter was load-bearing then — passing the role across the seam was
the mechanism that got git's remote names *out* of the domain type.
`046aafd` later dropped fork-specific routing as a product decision, and
from that commit on every implementor discarded the argument: six trait
methods, six ignoring bindings in the git impl and six more in the test
double, and three call sites in rwv core with no role to hand over,
naming the owned role to satisfy a signature.

It was kept anyway — as optionality for a backend that might one day
route differently — and guarded by tests asserting per role that every
role still resolved to the same remote. Both moves are ones a seam
invites, and both are wrong:

- **A parameter no implementor reads is not optionality, it is a
  fossil.** Nothing exercises it, so nothing reports when its meaning
  drifts; and it obliges callers that hold no such value to invent one,
  which is a defect no test on the implementor side can see.
- **A tripwire is the wrong instrument for a property available by
  construction.** With the parameter gone, "backends do not route by
  role" is a fact about the trait rather than a fact the suite rechecks,
  and reintroducing role routing becomes a visible trait change touching
  every implementor. Asserting per role that the roles agree only
  watches for a fossil starting to move.

So the parameter is gone from all six methods, and the seam states its
rule more simply than it could before: the remote name is entirely the
backend's business, selected by nothing on the domain side. A backend
that genuinely needs to route by role gets the parameter back in the
same change that gives it a reader.

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

**Concept:** push a named ref to the repo's conventional remote. For
git, `git push origin <ref>`.

**Anchor:** commit `6066ce1`, for a predecessor that took a role but not
the ref. The ref became a parameter when the branch model landed; the
role left by the route example (a) describes.

The impl owns the mechanism:

- The remote name.
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
verbs design discussion ("refs come from the manifest, not from
`git push` argument parsing"), the right shape was a trait method whose
contract names the *intent* ("publish this ref on the repo's
conventional remote") and hides the *mechanism* (`git push origin <ref>`
or some entirely different command on another VCS).

Which repos get pushed at all is verb policy, and the trait cannot see
it: the plan-time default scope (owned + fork; dependency and reference
excluded before the loop) lives in the push verb, and `push_ref` is
never told which role it is publishing. The asymmetry is deliberate. It
keeps the trait composable — a verb that wants to push only owned repos
filters first and then calls `push_ref` — and keeps verb-level policy
debuggable, because the default-scope choice sits in one visible
location instead of being distributed across the seam.

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
   `Command::new("git")` outside `src/git.rs`.
4. **Building a path out of git's on-disk layout.** The same gate
   refuses `.git`-shaped path construction outside `src/git.rs`, and
   names the helper to call instead. These three gate rules stop at the
   first test module, under any name: a `#[cfg(test)]` module may build
   a concrete backend.
5. **Naming the remote.** Two halves, both the compiler's. The constant
   holding the name is private to `src/git.rs`, so a `use` of it from
   core is `error[E0603]`; and no method on the `Vcs` trait accepts a
   remote name, so there is no parameter for a caller to spell one into.
   rwv manages one remote per clone: core reaches it through the methods
   that act on it — clone, push, resolve, add, read-URL — and renders
   its name, when a message needs it, through
   `Vcs::conventional_remote_name`, which answers for whichever backend
   the frame was given.

`scripts/ci-local.sh` runs the gate. It fails naming the file, the line
and which bypass it is.

What is left for a human is the part no gate can see — whether a *name*
carries git's vocabulary across the seam:

- **A remote name, in operator text.** Item 5 closes every path that
  *acts* on a remote; it does not close the sentences that *mention*
  one. That residue is now one site, and getting there corrected the
  count as much as it reduced it.

  The census used to read four: three telling the operator that
  `origin/HEAD` is unset and how to record it (`rwv add` twice,
  `rwv push` once), and one doctor finding for a URL mismatch. Measured
  against the tree it was six — `rwv fetch`'s non-fast-forward refusal
  ("a branch carrying commits origin has not seen") and `rwv update`'s
  ("diverged from the tip origin is on") were mentions nobody had
  counted, in messages whose subject is something else. A hand-written
  census undercounts in exactly that shape: it records the sites someone
  went looking for.

  The three set-head sites were really the third item in this list
  rather than this one — they spell a git command, `git remote set-head
  origin -a`, and respelling the remote name alone would leave the
  command standing. All three now come from
  `Vcs::remote_default_branch_repair_hint`, which carries the name and
  the command together because owning one without the other is what
  leaves a message half across the seam. The two `fetch`/`update`
  mentions were only ever naming the remote, so they render it through
  `Vcs::conventional_remote_name`.

  What remains is the doctor finding, whose wire `kind` carries the word
  too — the case below.
- **A published identifier carrying the name.** The doctor finding kind
  `origin-url-mismatch`, and the render text that echoes it. This one is
  not a message someone can rephrase — it is a token `--json` consumers
  match on, published under a promise of stability. It is a **stated
  exception**, decided under "A published identifier is not a message"
  below.

  It is also the reason enforcement here is a scan at all, where it once
  could not be. The argument against one used to be precision — a
  matcher over `origin` reported the sites someone had deliberately
  left, and a matcher that reports correct code gets turned off. With
  the message sites moved, the population is one, and one exemption
  naming one file is a scan someone will keep.
  `tests/core_remote_name_literal_test.rs` is that scan; it reads
  non-comment lines of `src/` outside the backend module, matching the
  standalone word only, so rwv's own `origin_dir` concept is invisible
  to it by construction.
- **A `.git*` file convention.** (`.gitattributes`, `.gitignore`,
  `.gitmodules`.) The convention belongs in `src/git.rs`; a caller
  outside it that needs one is in the wrong module.
- **A user-facing git-vocabulary string.** ("rebase", "cherry-pick",
  "merge --continue".) Per-VCS phrasing belongs in a trait method like
  `conflict_resolution_hint`.

### A published identifier is not a message

`origin-url-mismatch` names a git remote in the one namespace consumers
script against, and it stays. The decision is recorded here because the
rule above would otherwise read as covering it.

**Where the token is published.** The `sub_kind` of a `provenance`
finding in `rwv doctor --json`; an `enum` member of the committed
`docs/reference/schemas/doctor.json`; the generated
`docs/reference/explain/doctor.md`; and a heading in
`docs/reference/doctor-findings.md`, whose opening line promises "a
stable kebab-case `kind` tag" and whose lookup contract is one entry per
token, so that a finding met in the terminal is found without
translation. It is **not** a `--kind` filter value — that flag takes
top-level kinds, so `--kind origin-url-mismatch` refuses. The token is
something consumers read, not something they type.

**Rename.** `remote-url-mismatch` would be the better name under a
second backend. The cost is that every `--json` consumer matching the
current token stops matching, and stops **silently** — a `jq` selector
against a renamed key yields nothing rather than erroring, so the
failure looks like a clean repo. That set is not enumerable: this is a
published contract of a released version, and there is no register of
who reads it. The work itself is cheap and mechanical (the variant, the
regenerated schema and explain artifacts, the reference page, the
corpus mapping in `tests/`). The price is entirely paid by people
outside this repository, for a name nothing here branches on.

**Alias.** Publishing both spellings avoids the break and buys a second
namespace to keep in step forever — two schema members and two reference
entries for one condition, and consumers obliged to match both. It also
contradicts the reference page's own one-entry-per-token contract. This
repo has already written down that a second namespace "is a thing to
keep in sync, not a feature".

**Why the exception is principled and not merely cheapest.** The seam
exists so rwv core does not *depend* on a backend's vocabulary — so that
adding a second VCS is not a hunt through dozens of modules. A wire kind
creates no such dependency: no code branches on the word in it, no
resolution passes through it, and deleting the git backend tomorrow
would leave this token a naming wart rather than a compile error. What
the seam rule catches is core spelling a name it should have asked the
backend for. This is a name the outside world has already been told,
which is a different thing and carries a different cost.

The moment a rename buys something is the change that adds a second
backend, because that is when the token becomes genuinely ambiguous
rather than merely git-flavoured — and that change can carry the break
with a reason a consumer can read. Paying it now buys a better name for
a backend nobody has.

**What keeps this honest.** The exception is not prose alone.
`tests/core_remote_name_literal_test.rs` reports the site and exempts it
by file, with this reasoning at the entry, and the exemption is checked
in both directions: if the site stops existing, the standing entry fails
the test rather than sitting there. So the day someone does rename the
kind, the exemption comes out in the same change.

Each of the four worked examples above started as a change that
initially put VCS-specific code in rwv core; each one was refactored to
move the behavior across the seam. Every one of them predates the gate,
which is the argument for the gate: a convention that well-informed
authors violated repeatedly is a preference until something refuses it.

## Anchoring

The examples above each cite a closed work item and a landed commit. The
remote-naming convention (example (a)) is covered by:

- `tests/clone_default_remote_name_test.rs` — drives a clone under an
  operator `clone.defaultRemoteName` that is not the convention, and
  asserts the clone is one rwv can still read a remote back from.

The sync codepath that depends on examples (b) and (c) is covered by:

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
