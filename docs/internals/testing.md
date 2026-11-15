# Test quality

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

Standard for writing, reviewing, and deleting tests in this repo — the
companion to [code-style.md](code-style.md), which governs source. Read it
before adding a test, before trusting one in review, and before deleting
one.

The suite is deliberately accretive by default: every change lands with
pinning tests. This document is the counterweight — what a test must
evidence to enter the suite, what marks one as low-signal once here, and
when deletion is the correct outcome rather than vandalism.

## What a test is for

A test pins a promised behavior so that the defect it guards reddens it.
Every test must be able to answer two questions at review:

1. **Which defect does this catch?** Named concretely — inputs, state, and
   the wrong outcome — not "covers function X".
2. **What shows it still catches it?** Mutation evidence (below), recorded
   when the test lands.

A test that cannot answer both is decoration, whatever it asserts.

## Where a test lives

Three homes, and which one is forced by what the test reaches rather than
by how large the module has grown:

- **An inline `#[cfg(test)]` module beside the source** — the only home for
  a test whose subject the module does not export. A child module sees its
  parent's private items; nothing outside the crate does.
- **`tests/`** — a test that drives the shipped binary, or one that reaches
  only the library's `pub` surface. These link the library target and get
  the dev-dependency set; neither binary target is visible to them.
- **A sibling file declared `#[cfg(test)] mod tests;`** — the option that
  looks free. It is not, and the next section is why.

**Size is not a reason to move.** `src/sync.rs` carries a large block of
tests inside a large file, and they stay there: most of its tests name an
item the module does not export — private module-level items and associated
functions, each the subject of the test that names it. Relocating them means
exporting every one of those private items so that a test can watch. This
crate spends visibility deliberately — `src/main.rs` is a logic-free shim
exactly so a consent-token constructor can stay `pub(in crate::cli)` — and a
test is not what it is spent on.

**The integration-shaped tests are the ones that cannot move.** Every test
in that module which builds a real git topology reaches a private subject;
the minority that compile from `tests/` are string and enum round-trips and
a rendered-line check. "Move the integration-shaped half out" inverts under
measurement — what it would leave inline is the unit-shaped half. Even that
minority is not a byte-for-byte relocation: a test written inside the crate
spells its own modules `crate::`, which no external target resolves.

### Moving a module out of its host file changes what the gates read

`before_test_module` in `src/bin/generate-explain.rs` defines test scope as
an inline `#[cfg(test)]` that opens a body. A `#[cfg(test)] mod tests;`
declaration is deliberately not a boundary, so the file it names is read as
production by every gate that calls it:

- `check_vcs_seam_bypasses` reddens. A test may spawn git and mint a
  backend; production may not. `src/sync.rs`'s module moved to
  `src/sync/tests.rs` trips it — and the gate bails there, so the checks
  behind it go unmeasured.
- `src_code_identifiers` and `src_code_doc_filenames` widen *silently*,
  which is worse than reddening. They are the evidence and exemption sets
  `check_doc_symbol_refs` and `check_doc_citations` consult, and an
  identifier spelled only by that module's tests would begin counting as
  proof that a name a comment cites still exists. `src_code_identifiers`
  records that this exclusion held a live mutation.
- `check_env_input_reads` wants an allowlist entry for every `std::env`
  read the moved file carries.

`src/vcs/testing.rs` lives this way today and trips none of them, because it
is a fake `Vcs` that spawns nothing and mints nothing. It licenses a
hand-built double in its own file, not a fixture-heavy module.

A module that must move takes that scope with it: widen
`before_test_module` to follow the declaration into its file first, with its
own seeded failure, and measure what those checks report once they can see
it.

## Test binaries

`tests/` builds a handful of test binaries, one per directory: cargo makes
`tests/<area>/main.rs` a binary named `<area>`, and every test file is a
module of one of them, declared in that `main.rs`. A file keeps its name as
its module, so `tests/sync/sync_to_test.rs` is the module `sync_to_test` of
the `sync` binary and its tests are named `sync_to_test::<fn>`. The shared
helpers in `tests/common/` are one module tree every binary declares as
`common` (`#[path = "../common/mod.rs"] mod common;`), and a test file reaches
them with `use crate::common;`.

