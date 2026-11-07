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
operation again with the precondition's named override (rule 2).

The precondition is *named* — present in the source as a function or a
clearly-labeled inline check — so a reviewer auditing the call site can
verify the gate matches the destruction it guards. Anonymous guards
("if not dirty …") drift; named ones survive refactors.

Examples in the current tree:

- `rwv workweave delete` checks every checkout under the workweave, a
  manifest member or not, for uncommitted changes and for a detached HEAD
  holding commits no branch reaches before removing the workweave directory.
  Both refusals fire with actionable error messages. Its two override flags
  keep what they consent to losing as a backup first (rule 3). A branch of the
  workweave that has not landed is not deleted at all: the deletion rule keeps
  it, and `--discard-unlanded` is the override that backs it up and deletes it.
- `rwv sync-to --retire` applies the same checks and the same rule, with no
  override: its dirty check covers every checkout under the workweave, and a
  branch that has not landed survives.
- `rwv branch delete` deletes a branch only when everything on it has landed on
  the branch it lands into and no live lock pins a commit only it reaches;
  `--discard-unlanded` lifts the first of those after a backup, and nothing
  lifts the second.
- `rwv member remove --delete` refuses to delete a checkout with uncommitted
  changes or commits no other clone reaches, a backup holding such commits
  included, and refuses to delete the canonical store while another
  project's manifest still references the repo.
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
  refusal output to enumerate the loss, not just signaling that
  something is at risk.

**The house rule:** an override flag is named for the precondition it
waives, so the operator consents to a specific consequence rather than
a category. For a destructive override the consequence is a loss, so
the flag states what it destroys — `--discard-local-commits` names the
exact loss; the operator reading it knows what they are signing. That
is the destructive instance of the rule, not a separate rule: the same
naming discipline covers non-destructive gates too (`clone
--allow-non-empty-dir` names the bootstrap precondition it waives, not
a loss), with no exemption class needed to cover them.

Each named override is narrow — it bypasses one named precondition at a
time. It does not turn into "disable all checks" — each precondition
that an override bypasses is enumerated at the call site.

`abort --abandon-foreign-tip <repo>` shows how narrow "narrow" is meant
to be. It waives one precondition (the tip must be attributable to the
op) for one repo per occurrence, and has no all-repos form — whether a
particular repo's foreign commits are expendable is a judgment about
those commits, so a blanket spelling would answer it for repos the
operator never looked at. It is also the case where rule 3 does the
naming: the flag says *abandon*, not *discard*, because the tip is kept
as an `abandoned` backup before the branch moves off it. An override
buys a waiver of the precondition it names, never of rule 3.

`push --force` is the one exception to the naming rule: it is
not a precondition waiver but a **mode switch** — git's own vocabulary
for which of two operations `rwv push` is performing. Renaming it would
break the analogy to `git push --force` rather than sharpen it, so it
keeps git's name.

### 3. Discards stay recoverable

Discard-by-design operations — `rwv abort`, `rwv sync --discard-local-commits`,
`rwv workweave delete --discard-uncommitted` and `--discard-unmerged-commits`,
ref surgery during phase replay — must keep what they discard
recoverable where possible, and the operation's documentation must
say where. Two mechanisms do it, one per kind of discard.

**Named savepoints** cover what an operation itself moves. Ref surgery
operations write a `refs/rwv/pre-op/<op-id>` ref pointing at the
pre-operation state before any destructive write —
`refs/rwv/pre-op/<op-id>-target` in a `sync-to` target's repos, which
share a ref store with the source's. The ref is tied to a specific
operation id, and `rwv abort` knows how to roll back to it. A savepoint
lasts one operation: abort restores a repo only over tips the operation
itself made, so what it throws away is the operation's own output.

**Backups** cover a discard the operator consents to. A backup is a ref
an rwv verb makes before any step that would leave a commit or an
uncommitted edit unpreserved — reached by no branch, no remote-tracking
branch and no other backup. It lives under `refs/rwv/backup/<branch>/<id>`
(`<branch>` is `HEAD` for a detached HEAD), outside every namespace a
user ref lives in, and `rwv backup list` shows it with the commits only
it holds. Three discards make one first:

