# Changelog

This changelog starts at 0.23.0. Releases 0.4.0 through 0.22.0 shipped without
entries, and none are reconstructed here — the gap is a gap in the record, not a
claim that nothing changed in them.

## Unreleased

### Breaking: a malformed `hide-dotfiles` stops materialize and the install hooks

A `hide-dotfiles` that is not a boolean in `[integrations.vscode-workspace]` was
reported by `rwv doctor` as `malformed-settings`, but `rwv materialize` and the
install hooks ran past it, unlike a malformed setting of any other integration.
They now refuse over it, naming the key, and change nothing, so a wrong-typed
setting stops the same verbs whichever integration it is in. Write
`hide-dotfiles = true` or `hide-dotfiles = false`.

### Breaking: the stale-generation advisory's `kind` is `derived-state-stale`

An entry of the `advisories` array in `rwv sync --json` and `rwv doctor --json`
has `kind` `derived-state-stale`, the name of the doctor finding it repeats; it
was `derived_state_stale`.

### Breaking: `rwv.toml` refuses a `version` that reads as a tag or commit id

`version` declares the branch a member tracks. A value that reads as a release
tag (`v2.0`, `v0.3.4-rc1`) or a commit id (seven or more hex digits) used to
load, and `rwv fetch`, `rwv update` and `rwv push` each refused that member
when they reached it. The manifest now fails to parse at that line, naming the
value. `rwv add` refuses with `version-is-a-pin` when the branch it would
record is so named — the remote's default branch, or with `--new` the branch
the new repo is on — where it used to write it.

### Breaking: a `go-version` that is not a string is reported, not ignored

A `go-version` of the wrong type in `[integrations.go-work]`, such as
`go-version = 1.22` (a number), was read as if the key were absent, so `go.work`
was seeded from the members' `go` directives with no finding. It is now a
`malformed-settings` finding from `rwv doctor`, as a wrong-typed setting of
`cargo-workspace`, `static-files` and `vscode-workspace` already was, and
`go.work` is not authored while it stands: `rwv materialize` and `rwv activate`
refuse, naming the key. Write `go-version = "1.22"`.

### Breaking: `rwv status --json` spells the relation `no-lock`

A repo with no `rwv.lock` entry has `relation` `no-lock` in `rwv status --json`,
the spelling the table and the docs already used; it was `no_lock`.

### Breaking: `enabled` must be `true` or `false`

An `enabled` in an `[integrations.<name>]` table that is not a boolean, such as
`enabled = "false"`, used to be read as if the key were absent, so an
integration written off that way kept running at its default. It now fails the
manifest load like any other malformed key: every verb that reads the manifest
refuses it, naming the key, the table and the line, and `rwv doctor` reports the
project as `unparseable-project`. Write `enabled = true` or `enabled = false`.

### `rwv --version` leads with the release version on every build

A binary built inside a git checkout reported the checkout's `git describe` as
its version (`rwv v0.24.0-46-gae23167`, `rwv v0.24.0` at a release tag, a bare
commit id where no tag was reachable), while the same source built from an
archive reported `rwv 0.24.0`. The plugin envelope's `RWV_VERSION`, documented
as semver, and the version a clean `rwv doctor --all` records in the health
floor carried the describe string too. All three are now the release version
from `Cargo.toml` (`0.24.0`) on every build. A build inside a git checkout
prints its `git describe` after the version, in parentheses, as build
information: `rwv 0.24.0 (v0.24.0-46-gae23167)`. `rwv explain` opens its pages
with the same line.

A health-floor record whose version is not semver, as an earlier build wrote
when it was built from a git checkout (`v0.24.0`), reads as no record, and the
next clean `rwv doctor --all` writes a new one.

### Breaking: `rwv doctor` names a workweave by its project and name

A workweave name is unique only within its project. Every `rwv doctor --json`
finding that carries a `workweave` field now writes it as `<project>--<name>`,
the spelling `resolution.workweave` uses, where it wrote the bare name. Each
such finding's `absolute_path` lies in that workweave: with same-named
workweaves in two projects, a finding in one was placed in the other's
directory, at a path that need not exist. The text report and `--fix` name the
workweave the same way, and so do `rwv workweave delete` and `rwv sync-to
--retire` when they re-point a child workweave.