An area groups what is edited together, because a change to any file rebuilds
its whole binary: a binary per area keeps that rebuild to the area's files
rather than the suite's. `source` holds the structural pins, compile-fail and
seal probes and the tests of the test tooling; `docs` the doc-claim, explain,
prime, schema and refusal-entry tests; `process` plugins, `rwv each`, job
control and concurrent rwv processes; `vcs` the git seam and branch model;
`doctor` doctor, status and the health and state checks; `sync` sync, land,
abort, lock, push, pull, fetch and update; `weave` workweaves, branches,
members, clone, init, activation and project resolution; `integrations` the
ecosystem integrations, hooks and surfacing; `scenario` the scenario harness;
`kill_matrix` the matrix that kills every verb at every pause point it passes and
continues or aborts it, in a binary of its own because it runs as long as the rest
of the scenario binary and the gate runs binaries side by side.
`test_pause_build` is a binary of its own because the gate's `no-test-pause`
stage builds it without the `test-pause` feature, and a binary of one file is
all that stage should compile.

A file no binary declares never runs, and nothing reddens for it.
`tests/source/test_binary_layout_test.rs` fails on any `.rs` file under
`tests/` outside `fixtures/` that no binary's module tree reaches, that two
binaries reach, or that sits directly under `tests/` (cargo would make it a
binary of its own); it holds its module walk equal to the dep-info rustc
writes for its own binary. A new test file goes in the area it is edited
with, and its `mod` line goes in that area's `main.rs`.

### Running part of the suite

The gate's test stages pass `--release --features test-pause`, and so should
any run whose result you compare with one; they are left out below.

```sh
cargo test --test sync                                 # one area
cargo test --test sync -- sync_to_test::               # one file's tests
cargo test --test sync -- sync_to_test::a_test --exact # one test
```

A filter is a substring of the test's name, so `sync_to_test::` also selects a
file whose name ends in `sync_to_test`; `--exact` takes the whole name.

### Everything in a binary shares one process

libtest runs a binary's tests on threads of one process, so a test owns
nothing process-wide: not the environment, not the working directory, not a
signal handler or panic hook, not a fixed temp path or port. A test that needs
any of those starts a process — usually this binary again, playing a child
role — and passes what the child needs on its command line and environment.
Two rules come with that:

- **A child is selected by a derived name.** `common::test_name(f)` takes the
  `#[test]` function itself and returns the name libtest selects it by,
  having checked the binary lists that test exactly once; its `selector()`
  is the argument list. A name typed as a string goes stale when the test
  moves, and an `--exact` that matches nothing exits 0 having run nothing.
  `common::rerun_one_test` also requires the child to report one test run.
- **A program a test runs is written by a child process.** Exec fails with
  `ETXTBSY` while any process holds the file open for writing, and every
  child another thread forks inherits the test process's write handles until
  it execs. A script, hook or stand-in tool goes on disk through
  `common::write_executable`, `write_program` or `copy_executable`, which
  write it from a child process, never through `std::fs::write` and a
  permission change.

## Behavioral pins and structural pins

A **behavioral pin** asserts an observable outcome through a supported
surface: the shipped binary, published wire bytes, a rendered message. A
**structural pin** asserts a property of the source itself: a scan, a
count, a call-site arity, a compile-fail probe.

Behavioral is the default. A structural pin is licensed in exactly two
situations, and the license is stated in the test's own doc comment:

- **No behavioral assertion can distinguish** correct from broken on any
  platform or fixture the suite reaches. The shipped precedent is a
  single-read chokepoint whose divergence window is too small to drive
  (`tests/source/source_classification_single_read_test.rs`).
- **The property is a prohibition over an enumerable population** — a seam
  census, a destructive-call inventory
  (`tests/source/destructive_ops_audit_test.rs`).

Expense is not a license: a behavioral drive that would merely be slow is
still the required form. Only impossibility on every reachable platform and
fixture licenses the structural stand-in.

Obligations that come with the license:

- **State your own scope.** The pin's doc comment names what it reads —
  which files, which patterns — and therefore what is invisible to it. A
  regression outside a structural pin's scope is caught by nothing, so the
  scope statement is the coverage boundary and must be readable, not
  inferred from the implementation.
- **Count uses, not mentions.** Doc comments, test literals, and prose
  inside string literals are not call sites. An instrument that counts the
  thing quoting a name rather than the thing using it both over-counts
  (prose trips it) and can be satisfied by prose (nothing real behind the
  count).
