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
on-disk shape of a workweave, read as a license to manipulate that shape
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
- [state](./state.md) — every piece of state rwv keeps: files, refs, trailers,
  and what it leaves in the VCS's own structures, with each one's owner,
  lifetime and what a kill leaves.
- [op-state](./op-state.md) — field-level schemas of the `.rwv-op` owner record
  and the `.rwv-op-lease` pointer, for someone changing sync's resume logic.
  Consumers call back into `rwv` instead of parsing them.
- [sync-phase-machine](./sync-phase-machine.md) — the per-phase contract
  behind `rwv sync` and `rwv land`: what each phase persists, its re-entry
  rule, abort's slots, the `rwv.toml` stops, and the git realization of each
  mechanism.
- [concurrency](./concurrency.md) — the budget for judging interference
  concerns: git's correctness effort as goal and ceiling, the two tiers of
  state (including the claim that serializes fetches into one object store),
  and the attestation invariant.
- [lock-freshness-surfaces](./lock-freshness-surfaces.md) — which verbs write
  `rwv.lock` and which only read it, the three sites that compare a tip
  against it, the question each one answers, and why `rwv doctor --locked` is
  not a renderer over the violation pipeline.
- [repin-record](./repin-record.md) — the `Rwv-Repin` annotation a by-name
  lock commit carries, the one function that writes it, and how `pin`
  `waived` reads it.
- [branch-model](./branch-model.md) — what a branch is for, the MOVE / ATTACH /
  DESTROY / DESTROY-STORE classification of every ref write, the deletion rule,
  backups and their marks, and the type split (`AttachedRef`, `DestroyProof`,
  `BackupWitness`, consent tokens) that makes ref discipline hold by
  construction.
- [records-branch](./records-branch.md) — how each weave branch's mode and
  landing record is stored on the records branch, folded across remotes,
  written by compare-and-swap, dropped and published.
- [alignment-engine](./alignment-engine.md) — the one engine that decides and
  applies where each member of a workspace sits, and the workspace state it
  keeps.
- [landing](./landing.md) — the history setting, the lineage rule, what a
  landing plans, how it is composed before any ref moves, the landing claim,
  and stacks.
- [store-removal](./store-removal.md) — the judgment in front of every removal
  of a checkout that removes an object store: fetch first, refuse what no remote
  has, `--discard-unpublished`.
- [vcs-backend-contract](./vcs-backend-contract.md) — what the `Vcs` seam
  requires of a backend, by capability, and what core never does.
- [casefold-filesystem](./casefold-filesystem.md) — building a loopback ext4
  image with the `casefold` feature to measure the filesystem-identity match
  in workweave name resolution for real, and what each of the four affected
  call sites needs from it.
- [path-url-mapping](./path-url-mapping.md) — `placement` as the sole
  producer of a derived `RepoPath`, the `rwv member add --new` creation surface
  (`Registry::creation_params`/`plan_creation`, the creation-address
  grammar, two-sided `local` creation), and the `placement-disagreement`
  doctor finding's role-based exemptions.
- [member-population](./member-population.md) — how a member's tracked files
  reach a workspace: the one member shape, the `WorkspacePopulation`
  questions, reflink and hardlink placement with byte-exact verification,
  settle and read-only `reference` members, the per-platform hardlink
  declaration, role changes, and the migration from symlinked members.
- [git-floor](./git-floor.md) — the oldest git rwv runs on, why it is that
  release, the one check that refuses below it, the commands newer than it
  and the capability detection that holds them, and the git-floor stage that
  runs the suite at it.

## Where else to look

- [`CLAUDE.md`](../../CLAUDE.md) — source comment discipline, citation rules,
  document surface map, and operator prose register.
- [`ARCHITECTURE.md`](../../ARCHITECTURE.md) — module map, process model, the
  sync phase machine, the trait seams, and the plugin dispatch path.
- [`../explanation/joints/`](../explanation/joints/) — the normative contracts.
  These are published, and they are where a statement belongs if a user or a
  plugin author can observe it.
