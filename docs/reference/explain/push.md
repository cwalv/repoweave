# rwv push

## Purpose

Publish a cross-repo feature to shared remotes. Pushes member repos first,
then the project repo last — the project repo carries the committed lock that
pins manifest SHAs, so collaborators' `rwv clone` and `rwv update` must never
see a committed lock referencing unpushed manifest commits.

Refuses if invoked from a workweave (workweave branches shouldn't leak to
shared remotes). Refuses if the project repo has uncommitted changes to any
tracked file, or holds a `rwv.toml` or `rwv.lock` it has never committed: push
publishes the project repo's last commit, so the lock it checks has to be the
lock it publishes. The refusal judges the project repo on the `edits` kind:
`rwv.toml or rwv.lock uncommitted` when those are all that is, offering `rwv
lock --commit`, else `dirty`, offering the VCS's commit or stash. Untracked
files other than those two do not refuse. Judges every member repo's HEAD
against its lock entry on the `pin` axis (the pin check), and refuses a member
with no pin (`none`), a pin no ref reaches (`unreachable`), a HEAD behind the
pin (`behind`) or diverged from it (`diverged`), or commits beyond the pin that
no remote-tracking branch and no live lock reaches (`unpinned`, listed). Each
refusal prints the choice that clears it: `rwv lock --commit` records unpinned
commits, `rwv member pin` gives a member a pin, `rwv update` moves a checkout
that is behind its pin. Commits beyond the pin that are pushed, or pinned by
another project's live lock, are reported as `ahead` with the fact `pinned-by`
naming the lock, and the push goes on.

**Default scope:** bare `rwv push` (no `--role` / `--repo` selectors) pushes
only `owned` and `fork` repos — the roles the operator controls. `dependency`
and `reference` repos are skipped with a one-line notice each. To include
non-writable roles, use `--role` or `--repo` selectors (which override the
default):

```
rwv push --role dependency          # push all dependencies
rwv push --repo github/acme/dep     # push one specific dep
rwv push --role owned --role fork   # explicit; same as the bare default
```

**Where each branch goes.** A branch publishes to its push target, found in
git's order: its `pushRemote`, `remote.pushDefault`, the remote it takes in
from, and last the repo's url remote (a member's: the remote that has its
`url`; the project repo's: the one it was created with, else its only remote).
Before anything is pushed, `push` judges each branch on the `push-branch` kind
and refuses the ones that cannot publish: `no push target`, `foreign upstream`
(it takes in from a branch of another name), and `diverged` while it holds
commits of its own. Two states complete instead: a branch `behind` its push
branch is left unpushed with the choice to take it in, and a branch whose
upstream the remote deleted (`gone`) is left unpushed unless `--repo` or
`--role` names its repo. `--force` waives the divergence refusal and the
`behind` skip.

Agents call `rwv push --json` to get machine-readable per-repo outcomes so
they can react to partial failures without parsing human-readable text.

## Invocation

```
rwv push [--json] [-j <N>] [--dry-run] [--force] [--role <role>] [--repo <selector>] [--project <name>]
```

- `--json` emits machine-readable output (see Output below).
- `-j <N>` runs up to `N` member-repo pushes in parallel. Default is `1`
  (serial): every push authenticates and runs the repo's `pre-push` hook, so
  concurrent pushes would share one terminal's credential prompts and run
  those hooks side by side, and the default push set is already narrowed to
  the writable roles. The project-repo push always runs serially as the last
  step regardless of `-j`.