- **Derive the population; don't hand-enumerate it.** Where a closed set
  exists — a published schema, an exhaustive match, a trait's implementor
  list — derive the pin's population from it, so a new member reddens the
  pin instead of silently extending past it. A hand-written count is the
  weakest form: it misses new callers of an already-counted site, and its
  justification prose rots into a false enumeration.
- **Guard a walk with a second derivation and a witness, never a number.**
  A walk that has gone blind passes every assertion over what it found, so
  it needs a guard — but an exact count is edited to whatever the tree
  produced by every change that adds a member, and a floor is lowered by
  every change that removes one, and neither sees one member lost. Hold the
  walk equal to a second derivation of the same population (the schema
  schemars derives against the enum's declaration, a shape-blind scan
  against a shape-aware one, one direct run of a script against the runs a
  wrapper makes), and name one member the walk must contain. Where the walk
  has no second derivation, the named member is the guard.
- **A finding a structural pin records, it generates.** A census whose
  finding is a list — the refusal sites no test reads — is an artifact
  `cargo run --example generate-explain` writes and the gate's drift stage
  compares, not a fixture a test holds to a number. A change to the list
  shows in review as a diff, and a change that adds a member regenerates
  rather than hand-editing a table. The judgment a list cannot derive
  belongs at the site that owns it, in a form the scan reads: the class of
  an untokened error is `untokened!`'s first argument, not a column beside
  it.
- **Suppression scope equals naming scope.** If findings are reported
  per-file, exemptions are per-file too. A global exemption fed by a local
  mention widens the blind spot beyond the file that earned it.
- **Justification prose follows the same rule.** An allowlist entry whose
  justification names a type gate (a proof parameter, a sealed
  constructor) survives new callers; one that enumerates callers rots
  silently. Prefer the type-gate claim; where only a caller enumeration is
  available, assert the measured caller count so a new caller reddens the
  entry rather than the prose.

## Mutation evidence

A test enters the suite with recorded evidence that it reddens on the
defect it guards:

- **Bug fix:** revert the fix and run the new test. Revert *every* site the
  test asserts on — a partial revert can leave upstream code synthesizing
  the signal the old code read, and then the probe is the bug.
- **New mechanism or prohibition:** seed the violation and observe the red.
  If a script applies the seed, the script must assert its anchor matched —
  a patch that silently no-ops gates an unmutated tree and reads as
  evidence.
- **A compile failure is a void run, not a red.** A mutated tree that fails
  to build looks, at a glance, exactly like the red the mutation was meant
  to produce. A mutation is evidence only if the tree still compiles and a
  named assertion fired; record which.
- **Undo mutations by reversing the patch, never by checkout** — checkout
  restores the committed state and destroys uncommitted work.
- **A seeding script guards before it writes and proves its restore.** Before
  writing, check that the anchor matched exactly once and that the
  replacement is non-empty — an empty replacement leaves the reverse patch
  nothing to anchor on. After restoring, and after any interrupted run,
  compare the file to its pre-run bytes and require a clean
  `git status --porcelain`.
- **Check which assertion fired.** An older assertion may catch the
  mutation first, leaving the new pin unexercised behind a red that looks
  like proof.
- **Run each instrument on its own.** To learn which instruments catch a
  mutation, run one test file per invocation — its binary and its module,
  `cargo test --release --features test-pause --test <area> -- <file>::` —
  and record a verdict for each. A whole-binary run reports a red for the
  binary, not for the instrument, and `cargo test` without `--no-fail-fast`
  stops at the first failing binary, leaving every later one unrun rather
  than green.

A mutation that stays green is a finding, not a pass. Green has exactly
three explanations, and the recorded evidence must say which:

1. **Not reached.** Requires a reachability control — a poison mutation at
   the same site that does redden, proving the site executes.
2. **Reached but indistinguishable here** — the platform or fixture
   collapses the two behaviors. State the collapsing condition; this is
   the situation that licenses a structural pin (above).
3. **Caught by another instrument.** Name it. If both instruments are kept
   as necessary, prove necessity with opposed mutations — one that only
   the first catches, one that only the second catches. "Both catch this
   one" proves overlap, not necessity.

