# repoweave — source conventions

House rules for anyone editing `src/`, human or agent. They are not Rust
community consensus; the first one runs against common advice deliberately.

`CONTRIBUTING.md` covers whether to send a change at all. This file covers how
the code reads once you do.

## Comments are extremely sparse

Code speaks for itself. The default for a new comment is not to write it:
rename the binding, split the function, or change the type until the code says
what the comment would have said. A type that makes an illegal state
unrepresentable replaces the comment warning against it.

Write a comment only for something the code cannot state:

- an invariant a caller must uphold that no signature enforces;
- a deliberate deviation from the obvious implementation, and what breaks if
  someone "fixes" it;
- a fact about the world outside this repo — a git behaviour, a filesystem
  guarantee — that a reader cannot derive from the file in front of them.

Never write a comment that restates the code, narrates the steps below it, or
records what a change did or what the code used to do. The last of those
belongs in the commit message.

This inverts the usual "write the interface comment first" advice, and the
inversion is the point. A comment does not age with the code around it. A stale
comment is not neutral — it is an authoritative-sounding sentence sitting next
to the truth, and a reader skimming for context will believe it over the code.
Comments in this repo have described the opposite of what the function below
them did, survived for weeks in duplicate, and propagated into published
`rwv explain` output. Sparseness is the mitigation: a comment that was never
written cannot go stale.

## Comments do not cite trackers or documents

A comment states the invariant itself. It does not point somewhere else for the
reader to go find it.

Never, in any comment under `src/`:

- **a tracker ID** — `fo-…`, `#1234`, `PROJ-56`;
- **a section pointer into a design document** — `branch-model.md §3.3`,
  `plan §7.1 arm 7`, `Finding 1 of …`;
- **a path that does not resolve to a file in this repository** — most often a
  path into the workspace this repo is developed in (`docs/repoweave/…`,
  `docs/agent-persona/…`, `../../../../projects/…`). Someone holding only a
  clone of this repo has no such file. The reference is unfollowable from the
  day it is written, and it rots invisibly, because nothing can check a path
  that was never expected to resolve.

A path that *does* resolve here — `docs/explanation/joints/clone-topology.md`,
`docs/reference/cli.md` — may appear as a trailing pointer, after the comment
has already said the thing. A comment whose entire content is the reference
(`// See docs/explanation/joints/clone-topology.md.`) is a violation: delete it
and write the sentence it was standing in for.

Rationale belongs in `docs/explanation/joints/`. That is what those documents
are for. The comment carries the invariant; the joint doc carries the argument.

### Scope: comments, not strings

The document half of this rule governs comment text only. A string literal or a
path expression is a program operating on a path, not a comment citing a
document, and is unaffected:

```rust
include_str!("../docs/reference/explain/fetch.md")
root.join("docs/reference/cli.md")
bail!("add `cli-md:{path}` to docs/cli-coverage-allowlist.txt with a reason")
```

Tracker IDs are the exception: they are banned everywhere in `src/` and
`docs/`, string literals included. A user who meets one in an error message or
in `rwv explain` output has nothing to open. `check_no_tracker_ids` in
`src/bin/generate-explain.rs` already enforces that.

`tests/` is outside both rules. A test may name the regression it pins and
describe the scenario it reproduces.

### Escape hatch

A single site may keep an otherwise-forbidden path by annotating it on the line
above:

```rust
// weave-local-ref: <why it must stay>; does not resolve in a standalone clone
```

The trailing clause is part of the annotation, not decoration, and must be
literally true. It therefore covers exactly one case: a path outside this
repository. It is not available for a tracker ID, not available for a path that
does resolve here, and not a way to preserve a comment that a rewrite would
have removed.

Expect to reach for it close to never. The fix for an unfollowable reference is
almost always to state the invariant and delete the reference.
