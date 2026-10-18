# Changelog

This changelog starts at 0.23.0. Releases 0.4.0 through 0.22.0 shipped without
entries, and none are reconstructed here — the gap is a gap in the record, not a
claim that nothing changed in them.

## Unreleased

### Breaking: `rwv add <path>` no longer adopts a clone

An argument that is neither a URL nor registry shorthand reads as a path, which
could name a remote to clone from or a clone to register, so `rwv add` refuses
it with `unresolvable-repo-source` and names both spellings: `rwv add
file:///abs/path` clones from a local remote, and the new `rwv add --adopt
<path>` registers the clone already at that path under its origin URL.
`rwv fetch <plain path>` refuses the same way, naming `file://` and
`rwv fetch --project <name>`.

`rwv add <url-or-shorthand>` that finds a clone of another repo already at the
placement refuses with `foreign-clone-at-placement`, naming `--adopt`. It used
to register that clone under the URL you typed. A clone of the same repo is
registered as before.

### Breaking: unknown integrations and settings keys in `rwv.toml` are refused

An `[integrations.<name>]` table whose name is not a built-in integration, or
that carries a key its integration does not read, fails to load on every verb
that reads the manifest, the way an unknown top-level key does. The error names
the line, the accepted names or keys, and the nearest one: `hide-dotfile` gets
"did you mean `hide-dotfiles`?". Such a table or key used to be ignored. An
unknown key inside `[integrations.cargo-workspace.members."<repo>"]` is reported
as `malformed-settings`.

### Breaking: `rwv workweave create` runs the install hooks

A new workweave runs its integrations' install hooks (`npm install`, `uv sync`,
`cargo fetch`) as part of create, so it needs no separate `rwv materialize`.
`--no-materialize` skips them. A hook that fails once the workweave exists
leaves the workweave in place and exits 1, naming `rwv materialize` inside it;
under `--hook-mode` and `--claude-hook` the same failure is a warning and create
exits 0. A failure before the hooks still rolls the workweave back.

### Breaking: writing verbs refuse to act from another project's directory

At a primary weave root, a writing verb run from inside `projects/<other>/`
while `.rwv-active` selects a different project refuses with
`cwd-in-inactive-project` instead of writing into the active project. The
message names `--project <other>`, `--project <active>` and `rwv activate
<other>`. The verbs are `add`, `remove`, `role`, `lock`, `update`, `push`,
`sync`, `sync-to`, `doctor --fix` and `fetch` with no source. `rwv status`
still warns and proceeds.

### Breaking: `rwv push` refuses uncommitted project state

`rwv push` publishes the project repo's last commit, so it refuses with
`dirty-checkout` while the project repo has a staged or unstaged change to a
tracked file, or holds an `rwv.toml` or `rwv.lock` it has never committed. It
refuses before any network call, under `--dry-run` too, and names
`rwv lock --commit`, `rwv update --commit` and a `git commit` in the project
repo. An `rwv update` without `--commit` followed by `rwv push` used to publish
the old lock.

### Breaking: `--commit` carries a changed `rwv.toml`

`rwv lock --commit` and `rwv update --commit` stage `rwv.toml` with the lock
when it is modified or has never been committed. `lock --commit` used to refuse
a modified `rwv.toml` and leave an untracked one out. The generated commit
message names `rwv.toml` when it is carried, and the new `-m, --message`
replaces that message on both verbs. A change to any other file still refuses
with `dirty-checkout`.

### Breaking: `rwv sync` and `rwv sync-to` exit 1 when a prune is refused

When replay cannot prune a member checkout the replayed manifest no longer
names, because it holds uncommitted work, commits no other clone has, or a
store something else claims, the op still runs to completion and clears its
state, then exits 1 with an error listing the checkouts left on disk. It used
to exit 0. The `--json` envelopes gain `complete`, true when the op ran its
last phase. `rwv sync-to --retire` refuses with `retire-unpruned-checkout`
instead of deleting the workweave with that checkout's work in it: remove the
checkout, then run `rwv sync-to --continue`.

### Breaking: every workweave member is a worktree

