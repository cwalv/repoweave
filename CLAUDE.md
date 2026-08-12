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

A reader never lands on a dead end. Text says what it means where it stands,
and anything it points at is somewhere the reader can go.

The rule turns on **who the reader is**, not on what syntax the text is written
in. Two audiences, one question each:

- **Repo-internal text** — a comment read from a clone. May cite anything in
  the repo by resolving path; `docs/internals/` counts, because a cloner has
  it. A resolving citation is followable; a non-resolving one is not, and the
  reader must be able to open what it names. A citation resolves from one of
  two bases, and it must resolve from one: the **repo root**
  (`docs/explanation/joints/clone-topology.md`), or the **directory of the
  citing file** (a comment in `src/` naming `notes.md` means `src/notes.md`).
  Existing somewhere in the repo does not count — a bare `clone-topology.md`
  is a violation even though `docs/explanation/joints/clone-topology.md` is
  right there, because the reader holds a comment and not an index.
- **Operator-facing text** — a `bail!`, a `doctor` finding, an `rwv explain`
  page, `--help` output, and any comment a generator lifts onto one of those
  surfaces. Carries the rule or the action itself, and references only pages
  `docs/SUMMARY.md` lists (plus root README/ARCHITECTURE.md). A page under
  `docs/internals/` is not one of those; mdBook does not render it and the
  reader has no page to reach.

Citation demand is the graduation signal. When repo text wants to cite
something that is not yet in the repo, **graduate it** — pull it in from the
project workspace, `docs/internals/` counts as landing. When operator text
wants to reference something not yet published, **publish it** — a page under
`docs/reference/`, listed in `SUMMARY.md`, keyed by whatever token the machine
surface already uses (`docs/reference/doctor-findings.md` is keyed by the
`rwv doctor --json` `kind`; a second namespace is a thing to keep in sync,
not a feature).

Two shapes stay banned, string literals and generated surfaces included,
because they encode no route a reader can take:

- **a tracker ID** — an identifier that means nothing except as a lookup in a
  tracker the reader does not have. `fo-…` is one prefix; the ban is on the
  thing, not on a list of spellings. Banned everywhere, `tests/` included. A
  test may name the regression it pins in its own words; the ID itself is not
  the way to do that — that provenance is what the commit that added the test
  already carries. `check_no_tracker_ids` in `src/bin/generate-explain.rs`
  mechanises one prefix of this and deliberately not the rest; the next
  section says which, and why it stops there.
- **a comment whose entire content is a reference** — `// See
  docs/explanation/joints/clone-topology.md.`. Banned in `src/` and `docs/`.
  Delete it and write the sentence it was standing in for. A resolving
  trailing pointer is fine after the comment has said the thing.

`tests/` is outside `check_doc_citations`, which reads `src/` only. That is a
debt, not a principle, and the difference matters because the two read the
same from a green gate.

The reason it is not a principle: a test comment describing the fixture tree
it builds in a temp directory really is not citing a document, but that
describes a *minority* of the tree. Measured over `tests/`, a widened gate
reports 64 unfollowable references, and they are three different jobs rather
than one backlog:

- **13** are fixture paths — a filename the test itself writes into a temp
  tree. The gate would be wrong to report these, and they are the group the
  sentence above is true of.
- **32** name a document this repository really has, spelled so it does not
  resolve; eighteen are `branch-model.md`, which is
  `docs/internals/branch-model.md`. Mechanical — write the path that resolves.
- **19** name nothing here at all. Someone has to recover what the comment
  meant before it can be rewritten, which makes this the expensive group
  despite being the smallest.

So the sentence this clause used to carry was not false. It was outnumbered
four to one, which is worse than false: it read as a principle and was a
sampling error.

Enabling the gate means clearing the 51 first, and about 30 of them carry a
section pointer too — cutting across the second and third groups alike — so
those need the comment rewritten rather than the path corrected. Until that
happens the exemption stands on the size of that cleanup and nothing else.
**Re-measure before repeating these numbers**: they describe a tree that
moves, not a property of `tests/`.

