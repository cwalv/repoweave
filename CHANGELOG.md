# Changelog

This changelog starts at 0.23.0. Releases 0.4.0 through 0.22.0 shipped without
entries, and none are reconstructed here — the gap is a gap in the record, not a
claim that nothing changed in them.

## Unreleased

### `unrecognized-savepoint-ref` prints a deletion that runs, once per clone

The finding advised `git update-ref -d <ref>` without naming a repository, so
run from the weave root, where `rwv doctor` runs, it failed or deleted nothing.
It now prints `git -C <repo> update-ref -d <ref>` with the repository's
absolute path. A ref seen by the primary and its workweaves, which share one
ref store, is reported once instead of once per checkout.

### `rwv each` with no project selected names `rwv activate`

At a primary with no project selected, including the state `rwv doctor --fix`
leaves after removing an unreadable `.rwv-active`, `rwv each` refused with
"no workspace resolved" and advised `-C <path>` or `-w`, neither of which
selects a project. It now refuses with "no project resolved" and names
`rwv activate <project>`. Plugins in that state receive `RWV_WORKSPACE`
without `RWV_PROJECT`, where before they received neither, and the
`resolution` block in `--json` output carries `workspace` without `project`,
where before it was absent.

### A `go.work` that cannot be read stops `go-work` activation

When the `go.work` at the weave root could not be read, `go-work` treated it as
having no `use` entries, so entries for repos no longer in the project stayed
in it. The unreadable file is now an error that names it.

### `rwv doctor --fix` deletes the branches of seats removed by hand

A seat's directory deleted by hand leaves its worktree registration in the
store, and git refuses to delete a branch a registration holds even when its
directory is gone. `rwv doctor --fix` dropped those registrations only in
stores a manifest lists, so in any other store each safe stale ephemeral
branch the seat left failed with git's "used by worktree" error. Before its
first delete in a store, `--fix` now drops that store's registrations of
removed worktrees, listed in a manifest or not, and prints a line for each.
A locked registration stays, with the branch it holds.

The `stale-worktree-registration` repair no longer drops a registration whose
path holds a directory again; it is reported instead.

### A member a workweave's sync prunes no longer keeps the primary's clone claimed

When `rwv sync` or `rwv sync-to` pruned a member's checkout in a workweave,
it removed the worktree but left the branch the workweave was given in the
primary weave's clone, and rwv's ownership receipt for it. `rwv remove <path>
--delete` in the primary then refused with `store-still-claimed` for as long
as the workweave existed. The prune now releases the clone as `rwv workweave
delete` does: the branch is deleted, with its receipt, once the workweave's
parent or the primary weave holds its commits. A branch holding commits
neither holds stays, and a warning names it.

`rwv remove <path> --delete` in a workweave releases the clone the same way.
It deleted the branch when the clone's own checkout held its commits or when
nothing had been committed on it since rwv created it; it now deletes it when
the workweave's parent or the primary weave holds its commits, and otherwise
keeps it with a warning. That warning reads as `rwv workweave delete`'s does.

`rwv workweave delete` no longer follows its warning about a branch it keeps
with a second line calling the same branch one rwv did not record.

### `rwv remove --delete-shared-clone` without `--delete` is a usage error

`rwv remove <path> --delete-shared-clone` without `--delete` is refused as a
usage error, exit 2, naming the missing `--delete`. v0.24.0 accepted the
combination and went on to look the path up in the manifest.

### `reserved-repo-path` refuses a repo path of `(project)`

A repo path of `(project)` is refused with the new refusal `reserved-repo-path`.
`(project)` is how the project repo is named wherever a repo is a key, so a
member at that path could not be told from it. The refusal is listed in
`docs/reference/refusals.md`.

### A new project starts with no integration warnings

`rwv init <name>` printed two warnings about the project's
`<name>.code-workspace` as soon as it created the project, because nothing had
written that file yet. It now writes the integration files an empty manifest
implies before it activates the project, so a fresh project, and the `init`,
`add`, `fetch`, `activate` sequence that follows, prints no warning and leaves
`rwv doctor` nothing to report. `rwv init --adopt` still writes none: the
adopted project's members are not on disk until `rwv fetch`. A missing
`.code-workspace` is reported once rather than twice, by `rwv doctor` and every
other verb that reports it.