### `rwv prime` lists the integrations that run

The Integrations section of `rwv prime` named every `[integrations.<name>]`
table whose `enabled` key was not `false`, so a table without the key listed
`gita`, `pnpm-workspaces` or `static-files`, which stay off until `enabled =
true`, and an integration that runs by default was missing unless its table was
written. It now lists exactly the integrations that run for the project: those
with `enabled = true`, and those that run by default and have no `enabled =
false`.

### A member added by registry shorthand clones on every machine

`rwv add github/acme/x` records `url = "github/acme/x"` as typed and clones
from the URL the registry resolves it to, but every later clone of that member
handed git the shorthand and failed with `repository 'github/acme/x' does not
exist`: `rwv fetch` on a collaborator's machine, or after the clone was
removed, and `rwv sync` bringing the member into a weave without it. Every
clone now resolves a member's `url` through the built-in registries the way
`rwv add` does, and `rwv fetch` names the URL it clones from.

A member `url` that is a plain path refuses with `unresolvable-repo-source`
when a clone needs it, naming the `file://` spelling to write instead; git used
to be handed the path. An SCP-style URL on a host with no built-in registry
(`git@git.example.com:org/repo`) reads as a URL, not as `owner/repo`
shorthand, so `rwv fetch` and `rwv init --adopt` clone it as written rather
than a GitHub URL built from it.

### Breaking: relative path arguments are read from the directory rwv runs in

A relative path on the command line names the place it names in your shell, or
under `-C`/`-w`, in that directory. That holds for `rwv sync <source>`, `rwv
sync-to <target>`, `rwv workweave create --from`/`--dir`, `rwv workweave
set-container`, `rwv add --adopt <path>` and `rwv add --new --param root=<dir>`;
each used to read a relative path from the weave root, and `--param root`
refused one. `rwv remove <path>` and `rwv role <repo>` take a member as
`rwv.toml` lists it, not a filesystem path, and are unchanged.

`rwv add --adopt <path>` names the member by where the path lands: under the
primary weave root or under the workweave you run it from, the clone it
registers is the primary's at that member path. A path under neither refuses
with `adopt-path-outside-weave`; a clone of its own inside the workweave, at a
path where the primary has a different one, refuses with
`foreign-clone-in-workweave`, naming both.

### The install script needs `xz` only with GNU tar

`curl -fsSL https://cwalv.github.io/repoweave/install.sh | sh` refused on a Mac
with no `xz` on `PATH`, though macOS's tar unpacks the archive itself. It now
asks for `xz` only where `tar` is GNU tar, which hands the archive to it.

### Plugins start in the directory rwv runs in

A plugin starts in the `-C` directory, the `-w` workweave, or else the
directory rwv was run from, and on Unix its `PWD` names that directory, so a
relative argument means to a plugin what it means to a core verb. A plugin
found through a relative `PATH` entry still runs. `rwv each --results` and
`--joblog` with a relative path write under that directory.

### Member checkouts have their submodules

Every way rwv creates a member checkout (`rwv workweave create`, `rwv fetch`,
`rwv add`, `rwv sync`, `rwv sync-to`, `rwv doctor --fix`) checks out its
submodules, nested ones included, from what the canonical clone already holds,
never from the network, and places their files by reflink where the filesystem
supports it. A submodule whose recorded commit the canonical clone lacks stays
empty, and `rwv doctor` reports `uninitialized-submodule`, now with a
`sub_kind`: `fetched` (`--fix` checks it out locally) or `unfetched` (report
only). `submodules = "skip"` on a `[repositories."<path>"]` entry leaves a
member's submodules empty and unreported.

### Breaking: unknown keys in a `[repositories]` entry are refused

A key a `[repositories."<path>"]` entry does not define fails to load on every
verb that reads the manifest, naming the key, the way an unknown top-level key
does. It used to be ignored, so a misspelled `submodules` would have changed
nothing.

### `rwv add` refuses something other than a clone at the placement

A directory that is not a clone, empty or not, or a file at the placement
`rwv add` would use refuses with `non-clone-at-placement`, naming the placement,
before anything is cloned or written. It used to fail with a low-level VCS
error and no refusal name. `rwv add --adopt` refuses the same way when the
workweave's placement holds one.