**When letter and spirit disagree, spirit wins and the letter is a bug — file
it.** A rewrite of the rulebook that leaves a specific clause misfiring is
this file's own defect, and the fix is here, not a workaround at the site.

### Escape hatch

A single site may keep an otherwise-forbidden path by annotating it on the
line above:

```rust
// weave-local-ref: <why it must stay>; does not resolve in a standalone clone
```

The trailing clause is part of the annotation and must be literally true, so
it covers exactly one case: a path outside this repository. Not for a tracker
ID, not for a path that does resolve here, not a way to preserve a comment a
rewrite would have removed. Expect to reach for it close to never.

### What is mechanised, and what is not

Three gates in `src/bin/generate-explain.rs`:

- `check_no_tracker_ids` enforces the tracker-ID clause for **one prefix**:
  the literal `fo-`, not preceded by an alphanumeric, followed by four to
  eight lowercase letters or digits and an optional `.N` sub-ID. Nothing
  shorter, longer, capitalised, or differently prefixed. It reads `docs/`
  and `tests/` as well as `src/`, which the citation gate does not, and it
  also reads every `.md` and `.rs` file directly at the repo root —
  `CLAUDE.md` and `build.rs` included, so neither the file stating this ban
  nor the one root-level source file goes unchecked.

  That prefix is the retired one. The prefix the tracker issues now is
  `rwv-`, which is also this repo's own vocabulary namespace — some 480
  occurrences in `src/` alone, led by `rwv-active`, `rwv-workweave`,
  `rwv-op`, and `rwv-ours`, the merge driver written into `.gitattributes`
  and into `merge.rwv-ours.*` config keys, at 51 sites by itself. A general
  `PREFIX-slug` matcher reads every one of those as a tracker ID, and a
  matcher that reports correct code gets turned off. So this gate holds the
  one prefix it can hold without a suppression mechanism, and a second one
  is written in by hand by someone who has measured what it collides with
  first. The narrowness is a measurement, not an oversight — but **a green
  gate means no `fo-`-shaped ID, not no tracker ID.** Every other prefix is
  yours to catch while you read.
- `check_doc_citations` enforces the two-base resolution rule and the
  bare-pointer clause over comments in `src/`, for tokens whose last
  component ends in a document extension. A filename with no `/` counts, for
  `.md`. Two exemptions: a filename this repository's own non-test code
  operates on is an artifact the program handles, not a document the comment
  cites; and a bare filename accompanied **in the same comment block** by a
  resolving path is followable as it stands (which is what a markdown link
  already gives the reader).

  Inline `#[cfg(test)]` modules are in scope — a comment inside one asks its
  reader the same question any other does. This gate's own file is the
  exception and stops at its test boundary, because its fixtures are comment
  text held in string literals, which the scanner cannot tell from comments;
  scanning them turns every seeded-failure test into a finding against itself.
  So **a green gate means no unfollowable citation outside that one file**,
  and the sixteen fixtures behind that boundary are read by nothing.
- `check_no_internals_on_operator_surfaces` scans generated operator surfaces
  — `docs/reference/explain/**` and `docs/reference/schemas/*.json` — for
  `docs/internals/` paths. The generator is an audience boundary; what it
  lifts onto an operator page must be operator-clean.

**The section-pointer shape (`plan §7.1 arm 7`, `Finding 1 of …`) is not
mechanised as such.** Written against a bare filename it is caught, but only
because the filename does not resolve; the section token is invisible to the
gate. Written against a path that resolves, or against a document named with
no file extension, nothing fires.

**A green gate therefore does not mean the tree has no section pointers.** It
also does not mean `docs/` is clean: `check_doc_citations` reads `src/` only.
Inline `#[cfg(test)]` modules are in scope; the one place it stops at the test
boundary is its own file, whose fixtures are comment text it cannot tell from
comments. When you sweep by hand, match on the *shape* — a document name
followed by a section token — in every spelling. The sweep before this gate
matched `§` and left `D2`, `D4` and `D1–D3` standing in four files.

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

## A green gate is not coverage

Both rules above end with a "What is mechanised, and what is not". That is not
boilerplate. It is the same defect written down twice: **a check green-lights
what it examined, and reading it as green-lighting what it was pointed at is
the error.**

