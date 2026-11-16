# rwv backup

## Purpose

List, drop or prune the **backups** rwv keeps. A backup is a ref an rwv verb
makes before any step that would leave a commit or an uncommitted edit
unpreserved — reached by no branch, no remote-tracking branch and no other
backup. It holds work you consented to discard, so that discarding it is not
losing it.

Backups are made by:

- `rwv workweave delete --discard-unlanded` and `rwv branch delete
  --discard-unlanded`, which keep each branch they delete that had not landed
  at its tip, and the commits a detached HEAD holds that no branch reaches;
- `rwv workweave delete --discard-uncommitted`, which keeps each checkout's
  uncommitted edits as a commit on its HEAD (untracked files included, ignored
  ones not);
- `rwv abort`, which keeps the tip it resets a branch from, where that tip
  holds a commit no other branch reaches, in the branch's one slot
  `<branch>/aborted`, and the working copy it discards in a checkout in the
  checkout's one slot `<branch>/aborted-working-copy`; the next abort that
  keeps something there overwrites the slot;
- `rwv doctor --fix --merge-edited-drift`, which keeps the edits it merges
  onto a moved branch in the branch's slot `<branch>/taken-out` while they
  are out of the working copy, and drops it once they stand there again;
- `rwv fetch`, which keeps the old tip of a remote-tracking branch the remote
  rewrote, marked `superseded`, so work built on it stays reachable and is not
  counted as unpushed;
- `rwv doctor --fix`, which turns a savepoint no operation owns any more into
  one when the savepoint is the last thing reaching its commits.

A backup is named `<branch>/<id>`: the branch it backs up (`HEAD` for a
detached HEAD) and an id that begins with the mark, if it has one, and ends
with the abbreviated id of the commit it holds. Making the same backup twice
names the same one. An abort slot's id is its mark alone. It stays until it is dropped — no verb drops one on its
own, and nothing but `rwv backup` does.

### What `list` shows

For each repo of the project that holds a backup — the project repo as
`projects/<name>`, a member by its path — one line per backup: its name, the
commit it holds, `[superseded]` for a tip a fetch found the remote had rewritten away, `[rebased]` for a
branch a rebase rewrote, `[aborted]` and `[aborted-working-copy]` for abort's slots, `[taken-out]` for a merge of edits' slot, and `[redundant]` when a branch or remote-tracking
branch reaches everything the backup holds, so dropping it loses nothing.
Under each line, the commits it holds that no branch reaches, newest first.
That list is what is lost if the backup is dropped. `list` reads refs as last
fetched and contacts no remote, and runs while an operation is in progress.

### What `drop` and `prune` refuse: the pin rule

No backup is dropped when that would leave a **live pin** reached by no branch,
no remote-tracking branch and no backup. A live pin is a member's commit in an
`rwv.lock` at the tip of any local or remote-tracking branch of any project's
repo in the weave — every project, not only the one the verb runs in, because a
member's clone is shared. A branch with no lock is no weave branch and pins
nothing.

The refusal is a `pin` `would be unreachable` finding, listing each commit with
the locks that pin it, and nothing is dropped. The ways out are the ones it
prints: publish the work (`rwv push`), or publish the commit under a branch name
you choose so a branch reaches it, then drop.

`drop` and `prune` also refuse while an operation is in progress (`rwv abort`
or `--continue` first); `list` does not.

## Invocation

```
rwv backup list [--json] [--project <name>]
rwv backup drop <repo> <name>... [--project <name>]
rwv backup drop <repo> --all [--project <name>]
rwv backup prune [--project <name>]
```

- `<repo>` is a member's workspace-relative path, or `projects/<name>` for the
  project repo. No form drops across repos.
- `<name>` is a backup as `list` shows it; `drop` takes at least one. `--all`
  drops every backup of the repo instead, and is refused beside a name; a
  bare `drop <repo>` is a usage error that drops nothing. A name `list` does
  not show is reported and skipped, so a drop interrupted partway is finished
  by running it again.
- `prune` fetches every remote of every repo of the project with pruning, then
  drops each backup that is `[redundant]`. A remote it cannot fetch is named,
  and that repo's remote-tracking branches are not counted: a backup is dropped
  only on what a local branch or a confirmed fetch vouches for. A backup that
  holds commits no branch reaches is never pruned.
- `--project <name>` acts on that project instead of the active one (does not
  change `.rwv-active`).

Run `rwv backup --help` for the full clap surface.

## Output

