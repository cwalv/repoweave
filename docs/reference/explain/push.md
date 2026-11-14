# rwv push

## Purpose

Publish a weave branch: its members' branches first, then the branch's record
and the drops this machine has made, then the project branch last. The project
branch carries the committed lock that pins each member's commit, so
collaborators' `rwv clone` and `rwv update` must never see a lock that names a
commit no remote holds; the record travels with the branch it describes, so
every machine reads one record for a published branch.

`push` runs from any workspace, the primary or a workweave, on the weave
branch the project repo has checked out, which must have a record
(`target` `unrecorded` otherwise: `rwv branch set-mode`) and a name rwv
publishes. A project branch named `<project>--<workweave>`, the name a
workweave made without a branch of its own carries, is never published
(`name` `minted`). A project repo on a detached HEAD is a detached workspace,
which runs no push (`alignment` `project detached`). `--branch <W>` pushes
`W` from a workspace that has another branch checked out, judging each member
by `W`'s record, lock and branches as the refs hold them. `push` never writes
`rwv.lock`: a member holding a commit no lock pins refuses (`pin` `unpinned`,
naming `rwv lock --commit`), and so does a member whose lock has no entry (`pin`
`none`).

It refuses while the project repo's `rwv.toml` or `rwv.lock` has uncommitted
changes or was never committed (`edits` `rwv.toml or rwv.lock uncommitted`,
offering `rwv lock --commit`): push publishes the project repo's last commit
and plans from the two files it holds, so the lock it checks is the lock it
publishes. Other uncommitted files are not part of what is pushed: they are
named on one line, and the push goes on.

**Which repos.** Bare `rwv push` acts on the `owned` and `fork` members, the
branch's record and the project branch. A `dependency` or `reference` member
is pushed only when `--role` or `--repo` names it. A selector narrows the push
to the repos it names: the project branch and the record are pushed only when
`--repo projects/<name>` names the project repo.

```
rwv push --role dependency                        # push all dependencies, and nothing else
rwv push --repo github/acme/dep                   # push one specific dep
rwv push --repo github/acme/app --repo projects/web  # a member and the project branch
```

**The pin gate.** Before anything is pushed, each member the push acts on is
judged against the lock: it must be on the branch the checked-out branch's
record expects (`alignment`), have a pin (`pin` `none`, `unreachable`), reach
it (`behind`, `diverged`), and hold no commit beyond it that no remote-tracking
branch and no live lock reaches (`unpinned`). Commits beyond the pin that are
pushed, or pinned by another project's live lock, are reported as `ahead`
with the fact `pinned-by` naming the lock, and the push goes on. Every merge in
the range the project branch publishes is judged too: where both its parents
changed a member's pin to different commits under one declared branch, the
lock at the pushed tip must pin a commit containing both, unless the tip
declares the member elsewhere or a commit in the range at or after the merge,
the merge's own included, records re-pinning it backward by name to a commit
that does not reach the pin the merge left behind. rwv's by-name verbs record
that in an `Rwv-Repin` trailer on their lock commits; a commit made with git,
a raw merge whose lock conflict was settled by hand included, records none and
waives nothing, and a trailer whose pins disagree with what its commit's lock
changed waives nothing and is listed. A merge that does neither refuses as
`pin` `unmerged`, listing the merge and the pins and offering the VCS's merge
and `rwv member pin <m> <rev> --commit`; one a re-pin waived is reported as
`pin` `waived`, listing the merge, the re-pin and the verb that made it. After the members are pushed, the
project branch is withheld (`publish` `withheld`) while any pin its lock holds
is not reached as the gate requires: a feature's `owned` and `fork` pins by a
remote-tracking branch of the url remote or of the push target, a declared
branch's by the push branch of the branch each member declares, and a
`dependency` or `reference` member's by a remote-tracking branch of its url
remote, and a pin whose member is not materialized here is `pin`
`unreachable`. The refusal lists the branches that do reach each pin. Each
branch publishes the commit the push judged, so a branch that moves while the
push runs keeps its new commits for the next push.

