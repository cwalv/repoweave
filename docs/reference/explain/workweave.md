# rwv workweave

## Purpose

Create, delete, or list workweaves for a project. A workweave is a parallel
working directory that materializes every repo in the project's manifest
(including the project repo itself). Every `owned` and `fork` repo, and the
project repo, becomes a `git worktree` checkout of the canonical weave-root
clone on the branch `{project}--{name}`; a `dependency` or `reference` repo is
checked out detached at its pin. A `reference` repo's tracked files carry no write
bit, here and in the primary: every verb that writes or moves a member's files
settles them for its role, and `rwv doctor` reports a member whose files
disagree (`rwv explain member-permissions`). A member slot holding a symlink —
the shape older releases gave `reference` repos — is not one rwv operates on:
verbs that walk the workweave's members refuse it (`rwv explain
workweave-drift`), and `rwv doctor --fix` replaces it with a worktree. Each
workweave is fully
self-describing: it carries `.rwv-workweave`, recording the primary weave,
project, and parent workspace. That marker is a workweave root's only
identity file — `.rwv-active` is the primary root's project-selection
pointer, and the two are mutually exclusive.

Workweaves are the isolation primitive for concurrent work — the human works
in the primary weave while an agent works in `.workweaves/{project}--{name}/`.
They are also the landing model: `rwv land --retire` brings the workweave's
commits into the parent and deletes the workweave in one step.

The parent workspace recorded in `.rwv-workweave` becomes the default land
target for bare `rwv land` from inside the workweave.

With no branch flag, `create` makes a feature under the minted name
`{project}--{name}` onto the branch the parent's project repo has checked out,
with that branch's mode as it stands, and prints where it lands
(`target-recorded`). The minted name is never published. It refuses before it
makes anything when that branch has no record (`target` `unrecorded`, naming
`rwv branch set-mode`) or when the parent's project repo is on a detached HEAD
and no spelling is given.

The parent is provenance only: it does not switch, and what it holds
uncommitted, `rwv.toml` and `rwv.lock` included, stays there as it is, reported.
The workweave starts from commits.

## Subcommands

### `rwv workweave create <name>`

Create a new workweave for the active project, or the project `--project`
names, from the workspace the verb runs in. To fork a workweave from the one
you are in, run `create` inside it.

```
rwv workweave create <name> [--new-branch <W> | --branch <W> | --detach <rev>] [--target <T> | --declared] [--set-branch <m>=<b>]... [--set-new-branch <m>=<b>]... [--pin <m>=<rev>]... [--at <rev>] [--dir <path> | --basedir <path>] [--project <name>] [--no-materialize]
```

- `--new-branch <W>` — create `W` for the workweave as `rwv branch create`
  does: bare, a feature onto the parent's checked-out branch; `--target <T>`,
  a feature onto `T`; `--declared`, a branch whose members follow its own
  entries. A name any repo already has is refused (`name` `exists`).
- `--branch <W>` — open `W` by its record. A branch with no record is refused
  (`target` `unrecorded`): record it with `rwv branch set-mode`, then open it. A
  branch another workspace has checked out is refused (`checkout`).
- `--detach <rev>` — a detached workspace at any revision, a branch another
  workspace has checked out included. Every repo sits at the pins of that
  commit's lock, and no record is written.
- `--target <T>`, `--declared` — how a created branch is used; refused with
  `--branch`.
- `--set-branch <m>=<b>`, `--set-new-branch <m>=<b>`, `--pin <m>=<rev>` — declare
  an existing (or newly created) branch `b` for member `m` in the new branch's
  first commit, as `rwv member set-branch` does; the whole remainder after the
  first `=` is the branch name. `--pin <m>=<rev>` starts the branch for the same
  member at `<rev>`, as `member set-branch --at <rev>` does; a `--pin` with no
  `--set-branch` or `--set-new-branch` for its member, or two for one member, is
  refused. Every one is judged before anything is written.
- `--at <rev>` — start a created branch at this revision. Otherwise it starts
  at the parent's checked-out commit, or at the tip of the `--target` branch.
- `--dir <path>` — the exact path to place the workweave at; there is no
  default. Relative paths resolve against the directory rwv runs in, or the
  `-C`/`-w` directory. The absolute path is recorded in the project clone's workweave index, so every
  `find`-direction verb (list, delete, sync targets by bare name) resolves
  it via the registry — the workweave does not have to live under the
  default basedir to be addressable. Useful for big-disk workweaves and
  tmpfs experiments.
- `--basedir <path>` — the directory to place the workweave under for this
  invocation, in place of the recorded default: the workweave lands at
  `<path>/<project>--<name>`. Relative paths resolve as for `--dir`. Refused
  together with `--dir`, which names the workweave's own path rather than the
  directory above it.
- `--project <name>` — create the workweave for this project rather than the
  active project. Does not change `.rwv-active`.
- `--no-materialize` — skip the integration install hooks (`npm install`,
  `uv sync`, `cargo fetch`, …) in the new workweave. `rwv materialize` inside
  it runs them later.

**Placement.** Workweaves live at `<basedir>/<project>--<name>/` by default,
where `<basedir>` is recorded per-project in the workweave index the project clone keeps.
The default basedir is `<parent-of-primary>/.workweaves`; set it explicitly
with `rwv workweave set-default-create-basedir <path>`. `--basedir <path>`
replaces it for one invocation, and `--dir <path>` places the workweave at
exactly that path.

**Re-invocation.** Running an identical spelling on a clean workweave whose
`.rwv-workweave` marker matches (same primary, project and parent) returns it,
reported. Non-git state written by agents between invocations (`.runtime/`,
`.claude/`, etc.) is preserved, so this is the standard "ensure workweave
exists" path. Any difference (another branch spelling, uncommitted changes in
the workweave, another parent or no marker) is refused as `name` `taken`,
listing what differs, and changes nothing.

**What `create` writes:**
1. One `git worktree` per member repo (plus the project repo), at
   `{workweave}/{repo-path}/`: the project repo on the workweave's branch (or detached), a member of a
   feature that is `owned` or `fork` on a branch of the same name at its pin,
   and every other member detached at its pin. A branch of the chosen name
   that any repo already has is refused as `name` `exists`.
2. A copy of every rwv-owned generated file the source workspace has accepted
   — its lockfiles (carried when the workweave starts at the parent's checked-out commit), plus the `.rwv-owned-digests` record attesting them. These
   are gitignore-eligible, so the worktrees above arrive without them, and
   re-running the ecosystem generator here would resolve against a registry
   that has moved since the source resolved: the new workweave would hold a
   different dependency set than the workspace it forked from, with both
   reading clean. Copying is what makes a build in the workweave and a build
   at the source measure the same tree.
3. `workweave:` artifacts from `rwv.toml` — `copy:` entries are deep-copied;
   `link:` entries are absolute symlinks pointing at the source root.
4. `.rwv-workweave` marker recording `{primary, project, parent}`. `parent`
   is the workspace the verb ran in, which becomes the default
   land target for bare `rwv land` from inside the workweave. No
   `.rwv-active` is written beside it: selection is primary-only, and a
   workweave's project is fixed here at creation.
5. Integration activation: surfaces symlinks, runs the install hooks in the
   new workweave unless `--no-materialize` is passed, then reports a missing
   or drifted integration file as a warning naming `rwv doctor --fix`; it
   authors none.
   A hook whose tool is not on `PATH` is skipped with a `tool-missing`
   warning. A hook that fails leaves the workweave created and registered,
   and the command exits non-zero naming `rwv materialize`, which finishes it
   once the cause is fixed. Under `--hook-mode`, and for the `WorktreeCreate`
   request `--claude-hook` handles, the agent runtime is waiting for the
   workweave: the create exits `0`, prints the path as usual, and the failure
   is a warning on stderr naming `rwv materialize`.
6. An entry in the project clone's workweave index recording the workweave's
   absolute path and the workspace it was forked from. The index is kept in the
   directory the clone shares with its worktrees, outside the working tree, so
   nothing is added to the project repo's ignore surface.

### `rwv workweave set-default-create-basedir <path>`

Record the default basedir for the project — the directory `create` places
new workweaves under. Writes the `basedir` of the project clone's workweave
index. Absolute paths are used as-is;
relative paths resolve against the directory rwv runs in (or the `-C`/`-w`
directory). Existing entries in the
registry are preserved. Per-workweave `--dir` and `--basedir` on `create` are
unaffected. Takes `--project <name>` to record it for another project.

The recorded basedir is an explicit, audit-visible act, not ambient
process state: it lives in a file the operator can read, not in a shell
environment.

### `rwv workweave delete <name>`

Delete a workweave, and apply the deletion rule to its branches.

```
rwv workweave delete <name> [--discard-uncommitted] [--discard-unlanded] [--delete-remote] [--delete-remote-unlanded <repo>]... [--json] [--project <name>]
```

Refuses, before it changes anything, if:

- Any checkout under the workweave, a manifest member or not, has
  **uncommitted changes** (`edits` `dirty`).
- Any checkout's HEAD is detached holding **commits no branch or
  remote-tracking branch reaches** (`alignment` `detached with unpushed
  commits`).
- The workweave's branch is a feature other branches' records land into
  (`target` `dependents`), or is checked out in another workspace (`checkout`).

It then removes the worktrees, prunes their registrations and removes the
workweave directory whole: what its root holds outside the repos is scratch
that goes with the workweave, and ignored files in a checkout go with it, as
they do when the VCS removes a working tree. Before it removes the directory
it names, in one line, the entries of its root rwv did not create (how many,
and the first few); nothing there refuses the delete. The workweave's own branches are the rule's: the branch
the workweave is aligned for, when its record is a feature, and the branch of
that name in each member that lists it.

**The deletion rule.** The project branch, and in each member the branch of the
same name that no live weave branch declares, belong to the workweave. Members
are judged first. A local branch that belongs is deleted when:

1. every commit on it is reached by the branch it lands into, or that
   branch's remote-tracking branch (the project branch the record names as the
   target; in a member, its branch for that target, or any branch the target's
   history declared for it);
2. no live lock pins a commit that only it reaches, outside the workweave
   branch's own lock;
3. for the project branch, its published copy has landed too, and no member
   branch that belongs survives.

Otherwise it survives, with its record. Each survivor is reported with its
reason: `unlanded`, `pinned`, `open-head`, `members` or `unconfirmed`. The
pruning fetch covers only the remotes these branches have an upstream or a push
branch on, and a branch on a remote that could not be fetched is kept
(`unconfirmed`); a branch never published is judged on local refs alone. A
declared branch never belongs, so `main` and a release line's member branches
are never touched. When anything survives the exit status is **3**, and
`--json` lists what survived and why.

**`--discard-uncommitted`** waives the `edits` refusal; **`--discard-unlanded`**
waives the detached-commits refusal and lifts clause 1 for every branch of the
workweave, deleting a branch that has not landed. Neither discards for good.
Before anything is removed each keeps what it consents to losing as a backup
(`rwv backup list`): the edits as a commit on the checkout's HEAD (untracked
files included, ignored ones not), the commits a detached HEAD holds, and the
tip of each unlanded branch. A backup that cannot be written stops the delete
with nothing changed. The report lists what was discarded by kind: edits,
detached commits, unlanded branches.

**`--delete-remote`** also deletes each push branch of what belongs that has
landed on the target's remote-tracking branch, its upstream. **`--delete-remote-unlanded
<repo>`**, one repo per occurrence and needing `--discard-unlanded`, also
deletes that repo's push branch whether or not it landed, an open pull request's
head included, after keeping its tip as a backup, and keeps one (`pinned`)
while a published lock pins a commit no other remote-tracking branch reaches.
A push branch of any other
repo that has not landed is kept. Both are leased to the tip the last fetch
saw: a push branch someone pushed to since, or a remote that does not take the
delete, is left (`unconfirmed`, the answer listed). A declared-mode project
branch's push branch is deleted by `--delete-remote` once landed and never by
`--delete-remote-unlanded`.

**Records.** An entry on the records branch whose branch is gone locally and
from every remote, with no workspace of this machine aligned for it, is dropped
in the same write.

**Child adoption.** Before the workweave is destroyed, any living child
workweave that records it as `parent:` is re-pointed to the retiree's OWN
recorded parent (the grandparent; falls back to `primary`, which always
exists). One loud line is printed per child:
`adopted child workweave <name>: parent now <path>`. Branch names are NOT
rewritten, which is exactly why consumers must read the parent from the marker
or `rwv status --json .parent`, never from the branch name. The same adoption
step runs on `rwv land --retire`, which applies the same rule to the
retiring workweave's branches.

### `rwv workweave list [--project <name>]`

List existing workweave names for the project. One name per line.

## Invocation

```
rwv workweave create <name> [--new-branch <W> | --branch <W> | --detach <rev>] [--target <T> | --declared] [--at <rev>] [--dir <path> | --basedir <path>] [--project <name>] [--no-materialize]
rwv workweave delete <name> [--discard-uncommitted] [--discard-unlanded] [--delete-remote] [--delete-remote-unlanded <repo>]... [--json] [--project <name>]
rwv workweave list [--project <name>]
rwv workweave set-default-create-basedir <path> [--project <name>]
rwv workweave --hook-mode create <name> [...]
rwv workweave --claude-hook
```

The project is chosen as for any verb: `--project`, else the project the
workweave you are in belongs to, else the active project (`.rwv-active`).
`--hook-mode` makes `create` print only the new workweave's path to stdout.
`--claude-hook` reads a Claude Code hook payload from stdin and runs the
`WorktreeCreate` or `WorktreeRemove` request it names: a bare `create` or
`delete` for the project the payload's directory resolves to.

Run `rwv workweave --help` for the full clap surface.

## Output

`create` — no stdout output on success (workweave path to stdout only when
`--hook-mode` is set). Progress and warnings go to stderr, and so does the
output of the install hooks' tools, so the `--hook-mode` path is the only
thing on stdout. With `--no-materialize` it says so in one line: `[skipped]
core: the install hooks did not run (--no-materialize); `rwv materialize` runs
them`.

`delete` — what the rule did, one line each: `deleted <repo> <branch>
(<site>): <because>` for a branch it deleted, `left: <repo> <branch>: <reason>`
(on stderr, with the commits or locks behind it) for a survivor, and the
backups and discards by kind on stderr. With `--json` it emits one envelope,
whose `discarded` object lists `edits`, `detached_commits` and
`unlanded_branches`, and whose `deleted` and `survivors` arrays carry the rest:

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "WorkweaveDeleteJsonOutput",
  "description": "The envelope `rwv workweave delete --json` prints.",
  "type": "object",
  "required": [
    "$schema",
    "deleted",
    "discarded",
    "project",
    "records_dropped",
    "survivors"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "deleted": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/Deleted"
      }
    },
    "discarded": {
      "$ref": "#/definitions/DiscardedOutput"
    },
    "project": {
      "type": "string"
    },
    "records_dropped": {
      "description": "Branches whose records the drop rule removed.",
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "survivors": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/Survivor"
      }
    }
  },
  "definitions": {
    "ChoiceOutput": {
      "type": "object",
      "required": [
        "phrase",
        "run"
      ],
      "properties": {
        "id": {
          "description": "The choice's id on the axis's page; null for one it does not number.",
          "type": [
            "string",
            "null"
          ]
        },
        "phrase": {
          "type": "string"
        },
        "run": {
          "description": "The commands, one per line, as the terminal prints them.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "where": {
          "description": "The directory it must run in; null where it runs here or names its own directory.",
          "anyOf": [
            {
              "$ref": "#/definitions/WirePath"
            },
            {
              "type": "null"
            }
          ]
        }
      }
    },
    "Deleted": {
      "description": "A branch the rule deleted.",
      "type": "object",
      "required": [
        "because",
        "branch",
        "path",
        "site"
      ],
      "properties": {
        "backup": {
          "description": "The backup kept first, when the branch had not landed.",
          "type": [
            "string",
            "null"
          ]
        },
        "because": {
          "description": "What licensed it: the branch it landed on, or the backup kept first.",
          "type": "string"
        },
        "branch": {
          "type": "string"
        },
        "path": {
          "description": "The repo, as `rwv backup drop` names it.",
          "type": "string"
        },
        "site": {
          "description": "`local`, or `remote <name>` for a push branch.",
          "type": "string"
        }
      }
    },
    "DiscardedOutput": {
      "description": "What a workweave delete discarded, by kind.",
      "type": "object",
      "required": [
        "detached_commits",
        "edits",
        "unlanded_branches"
      ],
      "properties": {
        "detached_commits": {
          "type": "array",
          "items": {
            "$ref": "#/definitions/KeptWork"
          }
        },
        "edits": {
          "type": "array",
          "items": {
            "$ref": "#/definitions/KeptWork"
          }
        },
        "unlanded_branches": {
          "description": "Branches that had not landed, each deleted after the backup named.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/UnlandedBranch"
          }
        }
      }
    },
    "KeptWork": {
      "type": "object",
      "required": [
        "backup",
        "path"
      ],
      "properties": {
        "backup": {
          "description": "The name `rwv backup drop` takes.",
          "type": "string"
        },
        "path": {
          "type": "string"
        }
      }
    },
    "Reason": {
      "description": "Why a branch that belongs survived the rule.",
      "oneOf": [
        {
          "description": "It holds commits the branch it lands into does not reach.",
          "type": "string",
          "enum": [
            "unlanded"
          ]
        },
        {
          "description": "A live lock pins a commit that only it reaches.",
          "type": "string",
          "enum": [
            "pinned"
          ]
        },
        {
          "description": "Its published copy holds commits the branch it lands into does not reach: a pull request may be open against it.",
          "type": "string",
          "enum": [
            "open-head"
          ]
        },
        {
          "description": "A member branch that belongs to it survives.",
          "type": "string",
          "enum": [
            "members"
          ]
        },
        {
          "description": "A remote it has a copy on could not be fetched, or did not take the delete.",
          "type": "string",
          "enum": [
            "unconfirmed"
          ]
        }
      ]
    },
    "Survivor": {
      "description": "A branch the rule kept.",
      "type": "object",
      "required": [
        "branch",
        "choices",
        "detail",
        "path",
        "reason"
      ],
      "properties": {
        "branch": {
          "type": "string"
        },
        "choices": {
          "description": "What the operator can run to give the branch up.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/ChoiceOutput"
          }
        },
        "detail": {
          "description": "What the reason rests on: the commits, the locks, the remote's cause.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "path": {
          "type": "string"
        },
        "reason": {
          "$ref": "#/definitions/Reason"
        }
      }
    },
    "UnlandedBranch": {
      "type": "object",
      "required": [
        "backup",
        "branch",
        "path"
      ],
      "properties": {
        "backup": {
          "type": "string"
        },
        "branch": {
          "type": "string"
        },
        "path": {
          "type": "string"
        }
      }
    },
    "WirePath": {
      "description": "Absolute path, `/`-separated on every platform.",
      "type": "string"
    }
  }
}
```

