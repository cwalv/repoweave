# Member population

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

**Audience:** someone changing how a member's tracked files reach a workspace —
the `WorkspacePopulation` questions, the sharing engine in `src/share.rs`, the
settle chokepoint in `src/settle.rs`, role changes, or the migration from the
old symlink shape. It describes what is built. Symbol names are checked against
the tree at time of writing; nothing is cited by line number.

## 1. One member shape

Every member of a workweave, whatever its `role`, is a worktree of the
canonical clone, on the workweave's ephemeral branch. In the primary, a member
is the canonical clone itself. There is no second shape. A symlink at a member
slot is not a member checkout of any kind, and is refused or repaired (§8).
`rwv workweave <project> create --worktree-references`, which once asked for
a worktree where the default was a symlink, exits 2 saying every member is now
a worktree.

The symlink shape went because it broke the first rule of §2: it changed
behavior, not just cost. A symlinked member read the live canonical clone rather
than the revision the seat's lock pins, so a fetch in the primary changed every
seat's copy under it; a write through the link landed in the primary; its
absolute target stranded every seat when the weave moved; it needed privilege on
Windows; and every verb that walks members had to branch on which shape it was
looking at. The disk it saved is what §4's placement modes save instead, on
filesystems that support them.

A member worktree is born in one place, `birth_ephemeral_worktree` in
`src/workweave.rs`, reached from `workweave create`'s member loop and from
`materialize_member` (which `fetch`, `add`, `sync`, `sync-to` and
`doctor --fix` use to give a workweave a member it lacks). Its `WorktreeFiles`
argument says where the tracked files come from: `Shared` for members, placed
as §4 describes; `Written` for the project repo, which the VCS checks out
itself.

## 2. Two rules

**A mode changes how much disk a member costs, never how it behaves.** Every
placement mode in §4 ends in the same bytes, the same executable bit, and the
same recorded state as the backend's own checkout. A behavior difference
between modes is a bug.

**A `reference` member's tracked regular files have no write bits, in every
workspace, the primary included, in every mode.** The role says "no local
changes"; the permissions make it true on disk, and they are what makes sharing
one file between workspaces safe. Precisely:

- only tracked regular files are touched;
- a tracked symlink is never chmod'ed, because chmod follows the link and would
  change its target, possibly outside the repository;
- directories keep their write bits, so the VCS can still replace a file during
  an update, and untracked files (build output, editor swap files) can still be
  created in a reference tree;
- on Windows, "no write bits" is the read-only attribute.

Every other member's tracked files are writable by their owner.

## 3. The population questions

Population goes through the `Vcs` trait. `Vcs::population()` returns
`Option<&dyn WorkspacePopulation>`; `None` is a complete answer, and a backend
that gives it gets plain creation through `Vcs::create_worktree_on`. A backend
answers every population question or none, so no caller can half-populate a
workspace. Git is the first backend, not the definition: each question is one a
repository can be asked without naming git's mechanism.

1. **`create_unpopulated_worktree_on`** (with its VCS half
   `materialize_unpopulated_worktree_on_ref`): `create_worktree_on`'s receipt
   and branch handling, writing no tracked file. The new workspace already
   records what it is a checkout of. Git: `worktree add --no-checkout`, then
   `read-tree HEAD`, so the index holds the revision before any file exists.
   The next question reads attributes from that index (`check-attr --cached`),
   since no `.gitattributes` file is on disk yet.
2. **`tracked_files`**: every tracked file the workspace records, as
   `TrackedFile { path, kind }`. `TrackedFileKind` is `File { executable,
   identity }`, `Symlink` or `NestedRepository`, so only a regular file can
   carry an identity. A `ContentIdentity` is opaque: two equal identities from
   workspaces of one repository check out to equal bytes; unequal ones promise
   nothing. A file without one is never placed from elsewhere.

   Git gives an identity only where its checkout writes the blob's bytes
   unchanged: no `filter` (LFS included), no `ident`, no
   `working-tree-encoding`, and no conversion to CRLF. The identity is the blob
   plus every input to that decision — the `text`, `eol`, `crlf`, `ident`,
   `filter` and `working-tree-encoding` attributes and `core.autocrlf` /
   `core.eol` — so two contexts that would write different bytes never share
   an identity. The verbatim test (`checks_out_verbatim` in `src/git.rs`)
   mirrors git's own end-of-line decision and answers `false` for any value it
   does not recognise; a matrix test runs git's own `checkout-index` over each
   conversion attribute under every `core.autocrlf` and `core.eol` setting and
   fails if a file carrying an identity came out changed. Where text checks out
   with CRLF (Windows with `core.autocrlf=true`), most text files carry no
   identity: that costs disk, never correctness.
