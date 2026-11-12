# rwv branch

## Purpose

Record how a **weave branch** is used, list the weave branches with their
records, rename one, and delete one. A weave branch is a branch of the project repo that
carries `rwv.toml` and `rwv.lock`.

A branch's **record** is one of two modes:

- **`feature`** — work onto a **target** branch. A feature's `owned` and `fork`
  members sit on branches named after it, and it lands into its target. The
  record also keeps `as`, the target's own mode when the record was written.
- **`declared`** — members sit on the branches the branch's `rwv.toml` entries
  declare. The branch a fresh clone checks out starts as `declared`.

Records are kept on a branch of the project repo of their own, which has no
working tree and no history in common with any weave branch, so a record is
written without touching a checkout and travels with the repo. Every verb that
aligns members, locks, lands or publishes reads the checked-out branch's record
and refuses a branch that has none, naming `rwv fetch` (the record may be
published but not yet fetched) and `rwv branch set-mode` (say how the branch is
used). Nothing falls back to `declared`.

`rwv init` and `rwv clone` record the branch they leave checked out as
`declared`, and a bare `rwv workweave create` records the new workweave's branch
as a feature onto the branch the workspace it was made from has checked out.

### `set-mode`

Writes the record of `<W>` (the project repo's checked-out branch when no
branch is named) and nothing else: no branch moves, no checkout changes.
`<W>` must exist locally or on a remote as last fetched.

- `feature` needs `--target <T>`, which must itself have a record that reads as
  one value; its mode is stored as `as`. A `--target` whose chain of targets
  returns to `<W>`, `<W>` itself included, is refused as `target` `cycle`.
- `declared` takes no `--target`.
- Recording a feature prints where it lands (`target-recorded`): into `<T>`'s
  member branches when `<T>` is a feature, else into each branch `<T>` declares,
  listing the members whose entry in `<W>` differs from `<T>`'s.
- When `<W>`'s mode changes, each feature that targets `<W>` and recorded its
  mode differently is listed: its record keeps the old `as`, so it refuses to
  land until it is rebound with `rwv branch set-mode <dependent> feature
  --target <W>`.

A record settles a conflict too: a branch two writers recorded differently
holds both values, reads as `target` `conflict`, and `set-mode` writes the one
you choose. An entry that does not read (a hand commit, or a field a later rwv
writes) reads as `target` `unreadable` for that branch alone, naming the rwv
release that wrote it; `set-mode` rewrites it. The records of other branches
read as usual.

The records branch's own name, and every name below it, is not a weave branch's:
`set-mode` refuses it as `name` `reserved`.

### `list`

One line per weave branch, local or only on a remote as last fetched, marking
the checked-out one with `*`: the branch, then its record (`declared`, `feature
onto <T> (as <mode>)`, `unrecorded`, `conflict`, `unreadable`, `cycle`).

- `[unpublished]` — no remote's records, as last fetched, hold this entry. A
  record travels with a push of its branch.
- `[target gone]` — the target exists nowhere, local or remote.
- `[remote only]` — the branch exists only on a remote.

`list` reads refs as last fetched and writes nothing: the records of the
remotes it knows are merged in memory, not into the local records branch.

### `rename`

Renames the project repo's checked-out branch to `<new>`. The branch's record,
the member entries the workspace last aligned it by, the record of every branch
that lands into it (its `target` becomes `<new>`) and the workspace's aligned
branch move with it, in one commit of the records branch, and no entry is left
under the old name. A record that lands into the branch is published, like any
record, by a push of that branch (`rwv push --branch <dependent>`). In a feature
the same-named branch of each `owned` and `fork` member that has one is renamed
too; a declared-mode project branch's members sit on the branches `rwv.toml`
declares and are not touched.

Each renamed branch stops taking in from the remote branch it did. Every old
remote branch is reported on stderr as `left: <repo> <old>: <remote> still has
it`, with the `rwv branch delete <old> --target <new> --delete-remote` that
deletes it once it has landed on `<new>`.

A declared-mode project branch is protected against rewrites only: it can be
renamed. A rename made on the forge (the project's `main` renamed `trunk` on the
remote) is taken up with `rwv branch rename trunk --adopt`. A member branch
that some live weave branch declares is protected and is never renamed.

