# Destructive operations

Every rwv call that can destroy user work — git history rewrites,
working-tree clobbers, branch force-deletes, file or directory deletion
— is held to one policy. This page states the policy for contributors
and operators; the enforcement mechanism is in
`tests/destructive_ops_audit_test.rs`, which inventories every call
site and fails the build when one is added without an audit.

## The policy

Three rules, in order of priority:

### 1. Satisfy the precondition or stop

Every destructive call sits behind a **named precondition that refuses**.
The precondition is checked, and if it fails, the operation refuses
loudly — no silent fallback, no degraded mode, no "best effort". The
operator either resolves the condition themselves or invokes the
operation again with `--force` (rule 2).

The precondition is *named* — present in the source as a function or a
clearly-labelled inline check — so a reviewer auditing the call site can
verify the gate matches the destruction it guards. Anonymous guards
("if not dirty …") drift; named ones survive refactors.

Examples in the current tree:

- `rwv workweave delete` checks for uncommitted changes and unmerged
  commits before removing the workweave directory. Both refusals fire
  with actionable error messages.
- `rwv remove --delete` refuses to delete the canonical store while
  another project's manifest still references the repo.
- `rwv sync --discard-local-commits` Phase 1' discards commits only after the
  clean-project precondition has passed; the discarded commits remain
  recoverable via the `refs/rwv/pre-op/<op-id>` savepoint.

### 2. Named overrides are narrow and informed

When the operator invokes a destructive operation with a named override
flag, they must have been shown — or have had the chance to be shown —
what would be lost. "Informed" means:

- The output of the refusing path lists the specific lost-work items
  (which commits, which files, which dirty paths). The operator running
  the operation once without the override, reading the refusal, and
  re-running with the override has seen the loss list.
- The override flag is documented at the verb's CLI surface with a short
  one-line description of what consent is being granted. Hidden or
  undocumented override flags are not informed consent.
- Adding a new override-gated destructive path requires extending the
  refusal output to enumerate the loss, not just signalling that
  something is at risk.

**The house rule:** a flag's name states what it destroys — consent to
a consequence, never a category. `--discard-local-commits` names the
exact loss; the operator reading it knows what they are signing.

Each named override is narrow — it bypasses one named precondition at a
time. It does not turn into "disable all checks" — each precondition
that an override bypasses is enumerated at the call site.

### 3. Discards stay recoverable

Discard-by-design operations — `rwv abort`, `rwv sync --discard-local-commits`,
ref surgery during phase replay — must keep what they discard
recoverable where possible, and the operation's documentation must
say where.

The current mechanism is **named savepoints**: ref surgery operations
write a `refs/rwv/pre-op/<op-id>` ref pointing at the pre-operation
state before any destructive write. The ref namespace is rwv-internal
(no user ref ever lives under `refs/rwv/`); the ref is tied to a
specific operation id; and `rwv abort` knows how to roll back
to it.

Operations that *cannot* leave a recoverable artifact — e.g., disk
removal of a fully unreferenced workweave directory after every
precondition has confirmed the work is merged — say so explicitly in
the verb's documentation. The bar for "cannot" is high: a missing
savepoint is a defect, not a tradeoff.

## How the policy is enforced

`tests/destructive_ops_audit_test.rs` scans `src/` for a fixed list of
destructive patterns:

- `"--hard"` (git reset)
- `remove_dir_all` / `remove_file` (filesystem)
- `"-D"` (git branch force-delete)
- `"worktree", "remove"` (git worktree)
- `push("--force")` (git push)
- `"checkout"` (git checkout — may overwrite when forced)
- `"update-ref"` (ref surgery)

Each hit must appear in the test's `ALLOWLIST` with an exact count and
a justification that names which precondition (rule 1) protects it,
which `--force` flag bypasses it (rule 2), and how discards stay
recoverable (rule 3). The test fails the build when:

- A new call site appears without an allowlist entry.
- An existing site moves or duplicates without the count being
  updated.
- A forbidden pattern shows up (`"clean"`, `"stash"`, `"filter-branch"`,
  `"checkout", "-f"`, `"reflog"` — destruction vectors rwv has no
  audited use for).

The test header in `tests/destructive_ops_audit_test.rs` keeps the
operational summary — what the patterns are, what the allowlist
contract is, and how to update it when a call site moves. This
contributing page owns the *policy*; the test header owns the
*enforcement mechanics*.

## Adding a new destructive call site

When a PR introduces a destructive call:

1. **Name the precondition.** Put the refusal check in a function
   whose name describes the condition (`refuse_if_dirty`,
   `refuse_if_unmerged_commits`). Inline checks are acceptable when
   the condition is one line; multi-line checks belong in a function.
2. **Wire `--force` consciously.** If `--force` is allowed to bypass
   the precondition, the refusal output must list what would be lost,
   and the verb's clap-derive doc on `--force` must describe what
   consent is being granted.
3. **Add a savepoint if a roll-back is meaningful.** For ref surgery
   and `git reset --hard` paths, write a `refs/rwv/pre-op/<op-id>`
   ref before the destructive write. Wire the abort path to consume
   it.
4. **Update the allowlist.** Add an entry to `ALLOWLIST` in
   `tests/destructive_ops_audit_test.rs` with the file name, pattern,
   count, and a one-paragraph justification that names the
   precondition (rule 1), the `--force` behavior (rule 2), and the
   recovery path (rule 3).
5. **Update the verb's reference doc.** The verb's
   `docs/reference/explain/<verb>.md` should mention the destructive
   path and what protects it — the operator-facing half of the
   contract.

The reviewer's checklist mirrors the rules:

- Is there a precondition that *refuses*, named in source?
- Does the refusal enumerate what would be lost?
- If `--force` bypasses the precondition, is the loss-list shown
  unconditionally before the bypass?
- Is a savepoint written if the operation can be aborted?
- Is the allowlist entry concrete enough that a future contributor
  reading it can audit the site without re-deriving the policy?

## Why this lives in `contributing/`

The policy is two things at once:

- An **operator promise**: rwv won't silently destroy your work. Every
  destructive path is gated; every override is informed; every
  discard is recoverable where the mechanism allows it.
- A **contributor rule**: adding a destructive call site without
  satisfying the three rules above fails the build, and the
  enforcement is intentionally noisy at the commit that introduces
  the site.

The two halves cannot drift. Stating the policy in `contributing/` and
having the test header point here keeps one source of truth for both
audiences. The test header still carries the enforcement mechanics
(the pattern list, the scan algorithm, the allowlist contract) — the
shape of the verifier — while this page carries the *why* and the
*shape of new contributions*.

## Related

- `tests/destructive_ops_audit_test.rs` — the enforcement mechanism
  and the audited allowlist of current call sites.
- [shared-refs-drift](../explanation/joints/shared-refs-drift.md) —
  the safe-class / live-class classifier that gates several of the
  preconditions above.
- [sync-semantics](../explanation/joints/sync-semantics.md) — the
  phase model that the `refs/rwv/pre-op/<op-id>` savepoint protects.