Redundancy claims point the other way and carry the same burden: to show a
test redundant, remove it, apply the mutation it guarded, and show the
surviving instrument reddens — plus a control showing the removed test used
to. Green with both removed proves the coverage never existed, which is a
different finding.

Complementarity claims carry the same burden again: when two instruments
are described as complements ("variant width is that file's job;
byte-level minting is this one's"), the claim must name a defect each
alone would miss and show the other catches it. A claimed division of
labor can have a hole in the middle that each half's own stated remit
hides.

## Fixture discipline

- **For every input a fixture holds constant, ask whether production
  guarantees the constant.** If production does not, at least one test must
  diverge it. A property shared by every fixture is an assumption the suite
  cannot see past: an entire input plane can go uncovered behind one shared
  fixture default, and every bug in that plane arrives "tested".
- **A degenerate fixture blinds value assertions.** A fixture that
  collapses two representations into one — the same name for two roles, one
  integration where scope needs two, matching values where divergence is
  the subject — cannot tell them apart however exact its assertions look.
  Ask what the fixture collapses, not what the test asserts.
- **Build only shapes production writes.** A hand-built unsupported
  topology manufactures findings production cannot produce — and a fixture
  that builds one may itself be the defect the suite is encoding.
- **Port or skip, never quietly vacate.** When a precondition stops
  occurring, the test either ports to the new precondition or becomes a
  visible skip that names its lift condition. A skip that outlives its lift
  condition is the same defect one step later.
- **An existing test may encode the defect.** A red under your fix can be
  the old wrong behavior written down. Reread the failing assertion against
  the promise, not against the previous output.
- **A shared fixture serving several pins owes its own scope statement** —
  the inputs it holds constant and the members it never samples — and a
  compensating fixture elsewhere owes a back-pointer from the shared
  fixture. A silence stated only at the consuming site is invisible from
  the instrument.

## Low-signal marks

Any of these marks a test for scrutiny. None is an automatic deletion; each
is a question the test must survive:

- Asserts the fixture, not the subject — the assertion would pass against
  the fixture-construction code alone.
- Compares through a helper more tolerant than the production render — green
  whether or not production leaks. Assert through the render production
  uses.
- Substring match where agreement is the property — a pin meant to keep a
  header and its command in step cannot enforce agreement by containment.
- Necessary-but-insufficient assertions ([code-style.md](code-style.md)
  names this pattern for source review; it applies doubly to tests).
- A count standing in for an enumeration.
- One-outcome sampling on a multi-outcome surface — a conformance layer
  that drives only the success path validates nothing about failure bytes.

## Deletion, replacement, and what they carry

Deletion is a licensed outcome, not vandalism — this repo has done it on
principle and recorded it ([branch-model.md](branch-model.md) §4.4: an
invariant checked by construction should not also carry a tripwire). Three
licenses:

- **By-construction supersession.** The invariant became unrepresentable —
  a parameter deleted, a type sealed, a compile error where the violation
  was. The tripwire that watched for it retires in the same change.
- **Fossil.** The test pins capacity that no longer exists. Measure first —
  does anything still vary the input it pins? — then delete. Alpha-level
  software exists so that fossils don't; a fossil kept-and-documented is
  still a fossil.
- **Proven redundancy.** Per the mutation section: surviving instrument
  named and shown to redden, control shown.

**Replacement** is its own move, distinct from deletion and from accretion:
when a design change swaps the invariant itself, the new pin lands in the
same change that retires the old one, with its own mutation evidence —
never a gap between them.

A deletion or replacement records, in the change that performs it: the
measured vacuity or the surviving coverage, and — only for
live-but-contested capacity — the trigger that would justify reversal.

## Asserting a refusal

`tests/common/cli.rs` holds `run`, `succeed` and `refused`. A test that
expects a refusal asserts it with `refused(&out, State::<Variant>)` for a
judged state or `refused(&out, RefusalKind::<Variant>)` for a token: exit 1,
the judged line or the token's route, and the route lines last. A failure
any other way — a panic, a usage error, a different state of the same axis —
does not satisfy it.

It is also what the refusal census reads. `docs/internals/refusal-census.tsv`
credits a site as read only by a test that names the site's own state or
token in a `refused` call (or a token in a route assertion), so a test that
checks a refusal by substring, or by its axis alone, leaves the site unread
there.

## Prohibitions get a red test

A rule of the form "X must not happen", and every deliberate silence ("the
tool does NOT do Y"), gets a test that fails when the rule is broken:
compile-fail probes, asserted non-findings, pinned known-limits. Prose
prohibitions decay when the next reader "fixes" the asymmetry; a red test
converts that drift into a conversation with the original decision.

## Instruments are subjects

Gates, scans, and summaries are tested like the code they guard:

- Feed a known red through any new summary or filter before trusting its
  green — field-separator quirks and pattern edge cases drop lines
  silently.
- Per-suite numbers require self-labeling output — invoke one suite at a
  time, or pair each result block with the header naming its suite.
  Positional reading of multi-unit output fabricates attributions.
- Prove the gate cannot print green on failure: a terminal "all passed"
  line is stage-relative; the evidence is the enumeration of stages run,
  not the sentence.
- A check that never fires may be blocked upstream — assert the enumeration
  reaches your predicate, not only that the predicate is correct.
- Judge a gate from its output, never from a piped exit code.

## The gate and its tiers

`scripts/ci-local.sh` with no argument is the full gate, and it is what CI,
the release gate and the pre-push hook run. Two named tiers split it, and a third adds the kill matrix:

- `scripts/ci-local.sh --tier=seat` runs the stages a change is most likely
  to redden and the ones whose output it must commit — `check`, `test`,
  `no-test-pause`, `clippy`, `doc`, `fmt`, `drift` — for iterating on a
  change in its own checkout.
- `scripts/ci-local.sh --tier=landing` runs exactly the default stages seat
  skips — `windows`, `apple` and `test-ambient-autocrlf`, two cross-target
  checks and a second whole-suite run — once per landing.

Together they are the full gate, each stage once. The script writes down only
the seat list and computes landing from it, and
`tests/source/ci_local_stages_test.rs` holds the partition. A change touching
`cfg(windows)` or macOS-only code, or how line endings are read, written or compared, runs
the full gate in its own checkout, since those are exactly what the landing
stages catch. A tier run's terminal line names the tier and its stages, so
its log never reads as a full gate's. `--tier` and `--stages` are refused
together.

The kill matrix is not in the full gate. `scripts/ci-local.sh
--stages=kill-matrix` runs its cells (about a quarter of an hour) and sets
`RWV_KILL_MATRIX=run`, without which each shape test returns at once and prints
the stage that runs it; `--tier=release` is every default stage and
`kill-matrix`. Run it before a release and when a change touches an
operation's steps, pause points or abort. The source test that `POINTS` matches
the source, and the test that every point a shape passes has a cell or a
`same_as` entry, stay in the `test` stage.

Every stage that runs the suite hands it `RUST_TEST_THREADS`: half the
host's cores, never fewer than four (or every core, on a smaller host), unless
the caller set it. A test binary's tests all run in one process, and at one
thread per core several gates at once oversubscribe the host while one gate
alone runs no faster; the script's header gives the measurement.

## The scenario harness

`tests/scenario/scenario_harness_test.rs` runs branching scenarios step by step in a
hermetic sandbox and holds every step to a set of invariants. A scenario is
data: a [`Seed`](../../tests/common/scenario_sandbox.rs) saying what the
sandbox holds beyond its fixed shape, and a list of steps, each carrying its
expected outcome and what it may do that an invariant would otherwise report.
Three modules carry it, and each has one job:

- **`tests/common/scenario_sandbox.rs`** builds the world: bare remotes,
  the machines that clone them, and the forge's own clone of each origin. Every
  git and rwv it runs sees a `HOME` inside the sandbox. A seed can add, per
  member, a fork remote and two mirrors of the origin (one keeping branches the
  origin drops, one pruning to an exact copy), give a member a role other than
  `owned`, and install an `update` hook on any remote that refuses the records
  branch. A seed can also make every git the sandbox runs answer as release
  2.37 (`Seed::old_git`): a `git` earlier on `PATH` reports that release and
  refuses `merge-tree --write-tree` as it does, then runs the real git, so the
  fallback that merges in a checkout runs on a machine whose git has the
  command. Any remote can be made unreachable mid-scenario by moving its bare
  directory aside behind a drop guard, so a scenario that fails during an
  outage still leaves the remote where its URL points. The key a member entry
  declares its branch under and the `.gitattributes` line for the lock are one
  constant each, which the seed and every hand edit of an entry spell from.
- **`tests/common/scenario_runner.rs`** performs steps and checks their
  outcomes. A forge event that a push can express (a force-push, a branch
  rename, a rebase-merge) is a sequence of plain `Action::Git` steps in the
  forge's clone; one no push can express (moving a remote's default branch,
  syncing a mirror) runs in the bare remote itself. A step may also commit by
  hand with git's plumbing, which is how a fixture lands a records entry or a
  lock on a branch nothing has checked out.
- **`tests/common/scenario_invariants.rs`** snapshots the sandbox before
  and after each step and compares the two. Its module comment is the one
  statement of what each check holds, what it reads, and what it cannot see;
  keep it that way rather than restating checks elsewhere.

The three modules, with the step builders both binaries share
(`tests/common/scenario_steps.rs`), live under `tests/common/` because
the `scenario` and `kill_matrix` binaries both run them.

### The kill matrix

`tests/kill_matrix/kill_matrix_test.rs` kills each verb that records an
operation at every pause point the source declares that it passes, then runs
the verb's `--continue` in one sandbox and `rwv abort` in another. `POINTS` is
the one enumeration of pause points: a row per point, naming the cells that
reach it and, in `same_as`, the shapes that also pass it before what sets them
apart is live (a record being written, a scratch taken); a shape that stops,
drifts, retires or aborts differently gets a cell at every point after that.
A source test fails when the source declares a point no row holds and when a
row names a point the source no longer declares; another runs every shape once
per phase with the points each rwv passes traced to a file
(`RWV_TEST_PAUSE_TRACE`) and fails when a shape passes a point with neither a
cell nor a `same_as` entry, or a cell is named for a point the shape does not
pass. A shape is a verb in one setup (history, a stop, a carried dependent, an
in-place or as-refs landing); the cells are generated from the rows, one test
per shape. A cell passes when `--continue` ends in the state an unkilled run
ends in and when abort, typed in the seat and, for a landing, in its target,
puts every place back where `Step::capture` saw it. The end state compares
each place's branch tips (by commit subject and depth, since ids differ
between sandboxes), each branch's tree listing (without `rwv.lock` and
`rwv.toml`, whose text carries the sandbox path), the backups under
`refs/rwv/backup` (a tip-named backup as `<tip>`), status, and the manifest and
lock text. Abort leaves the backups as captured but for the slots it keeps,
named per shape. Steps run with no standing allowance: the killed verb, its
continue and its abort are given only the pins they rewind and the declared
branches they restore or rewrite, per shape. A point a verb passes before it
has written its record has a rerun in place of a continue. A point it passes
once its record is gone (`aligning-source`) has neither: the cell checks that
`rwv status` shows a member the verb left without a checkout as missing and
that `rwv doctor --all` names `rwv doctor --fix`, then compares the end state.
Two ends are by decision, the single-repo parity of a kill in a verb's last
instant: a landing with `--retire` killed at `ending` or `records-write` leaves
savepoints that `rwv doctor` reports as `orphaned-savepoint` and `rwv doctor
--fix` clears, keeping the seat's pre-rebase tips as backups; and one killed at
`records-write` also leaves the record of the branch it deleted, which the next
run of the deletion rule drops. Every cell passes on the tree.