- `--adopt` takes up a branch a remote already has under `<new>`: each renamed
  branch whose push target has `<new>` takes in from it, and the project branch
  must have one (`name` `no-such-branch` otherwise). Where the two have
  diverged the rename is refused (`push-branch` `diverged`, the unpushed
  commits listed), changing nothing. Without `--adopt` a `<new>` a remote has,
  as last fetched, is refused.
- The record that moves is the old branch's; where it has none and `<new>`
  has one, a run that stopped partway left it there and is completed. A
  `<new>` that already holds a record of another value is never overwritten:
  it is refused as `name` `exists`, or with `--adopt` as `target` `conflict`
  listing both values. An equal record is fine.

Refused, before anything changes, each as its own line: `<old>` with no record
that reads (`target` `unrecorded`, `conflict`, `unreadable`), a record that
lands into it and does not read, a member branch some live weave branch declares
(`protected` `declared`, listing the weave branches declaring it), a `<new>`
that exists as a local branch of a repo the rename moves or on a remote without
`--adopt` (`name` `exists`), a feature's `<new>` that a live weave branch
declares for a member (`name` `declared`) or that another project's feature uses
for a member both list (`name` `feature`), the records branch's name (`name`
`reserved`), and a branch checked out in another workspace (`checkout`).

The writes are ordered so that a run which stopped partway is a state the same
command, run again, completes: members first, then the record, then the project
branch.

### `delete`

Deletes `<W>` and the branches that belong to it, by the deletion rule. `<W>`
may exist only on a remote, as last fetched. What belongs to `<W>` is the
project branch `<W>` and, in each member `<W>`'s `rwv.toml` lists, the branch of
the same name that no live weave branch declares: a declared branch never
belongs, so `main` and a release line's member branches are never touched.

A branch that belongs is deleted when everything on it has landed on the branch
`<W>` lands into (the record's target, or `--target <T>`, which a branch that
is not a feature needs), when no live lock pins a commit that only it reaches,
and, for the project branch, when its published copy has landed and no member
branch that belongs survives. Members are judged first. Otherwise it survives
with its record, reported with its reason: `unlanded`, `pinned`, `open-head`,
`members` or `unconfirmed`. The exit status is then **3**, and `--json` lists
each survivor and why. A branch is a target's: `<T>` needs a record that reads
(`target` `unrecorded` otherwise).

Refused, before anything changes: a `<W>` checked out in any workspace
(`checkout`), a `<W>` other branches' records land into (`target`
`dependents`, one line for each), and a `<W>` that is not a feature with no
`--target` (`target` `no target`).

- `--discard-unlanded` also deletes a branch that has not landed, after keeping
  its tip as a backup (`rwv backup list`). It lifts the landed clause and no
  other: a pinned branch still survives.
- `--delete-remote` also deletes each push branch of what belongs that has
  landed on the target's remote-tracking branch.