- `--dry-run` prints the push plan without executing any pushes.
- `--force` force-pushes every repo in the operation.
- `--role` / `--repo` narrow the member-repo push loop (union semantics)
  and override the default `[owned, fork]` scope. When either flag is
  present, the caller's selectors are used verbatim — pass `--role owned
  --role fork` to reproduce the bare default explicitly. The
  pin check always runs against the full manifest regardless of these
  filters.
- `--project <name>` operates on the named project without changing
  `.rwv-active`.

Run `rwv push --help` for the full clap surface.

## Output

Default text output is one line per repo and a summary.

Under `--json`, output is the pretty-printed envelope, whatever `-j` is:

```
{
  "$schema": "<url>",
  "outcomes": [ { "kind": "...", "path": "...", "absolute_path": "...", ... }, ... ],
  "refused": [ { "path": "...", "axis": "...", "state": "...", "choices": [ ... ], ... }, ... ]
}
```

Outcome `kind` tags for member repos: `pushed`, `skipped` (e.g. filtered
by selector), `failed`. The project-repo record is always the last entry in `outcomes` and
uses kind `project-repo-pushed` or `project-repo-failed` — its kind tag is the
only way to distinguish it from member-repo records in the flat array. Failed
records carry a `message` field with the free-form error from the git push.

`refused` lists each repo push judged when it refused, with that state's
choices as `rwv explain <axis>` lists them: the project repo's `edits` when it
has uncommitted changes, before anything is pushed; and each member whose
push the remote rejected, as `publish` `withheld`, when the project repo was
not pushed because of it. It is empty, not absent, otherwise. `reported`
lists the repos judged `behind` or `gone` and left unpushed, with their
choices; it is empty when none was.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "PushJsonOutput",
  "description": "Top-level envelope for `rwv push --json`.\n\nShape: `{ \"$schema\": \"<url>\", \"outcomes\": [<PushOutcomeOutput>, ...] }`. Member-repo records appear first, sorted by repo path; the branch's record and the project-repo record follow, in that order. Consumers can distinguish the project-repo record by checking `kind` for `\"project-repo-pushed\"` or `\"project-repo-failed\"`.",
  "type": "object",
  "required": [
    "$schema",
    "outcomes",
    "refused",
    "reported"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "outcomes": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/PushOutcomeOutput"
      }
    },
    "refused": {
      "description": "Each repo push judged before refusing, with that state's choices. Empty, not absent, when nothing was refused on a repo's state; a project repo withheld because members' pushes failed lists each of those members as `publish` `withheld`.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/JudgedOutput"
      }
    },
    "reported": {
      "description": "Each repo push judged and went past without refusing it: a branch behind its push branch, one whose upstream is gone, a merge whose unmerged pin a later lock waived, with that state's choices.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/JudgedOutput"
      }
    },
    "resolution": {
      "description": "Resolved workspace coordinates (weave root, optional workweave identity, project). Absent when no project is resolved.",
      "anyOf": [
        {
          "$ref": "#/definitions/Resolution"
        },
        {
          "type": "null"
        }
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
          "description": "Where a workspace's checkouts sit against where the checked-out branch's record says they should.",
          "type": "string",
          "enum": [
            "alignment"
          ]
        }
      ]
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
    "PushOutcomeOutput": {
      "description": "One per-repo outcome record in `rwv push --json` output.\n\nMember-repo records use `kind` values `pushed`, `skipped`, and `failed`. The project-repo record uses `kind` values `project-repo-pushed` and `project-repo-failed`, and the record of the branch `records-published` and `records-rejected`, making each distinguishable from member-repo records in the same flat `outcomes` array.",
      "oneOf": [
        {
          "description": "Member repo was pushed successfully.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "kind": {
              "type": "string",
              "enum": [
                "pushed"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "Member repo was skipped (e.g. by a selector filter).",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "kind": {
              "type": "string",
              "enum": [
                "skipped"
              ]
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "Member repo push failed.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "message",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "kind": {
              "type": "string",
              "enum": [
                "failed"
              ]
            },
            "message": {
              "description": "Free-form error message from the git push attempt.",
              "type": "string"
            },
            "path": {
              "type": "string"
            }
          }
        },
        {
          "description": "The branch's record, and the drops this machine had made, reached the remote the project branch publishes to.",
          "type": "object",
          "required": [
            "absolute_path",
            "dropped",
            "kind",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "dropped": {
              "description": "The entries the publish removed from the remote's records.",
              "type": "array",
              "items": {
                "type": "string"
              }
            },
            "kind": {
              "type": "string",
              "enum": [
                "records-published"
              ]
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        },
        {
          "description": "The remote refused its records branch. For the remote's default branch recorded as `declared` the project branch was published all the same.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "message",
            "path",
            "project",
            "withheld"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "kind": {
              "type": "string",
              "enum": [
                "records-rejected"
              ]
            },
            "message": {
              "description": "What the remote answered.",
              "type": "string"
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            },
            "withheld": {
              "description": "Whether the project branch was withheld because of it.",
              "type": "boolean"
            }
          }
        },
        {
          "description": "Project repo was pushed successfully (always the last record).",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "kind": {
              "type": "string",
              "enum": [
                "project-repo-pushed"
              ]
            },
            "path": {
              "type": "string"
            },
            "project": {
              "description": "The project name (e.g. `\"my-app\"`). Distinguishes the project repo's path convention (`projects/<name>/`) from member-repo paths.",
              "type": "string"
            }
          }
        },
        {
          "description": "Project repo push failed.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "message",
            "path",
            "project"
          ],
          "properties": {
            "absolute_path": {
              "$ref": "#/definitions/WirePath"
            },
            "kind": {
              "type": "string",
              "enum": [
                "project-repo-failed"
              ]
            },
            "message": {
              "description": "Free-form error message from the git push attempt.",
              "type": "string"
            },
            "path": {
              "type": "string"
            },
            "project": {
              "type": "string"
            }
          }
        }
      ]
    },
    "Resolution": {
      "description": "Resolved workspace coordinates for `--json` output and the plugin env-var envelope.\n\nCarries `workspace` (primary root abs path), `workweave` (the `<project>--<name>` identity the registry records), `project` (resolved project name), and `workweave_unregistered`. No `kind` or `location` field: the checkout is one of three states, and two of them are already carried by `workweave`'s presence.\n\nThe third state needs a field of its own. A workweave whose directory no registry entry names has no identity, so `workweave` is absent for it — and absent is what the primary looks like. Without `workweave_unregistered` the two serialize identically, and a consumer reading the documented meaning of that absence is told, positively, that it is at the primary checkout.\n\nResults only — provenance (which chain step resolved the project, which flag addressed the workspace) is deliberately excluded: anything in default `--json` output becomes depended on, and the assertion use case needs the result, not the mechanism. Provenance appears only in the human-facing \"target:\" line printed to stderr.\n\nIsomorphic to the plugin env-var envelope (`RWV_WORKSPACE`/`RWV_WORKWEAVE`/`RWV_WORKWEAVE_UNREGISTERED`/`RWV_PROJECT`): both surfaces are pure projections of `WorkspaceContext::resolution`, never independently computed.",
      "type": "object",
      "required": [
        "workspace"
      ],
      "properties": {
        "project": {
          "description": "Resolved project name; absent when the workspace resolved but no project did (a primary with no project selected).",
          "type": [
            "string",
            "null"
          ]
        },
        "workspace": {
          "description": "Primary weave root.",
          "allOf": [
            {
              "$ref": "#/definitions/WirePath"
            }
          ]
        },
        "workweave": {
          "description": "Workweave identity (`<project>--<name>`), as the primary-side registry records it.\n\nAbsent at the primary, and absent for a workweave whose directory no registry entry names — identity is by record, so an unregistered workweave has no identity to report and rwv will not spell one from the directory name. Those two absences are told apart by `workweave_unregistered`, not by this field.",
          "type": [
            "string",
            "null"
          ]
        },
        "workweave_unregistered": {
          "description": "`true` when the invocation resolved into a workweave whose directory no registry entry names, so `workweave` above is absent for a reason that is not \"this is the primary\".\n\nSerialized only in that state, so the primary and a registered workweave emit exactly the bytes they emitted before this field existed. `rwv doctor --fix` registers such a directory, after which this is absent and `workweave` carries the identity.",
          "type": "boolean"
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

- `0` — all in-scope member repos pushed (non-writable roles skipped by
  default or by selector); project repo pushed.
- non-zero — at least one manifest push failed (project repo not pushed), or
  the project-repo push failed after manifest pushes succeeded.

## Examples

Push the active project and inspect outcomes:

```
rwv push --json | jq '.outcomes[] | {kind, path}'
```

Find failed push outcomes:

```
rwv push --json | jq '.outcomes[] | select(.kind == "failed" or .kind == "project-repo-failed")'
```

Identify the project-repo record:

```
rwv push --json | jq '.outcomes[] | select(.kind | startswith("project-repo-"))'
```

Push four member repos at a time:

```
rwv push -j 4
```

## Common errors

- *`projects/<name>: edits …`* — the files listed would not reach the remote,
  since push publishes the project repo's last commit. Run the choice printed
  beneath it (`rwv lock --commit` when only `rwv.toml` or `rwv.lock` is
  uncommitted, the VCS's commit or stash otherwise) and re-run.
- *`push-branch no push target`* — the branch has no remote to publish to: none
  is configured for it and the repo has no url remote. Run a choice printed
  beneath it (configure its push remote, or add the remote).
- *`push-branch diverged`* — the branch and its push branch each hold commits
  the other lacks, and the branch's own are listed. Take theirs in first.
- *`<member>: pin unpinned`* — the member holds the commits listed beyond its
  pin, which nothing else holds. Run `rwv lock --commit` to record them, or move
  the repo's branch back to its pin without detaching it: `git branch <name>`
  to keep the commits it moves off, then `git reset --keep <lock commit>`. A
  `git checkout` of the lock commit detaches HEAD, which push refuses.
- *`<member>: pin none`* — the lock has no entry for the member. `rwv member pin
  <member> <rev> --commit` gives it one.
- *`<member>: pin behind`* or *`pin diverged`* — the checkout is behind its pin,
  or has gone its own way. `rwv update` moves a checkout behind its pin;
  `rwv lock --repo <member> --commit` records the checkout, by name.
- *refusing to push from workweave* — run `rwv sync-to primary` (or
  `rwv sync-to`) to land changes on primary, then push from there.
- *project repo not on canonical branch* — check out the canonical branch
  before pushing.
- *`<member>: publish withheld`* — that member's push was rejected, listed
  beneath it, so the project repo was not pushed; members that pushed are
  published. Resolve the rejection, or choose where the member pushes, then
  re-run `rwv push`.
- *target unrecorded* — the checked-out branch of the project repo has no record
  of how it is used, so this verb, which publishes, refuses before changing
  anything. The record may be published but not yet fetched (`rwv fetch`);
  otherwise say how the branch is used with `rwv branch set-mode`.