**Where each branch goes.** A branch publishes to its push target, found in
git's order: its `pushRemote`, `remote.pushDefault`, the remote it takes in
from, and last the repo's url remote (a member's: the remote that has its
`url`; the project repo's: the one it was created with, else its only remote).
`push` fetches every remote first, pruning, and a remote the fetch could not
complete that the push publishes to or judges a pin by refuses it (`push-branch`
`unconfirmed`, the cause listed). Then it judges each branch on the
`push-branch` kind and refuses the ones that cannot publish: `no push target`,
`foreign upstream` (it takes in from a branch of another name), and `diverged`
while it holds commits of its own. Two states complete instead: a branch
`behind` its push branch is left unpushed with the choice to take it in, and a
branch whose upstream the remote deleted (`gone`) is left unpushed unless
`--repo` names its repo. A branch that took in from nothing takes in from its
push branch once pushed. A feature's member branch is pushed only when it holds
commits the target's pin for the member lacks, read in the target's lock at
the newer of the target and its own upstream; a target diverged from its
upstream refuses as `push-branch` `diverged`.

**A half-published composition.** A push that publishes members and then does
not publish the project branch (a pin withheld, or the remote refusing the
project branch because it moved after the fetch) leaves members' new commits
public while the published lock still pins the old ones. That state is safe to
sit in: every published lock still names commits that exist, nothing needs
undoing, and nothing is replaced. A project branch that is behind its push
branch, or has diverged from it, is refused before any member is published,
with how many commits it is ahead and behind (`ahead`, `behind`); a push that
refuses there has published nothing. Where the project branch has diverged,
`rwv pull --merge-upstream` merges the push branch into it, each member the two
sides both moved merged, the lock written from the merged members and a member
the merge drops removed; `rwv push` then publishes. Where the project branch is
only behind, `rwv pull` takes the push branch in. Where the push was withheld
or a member's push was rejected, the refusal names the member; fix that and
re-run `rwv push`, which publishes what is left.

**The record.** The branch's record, and every drop this machine has made of a
record whose branch is gone everywhere, is published to the project branch's
push target before the project branch, as one commit on that remote's records
branch: no other record on this machine is sent, and a conflicted or unreadable
one never is. A remote that moved meanwhile is fetched and the commit rebuilt.
A remote that refuses its records branch does not stop the push: the project
branch is published without its record there and the rejection is reported as
a fact (`publish` `records rejected`), which names what another machine
opening the branch will be asked.