- `--delete-remote-unlanded <repo>`, one repo per occurrence and needing
  `--discard-unlanded`, also deletes that repo's push branch whether or not it
  landed (a pull request's head included), after keeping its tip as a backup.
  The repo is its workspace-relative path, or `projects/<name>` for the project
  repo. It never deletes the push branch of a declared-mode project branch.
  Remote deletes are leased to the tip the last fetch saw.

The rule starts with a pruning fetch of only the remotes these branches have an
upstream or a push branch on. A branch on a remote that could not be fetched is
kept (`unconfirmed`), and nothing on it is deleted remotely. A branch never
published is judged on local refs, offline too.

When a branch is gone locally and from every remote, with no workspace of this
machine aligned for it, its entry on the records branch is dropped in the same
write: an entry is a file at the branch's name, so a stale `a` would block
recording `a/b`.

## Invocation

```
rwv branch set-mode [<W>] <feature|declared> [--target <T>] [--project <name>]
rwv branch list [--json] [--project <name>]
rwv branch rename <new> [--adopt] [--project <name>]
rwv branch delete <W> [--target <T>] [--discard-unlanded] [--delete-remote] [--delete-remote-unlanded <repo>]... [--json] [--project <name>]
```

- `<W>` and the mode are positional; the mode is always the last word, so a
  branch named `declared` is still a branch when two words are given.
- `--project <name>` acts on that project instead of the active one (does not
  change `.rwv-active`).

Run `rwv branch --help` for the full clap surface.

## Output

`set-mode` prints one line saying what is recorded, then any `target-recorded`
and dependent lines. `list` prints one line per branch, or `no weave branches`.
`rename` prints `renamed <repo>: <old> -> <new>` for each branch it renamed,
`moved the record of <old> to <new>: <record>`, `retargeted <dependent>` for
each record that lands into it and `adopted <repo>: <new> takes in from
<remote>/<new>` for each branch `--adopt` linked; each old remote branch is
`left: <repo> <old>: <remote> still has it` on stderr. `delete` prints `deleted <repo> <branch> (<site>): <because>` for each branch it
deleted and `dropped the record of <branch>` for each record it dropped; each
survivor is `left: <repo> <branch>: <reason>` on stderr, with what the reason
rests on beneath it.

`rwv branch list --json` and `rwv branch delete --json` each emit one envelope.
The list's:

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "BranchListJsonOutput",
  "description": "The envelope `rwv branch list --json` prints.",
  "type": "object",
  "required": [
    "$schema",
    "branches"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "branches": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/ListedBranch"
      }
    }
  },
  "definitions": {
    "ListedBranch": {
      "type": "object",
      "required": [
        "checked_out",
        "local",
        "name",
        "record",
        "target_gone"
      ],
      "properties": {
        "checked_out": {
          "description": "The project repo is on this branch.",
          "type": "boolean"
        },
        "local": {
          "description": "The branch exists locally; false for one only on a remote, as last fetched.",
          "type": "boolean"
        },
        "name": {
          "type": "string"
        },
        "published": {
          "description": "A remote's records, as last fetched, hold this entry. False for an entry only this machine has, which a push of the branch publishes.",
          "type": [
            "boolean",
            "null"
          ]
        },
        "record": {
          "$ref": "#/definitions/RecordOutput"
        },
        "target_gone": {
          "description": "The record's target exists nowhere, local or remote.",
          "type": "boolean"
        }
      }
    },
    "Mode": {
      "description": "How a weave branch is used.",
      "oneOf": [
        {
          "description": "Work onto a target branch: its `owned` and `fork` members sit on branches named after it.",
          "type": "string",
          "enum": [
            "feature"
          ]
        },
        {
          "description": "Members sit on the branches its member entries declare.",
          "type": "string",
          "enum": [
            "declared"
          ]
        }
      ]
    },
    "RecordOutput": {
      "description": "One weave branch's record, as `rwv branch list --json` carries it.",
      "oneOf": [
        {
          "type": "object",
          "required": [
            "state"
          ],
          "properties": {
            "state": {
              "type": "string",
              "enum": [
                "declared"
              ]
            }
          }
        },
        {
          "type": "object",
          "required": [
            "as",
            "state",
            "target_branch"
          ],
          "properties": {
            "as": {
              "$ref": "#/definitions/Mode"
            },
            "state": {
              "type": "string",
              "enum": [
                "feature"
              ]
            },
            "target_branch": {
              "description": "The branch the feature lands into.",
              "type": "string"
            }
          }
        },
        {
          "description": "The branch has no record here.",
          "type": "object",
          "required": [
            "state"
          ],
          "properties": {
            "state": {
              "type": "string",
              "enum": [
                "unrecorded"
              ]
            }
          }
        },
        {
          "description": "Two writers recorded the branch differently; both values are listed.",
          "type": "object",
          "required": [
            "state",
            "values"
          ],
          "properties": {
            "state": {
              "type": "string",
              "enum": [
                "conflict"
              ]
            },
            "values": {
              "type": "array",
              "items": {
                "$ref": "#/definitions/ValueOutput"
              }
            }
          }
        },
        {
          "description": "The entry does not read as a record.",
          "type": "object",
          "required": [
            "reason",
            "state"
          ],
          "properties": {
            "reason": {
              "type": "string"
            },
            "state": {
              "type": "string",
              "enum": [
                "unreadable"
              ]
            }
          }
        },
        {
          "description": "The branch's target chain returns to it.",
          "type": "object",
          "required": [
            "chain",
            "state"
          ],
          "properties": {
            "chain": {
              "type": "array",
              "items": {
                "type": "string"
              }
            },
            "state": {
              "type": "string",
              "enum": [
                "cycle"
              ]
            }
          }
        }
      ]
    },
    "ValueOutput": {
      "type": "object",
      "required": [
        "mode"
      ],
      "properties": {
        "as": {
          "anyOf": [
            {
              "$ref": "#/definitions/Mode"
            },
            {
              "type": "null"
            }
          ]
        },
        "mode": {
          "$ref": "#/definitions/Mode"
        },
        "target_branch": {
          "type": [
            "string",
            "null"
          ]
        }
      }
    }
  }
}
```

The delete's carries `branch`, `landed_on`, `deleted` (`path`, `branch`, `site`,
`because` and the `backup` kept first, when there was one), `survivors` (`path`,
`branch`, `reason`, `detail`) and `records_dropped`:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "BranchDeleteJsonOutput",
  "description": "The envelope `rwv branch delete --json` prints.",
  "type": "object",
  "required": [
    "$schema",
    "branch",
    "deleted",
    "landed_on",
    "records_dropped",
    "survivors"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "branch": {
      "type": "string"
    },
    "deleted": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/Deleted"
      }
    },
    "landed_on": {
      "description": "The branch the deleted branches were judged landed against.",
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
          "description": "A remote it has a copy on could not be fetched.",
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
        "detail",
        "path",
        "reason"
      ],
      "properties": {
        "branch": {
          "type": "string"
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
    }
  }
}
```