### `rwv materialize` reports missing and drifted integration files

`rwv activate` and `rwv workweave create` warned about an integration file that
was missing or had drifted, and `rwv materialize` said nothing. All three now
run the same report and word it the same way, naming `rwv doctor --fix`. It
runs after the install hooks, so a lock file the hooks write is no longer
reported missing just before they write it.

### A member adopted outside a registry directory is found on disk

`rwv add --adopt vendor/other` recorded the member, but `rwv doctor` then
reported it as a dangling reference and advised `rwv fetch`, which skipped it
because the clone was already there. Only checkouts under a registry directory
such as `github/`, at `<registry>/<owner>/<repo>` depth, were counted as on
disk. Every member a manifest names is now found at its recorded path, whatever
its top-level directory and depth, so its lock and branch state are checked
like any other member's. A named member missing from disk is still a dangling
reference, and `rwv fetch` clones it back to that path.

### Breaking: `--json` prints one envelope whatever `-j` is

`rwv fetch`, `rwv update`, `rwv push`, `rwv sync`, `rwv sync-to` and `rwv each`
printed one JSON envelope under `-j 1` and switched to NDJSON, one
self-describing record per line as each repo finished, under `-j N` with
`N > 1`. `fetch`, `update` and `each` default to `min(nproc, 8)` workers, so
`--json` without `-j` streamed on a multi-core machine and printed an envelope
on a single-core one. Each verb now prints its envelope under every `-j`, once
it is done, with its per-repo records in manifest order.

The stream is gone, with no flag to bring it back, and so are the nine schemas
that described its lines: `fetch-record.json`, `update-record.json`,
`push-record.json`, `sync-record.json`, `sync-to-record.json`,
`member-record.json`, `advance-record.json`, `placement-record.json` and
`each-record.json`. To follow repos as they finish, run the command through
`rwv each` with `--joblog FILE`, one line per repo as it finishes, or
`--results DIR`, each repo's output and outcome. The `-j` defaults are
unchanged.

### `detached-off-ephemeral-branch` names a reattach that keeps HEAD's commits

A workweave checkout detached while its ephemeral branch exists was told to
run `git switch <branch>` whatever the two held. When HEAD carried commits the
branch lacked, as in a review workweave after a commit on the pull request's
tip, the switch left them on no branch, reachable only through the reflog. The
advice now depends on how HEAD stands against the branch, reported as
`ancestry` in `rwv doctor --json`: `git switch <branch>` where the branch
carries HEAD; `git switch <branch> && git merge --ff-only <HEAD>`, which
fast-forwards the branch, where HEAD carries the branch; and, where the two
have diverged, `git switch -c <new-branch>` to keep HEAD's commits on a branch
first, then a rebase onto the workweave's branch and a fast-forward of it.

### On Windows, `rwv explain` and `rwv prime` print LF line endings

The pages `rwv explain` and `rwv prime` print are built into the binary. The
Windows build was made from a checkout under Git for Windows' default
`core.autocrlf=true`, so it carried those pages with CRLF line endings while
the rest of rwv's output used LF. The repository's files now check out with LF
on every platform, so these commands print the same bytes everywhere.

### The documentation site publishes releases only, each at its own address

The site at https://cwalv.github.io/repoweave/ was rebuilt from the main branch
on every push, so it could describe behavior no installable release has. It is
now built from release tags alone, after each release. Every release's
documentation stays at `/vX.Y.Z/`, and every page has a selector for switching
versions. An address with no version in it, the site root included, opens the
same page in the highest stable release: `/tutorial.html` opens
`/vX.Y.Z/tutorial.html`. `/install.sh` is that release's installer.

### A `\` in a surfaced name is part of the name on Unix

