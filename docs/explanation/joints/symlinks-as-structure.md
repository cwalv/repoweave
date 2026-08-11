# Symlinks as structure

rwv creates symbolic links in three places: a `role: reference` repo
materialized into a workweave, a workweave `link:` entry, and the surfacing of
a project's owned files at a workspace root. None of the three is a
space-saving convenience. In each, the link *is* the fact some other part of
rwv later reads off the disk, and a link that failed to appear is not a
degraded workspace — it is a workspace that lies about what it contains.

That is the seam this joint describes: the on-disk shape carries meaning, so
what may be written at those paths is not a free implementation choice. It
constrains every platform rwv runs on, and it is the reason the Windows
question has one answer rather than a menu.

## The link is the classification

[Clone topology](./clone-topology.md) states the reference-repo carve-out to
invariant I2: a `role: reference` checkout satisfies "one canonical store" by
*identity* — the workweave path is a symlink onto `<weave>/<repo_path>`, so
there is no second store to keep coherent. Nothing records that fact anywhere
else. `classify_checkout` reads `is_symlink()` at the checkout path and returns
`ReferenceAlias` or `Worktree`, and every downstream command routes on the
answer. There is no field, no marker file, and no manifest entry to consult;
`role` is read once, at creation, to pick what to write.

So the question "is this a shared read-only alias" is answered by asking the
filesystem what kind of thing is at that path. Five behaviours hang off the
answer:

| Reads `ReferenceAlias` and | If the path were not a symlink |
|---|---|
| sync's mutating phases skip it | savepoint, rebase/ff and abort's `reset --hard` all run against it |
| orphan pruning skips it | its path is handed to `git worktree remove` |
| dirty/divergence scans skip it | it acquires per-workweave dirty state and a HEAD that can diverge |
| workweave delete unlinks it with `remove_file` | delete takes the worktree-removal path |
| doctor's topology scan skips it | it is a second standalone store: I1 and I3 findings fire against it |

The surfacing links are structural in a second sense. A surfaced file is a
write-through path: an ecosystem tool that writes `Cargo.lock` at the weave
root must land those bytes in `projects/<project>/Cargo.lock`, which is the
copy under version control. Something other than a link at that path is a
second copy of a tracked file that nothing reconciles.

## What follows: a substitute is not a degradation

Because the classification is a read of the on-disk shape, anything written at
those paths that is not a symlink is read as the other thing. That turns every
"fall back to X" proposal into a correctness question rather than a
convenience/cost trade-off, and it closes three of them outright.

### Copies are closed

A copied reference checkout is a real directory, so `classify_checkout` returns
`Worktree` for it and it enters every path in the table above. It is not a
degraded reference repo; it is structurally indistinguishable from a repo the
user asked to work in. `rwv sync` will advance it, `rwv workweave delete` will
try to remove it as a worktree, and `rwv doctor` will report it as a standalone
store inside a workweave — correctly, because that is what it is.

A copied *surfacing* link is the same defect in the other direction: writes
land in the copy, the tracked file under `projects/` never sees them, and
nothing tells the operator which of the two is real.

### Hardlinks are closed

Two independent reasons, and the first one is rwv's own machinery.

rwv publishes owned files by atomic replace — write a sibling temp, `rename`
over the target (`durable_file::replace`). `rename` replaces the directory
entry; it does not write through to the inode. Every other name hardlinked to
the old inode keeps pointing at the *previous* contents, silently and with no
error anywhere. rwv would orphan its own links as a routine consequence of
writing a file correctly.

Second, hardlinks cannot name directories at all, so the reference alias and
any directory-valued surfacing entry are out of reach by construction. A
hardlink strategy could at best cover part of the sites, which means shipping
two mechanisms and two behaviours to test.

(`durable_file::create_new` does call `hard_link`, and that is not an exception
to this. It links a staged temp into place to get an atomic exclusive publish
and immediately removes the temp, so the link exists for the duration of one
call and no second name survives it.)