3. **`files_holding_recorded_content`**: of a list of paths, the files that
   hold exactly the bytes and the executable bit a checkout writes, right now.
   It is byte-exact, and it is the verifier: every placed file is checked with
   it. Git: hash each candidate with `hash-object --no-filters` and compare the
   index's blob — exact because an identity exists only where the checkout is
   the blob's bytes. `Vcs::tracked_dirty_file_names` cannot do this job: git
   compares after normalizing line endings, so a CRLF file placed in a
   `text=auto` path reads as clean, and after `read-tree` the index records no
   size, so `update-index --refresh` falls through to that normalizing compare
   and records the wrong file as clean.
4. **`finish_checkout`**: write every tracked file the workspace records that is
   missing, leave every present one alone, and leave the workspace as the
   backend's own checkout would, hooks included. Git: `checkout-index -u` on
   exactly the missing paths (`checkout-index -a` exits 1 for any file already
   present, so its status cannot tell a placed file from a failed write), then
   `update-index -q --refresh`, then the `post-checkout` hook with the arguments
   `worktree add` gives it (a null previous HEAD, the new HEAD, `1`), because
   `--no-checkout` skips that hook. A failing hook is `VcsError::HookRejected`.
   Running a hook on request needs `git hook run` (git 2.36), so the git backend
   answers no population questions on an older git.
5. **`check_out_paths`**: check named paths out again from what the workspace
   records, replacing what is there. This is the repair. Git:
   `checkout-index -f -u`, which unlinks and creates rather than writing into
   the existing file, read-only or not.
6. **`updates_replace_files`**: whether, on this platform, an update in one
   workspace changes neither the content nor the permissions of a file in
   another workspace that shares its storage. It gates the hardlink mode (§6).

## 4. Placing files

`src/share.rs` is VCS-neutral: it asks the questions above and moves files with
two filesystem primitives behind `FilePrimitives` (`reflink`, via the
`reflink-copy` crate's Linux `FICLONE`, macOS `clonefile` and Windows block
cloning; and `hardlink`), which a test replaces with a fake that can fail on
demand.

### Population

`create_worktree_populated` creates the workspace unpopulated, lists what a
checkout writes, places every file with an identity it can, finishes the
checkout, checks every placed path with `files_holding_recorded_content`, and
checks out again with `check_out_paths` every placed path that does not hold
its content. **The result is self-verifying:** a source that changes between
the check and the placement yields a repaired file, never a wrong one.

### Sources

Every other workspace of the same repository is a candidate, found through
`Vcs::list_worktrees`: the primary's checkout and every seat's. They are tried
in order — workspaces already at the target revision, then the primary, then
the rest — and the walk stops once every wanted file is covered. A file is
taken from a source only if `files_holding_recorded_content` reports the
source's copy as holding its recorded content and the source records the same
kind for it — identity and executable bit included, so a source whose
conversion settings would check the file out differently is never used.

### Modes and fallback

Per source, each mode the target allows is probed once — a sample file placed
into the destination and removed — before anything in the source is read, so a
mode the filesystem lacks costs one attempt rather than a hash of every source
file. Then, per file, in order:

| Mode | Placed for | Taken from |
|---|---|---|
| reflink | any role | any source holding the file |
| hardlink | a `reference` member, only where the backend declares `updates_replace_files` | a source that is a `reference` member where it lives (by its workweave's project manifest; in a primary, by every project listing it), and only a file with no write bits |
| written | everything not placed | the backend, in `finish_checkout` |

An `Unsupported`, `CrossesDevices` or `InvalidInput` error says the mode cannot
work between these two places at all (`cannot_place_between`): that source is
not tried in that mode again. Any other error fails only that file, which moves
on to the next mode. A hardlinked name whose file turns out to have write bits
is removed again and counts as a failure. So a non-`reference` member never
shares a file's storage by hardlink with another workspace, and nothing that
can be written in place is ever given a second name. That is not a gap to
close: many editors save by truncating and rewriting the existing file, which
through a hardlink would rewrite every workspace's copy. A writable member gets
reflink or written, never hardlink.

**No configuration, no recorded mode.** Nothing records how a file was placed,
and nothing configures it: a later operation asks the backend again. Hardlink
sharing is visible on disk as a link count above one; reflink sharing is not,
and need not be, because a cloned file behaves exactly like a copy.

### Re-sharing after a move

A VCS writes every file an update replaces as a new, unshared file.
`settle_moved` (and `settle_workspace_moved`) take the revision a member moved
from, ask `Vcs::changed_paths_between` what the move rewrote, and pass those
paths to `reshare`. A file is replaced only if it still holds its recorded
content, so a local change is never lost; the replacement is placed under a
staged name beside the file and renamed over it, then verified and repaired
like a placement. The revision a member moved from is the caller's: `update`
records the old tip, `fetch` reads HEAD before aligning, `sync` and `sync-to`
read each member's savepoint, and `abort` reads each member's pre-abort ref.
The backend's index is not refreshed afterwards: git's next status re-hashes a
replaced file once and writes the result, and the content is the same.

### Reporting

`FilePlacement { reflinked, hardlinked, written }` says how a checkout's files
were placed; `written` counts every tracked entry the backend wrote, repairs
included. Text reports name the member on `workweave create`, `add`, `fetch`,
`update`, `sync`, `sync-to`, `abort` and `doctor --fix`.

Under `--json`, counts live only in placement records,
`{path, absolute_path, files}`, each naming the checkout it describes, in
`fetch`, `update`, `sync` and `sync-to`. The envelope carries a `placements`
array, empty rather than absent when nothing was placed; NDJSON mode emits a
placement line as each placement happens, under a `$schema` of its own that
every verb shares. Outcome records never carry counts, because a placement need
not be of the checkout an outcome names: `sync-to` re-shares a repo in the seat
and in the target, and `fetch` in a workweave creates the seat's worktree while
its outcome names the canonical clone. Only the placement's own
`absolute_path` says which checkout it was.

## 5. Settle

`settle(vcs, member, role)` brings a member's tracked regular files to the
permissions its role asks for, and is the chokepoint every write site goes
through:

- `reference`: remove every write bit;
- any other role: restore the owner's write bit — but a file whose link count
  is above one is first replaced by a private copy (written beside it as
  `.<name>.rwv-unshare` and renamed over it). Write bits belong to the file,
  not the name, so restoring them in place would make every workspace sharing
  it writable. **Unshare before writable, always.**

A file already in the right state is left alone, so its ctime does not move.
Settle names files through `tracked_files`, so under a backend that answers no
population questions it changes nothing.

On Windows, stable Rust reads no link count, so settle cannot see that a file
is shared; nothing rwv makes is shared there only because no backend declares
the hardlink mode on Windows. A Windows declaration needs a link count first.

Every verb that writes or moves a member's files ends by settling it, in the
primary and in seats alike: `fetch` and `update` (through `settle_moved`),
`add`, `workweave create` and every `materialize_member`, `sync` and `sync-to`
(materialized members, then `settle_members` over every member after replay
and after advancing the target), `abort` after its restore, `rwv role`, and
`doctor --fix`. In `sync`, `sync-to` and `abort`, a member that cannot be
settled is named as a warning rather than failing the op, whose own work
stands. Doctor backstops it with the `member-permissions` finding — a
`reference` member with writable tracked files, or a member of another role
with read-only ones — whose `--fix` settles.

**The primary is not special.** Its reference checkouts are read-only and are a
source like any seat's. A role is per project, and one canonical clone in the
primary can serve several projects; settle takes the role of the project whose
verb is running, and `doctor` checks such a clone once, for the first project
listing it. Where those projects disagree, the clone's permissions follow
whichever settled it last, and the hardlink mode takes a primary checkout as a
`reference` source only where every project listing it agrees.

## 6. Backend conformance and the per-platform declaration

The hardlink mode is safe only if **an update in one workspace changes neither
the content nor the permissions of a file in another workspace**: the backend
must replace a file rather than write into it, and must not change a shared
file's permissions while removing its own name for it.

`tests/shared_file_conformance_test.rs` proves it per platform. It hardlinks a
tracked file into two worktrees and runs, in one of them, each update rwv runs
on a member — a detached and a branch checkout, `reset --hard`, a fast-forward,
a rebase with nothing to replay, the single-path checkout, the working-tree
refresh, and `check_out_paths` — with the shared file writable and without
write bits. It also checks that population leaves every placed file as placed,
that equal identities check out to equal bytes across workspaces whose
`core.autocrlf` differs, and that a link or a chmod moves a file's ctime.

The git backend declares `updates_replace_files` on Linux only, where those
tests have been observed green. Two tests pin the declaration per platform, so
changing it is a deliberate edit. macOS stays undeclared until its CI leg
shows the conformance file green. On Windows the property is expected not to
hold: Git for Windows clears the read-only attribute before deleting a file,
and on NTFS that attribute belongs to the file, not the name, so one
workspace's update would make every other workspace's copy writable. The
Windows arm of the conformance tests asserts that expected failure, so a change
in git turns it red. A platform without the declaration has two modes: reflink
and written.

**ctime.** Linking a file, or removing its write bits, moves its inode's ctime,
which git's stat check reads, so every workspace sharing the file re-hashes it
once on its next status (on 20,002 files, 195 MiB: 667 ms instead of 9 ms),
after which git's refreshed index carries it. This is Unix only: Git for
Windows reads a file's creation time as its ctime. rwv accepts that cost and
sets no `core.trustctime`: the alternative is per-worktree git configuration
that would have to follow the role through settle, `rwv role`, doctor and a
migration, plus a repository extension in every store that has a reference
member. If seats ever report status latency after seat creation, it is added
as one change — a `Vcs` question settle sets on entering `reference` and
clears on leaving, plus a doctor finding.

## 7. Role changes

A role is a field of a workspace's `rwv.toml`. With one member shape, a role
change reshapes nothing; the only disk state that depends on the role is the
permissions of §2 and, for `reference`, eligibility to share by hardlink. A
role change is therefore settle applied to a member whose files no longer match
its role:

- **to `reference`:** remove write bits; files placed or re-shared from then
  on may be hardlinked, and files already on disk stay as they are;
- **from `reference`:** unshare every file with a link count above one, then
  restore write bits. Only settle ever makes a member's files writable, and it
  always unshares first, so no path that changes a role can make another
  workspace's copy writable.

`rwv role <repo> <role>` (`src/role.rs`) edits the role in this workspace's
`rwv.toml`, regenerates the integrations' generated files as `add` and `remove`
do — a role can move a member in or out of an ecosystem's build graph, since
`reference` members are excluded from it — and the edit and the regeneration
land together or not at all. It then settles the member for whichever role
`rwv.toml` holds afterwards, so a regeneration that put the edit back leaves the
files as they were. It refuses a change to `reference` while the member has
uncommitted changes to tracked files, and an unchanged role prints a message
and writes nothing.

A hand edit of `rwv.toml` stays valid: the next `sync` or `sync-to` settles
every member for the role its manifest gives it, `abort` settles after
restoring an older manifest, and `doctor --fix` settles whatever
`member-permissions` reports. A role change made in a seat is local until it
lands; other workspaces follow when they sync.

## 8. Migration from symlinked members

Older releases materialized a workweave's `reference` members as a symlink to
the canonical clone. rwv no longer operates on that shape:

- **Refusal.** Every verb that walks a workweave's members — `sync`, `sync-to`,
  `abort`, `workweave create` (from a workweave) and `delete`, `update`,
  `status`, `lock`, `materialize` and `role` — refuses while any member slot is
  a symlink, with the `workweave-drift` refusal naming
  `rwv -C <workweave> doctor --fix` (`refuse_symlinked_members` in
  `src/workweave.rs`). A symlink at a member slot in the primary is the
  operator's own arrangement and is not refused.
- **Repair.** `rwv doctor` reports a symlinked slot as a `workweave-drift`
  finding — `symlink` when the canonical clone holds the revision the
  workweave's `rwv.lock` pins (or, with no pin, the canonical clone's HEAD),
  `symlink-unfetched` when it does not. `--fix` replaces a `symlink` slot with
  a worktree at that revision, through `materialize_member`, so its files are
  placed and settled like any member's; the link itself is removed and what it
  pointed at is never touched. A `symlink-unfetched` slot needs the revision
  first: `rwv fetch` run in the workweave brings the pinned revision into the
  canonical clone, cloning it if absent, then creates the worktree in place of
  the link.
- **Existing checkouts.** A `reference` checkout made before read-only
  settling, in the primary or a seat, is writable; `doctor` reports it as
  `member-permissions`, and `--fix` settles it.

The operator-facing entries for both tokens are in
`docs/reference/doctor-findings.md`; the refusal shares the finding's
`workweave-drift` entry.
