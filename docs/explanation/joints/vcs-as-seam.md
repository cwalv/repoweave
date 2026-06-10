# VCS as seam

repoweave operates on repositories. Today every repository it touches is
a git repository, but that is a current-state observation, not a
foundational assumption. The internal design treats the version control
system as a seam: a single layer where VCS-specific knowledge lives, and
everywhere else in rwv core uses the abstraction. This joint states the
principle and walks through four worked examples — each one a closed
refactor that pulled VCS-specific behavior across the seam, behind the
Vcs trait.

This joint is the inner-boundary counterpart to
[verb-vs-composition](./verb-vs-composition.md), which is the
outer-boundary principle. The two joints together form a symmetric
design contract: what belongs in rwv core (outer), what belongs in the
Vcs impl (inner).

## The principle

> Anywhere rwv core is about to use a VCS-specific name, text,
> mechanism, or file convention, the `Vcs` trait owns the abstraction;
> the VCS-specific impl handles the details.

"VCS-specific" includes:

- Command names (`git push`, `git rebase`, `hg pull`, `jj rebase`).
- Remote conventions (`origin`, `upstream`).
- File conventions (`.gitattributes`, `.gitignore`, `.hgrc`).
- Configuration mechanisms (`merge=ours`, `[core] sparseCheckout`).
- Error message text and recovery instructions ("git rebase --continue",
  "hg resolve --mark").
- In-flight state names ("mid-rebase", "mid-merge", "in-progress
  graft").

Each of these is a place where a single concept manifests differently
per VCS. Centralizing the concept-to-detail mapping in the Vcs impl
keeps rwv core readable: the higher layer says "push this branch on the
remote associated with this role" without knowing that git calls the
remote `upstream` for forks.

The trait surface lives in `src/vcs.rs`; the git implementation lives
in `src/git.rs`. Future implementations (jj, hg, sl) would each own
their own file; rwv core consumes the trait, not any one impl.

## Why this matters

Two failure modes the seam prevents:

- **Git-only tool by accident.** Without the seam, the easiest place
  for any new feature to put git-specific code is wherever the feature
  is being added. Over time, every module becomes git-aware. The
  switch cost (to support jj or hg) grows without bound, and the
  proposal "add a new VCS" hits a wall composed of dozens of
  unrelated patches.
- **Conceptually duplicated state.** Without the seam, the same
  concept ("the remote for a fork") gets coded in three places that
  drift apart. The push path computes one name; the fetch path
  computes a different one; the dry-run output disagrees with the
  actual run. Single-source-of-truth at the seam prevents this
  category of bug entirely.

The principle is also a code-review tool. If a PR adds a git command,
git-specific name, or git config flag *outside* `src/git.rs`, send it
back: the abstraction belongs in the Vcs trait.

## Worked examples

### (a) `Vcs::resolve_branch_on_remote` + role-aware remote naming

**Concept:** "look up branch X on whichever remote a repo of this role
uses." For git, fork repos use `upstream` as the remote name (so a stray
`git push` does not target the source-of-record); other roles use
`origin`.

**Anchor:** commit `1b76456`.

**Trait surface** (`src/vcs.rs:430`, `src/vcs.rs:442`):

```rust
fn clone_with_role(&self, url: &str, dest: &Path, role: Role)
    -> Result<(), VcsError>;

fn resolve_branch_on_remote(
    &self,
    repo: &Path,
    role: Role,
    branch: &RefName,
) -> Result<ResolvedRevisionId, VcsError>;
```

**Git impl** (`src/git.rs:241`, `src/git.rs:245`):

```rust
fn clone_with_role(&self, url: &str, dest: &Path, role: Role) -> Result<(), VcsError> {
    self.clone_repo_with_remote_name(url, dest, remote_name_for_role(role))
}

fn resolve_branch_on_remote(
    &self,
    repo: &Path,
    role: Role,
    branch: &RefName,
) -> Result<ResolvedRevisionId, VcsError> {
    let qualified = format!("{}/{}", remote_name_for_role(role), branch.as_str());
    self.resolve_revision(repo, &qualified)
}
```

The role-to-remote-name function (`remote_name_for_role`,
`src/git.rs:139`) is a single helper:

```rust
fn remote_name_for_role(role: Role) -> &'static str {
    match role {
        Role::Fork => "upstream",
        Role::Primary | Role::Dependency | Role::Reference => "origin",
    }
}
```

**Why this is the seam shape.** Before the refactor, the
`origin/<branch>` qualifier was spelled out in multiple call sites
inside `update.rs` / `fetch.rs`, each of which had to encode the
role-to-remote convention independently. Pulling the convention into
one helper and exposing role-aware resolution as a trait method means:

- rwv core never spells `origin` or `upstream`.
- The convention is decided once, in one place.
- A different VCS impl can choose a different convention (jj's `default`
  / hg's `default-push`) without rwv core caring.

There is no bare-branch fallback in the trait surface: when the role's
conventional remote doesn't have the branch, the trait returns
`VcsError::RevisionNotFound`, not the local branch tip. This prevents
the silent "we advanced to the local working state instead of the
remote target" failure mode.

### (b) `Vcs::conflict_resolution_hint`

**Concept:** the human-readable text we splice into sync's bail
messages explaining how to resume after the user resolves conflicts.
For git, this is "edit conflicted files; `git add <files>`; `git
rebase --continue`" — specific commands a user will type.

**Anchor:** commit `26ba786`.

**Trait surface** (`src/vcs.rs:561`):

```rust
fn conflict_resolution_hint(&self, op: ConflictOp) -> String;
```

`ConflictOp` is a small enum in the same file (`src/vcs.rs:182`) that
distinguishes the three in-flight ops sync's project-repo path can
leave behind: `Rebase`, `Merge`, `CherryPick`.

**Git impl** (`src/git.rs:477` plus the free helper at
`src/git.rs:153`):

```rust
fn conflict_resolution_hint(&self, op: ConflictOp) -> String {
    git_conflict_resolution_hint(op)
}

// helper:
fn git_conflict_resolution_hint(op: ConflictOp) -> String {
    let continue_cmd = match op {
        ConflictOp::Rebase => "git rebase --continue",
        ConflictOp::Merge => "git merge --continue",
        ConflictOp::CherryPick => "git cherry-pick --continue",
    };
    format!("  # edit conflicted files\n  git add <files>\n  {continue_cmd}")
}
```

**Call sites** in sync use the trait method to compose conflict-bail
messages — e.g., `src/sync.rs:803`:

```rust
let hint = GitVcs.conflict_resolution_hint(op);
```

**Why this is the seam shape.** Before the refactor, sync's
`bail!` macros embedded "git add" / "git rebase --continue" text
directly. The text was specific, helpful, and quietly git-only — and
the same phrasing appeared three times in slightly different forms
across the conflict-bail sites. Centralizing the per-op hint text in
the trait method means:

- rwv core never writes "git add" or "git rebase --continue".
- The hint is per-VCS *and* per-op.
- A hg impl returns `hg resolve --mark` text without changing any sync
  bail site.

The trait method is small on purpose: it returns a short block, not a
full bail message. Surrounding context (which repo, how to re-run
sync, how to abort) is composed in `src/sync.rs`. The seam carries the
VCS-specific noun phrases, not the entire prose.

### (c) `Vcs::set_replay_exclusion`

**Concept:** configure a repo such that during replay (rebase, merge)
any changes to a specified path are silently overridden by the replay
target's version. For git, this is a `merge=ours` entry in
`.gitattributes` paired with the inline `merge.ours.driver=true`
config wired up per-rebase.

**Anchor:** commit `d29bb2f`.

**Trait surface** (`src/vcs.rs:601`, with the companion query at
`src/vcs.rs:613`):

```rust
fn set_replay_exclusion(&self, repo: &Path, path: &Path) -> Result<(), VcsError>;

fn has_replay_exclusion(&self, repo: &Path, path: &Path) -> Result<bool, VcsError>;
```

**Git impl** (`src/git.rs:573`): appends `<path> merge=ours` to
`<repo>/.gitattributes`, idempotently. The `merge.ours.driver=true`
shell hook (which makes the `ours` driver succeed without modifying
the merged file) is set inline at `src/git.rs:519`-ish during the
rebase command itself, so no persistent `.git/config` change is
required.

**Used by** [sync-semantics](./sync-semantics.md)'s Phase 1' to keep
`rwv.lock` out of the merge inputs. The lock is regenerated from
manifest tips in Phase 3, so carrying user lock-edits through a rebase
would only produce noise. See
[lock-as-derived](./lock-as-derived.md) for why this is structurally
right.

**Why this is the seam shape.** Before the refactor, sync ran a custom
cherry-pick loop with per-commit lock-exclusion logic inline.
Replacing it with a one-shot `set_replay_exclusion` call (made once
at `rwv init` time) and a standard `rebase` call collapsed dozens of
lines of git-aware loop into one trait call plus one rebase call.

The win is not just shorter code — it is that the exclusion mechanism
is now properly per-VCS. A hg impl can implement `set_replay_exclusion`
via `[merge-patterns]` in `.hgrc`; a jj impl might do something
entirely different. Sync neither knows nor cares.

The companion `has_replay_exclusion` query exists so `rwv doctor` can
detect projects initialised before this path landed and offer to add
the missing entry. The detection logic in core stays VCS-agnostic.

### (d) `Vcs::push_with_role`

**Concept:** push the currently-checked-out branch on the remote
associated with the given role. For git, this resolves the current
branch via `current_ref` (refusing detached HEAD with a typed
`CommandFailed`), selects the remote via the role convention from
example (a), and runs `git push <remote> <branch>`.

**Anchor:** commit `6066ce1`.

**Trait surface** (`src/vcs.rs:471`):

```rust
fn push_with_role(&self, repo: &Path, role: Role, force: bool)
    -> Result<(), VcsError>;
```

**Git impl** (`src/git.rs:255`): see the file for the full body. The
relevant detail is that the impl owns:

- The `current_ref` lookup and the detached-HEAD failure mode.
- The role-to-remote selection (`remote_name_for_role`, shared with
  examples (a) and (d)).
- The `--force` flag spelling.
- The argument shape `git push <remote> <branch>`.

`src/push.rs` (the verb-level orchestrator) does the cross-repo work —
walking the manifest, applying selectors, ordering project-repo last,
checking the lock-state precondition — but never invokes git directly.
It calls `git.push_with_role(repo, role, force)`.

**Why this is the seam shape.** Before this method existed, the
push-loop draft constructed `git push` argument strings inline. Per the
verbs design discussion ("the trait captures the per-role push policy
— refs come from the manifest, not from `git push` argument
parsing"), the right shape was a trait method whose contract names the
*intent* ("push this branch on the role-conventional remote") and
hides the *mechanism* ("git push origin <branch>" or "git push
upstream <branch>" or some entirely different command on another VCS).

Note the trait-level Fork policy is *neutral*:
`push_with_role(Role::Fork)` will push to `upstream` (since that is
what the role convention selects). The "skip forks with an info line"
caller-side policy lives in `src/push.rs`. The trait stays a thin
shell over the VCS surface; the policy of which roles to push from
the loop lives where the loop lives. The asymmetry is deliberate: it
keeps the trait composable (a future verb that *did* want to push
forks could call `push_with_role(Role::Fork)` without surprises) and
keeps verb-level policy debuggable (the "skip forks" choice is one
visible line in one file).

## What this means for code review

Concrete checklist for reviewers when a PR adds VCS-aware code:

1. **Does the PR spell a git command in rwv core?** ("git ...",
   `Command::new("git")`, `.args(["rebase", ...])`.) If so outside
   `src/git.rs`, send it back — the wrapper belongs in the Vcs impl.
2. **Does the PR introduce a remote name?** ("origin", "upstream",
   "fork".) If outside `src/git.rs`, send it back — the naming
   policy is `remote_name_for_role`.
3. **Does the PR introduce a `.git*` file convention?**
   (`.gitattributes`, `.gitignore`, `.gitmodules`.) If outside
   `src/git.rs`, send it back — the file convention belongs in the
   Vcs impl.
4. **Does the PR introduce a user-facing git-vocabulary string?**
   ("rebase", "cherry-pick", "merge --continue".) If the string is
   shown to users from rwv core, send it back — the per-VCS phrasing
   belongs in a Vcs trait method like `conflict_resolution_hint`.

Each of the four worked examples above started as a PR that initially
put VCS-specific code in rwv core; each one was refactored to move the
behavior across the seam. The discipline isn't novel; the joint just
names it so reviewers have one canonical pointer.

## Anchoring

The examples above each cite a closed bead and a landed commit. The
sync codepath that depends on examples (b) and (c) is covered by:

- `tests/e2e_two_workweaves_test.rs` — exercises the `merge=ours`
  replay-exclusion path end-to-end.
- `tests/e2e_sync_abort_test.rs` — covers the conflict-hint text
  surfacing through the bail messages.

The push codepath (example (d)) is covered by:

- `tests/push_test.rs` — direct exercises of the push loop.
- `tests/doc_claims_push_test.rs` — anchors the documented Role::Fork
  skip and the project-repo-last ordering.

## Related joints

- [verb-vs-composition](./verb-vs-composition.md) — outer-boundary
  counterpart. The two joints together delimit rwv core.
- [verb-vs-vocabulary](./verb-vs-vocabulary.md) — naming half of the
  outer boundary; depends on this joint for the implementation-side
  half.
- [sync-semantics](./sync-semantics.md) — phase model that depends on
  examples (b) and (c).
- [lock-as-derived](./lock-as-derived.md) — the property that
  motivates `set_replay_exclusion` (example (c)) in the first place.
- [clone-topology](./clone-topology.md) — the tier-0 spec is
  VCS-neutral by design (canonical store, linked workspace, branch
  ownership); the git mapping there is the worked example, in the same
  shape as the examples in this joint.