`list` — one workweave name per line to stdout.

## Exit codes

- `0` — operation completed successfully.
- `3` — `delete` removed the workweave and the deletion rule kept at least one
  branch; the output lists each and why.
- non-zero — operation failed; see stderr for details. For `create`, a failed
  install hook is non-zero too, though the workweave was created and is kept —
  except under `--hook-mode` or `--claude-hook`, where it is a warning and the
  create exits `0`.

## Examples

Create a workweave for the active project:

```
rwv workweave create my-feature
```

Create one for the `foundations` project while another is active:

```
rwv workweave create my-feature --project foundations
```

Create a workweave forked from a peer workweave (useful for stacked work) by
running `create` inside the peer:

```
cd ../.workweaves/foundations--my-feature
rwv workweave create child-feature
```

Open a published branch in a workweave, or look at one another workspace has
checked out:

```
rwv workweave create review --branch feat-x
rwv workweave create look --detach feat-x
```

Land work and clean up in one step (from inside the workweave):

```
rwv land --retire
```

Delete a workweave whose work has already landed:

```
rwv workweave delete my-feature --project foundations
```

Delete a workweave regardless of state, keeping a backup of what is discarded,
and its landed push branches too:

```
rwv workweave delete my-feature --project foundations --discard-uncommitted --discard-unlanded --delete-remote
```

List all workweaves for a project:

```
rwv workweave list --project foundations
```

## Common errors

- *workweave has uncommitted changes; refusing to delete without
  --discard-uncommitted* — list the named paths; commit or discard the changes,
  or pass the flag to discard them.
- *alignment: detached with unpushed commits* — a checkout's detached HEAD
  holds commits no branch reaches. Keep them on a branch, or pass
  `--discard-unlanded` to keep them as a backup and remove the workweave.
- *left: ... unlanded* — the workweave is gone and a branch of it holds commits
  the branch it lands into does not reach. Land them, or run `rwv branch delete
  <branch> --discard-unlanded` to keep them as a backup and delete the branch.
- *name taken* — the workweave exists and is not what this spelling would
  make: uncommitted changes in it, another parent or branch, or no marker. The
  line lists what differs; nothing changed.
- *workweave created at …; only its install hooks did not complete* — an
  install hook failed in the new workweave. The workweave is usable; fix the
  cause the hook reported, then run `rwv materialize` inside it.