Take the strength of the evidence first. A survey of this repository's checks
found the defect four times. The work that fixed those four then reproduced it
three times *in itself*, inside a week, while its author was watching for
exactly this — and those three are the argument, because they are what the
defect looks like when someone is already looking for it:

- **The sweep that cleared the ground matched one spelling.** Before the
  citation gate could be widened, ~290 bare section pointers had to come out of
  `src/` comments. That sweep enumerated `§`. Four files wrote their section
  pointers `D2`, `D4`, `D1–D3` — same shape, different notation — and were left
  standing, to be found by the widened gate afterwards. A one-spelling matcher
  was used to clear the ground for the change about one-spelling matchers.
- **The replacement check passed a corrupted input on its first run.** The new
  half of `tests/doc_claims_cli_md_test.rs` reads documented flag values out of
  a markdown section and asserts clap accepts them. Its section slicer cut at
  the wrong offset; the odd backtick in a heading like ``### `rwv sync
  <source>` `` inverted backtick-span parity for the rest of the section; the
  parser read **zero** claims and reported nothing. A gate written specifically
  to catch "passes for the wrong reason" passed for the wrong reason, in its
  first hour.
- **The scope matcher recognised one module name.** `before_test_module` in
  `src/bin/generate-explain.rs` ended the scan at a literal `mod tests`, so
  `src/git.rs` — which names its test modules `branch_model_tests` and
  `derived_content_tests` — was scanned end to end. The freshly hardened gate
  then reported a comment the rule does not cover, and the two obvious ways to
  green were to edit a test-module comment to appease a buggy scope, or to ship
  the check disabled. Both are this section's subject.

The four the survey found, for the taxonomy:

- **one direction** — the cli.md gate compared documented flag *names* against
  `--help` and never checked anything else about them, so a documented flag
  *value* clap rejects was invisible. README advertised a `--strategy` value
  that hard-errors at parse time, through a green CI, on the first page a new
  user reads.
- **one syntactic form** — the citation gate required a `/` in a token before
  it would look at the token at all. Roughly eighty bare `<document>.md §N`
  citations sat unexamined behind that one character.
- **one spelling** — the tracker-ID matcher recognises the retired `fo-` prefix
  and not the one that replaced it. The live IDs that sat green behind it came
  out by hand; the successor prefix is this repo's own vocabulary namespace, so
  the clause above states the gate's one prefix rather than the gate growing to
  meet the rule.
- **one region of the file** — the citation gate stopped scanning at the first
  `#[cfg(test)]` module. The exclusion was deliberate and nothing re-examined
  it, so a non-resolving citation below that line stayed invisible for as long
  as it sat there: four did, three of them naming a document this repository
  has never contained. Those came out by hand and the gate now reads test
  modules — except in its own file, whose fixtures are comment text it cannot
  tell from comments. The exclusion is one file wide instead of every file,
  and nothing re-examines that one either.

Four habits, in the order they pay off:

1. **Ship a seeded-failure test with every check.** A fixture the check must
   report, asserting on the finding. Not "the tree is clean" — that passes when
   the check does nothing. Every instance above had a passing suite.
2. **Seed the failure in more than one place, and require every surface to
   react.** The corrupted-input pass was caught only because the bad value was
   planted in both README and `docs/reference/cli.md` and only one of the two
   tests went red. A single-plant check would have shown green and a dead gate
   would have shipped. One green is not evidence; the disagreement is.
3. **Pin non-vacuity separately, and permanently.** A check that walks a corpus
   and reports what it finds is indistinguishable, when green, from one that
   finds nothing because its parser broke. Assert the walk really yields the
   claims you think it does — a count asserted non-zero is the cheapest guard
   against this whole class, and every check here that iterates a parsed
   section wants one.
4. **Write down the residue next to the rule.** Not in the commit message —
   next to the prohibition, where the next person sweeping by hand will read
   it. If you cannot state what your check does not cover, you do not know.

When you widen a matcher, measure before you commit to the predicate. Run it,
read every site it reports, and separate the ones it is right about from the
ones it is merely loud about — the precision work is the design, and a matcher
that reports correct code will be turned off.
