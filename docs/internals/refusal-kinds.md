# Refusal kinds

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly is not supported.

The maintainer-facing half of the refusal register. The operator-facing half is
[Refusals](../reference/refusals.md), which has one entry per kind and per
token; this page is the rule for deciding whether a refusal gets a token at
all, where the token is allowed to come from, and what the funnel prints.

A refusal has one of two shapes. One that judges repo states is a
**report**: one line per repo and axis, each axis a **kind**, from the
state-to-choices table in `src/diagnosis.rs`. One that names a condition no
axis carries is a **token**, a `RefusalKind`. Both are refusals in the class
below, and both route through `rwv explain`.

## What carries a token

**rwv declined on purpose ⇒ token and entry.** The class is the whole test, and
it is deliberately not a per-refusal judgment about which ones deserve one:
absence has to mean something, so a refusal is not left untokened because it
felt minor.

In the class:

- **Precondition refusals** — rwv could have acted and declined.
- **Input validators**, including typed name and manifest errors that reach the
  terminal through `?` rather than a `bail!`.

Out of the class:

- **Environment and IO passthroughs.** A VCS or filesystem call failed. rwv did
  not decline; it was stopped.
- **Internal invariants.** The `internal:`-prefixed family. A reader who sees
  one has found a bug, and there is no operator exit to document.
- **Pre-dispatch argv shims and clap.** These reject an argument before any verb
  runs, and they exit 2 with a lowercase `error:` — a different class with a
  different exit code, not a refusal.
- **Pure failure tallies**, where nothing was withheld. Where an artifact *was*
  deliberately not written, the decline is real and carries a token.

Totality within the class is what makes the absence of a route line
informative. A refusal that declines on purpose and carries no kind is a
defect even when its message is good.

**The class is written at the site.** An error rwv mints outside the refusal
constructors goes through `crate::untokened!(<Class>, …)`, which renders
exactly as `anyhow!` does and names why it has no token: an `Untokened`
variant in `src/refusal.rs`, one per bullet above (integration failures, which
an operator reads as that integration's report, and malformed hook input from
the harness that invoked it, are classes of their own). `Owed` is the one
inside the class: rwv declined and the refusal has no token yet, and giving it
one is the fix. `tests/source/refusal_census_test.rs` refuses a bare `bail!`,
`anyhow!`, `ensure!` or `Error::msg` in production `src/`, so no site arrives
without the judgment.

## The census

`docs/internals/refusal-census.tsv` lists every refusal site in `src/` — each
judged state, each token, each `untokened!` class, per file — and the test
that reads it: the first test, by path, that asserts that site's own state or
token through `common::cli::refused` (or a token through a route assertion).
A judged state is read only through its own state; a test that reaches its
axis reads nothing. A `-` is a site whose wrong-but-valid token nothing would
notice. `cargo run --example generate-explain` writes it and the gate's drift
stage compares it, so a change that adds a site or a read regenerates it, and
the diff shows what moved. What counts as a site and as a read, and what
neither sees, is stated in `tests/common/refusal_census.rs`.

## One producer

**A token string is minted in exactly one place**: `rename_all = "kebab-case"`
on `RefusalKind`, and a kind's on `Axis`. Nothing else spells one — no `match`
arm returning a literal, no doc heading typed by hand. A second spelling is a
thing to keep in sync rather than a convenience, and the doctor kind's
three-place minting is the worked example of what that costs.

**A kind's states, facts and choices live in one table**, `src/diagnosis.rs`:
`Axis::row` for a kind's prose, `State::row` for each state's name, meaning and
the forms it offers, `ChoiceId::row` and `Form::row` for every choice and its
spellings, each with its Where. The terminal text, every `--json` record that
carries a judged repo, and the kind's entry on the refusals page are rendered
from it; the page is generated, so it cannot say something the refusal does not
print. A VCS action's spelling is the backend's (`Vcs::hint`); core fills its
placeholders from what the site judged.

**A kind that `rwv doctor` also reports is an axis whose `Axis::finding` names
that finding.** Its states, facts and choices live in the table like any
axis's, and the finding's facts — its sub-kinds, each with the severity and
`--fix` disposition the doctor register gives it — are read from the sub-kind
type (`UnparseableProjectKind::severity` and `fix_disposition`, which
`CheckViolation::severity` and `fix_disposition` delegate to), so the entry
never states either a second time. The kind has one entry, and it is on
[Doctor findings](../reference/doctor-findings.md), because that page is where
every condition doctor reports keeps its entry and where the `--fix`
dispositions are held to the register. The generator writes it there, in
place, from the heading ``### `<kind>` `` to the next heading of level three or
above; the heading is a stub the page's author places, once, in the topical
section it belongs to, and the Summary index rows for the kind stay
hand-written and are compared to the register as any finding's are. The
refusals page lists the kind with a link and holds no entry for it.
`explain::lookup` does not depend on which page carries an entry, so `rwv
explain <kind>` and `rwv explain <kind>/<sub-kind>` resolve it either way.

**A typed error can be a state of the table.** `PreSchemaManifest` reaches the
funnel through `?`; `diagnosis::report_of` reads it as the judged line
`unparseable-project here` for the project it was read in, named by the weave
root that `WorkspaceContext::resolve_unbound` finds from the manifest's
directory. It stays a typed error for `rwv doctor`, which classifies a finding
by it, and has no `RefusalKind`: a token naming a kind the table carries would
be a second way to refuse it.