A `static-files` entry or `[workweave] link` whose name holds a `\` was read
back from the weave root as though the `\` separated directories. `rwv doctor`
reported its link as undeclared, `rwv materialize --remove-undeclared-links`
removed it, and switching to another project left one in a subdirectory
behind. On Unix the link is now recognized under the name it was declared
with.

### A `\` in a checkout's directory name is part of the name on Unix

On Unix, `\` is an ordinary character in a filename, but the scan that finds
checkouts at member depth read a clone at `github/owner/a\b` as
`github/owner/a/b`, a directory that is not there, and read a name that is not
Unicode with a replacement character. A clone whose name no member path can
hold is now left out of the scan. The owned-digest ledger likewise recorded a
path dependency at `a\b` under the same key as one at `a/b`; each now has its
own.

### `rwv add --adopt` refuses a directory name no member path can spell

Adopting a clone whose path below the weave root passed through a directory
name holding a `\` refused with `backslash-in-repo-path`, whose advice is to
spell the path with `/`. A directory name that is not Unicode was read with a
replacement character, so the adopt looked for a clone that is not there and
refused with `no-clone-to-adopt`. Both now refuse with
`unspellable-directory-name`, which names the directory and which of the two it
is, and whose exit is to rename the directory and adopt the clone by its new
path. rwv does not pick the new name.

### `rwv sync-to <target> --retire` counts the target it landed into

A workweave landed into a target other than its recorded parent, by naming the
target, could not be retired that way. `rwv sync-to <target> --retire`
completed the landing, then refused with `unmerged-commits` because its delete
counted only the recorded parent and the primary as holding the work, and the
op stayed parked at the retire phase until `rwv abort`. The delete now also
counts the target the op has just synced into. A bare `rwv sync-to --retire`,
whose target is the recorded parent, behaves as before.

### `cargo-patch-shadowing` reads the project's own weave-level patches

`rwv doctor` read the weave-level `[patch]` tables for `cargo-patch-shadowing`
from `Cargo.toml` and `.cargo/config.toml` at the weave root, which are the
active project's files. For any other project the finding compared its members
against the active project's patches. It now reads `projects/<project>/`, and
`weave_config` names that file, as the derived-patch precheck already did.

### A `.rwv-active` that names no project refuses

A `.rwv-active` that was empty, unreadable, or held text that is not a project
name read as "no project selected", so a verb that takes its project from the
pointer acted on that guess. It now refuses with `unreadable-active-project`,
naming the file and the repair: `rwv activate <project>` replaces it, and
`rwv doctor --fix` removes it. `rwv doctor` reports it under the same token. A
bootstrap `rwv fetch <source>` into a directory holding one refuses before it
clones, where it used to activate over it. `rwv activate`, `abort`, `resolve`
and `-w` addressing no longer read the pointer at all, and a verb given
`--project` does not consult it.

### `rwv doctor` text names a commit by its 12-character abbreviation

`rwv doctor` and `rwv doctor --locked` printed full 40-character commit ids
where every other text surface prints the first 12 characters: the
`stale-lock` and `--locked` drift lines, `lock-sha-unreachable`, the
`branch-discipline` findings for a detached checkout or a leaked branch, and
the commits `--fix` names when it adopts or strands a branch. They now print
the abbreviation; a tag or branch name a lock pins is still printed in full.
`rwv doctor --json` keeps every full id.

### `rwv add --adopt` follows a symlink in the path it is given

The weave root a clone is adopted under, and the member path recorded for it,
come from where the path leads with its symlinks followed. A `..` after a
symlink used to be folded against the spelling, so `sym/../github/o/r` named
the weave's own `github/o/r` even when `sym` points elsewhere and the system
reads it as another directory's; it now names the clone the system does, and
refuses with `adopt-path-outside-weave` when that is outside the weave. A path
that reaches the weave through a symlinked spelling of its root is adopted
under that root rather than refused.

### Install hooks run the file `PATH` finds

On Windows a hook looked for `npm.cmd` or `pnpm.cmd` by name, so a tool
installed as an `.exe` (the standalone `pnpm` installer ships one) was reported
as `tool-missing` and its hook skipped. A hook now looks the tool up the way the
shell does, honoring `PATHEXT`, and runs the file it found, `.exe` or `.cmd`.
That file is also the one a relative `PATH` entry names from the directory you
ran `rwv` in: the hook used to read the entry again from the weave root, find
nothing, and fail the activation over an installed tool.

### Project and workweave names are checked where they arrive

`rwv fetch <source>` refuses a source whose repository name is not a legal
project name (`unrenderable-name` or `invalid-ref-name`) before it clones; it
used to create `projects/<name>` for a name `rwv activate` then refused. A
`.rwv-workweave-index` that records a name no workweave can have is reported
by `rwv doctor` as `unreadable-workweave-index`, naming the name; the entry
used to count as recorded while `-w` addressing and doctor's live-branch
checks passed over it. In `rwv status --json` and
`rwv doctor --json`, the schemas describe `role` as one of `owned`, `fork`,
`dependency` and `reference` rather than any string; the values are
unchanged.

### A path argument is absolute by the host's rule

A path given to `-C`, `rwv add --adopt`, `rwv add --new --param root=`,
`--params-json`, `rwv sync`, `rwv sync-to`, or `rwv workweave create --from`,
`--dir` or `set-container` is absolute when this host reads it as absolute, and
is otherwise read from the directory rwv runs in. `--param root=C:\x` on Linux
used to pass as absolute because it looked like a Windows path; it now names
`C:\x` under the directory rwv runs in, as any other relative path does. An
absolute Windows path in the verbatim spelling (`\\?\C:\x`) arrives as `C:\x`.
A refused `rwv add --adopt` names its re-run with the absolute path the argument
was read as.

### Every absolute path in `--json` is `/`-separated, and the schemas say so

On Windows, four `--json` paths reached the output in the host's spelling,
backslashes and all: a member's `absolute_path` in `rwv sync` and `rwv sync-to`,
the project repo's `absolute_path` in `rwv push`, the parent `path` in
`rwv status`, and the managed file's `path` in a `member-incompatibility` issue
from `rwv doctor`. They are now spelled as every other published path is:
absolute, `/`-separated on every platform. The published schemas describe each
such field as a `WirePath` with that spelling. Unix output is unchanged.

### Breaking: `canonical-holds-live-workweave-ref` names its workweave as `<project>--<name>`

In `rwv doctor --json`, a `branch-discipline` /
`canonical-holds-live-workweave-ref` finding carries `workweave`, the
`<project>--<name>` identity other findings carry under that key; it carried
`workweave_name` with the bare name, which two projects can share. The text
output names the workweave the same way.

### Breaking: a commit a doctor finding cannot name is `null`, not `""`

In `rwv doctor --json`, a `branch-discipline` / `detached` finding whose
pre-flat branch names no commit carries `legacy_branch.tip_sha: null`; it was
`""`. Every other `at_sha`, `branch_tip` and `tip_sha` is always a full commit
id. The published schemas describe a commit id as 40 lowercase hex digits (64
in a SHA-256 repository) rather than any string: in `doctor.json`, and for the
`tip` in `status.json`, whose values are unchanged.

### Abbreviated commit ids are 12 characters everywhere

A commit id abbreviated for reading was 7 characters in `rwv push`'s
lock-mismatch lines and `rwv lock --commit`'s message, 12 in `rwv status` and
most refusals, and whatever width git chose in the blocking commits `rwv sync`
and `rwv abort` list, the stopped commit a rebase conflict names, and `short`
in `rwv workweave log --json`. Every one is now the first 12 characters.
`rwv status` prints a lock entry pinned by name in full instead of cutting it
at 12 characters.

### A recorded tip that is not a commit id is refused where it is read

A tag or branch name in an in-flight op's recorded tips (`.rwv-op`) was read
as a tip, never matched the repo's HEAD, and made `rwv abort` report a
foreign-tip violation for a repo the op had moved. The record now fails to
load, naming the value, before any repo is classified. The health floor
record reads a malformed tip the way it reads any unparseable floor: as
absent.

### Breaking: `rwv workweave create --from <word>` names a workweave

`--from` now reads its source as `rwv sync` and `rwv sync-to` do: `primary`, a
bare workweave name of the project, or a path. A bare word used to name a
directory relative to where rwv ran, so `--from agent-7` reached `./agent-7`
rather than the workweave `agent-7`. It now forks from the workweave, and a bare
word that names no workweave refuses with `no-such-workweave`, saying to write
`./<dir>` for a directory. A path with a separator, a leading `.`, or an
absolute path still names a directory.

### `rwv doctor` reports a `url` no clone can use, and a relative remote

A member `url` that is neither a URL nor registry shorthand, such as a plain
path, is reported as `provenance` / `unresolvable-url`, an error naming the
`file://` spelling to write. Every clone of such a member refuses, and doctor
used to say nothing about it.

