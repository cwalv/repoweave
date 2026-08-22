# Weave root as presentation

The weave root is the one directory rwv arranges that is not a repository and
belongs to no repository. Everything in it is a member repo's checkout, a
symlink into the presented project's repo, a file rwv derives from committed
intent, or rwv's own pointer state. This joint states why that is a design
commitment rather than an accident — why the root is not a repo, and why files
reach it through links instead of living there — and the jurisdiction rule
that both answers imply. Two questions, one rule.

## What the root is for

Ecosystem tools resolve a workspace by location. Cargo walks up until it finds
the workspace `Cargo.toml` and expects `Cargo.lock` beside it; Go reads
`go.work` at the workspace root and writes `go.work.sum` next to it; an editor
opens the `.code-workspace` where it sits. The weave root exists so those
tools can see many repos as one workspace: it is the composition point, the
place where the presented project's view of its members is laid out in the
shape the tools demand.

Presentation is the operative word. The root presents **one project at a
time** — `.rwv-active` records which — and switching projects rewrites the
shared names at the root ([file-ownership](./file-ownership.md) states which
names are shared and which are per-project). The same project is also
presented again by every workweave, each with its own root. The root is a
view, and views multiply; what they are views *of* is versioned exactly once.

The presentation model is the oldest layer of the design — `activate` and the
weave image predate workweaves. Workweaves then absorbed part of activate's
job: a workweave presents its project unconditionally, so anyone who needs a
presentation that cannot switch out from under them takes a workweave rather
than activating the primary. What switching at the primary retains is the
ambient default: which project the root presents to a person or tool working
at the weave itself.

## How a root is recognised

The containment walk that finds a weave root from some directory inside it
tests one thing: **a weave root is a directory containing `projects/`.**
Nothing else — no registry-segment directory (`github/`, `local/`, …) counts
towards the test, and none is required for it either. A directory holding
only `projects/`, empty, is a weave root; this is not a special case carved
out for it — it is what lets `rwv init` bootstrap into an empty directory at
all, since the first thing it does is create `projects/` and then resolve
its own invocation before anything else exists.

A workweave is identified separately, by the `.rwv-workweave` marker file at
its root, not by the shape test above — a workweave directory also contains
`projects/` (its own worktree of the project repo), so shape alone cannot
tell a workweave from a primary weave.

This is the contract, not an approximation of one: a directory meeting the
shape is a weave root by definition. One consequence worth stating rather
than leaving implicit — first-ancestor-wins means a member checkout that
happens to contain its own top-level `projects/` directory (an unusual
tree, but not one the shape test can rule out) resolves as the weave from
inside it, and the real weave above it is never reached. That is the shape
test doing exactly what it is defined to do, not a bug in the walk.

## Why the root is not a repository

**Everything at the root already has an owner.**

| Path | Owner |
|---|---|
| `github/<org>/<repo>/` | that repo — its own history, its own remote |
| `projects/<name>/` | the project repo — committed intent (`rwv.toml`, `rwv.lock`) plus every surfaced file |
| root symlinks (`Cargo.toml`, `go.work`, …) | rwv — the delivery layer of surfacing |
| derived views the links resolve to | regenerated from committed intent by intent verbs |
| `.rwv-active`, `.workweaves/` | rwv — machine-local pointer state and the container of more views |

A repo at the root would be a repo whose every tracked path is either another
repository or derived state. The first half re-imports the nested-repo
ambiguity rwv exists to remove — every VCS tool run at the root would need
carve-outs for the member trees below it, and a cleanup command at the root
would reach into history it does not own. The second half versions outputs
alongside their inputs: the project repo already records the intent
(`rwv.toml`), the pin (`rwv.lock`), and the surfaced content the links resolve
to, and the root is reconstructable from those — `rwv fetch` then activation
rebuilds it from nothing. A root repo would be a second source of truth whose
every disagreement with the first is a new failure mode with no owner, for the
same reason a lockfile is derived rather than hand-held
([lock-as-derived](./lock-as-derived.md)).

The root's own resident state makes the same point from the other side:
`.rwv-active` is deliberately *unversioned* pointer state. "Which project this
machine presented when" is not project state — no clone of the project should
carry it, and no history of it means anything off this machine.

## Why links, and not files or copies

The tools dictate **where** files must appear: at the root. Version control
dictates **where** bytes must reside: in a repo — and the root is not one, so
a real file at the root is tracked by nothing, backed up by nothing, and
invisible to every verify pass. It evaporates with the weave.

The surfacing link is the adapter that satisfies both constraints with one
copy. The tool reads and writes at the root; the bytes land in
`projects/<project>/`, under version control, where a commit can see them.
The write-through direction is load-bearing, not incidental: a lock file an
ecosystem tool writes at the root must arrive in the committed copy, which is
why a dangling link is deliberately created for a file that does not exist yet
([symlinks-as-structure](./symlinks-as-structure.md)) and why a copy at that
path is a correctness failure — a second copy of a tracked file that nothing
reconciles.

Links are also the cheapest honest mechanism for a view. Minting one moves no
data; removing one destroys no data; the set of links is recomputable at any
time by diffing the presented project's declared names against the root. That
is what lets presentation switch, repair converge, and a workweave be built or
retired without ever copying project state.

## The jurisdiction rule

Both answers compress into one rule:

> **Nothing at the weave root is original.** Every path there is owned by a
> member repo, resolves into the project repo, or is rwv's own derived or
> pointer state. Jurisdiction over the *root* is therefore rwv's;
> authorship applies to *files*, never to links.

Three consequences:

- **A root symlink is rwv's artifact, unconditionally.** Creating or removing
  one needs no authorship adjudication, because removing a link destroys
  nothing — the file it resolves to keeps its owner and its bytes. A link at
  a name the presented project no longer declares is rwv residue, and rwv may
  offer to remove it (`rwv materialize --remove-undeclared-links`, consented
  per [destructive-operations](../destructive-operations.md)). The file
  behind such a link is never touched and never named as removable.
- **The file behind a link is governed by content ownership.** Who may write
  it, strip it, or delete it is [file-ownership](./file-ownership.md)'s Axis
  2, and none of those questions are answered at the root — they are answered
  in `projects/<project>/`, where the file actually lives.
- **A hand-placed file at the root is outside every channel.** No repo tracks
  it, no declaration regenerates it, no verify pass reports it, and a
  presentation switch or repair may clobber it. The supported way to put your
  own file at the root is the static-files integration: declaring the name is
  exactly what protects it, because every guarantee rwv makes at the root is
  keyed to declared names.

## Related joints

- [file-ownership](./file-ownership.md) — the two axes this joint's rule
  presumes: surfacing as the delivery layer, content ownership behind it.
- [symlinks-as-structure](./symlinks-as-structure.md) — why the link itself
  is the structural fact, and why copies, hardlinks, and warn-and-continue
  are closed.
- [clone-topology](./clone-topology.md) — the member repos' side of "every
  path has exactly one owner": one canonical store, linked checkouts.
- [lock-as-derived](./lock-as-derived.md) — the same inputs-not-outputs
  argument applied to lockfiles.
