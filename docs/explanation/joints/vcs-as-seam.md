# VCS as seam

repoweave operates on repositories through a seam: a single layer where
VCS-specific knowledge lives, with git as its first implementation rather
than its definition. Everywhere else, rwv core uses the abstraction. This
joint states the principle and walks through four worked examples, each a
refactor that pulled VCS-specific behavior across the seam, behind the Vcs
trait. The first carries a second lesson, about a parameter no backend
reads.

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
- Configuration mechanisms (`merge=rwv-lock`, `[core] sparseCheckout`).
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

The backend type is **private to the git backend's module**. Outside that module the
git implementation is reachable only as a `Vcs` handle. That is what
makes the principle above a compile-time property rather than a
convention: rwv core cannot name a backend, so every frame that speaks
to a repository has to accept a handle it could be given a different one
of.

This joint states the contract, in the vocabulary a reader who never
opens `src/` can check it in. The module and ownership model underneath
— which type lives where, how wide the trait is, and the call sites that
knowingly bypass it — is in `ARCHITECTURE.md`, at the repo root.

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
git-specific name, or git config flag *outside* the git backend's module, send it
back: the abstraction belongs in the Vcs trait.

## Worked examples

### (a) Remote roles, and the parameter the seam does not take

**Concept:** "act on the remote that plays this role." A repo can have
several remotes, and rwv acts on each for a different reason, so the trait
resolves a role to an opaque remote handle. rwv core holds the handle and
gives it back to the operations that act on it (fetching it, resolving a
branch on it, reading its default branch, pushing to it); it never spells a
remote name.

The roles are:

| Role | What it is |
|---|---|
| **Url remote** of a member | the remote that has the member's `url`. The seam finds it by the url, however the clone spells it; a clone that lacks it gets one before a verb fetches, named by the url's owner segment (a repo with no remote yet takes the VCS's first name). A name another url already holds refuses as `name` taken. |
| **Url remote** of the project repo | the remote `rwv clone`, `rwv init --provider` or `rwv member set-url projects/<name>` recorded, recorded in the clone's own configuration; else its only remote while it has exactly one; else none. The project has no member url to find it by. |
| **Push target** | where a branch publishes, in the VCS's own order: the branch's push remote, the repo's default push remote, the remote the branch takes in from, and last the url remote. A branch with none has no push target. |
| **Home remote** | where a new branch is taken from: the remote the operator configured for it, else the url remote. |
| **Upstream**, **push branch** | the remote branch a branch takes in from, and the same-named branch on its push target. |

The seam also reads where a branch tip stands to another by ancestry
(even, behind, ahead, diverged), and whether a remote's tracking branches
reach a commit.

**A handle is not a name.** A remote handle has no constructor outside the
backends and no way to read its name back as text; it displays, for the
message that has to say which remote a verb acted on. A remote git called
`origin`, `upstream`, or whatever the operator's `clone.defaultRemoteName`
chose is addressed the same way, so no name is special and a clone keeps the
name git gave it.

**Why this is the seam shape.** A remote name spelled at a call site puts one
VCS's convention into core, and makes every clone whose remote is named
otherwise one rwv cannot read back. Resolving roles at the seam decides each
convention once, in the VCS impl; a different VCS can resolve the same roles
by its own means (jj's `default` or hg's `default-push`) without core
noticing.

There is no bare-branch fallback in the trait surface: when the remote does
not have the branch, the trait reports the revision as not found, not the
local branch tip. This prevents the silent "we advanced to the local working
state instead of the remote target" failure mode.

**What this example teaches second, and the reason it is first.** A seam
invites carrying a domain value across it "in case" a backend wants it. The
natural candidate here is the manifest's `Role`, which would let a backend
route a fork to a remote named `upstream`. No backend routes by role, so the
methods take no role, and the two tempting alternatives are both wrong:

- **A parameter no implementor reads is not optionality, it is a
  fossil.** Nothing exercises it, so nothing reports when its meaning
  drifts; and it obliges callers that hold no such value to invent one,
  which is a defect no test on the implementor side can see.
- **A tripwire is the wrong instrument for a property available by
  construction.** Without the parameter, "backends do not route by role"
  is a fact about the trait rather than a fact the suite rechecks, and
  reintroducing role routing is a visible trait change touching every
  implementor. Asserting per role that the roles agree only watches for a
  fossil starting to move.

So the seam's rule is simple: the remote name is entirely the backend's
business. Core selects a remote by the role it plays, never by name and never
by a manifest role. A backend that genuinely needs to route by manifest role
gets the parameter in the same change that gives it a reader.

### (b) The conflict-resolution hint

**Concept:** the human-readable text we splice into sync's bail
messages explaining how to resume after the user resolves conflicts.
The VCS impl owns git-vocabulary steps (stage the resolution, run the
VCS-native continue for merge/cherry-pick ops). For rebase ops,
rwv has a native `rwv sync --continue` / `rwv land --continue` that
drives the remaining picks; the VCS impl stops at staging and rwv core
appends the `rwv <verb> --continue` line.

The hint operation takes the kind of in-flight op sync's project-repo
path can leave behind — a rebase, a merge or a cherry-pick — and
returns the hint block for it. The git impl is
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

### (c) The lock merge driver and derived content

**Concept:** a project repo declares how `rwv.lock` merges, and a repo may
declare paths whose content is derived. For git each declaration is a
`.gitattributes` line naming a merge driver, and a name resolves to nothing
until some config git can see defines `merge.<name>.driver`. The line is
committed and travels with a clone; the definition does not, and that
asymmetry is the whole shape of the thing.

- **The lock.** `rwv.lock merge=rwv-lock` names the lock merge driver, and
  `rwv init` and `rwv clone` define it in the clone's config as `rwv lock
  --merge-driver %O %A %B`. git finds `rwv` on `PATH` and runs it in every
  merge rwv does not make, where it merges pins three-way; a merge it cannot
  settle, or a driver git cannot run, leaves `rwv.lock` conflicted, and every
  verb refuses it until it is resolved. A project repo set up any other way
  is refused by every verb that writes it, as `missing-lock-merge-driver`,
  and `rwv doctor --fix` migrates it. The seam states this as
  three operations: report the setup state, declare the driver in the repo, and define it in the clone's configuration.
- **Derived content.** `<path> merge=rwv-ours` declares a path derived.
  Nothing defines `rwv-ours` durably: rwv's own replays define it inline, per
  git process, as `true` — the shell command that leaves the target side's
  copy standing — through the derived-content policy the replay states. The
  merges `sync` and `land` compose run in the checkout of the work being
  integrated, so they define it as the shell command that copies the merged-in
  side over it: the receiving side's copy stands, as after a rebase onto it.
  The merges `pull --merge-upstream` and `update --remote --merge-upstream`
  make bring the upstream in, and define it the same way: the upstream's copy
  stands, as after a rebase onto the upstream. A
  merge or rebase run outside rwv resolves the path as an ordinary textual
  conflict. That is noisy, not dangerous: the content is derived, so
  regenerate afterwards and the repo's own drift gate judges the result on
  either route. Repoweave's own tree declares its generated reference pages
  this way.

rwv's own merges and replays hold the lock merge driver at `true` the same
way, so they neither depend on `rwv` being on `PATH` nor stop on a lock the
driver would conflict: they settle `rwv.lock` themselves. A rebase replays
each commit as a three-way merge, so the hold matters to a `linear` `rwv
sync` as much as to a merge.

**Used by** [sync-semantics](./sync-semantics.md)'s Phase 1' to keep
`rwv.lock` out of the merge inputs. The lock is regenerated from
manifest tips in Phase 3, so carrying user lock-edits through a rebase
would only produce noise. See
[lock-as-derived](./lock-as-derived.md) for why this is structurally
right.

**Why this is the seam shape.** The declaration, its definition and the
hold are per-VCS. A hg impl can implement them via `[merge-patterns]` in
`.hgrc`; a jj impl might do something entirely different. Sync neither
knows nor cares. The setup query answers with a state per half rather than
a bool, because doctor asks twice — once to report, once to repair — and
both answers come from one read.

### (d) Publishing a ref

**Concept:** push a named ref to the remote it publishes to, its push
target. For git, `git push <remote> <ref>`.

The impl owns the mechanism:

- The force-push flag spelling.
- The argument shape `git push <remote> <ref>`.
- The order a push target is found in.

And what it explicitly does **not** own: the choice of which ref to
publish. The predecessor read the current branch inside the impl, which
put a policy decision — *what does publishing this repo mean?* — inside
the git wrapper, where no caller could see it and a detached HEAD
surfaced as a raw command failure from three layers down. The
publishing operation takes the ref as a value only the push verb can
construct, and the remote as the handle push-target resolution gave, so the
decision is made once at the publish gate and the
refusal for a detached checkout is stated there, in the verb's own
voice. Whether a push may replace the remote's branch is an
argument: fast-forward only, or a lease on the tip the push read, which
only a repo the operator named with `--overwrite` yields.

The push verb does the cross-repo work — walking the manifest, applying
selectors, ordering project-repo last, checking the lock-state
precondition — but never invokes git directly.

**Why this is the seam shape.** Before this method existed, the
push-loop draft constructed `git push` argument strings inline. Per the
verbs design discussion ("refs come from the manifest, not from
`git push` argument parsing"), the right shape was a trait method whose
contract names the *intent* ("publish this ref on its push target") and
hides the *mechanism* (`git push <remote> <ref>` or some entirely
different command on another VCS).

Which repos get pushed at all is verb policy, and the trait cannot see
it: the plan-time default scope (owned + fork; dependency and reference
excluded before the loop) lives in the push verb, and the publishing
operation is never told which role it is publishing. That separation
keeps the trait composable — a verb that wants to push only owned repos
filters first and then publishes — and keeps verb-level policy
debuggable, because the default-scope choice sits in one visible
location instead of being distributed across the seam.

## What enforces this

The parts of the principle that can be mechanized are mechanized, and a
reviewer should read a green build as having already answered them.

1. **Naming the backend.** The type is private to the git backend's module, so any
   reference from rwv core is `error[E0603]`. There is no way to write a
   new hardcoded backend that compiles.
2. **Minting one.** The one constructor that hands out a git backend is
   `pub`, because the test suite needs a concrete one. That means a verb
   could call it instead of accepting a handle — dispatching correctly
   while remaining impossible to substitute in a test. The `vcs-seam`
   gate refuses it outside the seam's own resolver module, where every resolver that mints
   a backend with no manifest entry to resolve from documents why git is
   the only answer it can give.
3. **Spawning git from scratch.** The same gate refuses
   `Command::new("git")` outside the git backend's module.
4. **Building a path out of git's on-disk layout.** The same gate
   refuses `.git`-shaped path construction outside the git backend's module, and
   names the helper to call instead. These three gate rules stop at the
   first test module, under any name: a `#[cfg(test)]` module may build
   a concrete backend.
5. **Naming the remote.** Two halves, both the compiler's. A remote
   crosses the seam as an opaque handle, which only a backend constructs
   and which gives its name to nobody but a message; and no method on the
   `Vcs` trait accepts text for a remote, so there is no parameter for a
   caller to spell one into. Core reaches a remote through the role that
   resolves to it — url remote, push target, home remote — and renders its
   name, when a message needs it, by displaying the handle.

`scripts/ci-local.sh` runs the gate. It fails naming the file, the line
and which bypass it is.

What is left for a human is the part no gate can see — whether a *name*
carries git's vocabulary across the seam:

- **A remote name, in operator text.** Item 5 closes every path that
  *acts* on a remote; it does not close the sentences that *mention*
  one. Those display the handle the verb resolved, which reads as the
  backend names it.

  A hand-written census of these mentions undercounts in one shape: it
  records the sites someone went looking for, and misses the ones in
  messages whose subject is something else, which mention a remote in
  passing.

  The hints telling the operator that a remote's `HEAD` is unset belong to
  the third item in this list rather than this one: they spell git
  commands such as `git remote set-head <remote> -a`, and respelling the
  remote name alone would leave the commands standing. They come from
  the seam's repair-hint operations, one for a remote and one for a clone
  `rwv member add` just made, each given the
  handle and carrying the name and the command together because owning one
  without the other leaves a message half across the seam.

  The one remaining site is the doctor finding, whose wire `kind` carries
  the word too — the case below.
- **A published identifier carrying the name.** The doctor finding kind
  `origin-url-mismatch`, and the render text that echoes it. This one is
  not a message someone can rephrase — it is a token `--json` consumers
  match on, published under a promise of stability. It is a **stated
  exception**, decided under "A published identifier is not a message"
  below.

  It is also why enforcement here can be a scan. A matcher over `origin`
  that reported correct sites would get turned off; with a population of
  one, one exemption naming one file is a scan someone will keep.
  `tests/source/core_remote_name_literal_test.rs` is that scan; it reads
  non-comment lines of `src/` outside the backend module, matching the
  standalone word only, so rwv's own `origin_dir` concept is invisible
  to it by construction.
- **A `.git*` file convention.** (`.gitattributes`, `.gitignore`,
  `.gitmodules`.) The convention belongs in the git backend's module; a caller
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
rather than merely git-flavored — and that change can carry the break
with a reason a consumer can read. Paying it now buys a better name for
a backend nobody has.

**How this is enforced.** The exception is not prose alone.
`tests/source/core_remote_name_literal_test.rs` reports the site and exempts it
by file, with this reasoning at the entry, and the exemption is checked
in both directions: if the site stops existing, the standing entry fails
the test rather than sitting there. So the day someone does rename the
kind, the exemption comes out in the same change.

Each of the four worked examples above started as a change that
initially put VCS-specific code in rwv core; each one was refactored to
move the behavior across the seam. Every one of them predates the gate,
which is the argument for the gate: a convention that well-informed
authors violated repeatedly is a preference until something refuses it.

## Adjacent seam: registries

A registry is a seam of its own, adjacent to `Vcs`
rather than inside it: a registry decides a repo's identity, its
canonical path, and — for `rwv member add --new` — what parameters creating one
needs; `Vcs` decides how the backend that registry named actually creates,
clones, or brands it. `CreationPlan.vcs` is the one field a registry
writes and a verb reads across that line — the registry constrains which
backend a creation could use, the verb still chooses, and the plan
records the choice made rather than leaving it implicit.

## Related joints

- [verb-vs-composition](./verb-vs-composition.md) — outer-boundary
  counterpart. The two joints together delimit rwv core.
- [verb-vs-vocabulary](./verb-vs-vocabulary.md) — naming half of the
  outer boundary; depends on this joint for the implementation-side
  half.
- [sync-semantics](./sync-semantics.md) — phase model that depends on
  examples (b) and (c).
- [lock-as-derived](./lock-as-derived.md) — the property that
  motivates the lock merge driver (example (c)) in the first place.
- [clone-topology](./clone-topology.md) — the tier-0 spec is
  VCS-neutral by design (canonical store, linked workspace, branch
  ownership); the git mapping there is the worked example, in the same
  shape as the examples in this joint.