### What a step declares

A step's `Allowance` is the scenario's statement of what it expects the step
to do, read only by the invariants:

- `may_rewind`, `may_strand`, `may_rewrite`, `may_restore` and
  `may_leave_unresolved` name the repos in which the step may move a pin
  backwards, leave a commit no ref reaches, move a declared branch to or with
  its upstream, put a declared branch back to a commit its tip contains (as
  `rwv abort` restores what its operation moved), or leave a pin at a commit
  no ref of its backing repository reaches (a lock published ahead of its
  member, or a clone of one). Each is the seam for an operation the step
  names, never a blanket pass: a check that is spared still runs for every
  repo the step does not name.
- Its outcome is an `Exit`. `Exit::Succeeds`; `Exit::Refuses`, rwv's
  refusal, which holds only for status 1, the `<repo>: <axis> <state>` line
  of the state named (or, for a refusal that names a condition rather than
  a repo state, its kind), and that axis's or kind's route among the
  `rwv explain` lines that end the output; `Exit::Code`, an exact status,
  for anything else whose status is the thing under test (rwv's status 1 is
  never one: it is a refusal and names what refused); and
  `Exit::Undecided`, a step whose
  outcome the design has not decided yet. Only an exit expectation decides
  a step: claims alone do not. `assert_green` announces each undecided step
  it ran through as a skip; `assert_fully_decided_green` refuses one.
  `every_refusal_step_names_its_state_and_every_claim_has_an_exit` walks
  every constructor in `scenarios.rs`, a population read from the file
  itself, and fails on an rwv step expecting status 1 as a code and on a
  step with claims and no exit; a new constructor goes in that test's list,
  which `every_scenario_constructor_is_walked` holds to the file.
