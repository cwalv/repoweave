# Refusal kinds

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly is not supported.

The maintainer-facing half of the refusal register. The operator-facing half is
[Refusals](../reference/refusals.md), which has one entry per token; this page
is the rule for deciding whether a refusal gets a token at all, where the token
is allowed to come from, and what the funnel prints.

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
informative. A new `bail!` that declines on purpose and carries no kind is a
defect even when its message is good.

## One producer

**The token string is minted in exactly one place**: `rename_all = "kebab-case"`
on `RefusalKind`. Nothing else spells a token — no `match` arm returning a
literal, no table, no doc heading typed by hand. A second spelling is a thing to
keep in sync rather than a convenience, and the doctor kind's three-place
minting is the worked example of what that costs.

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
them.

```
Error: <headline>
[
Caused by:
    <chain>
]

rwv explain <token>
```

- **The route line is bare.** Exactly `rwv explain <token>`, no prose around
  it. Constancy is what makes it skippable: a reader learns to stop at a fixed
  last line in one exposure, and prose would take that back.
- **Exactly one route line per error**, naming the outermost kind in the chain.
  A wrapped refusal routes to the condition that fired, not to the wrapper.
- **No kind in the chain ⇒ no route line**, and the output is byte-identical to
  what the runtime's own reporter produced before the funnel existed. Errors
  outside the class gain nothing.
- **Exit codes are unchanged.** A refusal exits 1 with a capitalised `Error:`;
  argv rejection exits 2 with a lowercase `error:`. That case split is the only
  class marker, and it is now deliberate rather than incidental.

## Adding a token

1. Add the variant. The token is its kebab-case name; do not spell it anywhere
   else.
2. Attach it at the site, or in the shared helper if the condition has several
   sites.
3. Write its entry. A refusal-only token gets a `###` section on
   [Refusals](../reference/refusals.md); a shared one already has an entry on
   [Doctor findings](../reference/doctor-findings.md) and must not get a second.
4. `tests/refusal_entry_test.rs` checks both directions — a token with no entry,
   and an entry naming no token — and derives both populations rather than
   listing them.

An entry is a junction, not a gloss: the condition, why the rule exists, the
exits **with the circumstances that select between them**, and where the rule
sits. A dead-end entry is the forbidden shape; a short one is fine.
