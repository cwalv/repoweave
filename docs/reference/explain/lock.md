# rwv lock

## Purpose

Record where the member checkouts are as the pins of `rwv.lock`. No network
access, no integration hooks, no manifest mutations. `lock` is a pure
local operation: it reads each acted-on member's HEAD from disk and writes
it into the project's `rwv.lock`, moving a pin forward only.

### What each entry records

Every entry is the commit id HEAD is on, never a tag or branch name, even
when a tag points at HEAD. A name can be moved or deleted after the lock is
written; a commit id cannot, so the lock resolves to the same tree however
the names around it change. The same holds for every writer of the lock,
including the `rwv sync` path that writes it internally and `rwv update
--remote`.

An older or hand-edited lock may still carry an entry pinned by name.
Verbs that read the lock resolve such an entry to wherever the name points
now and warn, naming the entry; the next `rwv lock` records the commit id.
An entry whose name no longer resolves is a pin no ref reaches.

### What `lock` moves, and what it refuses

`lock` records each member it acts on (every member, or those `--repo`
selects) at the commit its checkout is at, judged against the pin the lock
holds for it. Every other pin stays as it was.

- **Forward** (the checkout is ahead of its pin): recorded.
- **No pin** (the lock has no entry for the member, or no lock file, or a
  lock that does not parse; or the member has no commit yet): the member
  keeps none, and the run reports `pin` `none`. A first pin comes from a
  verb that names the commit: `rwv member add`, `rwv member pin`, `rwv
  update --remote`.
- **Backward** (the checkout is behind its pin or has diverged from it, or the
  pin is in no ref here): refused as `pin` `backwards` unless `--repo` names
  the member, which then moves its pin and is reported `re-pinned`.
- **Not aligned** (the member is not where the checked-out branch's record
  puts it): refused on the `alignment` axis, with `rwv update` as the choice.
- **Detached workspace or project repo**: refused (`alignment` `project
  detached`).

Every member is judged before `rwv.lock` is written, so a refusal changes
nothing, and all refusals are reported together.

A forward move over commits no remote-tracking branch reaches names them
when another project's lock for the member does not pin them. That
project's `rwv status` then reports the member as `pin` `ahead` with the
fact `pinned-by`.

### Lock as derived state

`rwv.lock` is derived state: every pin is a commit a member repo holds,
read from disk by the verbs that write it. This has two practical
consequences:

1. A hand edit of a pin is read as the previous pin: the next `lock` moves it
   forward to where the checkout is, or refuses to move it backward.
2. Lock conflicts across workweaves are never meaningful: after any merge,
   the correct lock is the record of the post-merge tips. `rwv sync`
   encodes this by excluding the lock from the merge phase and regenerating
   it in Phase 3.

### When to run `lock`

Run `rwv lock` after committing changes across member repos (or in a
single repo) to record the new cross-repo state. Common patterns:

- Manual cross-repo feature: commit in each affected repo, then `rwv lock`
  to capture the joint state.
- After a hand edit of `rwv.toml`: `rwv lock --commit` commits the whole file.
  It first runs the step of each setter for the entries that changed (a
  declared branch is pinned at its tip and the member switched onto it, a pin
  moved backward reported as `re-pinned <old> to <new>, by name`, a branch
  that exists nowhere refusing as `name` `no-such-branch`; a url relinks, or
  is reported waiting for a fetch, naming `rwv fetch`; a hand-added member
  gets its first pin), then commits every other
  change as it stands, `[integrations]` and `[workweave]` included, with the
  files its regeneration rewrites. Every refusal is judged before anything is
  written. The `member` verbs commit their own change.

Do **not** run `rwv lock` in place of `rwv update --remote`. `lock` records
*current local HEADs*; `update --remote` fetches from the network and advances
tips. If you want the latest upstream commits, use `rwv update --remote`.

`rwv clone` and `rwv update` never write the lock: a member the lock has no
entry for is left unpinned, and `rwv member pin` or `rwv update --remote`
gives it a pin.

### Stale-lock relationship

A checkout that does not stand at its pin (`rwv doctor` finding `pin`,
sub-kinds `ahead`, `unpinned`, `behind` and `diverged`) means the current repo
HEADs no longer match what `rwv.lock` records. Running `rwv lock` clears it for
a checkout ahead of its pin; one behind or diverged is cleared by `rwv lock
--repo <repo>` (record the checkout) or `rwv update` (move the checkout to the
pin). A member the lock has no entry for is `pin` `none`, and `rwv lock` gives it
none.

