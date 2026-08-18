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

**Size is not a reason to move.** `src/sync.rs` carries some 1,950 lines of
tests inside a 9,100-line file and they stay there: 45 of its 58 tests name
an item the module does not export — 19 module-level items and three
associated functions, each the subject of the test that names it. Relocating
them means exporting `prune_dropped_repo`, `check_store_unclaimed`,
`materialize_missing_repo`, `ff_advance_repo`, `apply_strategy` and the rest
so that a test can watch. This crate spends visibility deliberately —
`src/main.rs` is a logic-free shim exactly so a consent-token constructor can
stay `pub(in crate::cli)` — and a test is not what it is spent on.

**The integration-shaped tests are the ones that cannot move.** Every test
in that module which builds a real git topology reaches a private subject;
the thirteen that compile from `tests/` are string and enum round-trips and
one rendered line. "Move the integration-shaped half out" inverts under
measurement — what it would leave inline is the unit-shaped half. Even those
thirteen are not a byte-for-byte relocation: a test written inside the crate
spells its own modules `crate::`, which no external target resolves.

### Moving a module out of its host file changes what the gates read

`before_test_module` in `src/bin/generate-explain.rs` defines test scope as
an inline `#[cfg(test)]` that opens a body. A `#[cfg(test)] mod tests;`
declaration is deliberately not a boundary, so the file it names is read as
production by every gate that calls it:

- `check_vcs_seam_bypasses` reddens. A test may spawn git and mint a
  backend; production may not. `src/sync.rs`'s module moved to
  `src/sync/tests.rs` reports 62 lines, measured — and the gate bails
  there, so the checks behind it go unmeasured.
- `src_code_identifiers` and `src_code_doc_filenames` widen *silently*,
  which is worse than reddening. They are the evidence and exemption sets
  `check_doc_symbol_refs` and `check_doc_citations` consult, and 112
  identifiers spelled only by that module's tests would begin counting as
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

## Behavioural pins and structural pins

A **behavioural pin** asserts an observable outcome through a supported
surface: the shipped binary, published wire bytes, a rendered message. A
**structural pin** asserts a property of the source itself: a scan, a
count, a call-site arity, a compile-fail probe.

Behavioural is the default. A structural pin is licensed in exactly two
situations, and the license is stated in the test's own doc comment:

- **No behavioural assertion can distinguish** correct from broken on any
  platform or fixture the suite reaches. Two shipped precedents: a spelling
  seam whose two spellings are byte-identical on the platform the suite
  runs on (`operator_path` vs `.display()` off Windows — dunce
  simplification is the identity there by construction), and a single-read
  chokepoint whose divergence window is too small to drive
  (`tests/checkout_classification_single_read_test.rs`).
- **The property is a prohibition over an enumerable population** — a seam
  census, a destructive-call inventory
  (`tests/destructive_ops_audit_test.rs`).

Expense is not a license: a behavioural drive that would merely be slow is
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
- **Suppression scope equals naming scope.** If findings are reported
  per-file, exemptions are per-file too. A global exemption fed by a local
  mention widens the blind spot beyond the file that earned it.
- **Justification prose follows the same rule.** An allowlist entry whose
  justification names a type gate (a receipt parameter, a sealed
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
- **Check which assertion fired.** An older assertion may catch the
  mutation first, leaving the new pin unexercised behind a red that looks
  like proof.

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
labour can have a hole in the middle that each half's own stated remit
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
- Per-suite numbers require self-labelling output — invoke one suite at a
  time, or pair each result block with the header naming its suite.
  Positional reading of multi-unit output fabricates attributions.
- Prove the gate cannot print green on failure: a terminal "all passed"
  line is stage-relative; the evidence is the enumeration of stages run,
  not the sentence.
- A check that never fires may be blocked upstream — assert the enumeration
  reaches your predicate, not only that the predicate is correct.
- Judge a gate from its output, never from a piped exit code.