- Its claims are facts about the state afterward: marks reachable from a
  ref, or not (`Lacks`), on a remote it names (`RemoteContains`, any remote, a fork or a
  mirror included), a pin containing a mark, a workweave gone, a line
  judging a repo in a state (`Judged`), the step's output holding a text,
  or one of its lines being exactly a text (`OutputLine`, for a listing,
  where a substring would match its neighbors), a checkout on a branch, a
  branch's upstream on a remote (`UpstreamOn`), the project a weave root
  presents (`Presents`), a machine's entry on the records branch
  (`RecordIs`), and `LocalStateUnchanged`, which compares every checkout at
  a place — HEAD, branches, uncommitted changes and the project's lock —
  before and after the step.
- A step that needs a verb rwv does not have is `Step::pending`, naming the
  `MissingVerb`; the scenario stops there and its test asserts that it
  does.
- Its remote writes say which refs of which remotes the step may change, and
  how: a push creates a branch or moves it forward, an overwrite may move it
  anywhere, a delete removes it, and the forge's sync of a mirror may change
  any of its refs. A forge step declares its own; every other step that writes
  a remote must say so, or the write is reported.

### Adding an instrument

One instrument is one `Violation` variant, its `Invariant`, and one function
that `check` calls, all in `invariants.rs`, plus a paragraph in that module's
comment. Whatever it needs to read goes into `Snapshot`, so every check reads
the same state. It lands with a seeded-failure test in
`tests/scenario/scenario_harness_test.rs` that drives the violation through real steps
and asserts on the violation itself, not only on a red report; where the
instrument derives the set of refs it watches, add a test showing it watches
nothing it should not.