`rwv doctor --locked` is the scriptable precondition check: zero exit iff
every repo tip matches the lock.

## Invocation

```
rwv lock [--dirty] [--repo <selector>]... [--commit [-m <MSG>]] [--project <name>]
```

- `--dirty` — skip the uncommitted-changes check. By default, `lock` refuses
  to record any repo with uncommitted changes to tracked files (staged,
  unstaged), because that content has no SHA and cannot be reproducibly
  recovered. Untracked files never refuse: they cannot change HEAD. `--dirty`
  bypasses the check; use only when you deliberately want to record an
  in-flight state (e.g., a workweave that hasn't been cleaned up yet).
- `--repo <selector>` — record only the members the selector matches (an exact
  path, `glob:<pat>` or `re:<pat>`; repeat for union), and let each of them
  move its pin backward. A selector matching no member refuses with
  `repo-not-in-manifest`.
- `--commit` — after writing `rwv.lock`, stage and commit it from the project
  directory, with an auto-generated message listing which repos advanced or were dropped. A
  `rwv.toml` with uncommitted changes, or one never committed, goes into the
  same commit: the lock is derived from the manifest, so a membership edit and
  the relock it calls for are one change. The message then also names
  `rwv.toml` (`lock: refresh 2 repos, record rwv.toml`, `lock: drop <repo>` for a removal, or `lock: record
  rwv.toml` when no pin moved). Refuses if the project repo has
  uncommitted changes outside `rwv.lock` and `rwv.toml` — the auto-commit must
  not bundle unrelated work-in-progress.
- `-m, --message <MSG>` — the commit message for `--commit`, in place of the
  generated one. Without `--commit` it is a usage error (exit 2).
- `--project <name>` — operate on this project rather than the active project.
  Does not change `.rwv-active`.

Run `rwv lock --help` for the full clap surface.

## Output

`Wrote <path>` to stderr on success (one line naming the lock file written),
then a line for each pin the run moved backward by name (`re-pinned`) and the
`pin` `none` lines for the members that keep no pin.

When `--commit` is set, also prints `Committed rwv.lock`, or `Committed rwv.lock,
rwv.toml` when the manifest went into the commit (or `Lock unchanged, nothing to
commit.` if nothing changed).

## Exit codes

- `0` — lock written successfully.
- non-zero — workspace could not be resolved, manifest parse failure, a repo
  has uncommitted changes (without `--dirty`), a pin would move backward, a
  member is not aligned, or a git operation failed. Nothing is written.

## Examples

Record current HEADs:

```
rwv lock
```

Record a checkout that is behind its pin, by name:

```
rwv lock --repo github/acme/server --commit
```

Record and commit in one step:

```
rwv lock --commit
```

Commit under your own message:

```
rwv lock --commit -m "pin web-app for the 1.4 release"
```

Record even when repos have uncommitted changes:

```
rwv lock --dirty
```

## Common errors

- *repo has uncommitted changes to tracked files; commit or use --dirty to override* — one or
  more repos have staged or unstaged edits to tracked files. Commit the changes, or pass
  `--dirty` to record the committed portion of HEAD anyway.
- *project repo has uncommitted changes outside rwv.lock and rwv.toml* —
  `--commit` was passed but the project repo has work-in-progress beyond the
  lock and the manifest. Commit or stash the other changes before using
  `--commit`.
- *workspace could not be resolved* — CWD is not inside a weave or workweave;
  run from within the workspace.
- *pin backwards* — a member's HEAD is behind the pin, has diverged from it, or
  the pin is in no ref here. `rwv lock --repo <repo>` records the checkout by
  name; `rwv update` moves the checkout to the pin instead.
- *pin none* — the lock has no pin for a member. `rwv member pin <repo> <rev>
  --commit` gives it one.
- *target unrecorded* — the checked-out branch of the project repo has no record
  of how it is used, so this verb, which locks, refuses before changing
  anything. The record may be published but not yet fetched (`rwv fetch`);
  otherwise say how the branch is used with `rwv branch set-mode`.
