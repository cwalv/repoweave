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

## Comments do not name symbols that no longer exist

A comment that names a function, method, type or variant as part of this
codebase is making a checkable claim. Check it against the **code**, not
against the other comments.

The test is **occurrence outside a comment**. A name that appears in `src/`
only inside comment text does not exist here: it was deleted, renamed, or never
written. Prose about its removal is what the commit message is for.

### Do not use a mention count

The obvious check — grep the name, see if anything comes back — is wrong in the
one situation that matters, and it is worth knowing why before you reach for
it.

A symbol that was just deleted is a symbol people are actively writing about.
Its mention count goes *up* at the moment it stops existing. One deleted method
on the VCS seam kept eleven mentions across seven files, every one of them prose
about the deletion; a grep returned eleven hits and a sweep read that as proof
the symbol was live. Sitting among those eleven was a comment listing it as a
current member of the trait. The count was highest exactly where the rule was
weakest.

So:

```sh
# WRONG — a hit count includes the comments discussing the deletion
git grep -c <name> -- src/

# RIGHT — drop comment lines; what remains is code that uses the name
git grep -n <name> -- src/ | grep -v '^\S*:[0-9]*: *//'
```

Read what survives rather than counting it. A string literal quoting the name
survives the filter too — the gate's own test fixtures quote a deleted method on
purpose — and a fixture is not a use.

Comment text is the surface making the claim. It cannot also be the evidence
for it.

### What is mechanised, and what is not

`check_doc_symbol_refs` in `src/bin/generate-explain.rs` enforces this for the
**qualified** shape only: a comment writing `` `Type::member` ``, where `Type`
is a name this code uses, requires `member` to occur outside a comment.

A **bare** identifier is not checked, and that is the shape the rule was written
for. The gate cannot separate a deleted method from a `std` function, a shell
command, a parameter name, a hypothetical in an example, or an ordinary English
word in backticks — measured across every backtick-quoted bare identifier in
`src/`, the predicate reports 28 sites of which 21 are correct. Suppressing
those would need five different justifications, and an inline annotation has to
state a reason that is true where it sits; one annotation covering all five is
an allowlist written inline. See that function's doc comment for the full
measurement.

**A green gate therefore does not mean the tree has no stale symbol
references.** When you sweep by hand, apply the occurrence-outside-a-comment
test to bare identifiers yourself, and expect to adjudicate: most of what it
turns up will be a legitimate foreign name, and the residue is what you are
looking for.

### Naming something that is gone

If a comment genuinely needs to say what changed, it is already violating the
sparseness rule above — that belongs in the commit message. Delete it. If the
contrast is load-bearing, state the invariant that holds now and leave the dead
name out; an unqualified mention of a foreign name (`into_boxed_path` rather
than the qualified form) is out of scope by construction, and is the way past a
wrong report from the gate.