**Replacing a push branch.** `--overwrite <repo>`, naming each repo, replaces a
diverged push branch of a branch no weave branch declares and that is not a
declared-mode project branch. The remote tip is kept as a backup first, the
replacement is leased to that tip, so a remote that moved since the fetch
refuses it, and it is refused (`pin` `would be unreachable`) while a live
lock pins a commit that, once replaced, no remote-tracking branch reaches (a
published project branch's lock) or nothing reaches (a local one). The lock of
the project branch this push republishes fast-forward is exempt. `push` offers
it nowhere.

Agents call `rwv push --json` to get machine-readable per-repo outcomes so
they can react to partial failures without parsing human-readable text.

## Invocation

```
rwv push [--json] [-j <N>] [--dry-run] [--branch <W>] [--overwrite <repo>]... [--role <role>] [--repo <selector>] [--project <name>]
```

- `--json` emits machine-readable output (see Output below).
- `-j <N>` runs up to `N` member-repo fetches and pushes in parallel. Default
  is `1` (serial): every push authenticates and runs the repo's `pre-push`
  hook, so concurrent pushes would share one terminal's credential prompts and
  run those hooks side by side, and the default push set is already narrowed
  to the writable roles. The project-repo push always runs serially as the
  last step regardless of `-j`.
- `--dry-run` runs the same pruning fetch as the push and prints the push plan without pushing; every refusal the push would make is made, and a remote the fetch cannot complete refuses as `push-branch` `unconfirmed`.
- `--branch <W>` pushes weave branch `W` instead of the checked-out one.
- `--overwrite <repo>` replaces that repo's diverged push branch, leased and
  backed up, as above; repeat it per repo. The project repo is
  `projects/<name>`.
- `--role` / `--repo` narrow what the push acts on (union semantics). A
  `dependency` or `reference` member is pushed only when one names it.
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
by selector), `failed`. The branch's record follows as `records-published`
(listing the entries it dropped from the remote) or `records-rejected` (with
the remote's answer; the project branch is published all the same). The project-repo record is always the last entry in `outcomes` and
uses kind `project-repo-pushed` or `project-repo-failed` — its kind tag is the
only way to distinguish it from member-repo records in the flat array. Failed
records carry a `message` field with the free-form error from the git push.

`refused` lists each repo push judged when it refused, with that state's
choices as `rwv explain <axis>` lists them: the project repo's `edits` when it
has an uncommitted `rwv.toml` or `rwv.lock`, before anything is pushed; and each member whose
push the remote rejected, as `publish` `withheld`, when the project repo was
not pushed because of it, and the project repo the same way when its own push
was rejected. It is empty, not absent, otherwise. `reported`
lists the repos judged `behind` or `gone`, and the merges a re-pin by name waived
(`pin` `waived`), and went past; it is empty when none was.

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
            "project"
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
              "description": "What the remote answered. The project branch is published all the same, without its record there.",
              "type": "string"
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

- `0` — every in-scope member repo pushed or left as it was, the record
  published, and the project repo pushed.
- non-zero — a refusal before anything was pushed; a member push failed
  (project repo not pushed); the project branch was withheld because a pin it
  holds is not reached; or the project-repo
  push failed after the member pushes succeeded.

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

Replace a rewritten feature branch of one member, keeping the remote tip as a backup:

```
rwv push --overwrite github/acme/app
```

Push four member repos at a time:

```
rwv push -j 4
```

## Common errors

- *`projects/<name>: edits rwv.toml or rwv.lock uncommitted`* — the files
  listed would not reach the remote, since push publishes the project repo's
  last commit. Run `rwv lock --commit` and re-run.
- *`push-branch no push target`* — the branch has no remote to publish to: none
  is configured for it and the repo has no url remote. Run a choice printed
  beneath it (configure its push remote, or add the remote).
- *`push-branch diverged`* — the branch and its push branch each hold commits
  the other lacks: `ahead` and `behind` give how many each, and the branch's own
  are listed. For the project repo, `rwv pull --merge-upstream` merges theirs
  in; then re-run `rwv push`. Nothing was published.
- *`push-branch behind`* — the push branch holds `behind` commits the branch
  lacks. Take them in first, by the printed choice.
- *`<member>: pin unpinned`* — the member holds the commits listed beyond its
  pin, which nothing else holds. Run `rwv lock --commit` to record them, or move
  the repo's branch back to its pin without detaching it: `git branch <name>`
  to keep the commits it moves off, then `git reset --keep <lock commit>`. A
  `git checkout` of the lock commit detaches HEAD, which push refuses.
- *`<member>: pin none`* — the lock has no entry for the member. `rwv member pin
  <member> <rev> --commit` gives it one.
- *`<member>: pin behind`* or *`pin diverged`* — the checkout is behind its pin,
  or has gone its own way. `rwv update` moves a checkout behind its pin;
  `rwv member pin <member> HEAD --commit` pins the checkout, by name.
- *`projects/<name>: name minted`* — the project branch is the name a
  workweave made without a branch of its own carries, which is never
  published. Give the work a branch of its own to publish it.
- *`<repo>: push-branch unconfirmed`* — the last fetch of a remote the push
  needs could not complete, the cause listed. Nothing was pushed; run `rwv
  fetch` once the remote answers, then re-run.
- *`<member>: pin unmerged`* — a merge in the range joined two pins of the
  member without joining the member. Merge the other pin into the member's
  checkout (the printed choice), then `rwv lock --commit`.
- *`<member>: publish withheld`* — either that member's push was rejected, or a
  pin the project branch's lock holds is reached by no remote-tracking branch
  that must reach it; what the remote answered, or the branches that do reach
  the pin, are listed beneath it, so the project repo was not pushed and the
  members that pushed are published. Publish the member (`rwv push --repo
  <member>`, naming a `dependency` or `reference` one), or choose where it
  pushes, then re-run `rwv push`.
- *`projects/<name>: publish records rejected`* — a fact, not a refusal: the
  remote refused the branch that holds the records, and the project branch was
  published without its record there. Another machine opening the branch finds
  it unrecorded and is asked once how to treat it (`rwv fetch` where the record
  may be published elsewhere, else `rwv branch set-mode`).
- *target unrecorded* — the checked-out branch of the project repo has no record
  of how it is used, so this verb, which publishes, refuses before changing
  anything. The record may be published but not yet fetched (`rwv fetch`);
  otherwise say how the branch is used with `rwv branch set-mode`.