A `reference` member of a workweave is a worktree of the canonical clone on the
workweave's branch, like every other member, not a symlink to the canonical
clone. A workweave still holding a symlinked member is unmigrated: the verbs
that walk its members refuse with `workweave-drift`, naming
`rwv -C <workweave> doctor --fix`, which replaces the link with a worktree.
`rwv workweave create --worktree-references` is gone.

### Breaking: `reference` members are read-only

A `reference` member's tracked files carry no write bit, in the primary weave
and in every workweave; every other member's are writable by their owner. On
Windows the read-only attribute stands in for the write bits. Every verb that
writes member files sets their permissions as it goes, and `rwv doctor` reports
a member whose files disagree with its role as `member-permissions`, which
`--fix` repairs. An existing weave's `reference` members report
`writable-reference` until `rwv doctor --fix` sets their permissions.

### Breaking: `missing-role` leaves doctor's finding vocabulary

Nothing raised it: an entry with no `role` fails to parse and is reported as
`unparseable-project`. `rwv doctor --kind missing-role` refuses with
`unknown-finding-kind`, and the kind is gone from `doctor.json`.

### Breaking: the `[lock]` table is gone from `rwv.toml`

`[lock]` and its `forgo-tag-names` key are removed; `rwv.lock` always records
commit ids. An `rwv.toml` still carrying the table fails to load with an error
saying the table was retired. Delete it.

### Added

- `rwv role <repo> <role>` changes a member's role in the active project,
  regenerates the integrations' files and sets the member's file permissions
  for the new role. A change to `reference` refuses while the member has
  uncommitted changes.
- `rwv add --adopt <path>` registers the clone at `<path>` under the weave root,
  recording its configured origin URL. A path holding no clone refuses with
  `no-clone-to-adopt`.
- `rwv add --new` creates through the registry its address names. A
  `local/<owner>/<repo>` creation takes `--param root=<dir>`, an absolute
  directory outside the weave, makes a bare upstream there and clones the member
  from it, so `origin` is real from the start. `--params-json` passes the
  parameters as one JSON object. New refusals: `missing-creation-param`,
  `unusable-creation-param`, `occupied-placement` and
  `provider-cannot-mint-url`.
- `--no-materialize` on `rwv add`, `remove`, `role`, `update`,
  `workweave create`, `fetch` and `init --adopt` skips the install hooks;
  `rwv materialize` runs them later.
- Verbs say what they skipped. A `[skipped] core:` line names each side effect a
  run left undone (install hooks, integration files, weave-root links,
  activation), why, and the verb that performs it. `rwv fetch --json` and
  `rwv update --json` carry the same records in a `skipped` array.
- `rwv fetch --project <name>` fetches the members of a project other than the
  active one, in place, without changing `.rwv-active`.
- `rwv lock -m` and `rwv update -m` set the message of the `--commit` commit.
- `rwv doctor --fix` replaces a full clone standing in a workweave
  (`clone-topology` / `standalone-in-workweave`) with a worktree of the
  canonical clone. It changes nothing while the clone holds commits, files or
  worktrees the canonical clone lacks, and lists each one.
- `rwv doctor --kind` accepts integration issue kinds such as `tool-missing` and
  `managed-file-drift`. Under `--json`, any finding in the filtered view exits
  1, warnings included, so `rwv doctor --json --kind tool-missing` fails a CI
  job whose install hooks could not run.
- Member files are shared between workspaces of the same repo. A new worktree's
  tracked files are cloned by reflink from another workspace wherever the
  filesystem supports it, and written by git elsewhere; on Linux a `reference`
  member's read-only files can be hard-linked instead. Verbs that place files
  report `N reflinked, N hardlinked, N written` per checkout, and the `--json`
  output of `rwv fetch`, `update`, `sync` and `sync-to` lists a `placements`
  record per checkout.
- A `static-files` entry can read the primary's copy:
  `{ path = ".beads", from = "primary" }` links every workweave to the primary
  weave's `projects/<project>/<path>`, so state that every workweave shares has
  one copy.