### Warn-and-continue is closed

A link that could not be created is a missing structural fact, and every
consumer reads its absence as a different, well-formed state: an absent
reference checkout looks like a manifest that grew since the workweave was
made, an absent surfacing link looks like a project that does not declare that
file. A warning on stderr, followed by a success exit, hands the operator a
workspace that will misbehave later somewhere else.

The rule is refusal at the first failed link, carrying what to do about it.

## Windows

Windows gets full symlink parity: `rwv` creates real symbolic links there, with
no fallback mechanism of any kind. One mechanism, one set of semantics, one
behaviour to test.

The cost is a precondition. `CreateSymbolicLinkW` succeeds for a process
holding `SeCreateSymbolicLink` (an elevated process, in practice), or on a
machine with **Developer Mode** enabled. Developer Mode is not a privilege rwv
can request or scope to itself: it is a machine-wide policy an administrator
sets once. So the precondition is stated to the operator when it is not met,
and it is stated as the two things a person can actually do — enable Developer
Mode, or run elevated.

Nothing Windows-specific is needed on the happy path. Rust's
`std::os::windows::fs::symlink_file` and `symlink_dir` both add
`SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE` to the flags they pass, which is
the opt-in that makes Developer Mode apply, and they retry without it on the
older Windows versions that reject the flag.

### The kind decision

This is the part a Unix reader cannot see. `std::os::unix::fs::symlink` takes a
target and a link path and nothing else. Windows has two calls, and a link
created with the wrong one is broken: a `symlink_file` link whose target is a
directory does not resolve as a directory.

So **every place rwv creates a symlink is a place where a directory-vs-file
decision is being made**, whether or not the code that makes it is visible. The
rule:

- The reference alias always names a directory — the canonical clone.
- Every other site classifies the source path on disk: a directory yields a
  directory link, and anything else — including a path that does not exist yet
  — yields a file link.

The absent case is not a guess about the unknown. Surfacing deliberately
creates dangling links so that a lock file an ecosystem tool has not written
yet lands in the project directory when it appears, and those are files. What
the rule genuinely cannot cover is a target that changes kind after the link
was made; that is a broken link on Windows and a working one on Unix, and no
creation-time rule reaches it.

The decision is made in platform-independent code and handed to the platform
call, rather than inside the `#[cfg(windows)]` arm. On Unix the kind is carried
and discarded — which is the point, because it makes the decision visible to
every reader and reachable by a test on the platform CI actually runs.

## Recorded, not built: how this would degrade

If a Windows operator ever appears who cannot enable Developer Mode and will
not run elevated, the shape of the answer is already known, and it is written
down here so that work starts from this analysis rather than from scratch. It
is **not implemented**, and nothing in the tree is arranged in anticipation of
it.

- **Directory links become junctions.** A junction is a reparse point, and its
  tag `IO_REPARSE_TAG_MOUNT_POINT` carries the name-surrogate bit that Rust's
  `FileType::is_symlink` tests. So a junction reports as a symlink through
  `Path::is_symlink`, and `classify_checkout` classifies it as
  `ReferenceAlias` with no change — the detectability the table above depends
  on survives. Junctions need no privilege and no Developer Mode. They are
  absolute-only and local-volume-only, which the reference alias already is.
- **File links become attested copies.** A copy that is registered in the
  owned-file digest ledger is not the structurally-invisible copy this joint
  closes: drift between the copy and its source becomes a `doctor` staleness
  finding, and `rwv materialize` re-derives it. The write-through direction is
  still lost, which is a real behaviour change and would have to be stated to
  the operator, not papered over.

The revisit trigger is structural, not a date: **a real Windows adopter for
whom the precondition cannot be met.** Windows CI does not trigger it — GitHub's
Windows runner images enable Developer Mode at image build time and run the job
as an administrator with UAC disabled, so both sufficient conditions hold there
and the full-parity path is what CI exercises.
