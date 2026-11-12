# Lock-freshness surfaces

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

Three places in `src/` compare a repo's tip against its `rwv.lock` entry. They
are not three implementations of one check, and this file is why — written
because the surface reading of the code says they are, and acting on that
reading would weaken the cheapest of them.

## The three, and the question each answers

| Site | Question | Walks | Vocabulary |
| --- | --- | --- | --- |
| `run_check_locked` (`src/check.rs`) | Does the lock describe this workspace? | the **raw** lock | `ok` / `tip ≠ lock` / `missing on disk` / `lock pins unknown revision`, plus an exit code |
| `scan_pins` (`src/check.rs`) | Do the conventions hold? | the **resolved** lock, and the **raw** lock for coverage | `CheckViolation::Pin`, rendered as text or as the `pin` `kind` on the wire |
| `classify_lock_relations` (`src/sync.rs`) → `compute_relation` (`src/status.rs`) | Which way, and by how much? | the manifest, syncable checkouts only | `LockRelation` (`ok`/`ahead`/`behind`/`diverged`/…), shared with `rwv status` |

The differences that matter are the walks, not the comparisons. Every one of
the three compares two `ResolvedRevisionId`s with `==`; there is no algorithm to
share. What each surface *enumerates* is where they part, and it is the
enumeration that decides what each can and cannot report.

## What writes the lock

`rwv lock` (`lock` in `src/lock.rs`), `rwv update --remote`
(`run_update_remote` in `src/update_remote.rs`) and the relock phase of
`rwv sync` and `rwv land` (`run_relock` in `src/sync.rs`) write `rwv.lock`.
`rwv update --remote` writes it once its worker pool has joined and every repo
advanced; it and `rwv lock` both go through `write_project_lock`, so the
snapshot they take is the same function of the manifest and the checkouts.

Every other verb that takes a project in reads the lock and writes nothing.
`rwv clone` and `rwv update` (`bring_members` in `src/update.rs`) place each
member at the commit its entry pins. When `bring_members` creates a member the
lock has no entry for, or whose entry no ref in the clone reaches, it checks the
member out on its declared branch at the remote tip and records nothing;
`rwv clone` lists it as `pin` `none` or `pin` `unreachable` and exits with
status 3, and `rwv update` leaves a present clone with no entry where it is.
`rwv fetch` moves no checkout, and `rwv pull` writes no lock of its own: its
`rwv update` step reads the lock the fast-forwarded project branch carries. A
tip that differs from its entry after any of these is a state for `rwv doctor`
to report as a `pin` sub-kind, not one the verb repairs.

## Why `--locked` is not a renderer over `pin`

`--locked` is **lock-total**: it iterates the raw lock, so every entry gets a
verdict. The pipeline is **manifest-total**, and its freshness comparison
consumes the resolved lock, which `LockFile::resolve_versions` builds by
dropping any entry whose repo directory is absent from disk or whose revision
this clone cannot resolve. `scan_pins` reads what `load_doctor_world` read —
the resolved lock, each repo's HEAD and the entries that failed to resolve — so
it cannot recover an entry dropped for an absent repo.

Coverage is the one part of the pipeline that reads the **raw** lock, because
"does the lock name this repo" is a question about the file and not about what
resolved. Answered against the resolved lock, both of the states
`resolve_versions` drops read as no entry at all, and each would earn a
`pin` `none` telling the operator to write a line `rwv.lock` already has.

That makes one condition unreachable through `pin` rather than merely
spelled differently: **a lock entry whose repo is not on disk.** `--locked`
reports it and names `rwv sync`. The pipeline reaches `dangling-reference` for
it only from the manifest side, and only when the role is not `reference` —
a reference clone is allowed to be absent, and that exemption is deliberate.

So routing `--locked` through the pipeline would silently narrow a documented,
scripted gate: a `reference`-role repo absent from disk would stop failing
`rwv doctor --locked`. Recovering the coverage means giving the pipeline a
diagnosis it does not have today, which is a new `sub_kind` on the committed
`doctor --json` schema — a wider change than the duplication costs.

`--json` for `--locked` is a real gain and this decision forgoes it. It is
recoverable later without merging the two: the flag is currently
`conflicts_with`, and a wire renderer over `run_check_locked`'s own four
verdicts would serve it without the pipeline.

## What `--locked` costs, measured

Not asserted — measured, on a five-project weave with 80+ workweaves and 14
lock entries, release build, three runs:

| | wall clock |
| --- | --- |
| `rwv doctor --locked`, whole process | 0.07 s |
| `load_doctor_world` alone | 0.23 – 0.55 s |
| `collect_doctor_violations` | 2.95 – 3.75 s |
| `collect_doctor_issues` | 0.055 – 0.083 s |

Reading the table: the full pipeline is ~47× `--locked`. Even a hypothetical
renderer that ran `load_doctor_world` and `scan_pins` and skipped every
scan is 3–8×, because the world load reads HEAD for every repo on disk while
`--locked` reads only the repos the lock names. The "cheaper precondition"
claim the reference pages make is true, and this is its size.

## What keeps them from drifting

`tests/doctor/lock_totality_agreement_test.rs`. Two properties, deliberately split:

- Where both surfaces enumerate the same entry, they must report the same two
  revisions in the same spelling. The fixture pins a **tag**, because
  `ResolvedRevisionId` carries a canonical form and a display form that
  coincide under a SHA-form lock — which is the only form the older per-surface
  tests use, so they cannot see a surface that switches to the other form.
- On a fixture with a `reference`-role lock entry absent from disk, the whole
  violation set for that repo is pinned — empty. A second `reference`-role repo,
  absent from disk and absent from the lock, is pinned to `pin` `none` in
  the same fixture, so the emptiness distinguishes a lock that covers the repo
  from a coverage check that reports nothing at all.

## Residue

- The pipeline has nothing to say about an absent-on-disk `reference`-role lock
  entry, and that is the whole of what it says. `--locked` is the surface that
  covers the state; a pipeline finding for it would be a new `sub_kind` on the
  committed schema, which is the change the section above declines.
- `--locked` fails on an absent `reference`-role repo. `rwv sync`'s own gate
  skips reference aliases and absent repos entirely, so `--locked` refuses
  where `rwv sync` proceeds — which the reference pages' "the precondition
  `rwv sync` enforces" phrasing does not admit. Unmeasured beyond reading the
  two enumerations.
- Nothing pins the third site against the other two. `scan_pins` judges a
  checkout with `pins::standing`, which reads the live locks of every project,
  where `compute_relation` compares two revisions, so no test compares a
  `LockRelation` against a `pin` sub-kind.
- `run_check_locked` and `status`' `project_names_for_ctx` hold the same
  three-arm project-scope match. That duplication is real and is not what this
  file is about.