### `rwv add --adopt` names the right repair for a remote with no branch

When the clone's `origin/HEAD` is unset, the refusal tells a clone that has
never fetched to `git fetch origin` first, then names `git remote set-head
origin -a` if the remote has a branch and the first push if it has none.

### `rwv workweave delete` leaves nothing behind for checkouts the manifest does not list

A checkout at member depth that the manifest does not name is checked and
cleaned up like a member: delete refuses when it hosts another directory's
worktree, unregisters its worktree, and retires the branch rwv made for it. A
member a workweave added and that workweave's deletion no longer leaves the
primary unable to `rwv remove <path> --delete` it.

### `rwv doctor --json` reports `safe_to_fix` as what `--fix` will do

`safe_to_fix` is `true` only for a finding `--fix` acts on. `config-rejected`
and `tool-missing` findings used to say `true` though `--fix` never repairs
them.

### `rwv lock --commit` names a dropped repo

A lock commit that drops a member's entry says so in its subject (`lock: drop
<path>`, or `lock: drop N repos`) and lists each dropped entry in its body.

### `rwv workweave log` names a missing checkout

A member checkout missing from the workweave or its parent is reported as
missing, naming `rwv fetch`, instead of as a failure to run git.

### `rwv each` ships with rwv

`rwv each` runs a command in every repo of the checkout you are in, in
parallel: `rwv each --role fork -- git checkout -b feat/x`. It is a plugin,
`rwv-each`, installed beside `rwv`. With no `--role` or `--repo` it runs in
every repo except reference repos; output lines are prefixed with their repo
(`--group` holds each repo's output until it finishes); every repo runs even
after one fails, and a summary names the failures; the exit status is 0 only if
every repo succeeded. A bare command name is found on `PATH` once, before any
repo runs, and every repo runs that program. `--json`, `--fail-fast`,
`--dry-run`, `--shell`, `--timeout`, `--joblog`, `--results` and `--progress`
are described in the `rwv each` reference. Ctrl-C and `--timeout` end a
command's whole process tree.

### Every release channel installs `rwv-each` beside `rwv`, and no longer `generate-explain`

The release archives, the Homebrew formula, the PyPI wheels, the install script
and `cargo install --git https://github.com/cwalv/repoweave` all carry `rwv-each` next to `rwv`. Earlier
archives, formulae and crates also installed `generate-explain`, a tool that
regenerates this repository's own documentation; it is no longer shipped.
After upgrading by hand, `rm` any `generate-explain` an earlier install left
beside `rwv`.

### rwv waits for a plugin through Ctrl-C

A Ctrl-C while a plugin runs no longer ends `rwv` ahead of the plugin. `rwv`
waits for the plugin to exit and reports the plugin's own exit status, so a
plugin that winds down on an interrupt is not cut off from the terminal and
its status is the one the shell sees.

### `ff-impossible` and `rebase-failed` always carry a `cause`

In `rwv sync --json` and `rwv sync-to --json`, a failed outcome whose
`failure.kind` is `ff-impossible` or `rebase-failed` always has
`failure.cause`, and the published schemas list it as required for those two
kinds. A consumer can read `failure.cause.kind` on them without an absence
check. `head-unreadable` still omits `cause` when it has none.

### Error messages keep their causes

Messages that relay an underlying failure now print its whole cause chain
rather than only its outermost line, so a failure reported by `rwv doctor
--fix`, an ownership-receipt warning from a workweave verb, or an integration
hook names what actually went wrong beneath the step that failed.

### `rwv status` takes `--role` and `--repo`

`rwv status` accepts the selector flags `fetch`, `update` and `push` take:
`--role <role>` and `--repo <selector>` (exact path, `re:<pat>`, or
`glob:<pat>`), repeatable and combined as a union. They narrow both the table
and the `--json` `repos` array. A filter that matches no repo reports nothing
and exits 0.

## 0.24.0

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
registered as before; a `file://` URL and the path it names, drive-letter or UNC
on Windows, count as the same repo.

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

### Breaking: `rwv doctor`'s exit code treats text and `--json` alike

Without `--kind`, `rwv doctor --json` exits non-zero only when a finding —
violation or integration issue — is error-severity, matching the text report.
It used to exit 1 on any violation, warnings included, so a weave whose only
finding was a warning such as `missing-merge-driver-config` failed `--json`
while the text report exited 0.

