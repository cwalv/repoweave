# rwv cherry-pick

## Purpose

Port one project commit onto the checked-out feature: what the commit
introduced is applied as new commits in each repo it moves. To port a fix
into a line, run it in a feature whose target is that line, then land the
feature with `rwv land`. A declared branch refuses as `target` `no target`,
naming `rwv branch create <W>` and `rwv workweave create <ww>`, which open a
feature onto it.

The commit's `rwv.lock` is read against its parent's; a merge commit is read
against the parent `-m <n>` names. Every refusal is judged before anything
moves, and all of them are reported together. Then, under an op record:

1. **Members.** Each `owned` or `fork` member whose pin the commit moves
   forward gains, on its branch here, copies of the commits the new pin
   reaches and the old one does not, less those this branch already holds,
   oldest first; each copy records the commit it copies. A copy that comes
   out empty is dropped and reported. Where the new pin is a merge whose
   parent `-m` names is the old pin, that merge is copied against the same
   parent instead, so a landing made by merge ports whole. Each `dependency`
   or `reference` member is never copied: its new pin is carried by name,
   and a pin here that already reaches it is kept.
2. **The project commit** applies the commit's project files other than
   `rwv.lock`, with the relock: each copied member's new tip, each carried
   pin, and each `--except` member's checked-out tip. It keeps the commit's
   message and author and records the commit it mirrors. A commit leaving
   nothing to port makes no commit, so a rerun after a completed port
   changes nothing.
3. **Alignment.** Every member is aligned as `rwv update` aligns it, to the
   lock just committed.

**Entries are never ported.** Each change the commit made to a member entry
(its declared branch, url, role, `history` or `recurse-submodules`, or the
member added or removed) or to the top-level `history` is reported
`not-ported`, with the parent's value, the commit's and this branch's, and the
setter that would port it. This branch keeps its own. The other top-level
tables, `[integrations]` and `[workweave]`, port as content.

## Refusals

| Axis | State | When |
|---|---|---|
| `target` | `no target` | the checked-out branch is declared, not a feature |
| `name` | `no-such-branch` | the revision names nothing in the project repo |
| `port` | `mainline` | a merge commit without `-m`, an `-m` naming a parent the commit lacks, or an `-m` on a commit that is no merge |
| `port` | `merge in range` | the commits a member's copy would make hold a merge |
| `port` | `backward` | the commit moves a member's pin backward, a diverged change included |
| `port` | `not listed` | the commit moves a member this branch does not list |
| `pin` | `unreachable` | the commit or its parent pins a member commit this clone lacks |
| `pin` | `not on declared branch` | a carried pin is on neither the member's declared branch nor its upstream |
| `pin` | `ahead`, `unpinned` | a member the port copies onto is not at this branch's pin |
| `pin` | `backwards` | an `--except` member's tip does not descend from its pin here |
| `edits` | `dirty`, `mid-operation` | a repo the port moves holds uncommitted changes or an operation in progress |
| `edits` | `rwv.toml or rwv.lock uncommitted` | the project repo's lock files hold uncommitted changes |
| `alignment` | any | a member the port copies onto, or one `--except` names, is not where the branch expects it |

Each `port` refusal for a member lists what its change holds and prints its
port by hand, then the rerun with `--except <m>`, which leaves that member out
and records it as it stands. `inapplicable-flag` refuses an `--except` naming
a repo the commit does not move, and `untracked-collision` a file the commit
writes into the project repo that stands there untracked.