`list` prints one block per repo that holds a backup, or `no backups`. `drop`
and `prune` print `dropped <repo>: <name> (held <short id>)` for each backup
they drop; `prune` prints `nothing to prune` when none is redundant.

`rwv backup list --json` emits one envelope, with `repos` listing only repos
that hold a backup:

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "BackupJsonOutput",
  "description": "The envelope `rwv backup list --json` prints.",
  "type": "object",
  "required": [
    "$schema",
    "repos"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "repos": {
      "description": "Only repos that hold a backup.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/RepoBackupsOutput"
      }
    }
  },
  "definitions": {
    "BackupMark": {
      "description": "What a backup says about the step that made it. An ordinary backup says nothing.",
      "oneOf": [
        {
          "description": "Made before a rebase rewrote the branch.",
          "type": "string",
          "enum": [
            "rebased"
          ]
        },
        {
          "description": "The tip of a remote-tracking branch a fetch found rewritten.",
          "type": "string",
          "enum": [
            "superseded"
          ]
        },
        {
          "description": "The branch's one slot for the tip `rwv abort` last reset it from; the next abort that resets the branch overwrites it.",
          "type": "string",
          "enum": [
            "aborted"
          ]
        },
        {
          "description": "The checkout's one slot for the working copy `rwv abort` last discarded on it; the next abort that discards one there overwrites it.",
          "type": "string",
          "enum": [
            "aborted-working-copy"
          ]
        },
        {
          "description": "The checkout's one slot for the edits a merge onto its moved branch has out of the working copy, dropped once they stand there again.",
          "type": "string",
          "enum": [
            "taken-out"
          ]
        }
      ]
    },
    "BackupOutput": {
      "type": "object",
      "required": [
        "held",
        "name",
        "redundant",
        "tip"
      ],
      "properties": {
        "held": {
          "description": "The commits the backup holds that no branch reaches, newest first.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/HeldCommitOutput"
          }
        },
        "mark": {
          "description": "`rebased` for a branch a rebase rewrote; absent for any other.",
          "anyOf": [
            {
              "$ref": "#/definitions/BackupMark"
            },
            {
              "type": "null"
            }
          ]
        },
        "name": {
          "description": "The name `rwv backup drop` takes.",
          "type": "string"
        },
        "redundant": {
          "description": "A branch reaches everything the backup holds, so dropping it loses nothing.",
          "type": "boolean"
        },
        "tip": {
          "description": "The commit the backup holds.",
          "allOf": [
            {
              "$ref": "#/definitions/CommitId"
            }
          ]
        }
      }
    },
    "CommitId": {
      "description": "A full commit id: 40 lowercase hex digits, or 64 in a SHA-256 repository.",
      "type": "string",
      "pattern": "^([0-9a-f]{40}|[0-9a-f]{64})$"
    },
    "HeldCommitOutput": {
      "type": "object",
      "required": [
        "sha",
        "short",
        "subject"
      ],
      "properties": {
        "sha": {
          "$ref": "#/definitions/CommitId"
        },
        "short": {
          "type": "string"
        },
        "subject": {
          "type": "string"
        }
      }
    },
    "RepoBackupsOutput": {
      "type": "object",
      "required": [
        "backups",
        "path"
      ],
      "properties": {
        "backups": {
          "type": "array",
          "items": {
            "$ref": "#/definitions/BackupOutput"
          }
        },
        "path": {
          "description": "The repo as `rwv backup drop` names it: its workspace-relative path, or `projects/<name>` for the project repo.",
          "type": "string"
        }
      }
    }
  }
}
```

## Exit codes

- `0` — listed, dropped or pruned (including nothing to do).
- non-zero — a refusal (the pin rule, an operation in progress), a repo whose
  backups could not be read, or a drop that failed partway.

## Examples

See what the backups hold, then drop one:

```
rwv backup list
rwv backup drop github/acme/app proj--anon/3f2a9c1d5e7b
```

Drop everything a branch already reaches:

```
rwv backup prune
```

Keep a tip `abort` kept: publish it under a branch of its own before dropping
anything that reaches it.

## Common errors

- *pin would be unreachable* — a live lock in this project or another pins a
  commit only the backups being dropped reach. The refusal lists the commit and
  the locks. Publish the commit (`rwv push`, or push it under a branch name) so
  a branch reaches it, then drop.
- *no backup named `<name>`* — `list` does not show it: already dropped, or
  mistyped. Names include the branch (`<branch>/<id>`).
- *`<repo>` is no repo in this weave* — `<repo>` is a member path or
  `projects/<name>`; `rwv backup list` names the repos that hold backups.