A clone whose remote is a relative path is reported as `provenance` /
`relative-remote`, a warning naming the `git remote set-url` command that gives
it the absolute URL the manifest names. git reads a relative remote from each
checkout's own directory, and a member's workweave checkouts share the
canonical clone's remote, so the path reached a different remote from each; it
used to read as an `origin-url-mismatch`. `rwv add --adopt` refuses such a
clone with `no-remote-url`, where it recorded the path as the member's `url`.

### Breaking: a malformed `hide-dotfiles` stops materialize and the install hooks

A `hide-dotfiles` that is not a boolean in `[integrations.vscode-workspace]` was
reported by `rwv doctor` as `malformed-settings`, but `rwv materialize` and the
install hooks ran past it, unlike a malformed setting of any other integration.
They now refuse over it, naming the key, and change nothing, so a wrong-typed
setting stops the same verbs whichever integration it is in. Write
`hide-dotfiles = true` or `hide-dotfiles = false`.

### A member's URL names one remote however it is spelled

Two URLs for a member are the same when they name the same remote, with
registry shorthand read as the URL it resolves to. `rwv doctor` no longer
reports `origin-url-mismatch` for a member added as `github/acme/x` whose clone
records `https://github.com/acme/x.git`, and `rwv add https://github.com/acme/x`
or `rwv add github/acme/x --new` finishes that member where it refused with
`occupied-placement`. A `url` that resolves to no remote, such as a plain path,
raises no `origin-url-mismatch`; a clone of it refuses with
`unresolvable-repo-source`. The finding names an `origin` that is an absolute
path by its `file://` URL, the spelling `rwv.toml` takes.

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

