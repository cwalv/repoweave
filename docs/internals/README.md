# Internals

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

Maintainer-facing material: the conventions rwv's own source is written to, and
the contracts that only bind someone editing `src/`.

**This directory is deliberately absent from [`../SUMMARY.md`](../SUMMARY.md).**
It is not part of the published book. Anyone who needs it has the checkout, and
keeping it unpublished is what lets the rest of `docs/` stay uniformly
user-facing. The rigor signal that would otherwise justify publishing internals
is carried by [`ARCHITECTURE.md`](../../ARCHITECTURE.md), which renders on the
repository's landing page.

## The banner, and why every file here carries it

The banner above opens every file in this directory, verbatim.

It is here because precise documentation of internals reads as permission to use
them. Moving a document out of the published book narrows its *audience*; it
does nothing about a reader who greps the checkout, which is how most of this
material will actually be found. Location is filing, not a boundary. The banner
is the boundary — it travels with the text to wherever it is read from.

The failure it exists to prevent is specific: an accurate description of the
on-disk shape of a workweave, read as a licence to manipulate that shape
directly, followed by a file-level operation that leaves rwv's recorded state
disagreeing with the tree. `rwv doctor`'s tree-integrity checks are the backstop
that catches that class of damage; the banner is the part that tries to make the
backstop unnecessary.

## Contents

- [writing-integrations](./writing-integrations.md) — implementing an
  `Integration` in `src/integrations/`: the trait, the context it receives, and
  registration.
- [code-style](./code-style.md) — conventions for rwv source that clippy and
  rustfmt do not enforce.
- [testing](./testing.md) — what a test must evidence to enter the suite, when
  a structural pin is licensed, the mutation-evidence standard, and when
  deletion is the correct outcome.
- [op-state](./op-state.md) — field-level schemas of the `.rwv-op` owner record
  and the `.rwv-op-lease` pointer, for someone changing sync's resume logic.
  Consumers call back into `rwv` instead of parsing them.
- [concurrency](./concurrency.md) — the budget for judging interference
  concerns: git's correctness effort as goal and ceiling, the two tiers of
  state, and the attestation invariant.
- [lock-freshness-surfaces](./lock-freshness-surfaces.md) — the three sites that
  compare a tip against `rwv.lock`, the question each one answers, and why
  `rwv doctor --locked` is not a renderer over the violation pipeline.
- [branch-model](./branch-model.md) — the MOVE / ATTACH / DESTROY /
  DESTROY-STORE classification and the type split (`AttachedRef`,
  `OwnedRef`, warrants, consent tokens) that makes ref discipline hold by
  construction. Cited by section from `docs/reference/explain/doctor.md` and
  `docs/reference/schemas/doctor.json`, so its section numbering is
  load-bearing.
- [casefold-filesystem](./casefold-filesystem.md) — building a loopback ext4
  image with the `casefold` feature to measure the filesystem-identity match
  in workweave name resolution for real, and what each of the four affected
  call sites needs from it.

## Where else to look

- [`ARCHITECTURE.md`](../../ARCHITECTURE.md) — module map, process model, the
  sync phase machine, the trait seams, and the plugin dispatch path.
- [`../explanation/joints/`](../explanation/joints/) — the normative contracts.
  These are published, and they are where a statement belongs if a user or a
  plugin author can observe it.