With `--kind`, a finding the filter admits exits non-zero regardless of
severity, in both modes — `rwv doctor --kind tool-missing` included. The text
report used to ignore `--kind` for its exit code and apply the error-only rule
even under a filter that matched a warning.

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
  parameters as one JSON object. A creation whose clone takes a detached HEAD
  from an upstream already under the root refuses, naming that upstream and the
  `git -C <upstream> symbolic-ref HEAD refs/heads/<branch>` that repairs it. New
  refusals: `missing-creation-param`, `unusable-creation-param`,
  `occupied-placement` and `provider-cannot-mint-url`.
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
  worktrees the canonical clone lacks, and lists each one: for ignored files it
  names `git clean -X -d -n` and `git clean -X -d -f`, and when the canonical
  clone is missing it names `rwv fetch --project <project>` with the
  workweave's project.
- `rwv doctor --kind` accepts integration issue kinds such as `tool-missing` and
  `managed-file-drift`, so `rwv doctor --kind tool-missing` fails a CI job whose
  install hooks could not run.
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
- New doctor findings:
  - `unknown-segment-clone`: a repo under a top-level directory that no
    registry and no manifest names, build output under `node_modules/` and
    `target/` excepted. It names `rwv add --adopt` with the path the clone's
    origin URL places it at, moving it there first.
  - `placement-disagreement`: a manifest path that its own URL does not derive
    to. It names moving the checkout to the path its URL places at, unless it
    is already there, re-keying the entry in `rwv.toml`, and `rwv lock`.
  - `lock-pinned-by-name`: a lock entry holding a tag or branch name, which
    `rwv status --json` also marks as `lock_pinned_by_name`.
  - `workweave-drift` sub-kinds for a member a workweave lacks or still holds
    as a symlink, and `extra` for a checkout a member removed by hand left
    behind.
  - `member-permissions`, and `role-disagreement`, a canonical clone the
    projects listing it disagree on.
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
  occupant named, a file standing where a directory belongs included, and no
  link changed. An intent verb whose regeneration fails before the install hooks
  puts `rwv.toml` and every file it rewrote back, and the error says so.
- A lock file an ecosystem tool wrote as a real file at the weave root, past its
  link, is taken into the project by `add`, `remove`, `update`, `materialize`
  and `doctor --fix` when the project's copy is absent, identical, at the
  generation rwv recorded, or committed and clean. Otherwise both copies are
  named and left alone, and `rwv activate` refuses to switch projects over it
  with `conflicting-root-write`.
- Re-running `rwv add` for an entry `rwv.toml` already names finishes it: it
  clones a missing canonical clone, creates the workweave's worktree and
  regenerates. A URL naming a different repo refuses with `occupied-placement`,
  naming `rwv remove <path> --delete`: in a workweave whose clone is on disk,
  there and then in the primary weave.
- `rwv remove <path>` finishes an entry already removed from `rwv.toml` by hand
  while this workspace still holds its checkout. `--delete` removes the checkout
  in the workspace the command runs in, the canonical clone at the primary weave
  or the worktree in a workweave, after the guards that refuse uncommitted
  changes, commits no other clone reaches and a store something else claims.
  In a workweave it also deletes the branch the workweave was given in the
  canonical clone, unless that branch holds commits the clone's checkout lacks.
  Its refusals name `rwv remove`, not `rwv sync`.
- A refused `rwv add --new` removes the bare upstream, member and directories it
  created, and says so. A refused `rwv add` names the clone or worktree it keeps
  for a re-run, with the command that removes it: in a workweave,
  `rwv remove <path> --delete` there for its worktree and then in the primary
  weave for its clone.
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

- A `no-remote-default-branch` refusal names a repair that works. `rwv push` and
  `rwv add --adopt` name `git fetch origin && git remote set-head origin -a`,
  no longer offering "push once with an explicit branch", which is what leaves
  `origin/HEAD` unset, and `rwv add --adopt` names itself as the re-run.
  `rwv add <url>` names the repair for its case, in the clone it keeps: for a
  remote with no branch yet, commit there, push the branch and record it with
  `git remote set-head origin <branch>`; for a remote whose HEAD names none of
  its branches, check out one of those it lists and record it the same way. It
  named `git remote set-head origin -a`, which fails in both.