### Windows: paths carry no `\\?\` prefix, a weave on a network share included

A path rwv resolves on Windows — a weave root, a workweave, a `-C` directory —
is spelled `C:\…`, or `\\server\share\…` for a weave on a network share or a
mapped drive, in refusals, error messages, `--json` and `file://` URLs. An error
message could spell one `\\?\C:\…`, and a weave on a share showed
`\\?\UNC\server\share\…` in operator text and `//?/UNC/…` on `--json`, minted
a `file://` URL git could not clone, and handed git a path it refused. A clone
whose `origin` is recorded as a `\\?\` path now compares equal to the same path
spelled plain. A path with no plain spelling — longer than 260 characters, or
naming a reserved device such as `CON` — keeps the prefix. A member or project
under the root is spelled with `\` throughout, `C:\w\github\org\repo`, never
`C:\w\github/org/repo`, and so is a surfaced file such as `gita/repos.csv`, in
operator text and in the symlink target surfacing writes. A workweave marker,
workweave index or in-progress operation record written by an earlier rwv in
the `\\?\` spelling is read as the plain spelling, with no migration step.

### A recorded path names its directory in any spelling

Workweave markers, workweave indexes and in-progress operation records hold
each path resolved, and resolve it again when read, so a record written through
a symlinked spelling of a directory — macOS's `/var` for `/private/var`, a
symlinked weave root — matches that directory wherever rwv compares it.

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
command's whole process tree, including anything the command left running when
it exited. `--joblog` and `--results` refuse `-`, since stdout carries the
commands' output; `./-` names a file called `-`.

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
the line, the accepted names or keys, and the nearest one when one is close:
`hide-dotfile` gets "did you mean `hide-dotfiles`?". Such a table or key used to
be ignored. An unknown key inside
`[integrations.cargo-workspace.members."<repo>"]` is reported as
`malformed-settings`.

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
  `{ path = ".tracker", from = "primary" }` links every workweave to the primary
  weave's `projects/<project>/<path>`, so state that every workweave shares has
  one copy.
- New doctor findings:
  - `unknown-segment-clone`: a repo under a top-level directory that no
    registry and no manifest names, build output under `node_modules/` and
    `target/` excepted. It names `rwv add --adopt` with the path the clone's
    origin URL places it at, moving it there first.
  - `placement-disagreement`: an `owned` or `dependency` member's manifest
    path that its own URL does not derive to. It names moving the checkout to
    the path its URL places at, unless it is already there, re-keying the
    entry in `rwv.toml`, and `rwv lock`.
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