**A choice prints only once its verb ships.** Every choice is data, shipped or
not; a form whose `shipped` is false, or that runs after one that is, prints on
no surface. Shipping a verb is flipping `shipped` on the forms that spell it;
a form's spelling is the CLI's as it stands, so renaming a verb renames its
forms. `tests/docs/diagnosis_table_test.rs` parses every line a state prints, and
every shipped form, through the clap tree, refusing the plugin fallback that
would otherwise accept any verb.

The kind rides the error value, not its text:

- Attach at the site with `refuse!`, or inside the shared helper when several
  sites are one condition. A helper that serves several conditions returns the
  kind rather than a string, so the caller does not re-derive it.
- Recover by walking the anyhow chain, outermost kind first. A `.context()`
  wrap sits above the tag rather than replacing it, so a refusal wrapped by its
  caller still answers with the condition that fired.
- A decorator that adds text to another refusal takes the **error**, not its
  rendered string. Flattening with `format!("{e:#}")` discards the kind before
  the outer error can carry it.

A condition that a `rwv doctor` finding or a `VcsError` already names **shares
that token**. One condition has one name everywhere it appears, and one entry
wherever that entry already lives. The two spellings move together or not at
all; `a_shared_condition_spells_one_token_in_both_registers` in `src/refusal.rs`
reads both out of the code that owns them rather than asserting a literal.

## What the funnel prints

One funnel, at the `main` boundary. Verbs return errors; they do not print
them. A token refusal prints:

```
Error: <headline>
[
Caused by:
    <chain>
]

rwv explain <token>
```

A report prints, any context wrapped above it first:

```
rwv <verb>: <repo>: <kind> <state>[ (<subject>)]
    <fact or listed item>
[<note>]
choices:
  <phrase>[, in <directory>]
    <command>

rwv explain <kind>
[rwv explain <kind>]
```

- **Route lines, one per kind.** A report prints one bare `rwv explain <kind>`
  per distinct kind it judged, in the order their first lines print, and
  nothing after them. A token refusal prints exactly one.
- **Lines, then choices.** A repo's lines print in axis order, repos in the
  order they were judged. Each choice prints once per phrase and directory,
  the commands every judged line offered beneath it.
- **Every judged state before any change.** A verb judges every repo its
  preflight covers, on every axis that preflight reads, before refusing, so
  one run names every state it found. A token refusal still stops at its own
  check, in the order the verb runs them.
- **`--json`.** A verb's envelope carries the judged repos in `refused`; a
  verb that refused before printing anything gets the refusal envelope
  (`docs/reference/schemas/refusal.json`) from the funnel, which knows nothing
  was printed because every stdout write goes through `crate::stdout`.

- **The route line is bare.** Exactly `rwv explain <token>`, no prose around
  it. Constancy is what makes it skippable: a reader learns to stop at a fixed
  run of last lines in one exposure, and prose would take that back.
- **One route line per token refusal**, naming the outermost kind in the chain.
  A wrapped refusal routes to the condition that fired, not to the wrapper.
- **No kind in the chain ⇒ no route line**, and the output is byte-identical to
  what the runtime's own reporter produced before the funnel existed. Errors
  outside the class gain nothing.
- **Exit codes.** A refusal exits 1, with a capitalized `Error:` or with its
  `rwv <verb>:` lines; argv rejection exits 2 with a lowercase `error:`.

## Adding a token

1. Add the variant. The token is its kebab-case name; do not spell it anywhere
   else.
2. Attach it at the site, or in the shared helper if the condition has several
   sites.
3. Write its entry. A refusal-only token gets a `###` section under Tokens in
   `docs/reference/templates/refusals.md.tmpl`, from which `cargo run --example
   generate-explain` renders [Refusals](../reference/refusals.md); a shared one
   already has an entry on [Doctor findings](../reference/doctor-findings.md)
   and must not get a second.
4. `tests/docs/refusal_entry_test.rs` checks both directions — a token with no entry,
   and an entry naming no token — and derives both populations rather than
   listing them.

## Adding a state

1. Add the `State` variant, its arm in `State::row` and its place in
   `State::ALL`; a new axis adds an `Axis` variant and its `Axis::row` arm
   too, in the order its lines should print. An axis that is also a doctor
   finding returns its `Finding` from `Axis::finding` and puts the heading
   stub of its entry on the findings page; `Axis::finding` matches every axis,
   so an axis added without a decision does not compile.
2. Offer the forms the state prints, in order. A choice whose verb has not
   shipped is offered all the same, marked unshipped on its form; it prints
   once its verb ships and the mark is flipped.
   A state may offer none, where the design's choice table names none for it
   (`unparseable-project` `malformed`); then no `choices:` block prints and the
   route line still does. The test over `State::ALL` lists such states, so one
   is a decision and not an omission.
3. Judge it at each site — `State::…of(repo)`, its listing and bindings,
   `.judge(vcs)` — into the verb's `Report`, and refuse once every repo the
   check covers has been judged.
4. Regenerate. The kind's entry — on the refusals page, or on the findings page
   for a kind doctor also reports — is the table's output;
   `tests/docs/refusal_entry_test.rs` holds the page to it, and the gate's drift
   stage to what was committed.

An entry is a junction, not a gloss: the condition, why the rule exists, the
exits **with the circumstances that select between them**, and where the rule
sits. A dead-end entry is the forbidden shape; a short one is fine.