- The lock-state-mismatch hint, and the `rwv explain push` page, name
  `rwv lock --commit` or `git reset --keep <lock commit>` in place of a
  `git checkout` of the lock commit, which detaches HEAD and is refused next.
- `rwv doctor --locked` names `rwv fetch` with no source for a member missing on
  disk, and a verb that refuses `missing-canonical-clone` for a workweave
  member names `rwv fetch --project <project>` with the workweave's project, run
  at the primary weave. Both named `rwv sync`, which takes a source.
- The `store-still-claimed` refusal over live worktrees names
  `rwv workweave <project> delete <name>`, the spelling the CLI accepts. It
  used to drop the project, which the CLI rejects.
- `rwv lock`'s detached-HEAD warning says a later `rwv fetch` attaches a
  branch at the pin. It used to say fetch would materialize the member
  detached, which is the opposite of what `rwv fetch` does.
- `rwv init <name> --provider <registry>/<owner>` refused as
  `malformed-provider`, `unknown-registry` or `provider-cannot-mint-url`
  creates nothing, so the plain `rwv init <name>` the refusal names succeeds. It
  used to leave the project directory behind, and the retry refused as
  `project-dir-occupied`.
- `rwv fetch <source>` refused as `project-dir-occupied` names
  `rwv fetch --project <name>` for the project already there, and an empty
  directory to fetch the other one into. It suggested a scoped project path,
  which no `rwv fetch` spelling can request.
- `tracked-index` names `git -C <project dir> rm --cached .rwv-workweave-index`
  and committing the removal, and says the index needs no `.gitignore` entry,
  since rwv excludes it through the project repo's `.git/info/exclude`. It
  named `git rm --cached <absolute path>`, which fails outside the project
  repo, and adding the index to `.gitignore`.
- A workweave checkout detached while its ephemeral branch exists, such as a
  review workweave holding a pull request's tip, reports
  `branch-discipline` / `detached-off-ephemeral-branch`. It names the
  `git switch <branch>` that reattaches it, and whether that moves the working
  tree. It reported `detached`, offering `--adopt-detached-checkouts`, which
  refuses to mint a branch that already exists; that flag now leaves such a
  checkout alone.
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
  `GITA_PROJECT_HOME` set to the weave root. On Windows each `repos.csv` path is
  spelled the way gita spells it, backslash-separated with no `\\?\` verbatim
  prefix. Files in the old format report `managed-file-drift` until
  regenerated.
- Disabling cargo-workspace and running `rwv materialize` removes rwv's entries
  from the project's `.cargo/config.toml`, the weave-root `.cargo/config.toml`
  link, and the weave-root `.cargo/` when that leaves it empty.
- cargo-workspace derives a `[patch]` entry wherever cargo's own semver rules
  accept the in-weave version. A plain requirement such as `2.4.0` against an
  in-weave `2.8.3` used to be skipped with a warning. The
  `# excluded: <path> (opted out)` comment is written again.
- go-work reports `member-incompatibility` only where Go enforces it (a member
  requiring go 1.21 or newer), and no longer doubles its marker comment when a
  second module joins a `use (...)` block.
- `rwv fetch <source>` sets the `rwv-ours` merge driver in the cloned project
  repo when its `.gitattributes` assigns it to `rwv.lock`.
- `rwv add github/<owner>/<repo>` for a repo not yet cloned clones from the URL
  the registry resolves, instead of passing the shorthand to git.
- `origin-url-mismatch` compares against the remote's configured URL, so a
  `url.<base>.insteadOf` rewrite no longer reports a mismatch.
- Clone-URL comparison decodes a percent-encoded `file://` URL, so a remote
  whose path holds a space no longer reads as a different repo.
- On Windows, a drive-letter path in either separator, or a UNC path, names the
  same remote as the `file://` URL for it, so `rwv doctor` no longer reports
  `origin-url-mismatch` for a clone whose origin records the remote's path
  while `rwv.toml` names it by URL. On every platform, a
  `file://server/share/…` URL no longer matches `https://server/share/…`.
- On Windows, the paths `rwv add` and `rwv remove` print carry no `\\?\`
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