## Exit codes

- `0` — the record was written (or already held), the listing printed, the
  rename done, or `delete` deleted everything that belongs to the branch.
- `3` — `delete` finished and the rule kept at least one branch; the output
  lists each and why.
- non-zero — a refusal (`target`, `name`, `checkout`) or a branch the verb could
  not read.

## Examples

Say that a branch made by hand is the project's trunk:

```
rwv branch set-mode main declared
```

Say that a branch is a feature that lands into `main`:

```
rwv branch set-mode feat-x feature --target main
```

See every weave branch and how it is used:

```
rwv branch list
```

Give a workweave made without a branch its real name:

```
rwv branch rename fix-123
```

Take up a rename made on the forge, in a workspace on the old name:

```
rwv branch rename trunk --adopt
```

Delete a feature once it has landed, with its member branches:

```
rwv branch delete feat-x
```

Delete one that was merged elsewhere (a squash, say), keeping a backup, and its
published branches too:

```
rwv branch delete feat-x --discard-unlanded --delete-remote
```

## Common errors

- *target unrecorded* — the branch has no record here. Its record may be
  published but not yet fetched (`rwv fetch`); otherwise say how it is used with
  `rwv branch set-mode`. When it is a `--target` that has no record, record that
  branch first.
- *target conflict* — two writers recorded the branch differently. Both values
  are listed; `rwv branch set-mode` writes the one you choose.
- *target unreadable* — the entry does not read as a record; the refusal says why
  and which rwv release wrote it. `rwv branch set-mode` rewrites it.
- *target cycle* — the chain of targets returns to the branch. The chain is
  listed; pick a `--target` outside it.
- *name reserved* — the name is the one rwv keeps its branch records under, or is
  below it.
- *target dependents* — other branches' records land into the branch. Each is
  listed; rebind it with `rwv branch set-mode <dependent> feature --target
  <T>`.
- *target no target* — the branch is declared, so its record names no branch it
  landed on. Name one with `--target <T>`.
- *protected declared* — a member branch the rename would move is declared by the
  listed weave branches, and is never renamed. Rename the feature to a name no
  weave branch declares for the member's branch: the refusal names the branches
  declaring it.
- *name exists* — `<new>` is a local branch, or a remote has it as last fetched.
  Choose another name, or with `--adopt` take up the remote's.
- *name declared*, *name feature* — a feature cannot take a name that a member's
  declared branch has, nor one another project's feature uses for a member both
  list.
- *checkout* — the branch is checked out in another workspace (named). Switch
  that workspace off it, or delete its workweave.
- *name no-such-branch* — `<W>` is a branch nowhere in the project repo, local or
  remote as last fetched. `rwv fetch` brings in what a remote has published.