- New doctor findings: `unknown-segment-clone`, a repo under a top-level
  directory that no registry and no manifest names, build output under
  `node_modules/` and `target/` excepted; `placement-disagreement`, a
  manifest path that its own URL does not derive to; `lock-pinned-by-name`, a
  lock entry holding a tag or branch name, which `rwv status --json` also marks
  as `lock_pinned_by_name`; `workweave-drift` sub-kinds for a member a
  workweave lacks or still holds as a symlink, and `extra` for a checkout a
  member removed by hand left behind; `member-permissions`; and
  `role-disagreement`, a canonical clone the projects listing it disagree on.
- `rwv doctor` checks gita: a missing or stale `repos.csv` or `groups.csv`
  reports `managed-file-missing` or `managed-file-drift`, and `rwv doctor --fix`
  writes them.
- `rwv sync` and `rwv sync-to` report the members replay kept, pruned or could
  not prune in a `members` array; under `-j N` they stream as `member-record`
  lines, and `sync-to` streams each target advance as an `advance-record` line.

### Changed

- `rwv fetch` with no source, no project active and exactly one project in the
  weave activates that project after fetching it. A first `rwv fetch --frozen
  <source>`, or one filtered by `--role` or `--repo`, activates the project it
  creates, as an unfiltered one does.
- A hook whose tool is not on `PATH` is a `tool-missing` warning naming the tool
  and `rwv materialize`, not a failure, and the other integrations' hooks still
  run. A tool that runs and fails is still an error.
- Install tools' output goes to stderr, so the stdout of `--json` and
  `--hook-mode` stays machine-readable while hooks run.