A conflict stops the port with its op record kept (`op-parked`): resolve and
stage it in the checkout named, then `rwv cherry-pick --continue`, or
`rwv abort` to put every repo back. At a stop in the project repo `rwv.toml`
holds this branch's entries and the ported tables, so `rwv abort` and the
setters still read it; where a ported table changed on both sides, it holds
this branch's tables and the stop shows the commit's, to set as wanted and
stage. Where the VCS merged such a table's text with no conflict, the text is
compared with the merge of both sides' values (the commit's parent, this
branch's tip and the commit) in each key at most one side changed, a key both
changed keeping the VCS's text; where it does not parse or differs, the port
stops the same way, listing what differs. A table only one side changed is
taken by value, and entries and `history` are never ported, so neither is
compared. There is no `--keep` at this stop: edit `rwv.toml`, stage it, then
`rwv cherry-pick --continue`, or `rwv abort`. A step the VCS refuses once the port has started (an untracked file in
a member's way, a hook) also holds the op (`op-parked`), naming what failed.

`--continue` rechecks every repo the port moves before it moves anything: a
member on another branch than the one the port copies onto refuses
(`head-not-on-branch`), and a commit the port did not make, such as one made
by hand in a member during the stop, is a foreign tip (`foreign-tip`), never
taken for a copy; it names the way out, moving that commit off. An `--except`
member is judged again at its tip, so a hand port finished during the stop is
recorded where it ends.

## Invocation

```
rwv cherry-pick <project-rev> [-m <n>] [--except <repo>]... [--json]
                [--project <name>]
rwv cherry-pick --continue
```

- `-m <n>` — pick a merge commit against its parent `n`, from 1.
- `--except <repo>` — leave this member out of the port; its checked-out tip
  is recorded, forward only. Repeat per member.
- `--json` — emit machine-readable output (see Output below).
- `--project <name>` — operate on this project instead of the active project.
- `--continue` — resume a port a conflict stopped; every parameter comes from
  the op record.

Run `rwv cherry-pick --help` for the full clap surface.

## Output

Text output is one line per member copy (`copied <id> as <id>`) and per
dropped copy, one `not-ported` block per entry change with its setter, the
lines `rwv update` prints for the members it aligns, and a closing
`<project>: ported <id> as <id>`. With nothing to port it says the branch
already holds what the commit introduced.

Under `--json`, output is one pretty-printed envelope:

```
{
  "$schema": "<url>",
  "rev": "<id>",
  "members": [ { "path": "...", "copied": [ { "from": "...", "to": "..." } ], "dropped": [], "left_out": [] } ],
  "carried": [ { "path": "...", "from": "...", "to": "..." } ],
  "excepted": [ { "path": "...", "tip": "..." } ],
  "not_ported": [ { "entry": "...", "field": "...", "parent": "...", "rev": "...", "here": "..." } ],
  "commit": "<id>",
  "complete": true,
  "refused": [ ... ]
}
```

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "CherryPickJsonOutput",
  "description": "Top-level envelope for `rwv cherry-pick --json`.",
  "type": "object",
  "required": [
    "$schema",
    "carried",
    "complete",
    "excepted",
    "members",
    "not_ported",
    "refused"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "carried": {
      "description": "Each `dependency` or `reference` member whose pin the port carries.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/CarriedOutput"
      }
    },
    "commit": {
      "description": "The project commit the port made; absent where it made none.",
      "type": [
        "string",
        "null"
      ]
    },
    "complete": {
      "description": "True iff the port ran to its end and cleared its op-state.",
      "type": "boolean"
    },
    "excepted": {
      "description": "Each member `--except` left out, with the tip recorded for it.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/ExceptedOutput"
      }
    },
    "members": {
      "description": "Each `owned` or `fork` member the port copies onto.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/MemberPickOutput"
      }
    },
    "not_ported": {
      "description": "Each change to a member entry or the top-level `history` the commit made that the port leaves to its setter.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/NotPortedOutput"
      }
    },
    "refused": {
      "description": "Each repo the run judged when it refused, with that state's choices.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/JudgedOutput"
      }
    },
    "rev": {
      "description": "The project commit ported, once it resolved.",
      "type": [
        "string",
        "null"
      ]
    }
  },
  "definitions": {
    "Axis": {
      "description": "What a refusal judged a repo on. Its token is the kind `rwv explain` serves; the order here is the order a repo's lines print in.",
      "oneOf": [
        {
          "description": "Uncommitted work, or a VCS operation in progress, in a checkout.",
          "type": "string",
          "enum": [
            "edits"
          ]
        },
        {
          "description": "Another checkout derives from what the verb would remove.",
          "type": "string",
          "enum": [
            "checkout"
          ]
        },
        {
          "description": "A branch and the branch it takes in from or publishes to.",
          "type": "string",
          "enum": [
            "push-branch"
          ]
        },
        {
          "description": "A name the verb was given is unusable.",
          "type": "string",
          "enum": [
            "name"
          ]
        },
        {
          "description": "What `rwv push` withheld.",
          "type": "string",
          "enum": [
            "publish"
          ]
        },
        {
          "description": "A project whose `rwv.toml` does not load.",
          "type": "string",
          "enum": [
            "unparseable-project"
          ]
        },
        {
          "description": "The commit a lock pins for a member, and whether a ref reaches it.",
          "type": "string",
          "enum": [
            "pin"
          ]
        },
        {
          "description": "A weave branch's record: how it is used and the branch it lands into.",
          "type": "string",
          "enum": [
            "target"
          ]
        },
        {
          "description": "How a branch accepts landings and takes its target's work in.",
          "type": "string",
          "enum": [
            "history"
          ]
        },
        {
          "description": "Projects sharing a member that record different `history` for it.",
          "type": "string",
          "enum": [
            "history-disagreement"
          ]
        },
        {
          "description": "Where a workspace's checkouts sit against where the checked-out branch's record says they should.",
          "type": "string",
          "enum": [
            "alignment"
          ]
        },
        {
          "description": "A branch rwv never rewrites, renames or deletes.",
          "type": "string",
          "enum": [
            "protected"
          ]
        },
        {
          "description": "A project repo whose lock merge driver is not set up as this release sets it up.",
          "type": "string",
          "enum": [
            "missing-lock-merge-driver"
          ]
        },
        {
          "description": "A project commit `rwv cherry-pick` cannot port as it stands.",
          "type": "string",
          "enum": [
            "port"
          ]
        }
      ]
    },
    "CarriedOutput": {
      "type": "object",
      "required": [
        "path",
        "to"
      ],
      "properties": {
        "from": {
          "description": "The pin this branch held; absent where it held none.",
          "type": [
            "string",
            "null"
          ]
        },
        "path": {
          "type": "string"
        },
        "to": {
          "type": "string"
        }
      }
    },
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
    "CopiedOutput": {
      "type": "object",
      "required": [
        "from",
        "to"
      ],
      "properties": {
        "from": {
          "type": "string"
        },
        "to": {
          "type": "string"
        }
      }
    },
    "ExceptedOutput": {
      "type": "object",
      "required": [
        "path",
        "tip"
      ],
      "properties": {
        "path": {
          "type": "string"
        },
        "tip": {
          "type": "string"
        }
      }
    },
    "FactKind": {
      "description": "A fact about a judged repo. Facts are reported; they never select a choice.",
      "oneOf": [
        {
          "description": "A superseded tip was recorded for the branch.",
          "type": "string",
          "enum": [
            "rewritten"
          ]
        },
        {
          "description": "The branch is an ancestor of where it lands.",
          "type": "string",
          "enum": [
            "landed"
          ]
        },
        {
          "description": "Another project's live lock pins the commits.",
          "type": "string",
          "enum": [
            "pinned-by"
          ]
        },
        {
          "description": "Other live weave branches declare the branch.",
          "type": "string",
          "enum": [
            "shared"
          ]
        },
        {
          "description": "The pin is on the branch it lands into.",
          "type": "string",
          "enum": [
            "pin-landed"
          ]
        },
        {
          "description": "The member was left detached because its branch is checked out elsewhere.",
          "type": "string",
          "enum": [
            "checkout"
          ]
        },
        {
          "description": "The unlanded commits carry changes equivalent to commits where they land.",
          "type": "string",
          "enum": [
            "patch-equivalent"
          ]
        },
        {
          "description": "How many commits the branch holds that its push branch lacks.",
          "type": "string",
          "enum": [
            "ahead"
          ]
        },
        {
          "description": "How many commits the push branch holds that the branch lacks.",
          "type": "string",
          "enum": [
            "behind"
          ]
        }
      ]
    },
    "FactOutput": {
      "type": "object",
      "required": [
        "fact"
      ],
      "properties": {
        "about": {
          "type": [
            "string",
            "null"
          ]
        },
        "fact": {
          "$ref": "#/definitions/FactKind"
        }
      }
    },
    "JudgedOutput": {
      "description": "One judged repo, as `--json` carries it.",
      "type": "object",
      "required": [
        "axis",
        "choices",
        "facts",
        "listed",
        "path",
        "state"
      ],
      "properties": {
        "axis": {
          "description": "What it was judged on; `rwv explain <axis>` serves the axis's page.",
          "allOf": [
            {
              "$ref": "#/definitions/Axis"
            }
          ]
        },
        "choices": {
          "description": "What this state can run, in the order the page lists it.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/ChoiceOutput"
          }
        },
        "facts": {
          "type": "array",
          "items": {
            "$ref": "#/definitions/FactOutput"
          }
        },
        "listed": {
          "description": "The files, paths or commits the state lists.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "path": {
          "description": "The repo's path, `projects/<name>` for the project repo.",
          "type": "string"
        },
        "state": {
          "description": "The state it was judged in, as that page names it.",
          "type": "string"
        },
        "subject": {
          "description": "What the state names, when it names something.",
          "type": [
            "string",
            "null"
          ]
        },
        "workspace": {
          "description": "The workspace the repo was judged in, when that is not the one the verb ran in.",
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
    "MemberPickOutput": {
      "type": "object",
      "required": [
        "copied",
        "dropped",
        "left_out",
        "path"
      ],
      "properties": {
        "copied": {
          "description": "Each commit copied, with the commit that copies it.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/CopiedOutput"
          }
        },
        "dropped": {
          "description": "The commits whose copy came out empty, their change already here.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "left_out": {
          "description": "The commits of the range this branch already holds, not copied.",
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "path": {
          "type": "string"
        }
      }
    },
    "NotPortedOutput": {
      "type": "object",
      "required": [
        "entry",
        "field"
      ],
      "properties": {
        "entry": {
          "description": "The member's path, or `projects/<p>` for the top-level `history`.",
          "type": "string"
        },
        "field": {
          "description": "The field, or `added` / `removed` for the member itself.",
          "type": "string"
        },
        "here": {
          "type": [
            "string",
            "null"
          ]
        },
        "parent": {
          "type": [
            "string",
            "null"
          ]
        },
        "rev": {
          "type": [
            "string",
            "null"
          ]
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

## Exit codes

- `0` — the port completed, or there was nothing to port.
- `1` — it refused before anything moved, or a conflict stopped it with its
  op record kept.

## Examples

Port a fix from `main` into a feature off `stable`:

```
rwv cherry-pick 4f2a9c1
```

Port a landing made by merge, against its first parent:

```
rwv cherry-pick 4f2a9c1 -m 1
```

Port a commit whose `lib` range was copied by hand:

```
rwv cherry-pick 4f2a9c1 --except github/o/lib
```
