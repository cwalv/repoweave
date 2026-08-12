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
| `find_violations`' `StaleLock` arm (`src/check.rs`) | Do the conventions hold? | the **resolved** lock | `CheckViolation`, rendered as text or as a `kind` on the wire |
| `classify_lock_relations` (`src/sync.rs`) → `compute_relation` (`src/status.rs`) | Which way, and by how much? | the manifest, syncable checkouts only | `LockRelation` (`ok`/`ahead`/`behind`/`diverged`/…), shared with `rwv status` |

The differences that matter are the walks, not the comparisons. Every one of
the three compares two `ResolvedRevisionId`s with `==`; there is no algorithm to
share. What each surface *enumerates* is where they part, and it is the
enumeration that decides what each can and cannot report.

## Why `--locked` is not a renderer over `StaleLock`

`--locked` is **lock-total**: it iterates the raw lock, so every entry gets a
verdict. The pipeline is **manifest-total** and consumes the resolved lock,
which `LockFile::resolve_versions` builds by dropping any entry whose repo
directory is absent from disk. `find_violations` is pure by construction — the
filesystem reads happen before it — so it cannot recover a dropped entry.

That makes one condition unreachable through `StaleLock` rather than merely
spelled differently: **a lock entry whose repo is not on disk.** `--locked`
reports it and names `rwv sync`. The pipeline reaches `dangling-reference` for
it only from the manifest side, and only when the role is not `reference` —
a reference clone is allowed to be absent, and that exemption is deliberate.

So routing `--locked` through the pipeline would silently narrow a documented,
scripted gate: a `reference`-role repo absent from disk would stop failing
`rwv doctor --locked`. Recovering the coverage means giving the pipeline a
diagnosis it does not have today, which is a new `kind` on the committed
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
renderer that ran `load_doctor_world` and the `StaleLock` arm and skipped every
scan is 3–8×, because the world load reads HEAD for every repo on disk while
`--locked` reads only the repos the lock names. The "cheaper precondition"
claim the reference pages make is true, and this is its size.

## What keeps them from drifting

`tests/lock_totality_agreement_test.rs`. Two properties, deliberately split:

- Where both surfaces enumerate the same entry, they must report the same two
  revisions in the same spelling. The fixture pins a **tag**, because
  `ResolvedRevisionId` carries a canonical form and a display form that
  coincide under a SHA-form lock — which is the only form the older per-surface
  tests use, so they cannot see a surface that switches to the other form.
- On a fixture with a `reference`-role lock entry absent from disk, the whole
  violation set for that repo is pinned.

## Residue

- The second test pins `incomplete-lock` as the pipeline's only finding for an
  absent-on-disk lock entry. That finding is **wrong**: its remedy is to add a
  lock entry the fixture's `rwv.lock` already carries. It is pinned as measured,
  not as intended. Narrowing the coverage check to the raw lock fixes it and
  reddens that assertion; the pin is what should change.
- `--locked` fails on an absent `reference`-role repo. `rwv sync`'s own gate
  skips reference aliases and absent repos entirely, so `--locked` refuses
  where `rwv sync` proceeds — which the reference pages' "the precondition
  `rwv sync` enforces" phrasing does not admit. Unmeasured beyond reading the
  two enumerations.
- Nothing pins the third site against the other two. `compute_relation` needs
  per-repo ancestry queries, so `find_violations` cannot call it without giving
  up its purity, and no test compares a `LockRelation` against a `StaleLock`.
- `run_check_locked` and `status`' `project_names_for_ctx` hold the same
  three-arm project-scope match. That duplication is real and is not what this
  file is about.