- cargo-workspace's `rustc-wrapper` key is fitted at every materialization
  (`materialize`, `activate`, a fetch's first activation and `workweave
  create`): written where the wrapper is on `PATH`, removed where it is not.
- `rwv workweave create --capture-dirty` moves the captured `rwv.toml` and
  `rwv.lock` edits: once the workweave exists, the source's copies go back to
  their committed versions, so a later `rwv sync-to` from the workweave does not
  refuse on them. Other uncommitted files stay in the source.
- `rwv workweave create` copies the untracked files an enabled integration
  manages, such as a gitignored `.cargo/config.toml`, from the source into the
  new workweave.
- Replay prunes only the members the source dropped. A member added on this side
  that the source has not seen is kept, with a line saying so; it used to be
  pruned during its own landing. Pruning runs after every member repo has
  replayed, so a parked op removes nothing.
- Surfacing is all-or-nothing: an occupant at any link path refuses with every
  occupant named and no link changed. An intent verb whose regeneration fails
  before the install hooks puts `rwv.toml` and every file it rewrote back, and
  the error says so.
- A lock file an ecosystem tool wrote as a real file at the weave root, past its
  link, is taken into the project by `add`, `remove`, `update`, `materialize`
  and `doctor --fix` when the project's copy is absent, identical, at the
  generation rwv recorded, or committed and clean. Otherwise both copies are
  named and left alone, and `rwv activate` refuses to switch projects over it
  with `conflicting-root-write`.
- Re-running `rwv add` for an entry `rwv.toml` already names finishes it: it
  clones a missing canonical clone, creates the workweave's worktree and
  regenerates. A URL naming a different repo refuses with `occupied-placement`.
- `rwv remove <path>` finishes an entry already removed from `rwv.toml` by hand
  while this workspace still holds its checkout. `--delete` removes the checkout
  in the workspace the command runs in, the canonical clone at the primary weave
  or the worktree in a workweave, after the guards that refuse uncommitted
  changes, commits no other clone reaches and a store something else claims.
  Its refusals name `rwv remove`, not `rwv sync`.
- A refused `rwv add --new` removes the bare upstream, member and directories it
  created, and says so. A refused `rwv add` names the clone or worktree it keeps
  for a re-run, with the command that removes it.
- `rwv add` reports `member-incompatibility` when the member it adds needs more
  than the workspace file pins, as `rwv update` does.
- rwv refuses to write a managed file whose generated content does not parse
  back, leaving the file as it was. The refusal arrives as an integration error.
- A sync whose op-start relock cannot commit refuses before any work and
  restores `rwv.lock`.
- A verb waits up to 10 seconds, not 2, for another rwv to release the workweave
  index or the owned-digest ledger before refusing with `state-claim-held`.
- `rwv abort` labels the lines for the op's other workspace `[source]` or
  `[target]`, and its help summary says it rolls back every workspace the op
  touched.
- A refused prune's remedy reads "then remove the checkout by hand" in place of
  "re-run sync".
- `rwv fetch <source>` with another project active names `rwv activate
  <fetched>` when it skips the activation.
- `rwv explain` opens every page with `rwv <version>`, the line `rwv --version`
  prints, and the committed explain pages carry no version stamp.
- A weave root is a directory holding `projects/`. A directory holding only
  registry directories such as `github/` no longer resolves as one.
- Help text and messages say "member repo" and "weave root" throughout, in
  American spelling.

### Fixed

- The unset `origin/HEAD` hint no longer offers "push once with an explicit
  branch", which is what leaves it unset; it names
  `git remote set-head origin -a`.
- The lock-state-mismatch hint, and the `rwv explain push` page, name
  `rwv lock --commit` or `git reset --keep <lock commit>` in place of a
  `git checkout` of the lock commit, which detaches HEAD and is refused next.
- `rwv doctor --locked` names `rwv fetch` with no source for a member missing on
  disk, and a workweave verb that finds no canonical clone names the same fetch,
  run at the primary weave. Both named `rwv sync`, which takes a source.
- `rwv remove --delete` regenerates the integration files as they stand once the
  checkout is gone, so the `.code-workspace` excludes name no deleted path and
  doctor reports no drift.
- `rwv workweave delete` weighs every checkout under the workweave, including
  ones no manifest member names, before removing it: uncommitted work refuses
  with `uncommitted-work` and commits no baseline holds with `unmerged-commits`,
  each listed as "(not a manifest member)". `create --replace-existing` and
  `sync-to --retire` cover the same checkouts. They used to be deleted
  unweighed.
- `rwv abort` run from the target of a parked `sync-to` restores both
  workspaces, as it does from the workweave. It used to refuse the workweave's
  repos as `foreign-tip`.
- `rwv sync-to` into a workweave that lacks a member its lock pins creates the
  member there, instead of parking in replay.
- A claim on the workweave index or the owned-digest ledger that rwv cannot
  delete when it is done is retried, and if the delete keeps failing the verb
  fails with an error naming the claim file to delete, even when its own work
  succeeded. The file used to stay behind silently, and every later write
  refused on it.
- `rwv materialize` refuses up front, changing nothing, while an enabled
  integration's settings block is malformed. It used to drop owned-file
  attestations before refusing at the hooks. `rwv doctor` reports such a block
  once, and stops reporting the integration's own weave-root links as undeclared
  while it is malformed.
- `workspace-package = true` under `[integrations.cargo-workspace]` writes a
  `Cargo.toml` cargo can parse; the marker comment sits on its own line above
  `[workspace.package]`.
- The gita integration writes `repos.csv` and `groups.csv` in the headerless
  format gita reads, so `gita ll <role>` and `gita super <role>` work with
  `GITA_PROJECT_HOME` set to the weave root. Files in the old format report
  `managed-file-drift` until regenerated.
- Disabling cargo-workspace and running `rwv materialize` removes rwv's entries
  from the project's `.cargo/config.toml`, the weave-root `.cargo/config.toml`
  link, and the weave-root `.cargo/` when that leaves it empty.
- `rwv fetch <source>` sets the `rwv-ours` merge driver in the cloned project
  repo when its `.gitattributes` assigns it to `rwv.lock`.
- `origin-url-mismatch` compares against the remote's configured URL, so a
  `url.<base>.insteadOf` rewrite no longer reports a mismatch.
- `rwv add github/<owner>/<repo>` for a repo not yet cloned clones from the URL
  the registry resolves, instead of passing the shorthand to git.
- Clone-URL comparison decodes a percent-encoded `file://` URL, so a remote
  whose path holds a space no longer reads as a different repo.
- cargo-workspace derives a `[patch]` entry wherever cargo's own semver rules
  accept the in-weave version. A plain requirement such as `2.4.0` against an
  in-weave `2.8.3` used to be skipped with a warning. The
  `# excluded: <path> (opted out)` comment is written again.
- go-work reports `member-incompatibility` only where Go enforces it (a member
  requiring go 1.21 or newer), and no longer doubles its marker comment when a
  second module joins a `use (...)` block.
- An activation error over a file standing where a directory belongs names the
  file, on Windows as on Unix.
- On Windows, the paths `rwv add` and `rwv remove` print, including a refused
  add's "removed …" and "kept the clone it made at …" lines, carry no `\\?\`
  verbatim prefix. Unix output is unchanged.
- On Windows, `rwv --help` and the `plugins` array of `rwv doctor --json` name
  a plugin by the verb that runs it: `rwv-lint.exe` is listed as `lint`, not
  `lint.exe`. A `rwv-*` file is a plugin when its extension is in `PATHEXT`, and
  two such files for one verb in one directory are reported as duplicates, the
  one dispatch runs winning in `PATHEXT` order. Unix listing is unchanged.

### Documentation

- The branch-model explanation pages, `branches` and `pyramid-of-stability`,
  are withdrawn from the book until the branch model lands. Pages that linked to
  them point at `lock-as-derived` and `workweave-lifecycle`.
- The reference pages (`cli.md`, `refusals.md`, `doctor-findings.md`, the
  glossary, formats, roles and the plugin protocol), the explanation pages and
  the README describe this release's binary, including every refusal and
  finding above.
- `rwv explain state-claim-held` says to delete the claim file the refusal
  names; `rwv doctor` does not repair a stale claim.
- The integration pages follow one template and describe the shipped behavior.
- The tutorial, the how-to recipes and `adjacent-tools.md` run as written.
- The Concepts pages are grouped into Layout & files, Workweaves & branches, and
  Lock & sync. `sync-semantics` keeps the operator concepts; sync's per-phase
  contract moved to the maintainer documentation.
- `rwv fetch --help` no longer says `--json` without `-j` emits an envelope:
  without `-j` the job count is min(nproc, 8). `rwv lock --help` names
  `rwv materialize`, and `rwv setup claude --help` names every hook it
  registers.

## 0.23.0

### Refusals carry stable names

When `rwv` declines to act on purpose, it now prints a stable kebab-case token
for the *condition* and ends the message with `rwv explain <token>`. The token
names the condition rather than the message, which may be reworded, or the site,
which may move — so what you looked up once stays lookupable.

- `rwv explain <token>` serves that token's entry. The entry is sliced out of the
  published page rather than kept as a second copy, so the terminal and the site
  cannot drift into two spellings.
- `docs/reference/refusals.md` is new and carries one entry per refusal-only
  token, 78 of them. A condition `rwv doctor` also reports keeps its single entry
  on `docs/reference/doctor-findings.md`: one condition has one name and one
  entry, whichever surface you met it on.
- `rwv explain` also serves the VCS wire kinds and the sync failure kinds
  `ff-impossible` and `rebase-failed`. `rwv sync --json` published those and
  nothing explained them.
- The suggestion hint for an unrecognized name now spans tokens as well as verbs.

### Breaking: three tokens leave the `--json` VCS wire surface

`branch-already-exists`, `worktree-exists` and `uncommitted-changes` are gone
from `VcsErrorOutput`. A consumer branching on `cause.kind` can no longer receive
them — it never could. Each was published, schema-conformant and
conformance-tested, and no production path constructed any of them.

The conditions themselves are still reported, and always were, under other
names. A `worktree add` onto a taken path and a git call that fails on a dirty
tree come back as `command-failed` carrying git's own stderr, and
`dirty-checkout` names the dirty-tree condition on the refusal register.

### Fixed

- `rwv activate` no longer dies with a `rwv doctor --fix` remedy when an
  `[integrations.<name>]` block does not parse. It refuses the install hooks up
  front and tells you to edit `rwv.toml`. The old advice pointed at a
  regeneration that has to read the block it had just failed to read.
- Malformed integration settings are their own condition, `malformed-settings`,
  on every verb that meets them. They used to surface as `integration-failed` —
  the runner's one catch-all kind, naming neither the field nor a remedy — and
  `rwv activate` was the last verb still routing them there. `--fix` no longer
  offers a repair for them, because the repair is an edit to `rwv.toml`.
- `rwv add <path> --new --role <role>` records the role you asked for. It parsed
  `--role` and wrote `owned` regardless.
- Errors from `rwv sync`, `rwv sync-to` and the workweave phase transitions reach
  you whole. Ten printing sites rendered only an error's top sentence and dropped
  the context beneath it — including git's own account of why it refused. They
  print the full chain now.
- `rwv sync --json` carries a typed cause on the `ff-impossible` arm. The arm had
  the `VcsError` in hand and discarded it, so a consumer following the documented
  advice to branch on `cause.kind` instead of parsing the message got nothing to
  branch on.
- `untracked-collision` is reported only for untracked collisions. A fast-forward
  refused because a *tracked* file was modified was reported under that name,
  carrying a remedy — move the file — that does not apply to it. The obstructing
  set is now computed instead of parsed back out of git's refusal text, which
  also covers hosts that disable `advice.commitBeforeMerge` (GitHub's macOS
  runner image among them); there the parse recovered nothing and a collision
  arrived as `command-failed` with no path and no `--continue` hint.
- `rwv doctor` sees members outside the built-in registry segments. `rwv add`
  mints the first path segment from the source URL — `local` for `file://`, the
  URL's own host for anything else — while the disk scan walked three hardcoded
  directories. A member outside those three was invisible to every scan keyed on
  that walk: its manifest entry reported as `dangling-reference` advising
  `rwv fetch`, which cannot clear it, and its lock was never compared against its
  HEAD. The walk set is derived from what the manifests name.
- `unreadable-owned-state` advises a repair that works. In a project with no
  cargo workspace, `rwv materialize` exited 0 and left the record byte-identical;
  it rebuilds the ledger there now. Separately, the finding walks every project
  while the repair reaches only the presented one, so the advice names the
  activation route when the affected project is not the active one.
- A marker carrying `primary:` with no usable `project:` no longer reports as
  auto-fixable and then errors under `--fix`. It reports unreadable, with a
  detail that says why.
- `Error: Error:` is gone. Four `rwv add` / `rwv remove` messages spelled their
  own decoration in front of the one the reporter adds, and `rwv fetch`'s
  occupied-project refusal reached stderr twice.
- `rwv explain <unknown>` no longer answers with a suggestion that replaces the
  whole input. The threshold was a flat edit distance of 2, which puts the
  two-character token `io` within reach of any name four characters or shorter —
  and since a suggestion is checked before the external-command fallback, an
  operator running a real plugin verb was told about `io` instead of being
  pointed at their own tool.
- An undeclared-link finding spells its target the way `rwv` spells it, so the
  path in the message can be pasted back into the manifest the name came from.
  On Windows the two halves of that sentence disagreed.
- The refusal over a persisted `strategy="merge"` op-state names its exit —
  `rwv abort`, then re-invoke. That path existed only in a source comment.

### Documentation

- `docs/reference/refusals.md`: new, one entry per refusal token.
- `docs/reference/doctor-findings.md`: one servable entry per integration kind.
- `--remove-undeclared-links` is documented in the surfacing how-to. The flag
  already shipped in 0.22.0; only its documentation is new.
- Registry documentation corrected on four points: `RepoUrl` has no `File`
  variant; registries are fixed at build time rather than user-configurable;
  `--new` takes a canonical path and derives the URL from it, not the reverse;
  and `rwv.toml` is TOML, not YAML.
- Anchors under `docs/` are checked, and the three that did not resolve are
  repaired.

### Internal

- Blocking CI runs `cargo test --no-fail-fast`, so a red leg reports every
  failure instead of a lower bound on them.
- The internals-on-operator-surfaces gate covers every page `docs/SUMMARY.md`
  lists, not only the two generated directories.
- Test and census work across the refusal register, doctor findings, the skip
  audit and the lock-freshness fixtures. No operator-visible effect.