- `rwv workweave delete --discard-unmerged-commits` keeps each branch tip
  it deletes, and what a detached HEAD holds that no branch reaches;
- `rwv workweave delete --discard-uncommitted` keeps the edits of each
  checkout as a commit on its HEAD (untracked files included, ignored
  ones not), then the backup ref;
- `rwv abort --abandon-foreign-tip <repo>` keeps the foreign tip, marked
  `abandoned`.

The backup is written first and the delete or the reset runs only once it
exists: a backup that cannot be written stops the verb with nothing
changed.

A backup stays until it is dropped. `rwv backup drop <repo> [<name>...]`
drops the named backups of one repo, or all of them, and `rwv backup
prune` drops those a branch or remote-tracking branch reaches in full,
after a pruning fetch. Both obey the **pin rule**: nothing is dropped
that would leave a commit some project's live lock pins reached by no
branch, no remote-tracking branch and no backup — in any project of the
weave, not only the one the verb runs in. The refusal names the locks.

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
which named override bypasses it (rule 2), and how discards stay
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
2. **Wire the override consciously.** If a named override flag is
   allowed to bypass the precondition, the refusal output must list
   what would be lost, and the verb's clap-derive doc on that flag
   must describe what consent is being granted.
3. **Add a savepoint or a backup.** For ref surgery and `git reset
   --hard` paths an operation can be aborted out of, write a
   `refs/rwv/pre-op/<op-id>` ref before the destructive write and wire
   the abort path to consume it. For a discard the operator consents to,
   make the backup first through `Vcs::create_backup` and take the
   witness it returns: the proof that destroys a branch
   (`DestroyProof::backed_up`) runs the backup itself, so a discard
   without one does not build.
4. **Update the allowlist.** Add an entry to `ALLOWLIST` in
   `tests/destructive_ops_audit_test.rs` with the file name, pattern,
   count, and a one-paragraph justification that names the
   precondition (rule 1), the override behavior (rule 2), and the
   recovery path (rule 3).
5. **Update the verb's reference doc.** The verb's
   `docs/reference/explain/<verb>.md` should mention the destructive
   path and what protects it — the operator-facing half of the
   contract.

The reviewer's checklist mirrors the rules:

- Is there a precondition that *refuses*, named in source?
- Does the refusal enumerate what would be lost?
- If a named override bypasses the precondition, is the loss-list shown
  unconditionally before the bypass?
- Is a savepoint written if the operation can be aborted, and a backup
  made first if the operator consents to the loss?
- Is the allowlist entry concrete enough that a future contributor
  reading it can audit the site without re-deriving the policy?

## Policy and enforcement — two halves of one contract

The policy is two things at once:

- An **operator promise**: rwv won't silently destroy your work. Every
  destructive path is gated; every override is informed; every
  discard is recoverable where the mechanism allows it.
- A **source rule**: adding a destructive call site without
  satisfying the three rules above fails the build, and the
  enforcement is intentionally noisy at the commit that introduces
  the site.

The two halves cannot drift. Stating the policy here and having the
test header point here keeps one source of truth for both audiences.
The test header still carries the enforcement mechanics (the pattern
list, the scan algorithm, the allowlist contract) — the shape of the
verifier — while this page carries the *why* and the *shape of new
call sites*.

## Related

- `tests/destructive_ops_audit_test.rs` — the enforcement mechanism
  and the audited allowlist of current call sites.
- [shared-refs-drift](./joints/shared-refs-drift.md) —
  the safe-class / live-class classifier that gates several of the
  preconditions above.
- [sync-semantics](./joints/sync-semantics.md) — the
  phase model that the `refs/rwv/pre-op/<op-id>` savepoint protects.
- [`rwv backup`](../reference/cli.md#rwv-backup) — listing, dropping and
  pruning backups.