### Holding rwv at a pause point

A race inside one verb cannot be driven by whole-process steps. For those,
`src/test_pause.rs` provides named points: `test_pause::point(name)` compiles
to nothing unless the crate is built with the `test-pause` cargo feature, and
waits only when `RWV_TEST_PAUSE` is `<point>@<dir>` naming that point. The
first pass writes `<dir>/paused` and waits until `<dir>/release` exists; a
value of any other shape stops the process. A point sits immediately before a
compare-and-swap, so what the test changes while the process waits is what
the compare then reads. `RWV_TEST_PAUSE_TRACE=<file>` appends the name of every
point a process passes to `<file>`, which is how a test learns what a verb
reaches without holding it.

The driving side is `tests/common/pause.rs`, and in the harness it is a pair
of steps: one spawns rwv armed at a point and ends once it is held, and one
releases it and takes its exit as the step's outcome. Between them the held
process is blocked, so every step's invariants read a state nothing is
changing. Every rwv a test spawns has the arming variable removed, so only
that step arms one.

The feature reaches the binary because the `rwv` an integration test spawns
is the one cargo builds for that `cargo test` invocation:
`scripts/ci-local.sh` passes `--features test-pause` to both test stages, and
CI and the pre-push hook run that script. Two cargo invocations with
different features in one target directory relink the same
`target/<profile>/rwv` under each other, so run one at a time. A test that
holds rwv is written through the `holding_rwv!` macro, which compiles its body
in every build, reads the feature with `cfg!`, and announces a skip in a
build without it.

`debug_assertions` would not do: the gate runs the suite in release. That a
build without the feature carries no pause is held by
`tests/test_pause_build/test_pause_build_test.rs`: the gate's `no-test-pause` stage builds it
with no feature flags, as releases build, and asserts the spawned binary
holds no byte of the arming variable's name, whatever the feature defaults
are. In the test stages the same file asserts the binary does hold it, so
the scan is shown able to see it.
