# rwv update

## Purpose

Advance the project's `rwv.lock` to the latest HEAD of each repo's branch
and re-snapshot the lock (network bump). `update` is semantically analogous
to `cargo update` / `npm update` — it is the verb that mutates the lock by
pulling fresh tips from the network.

The key distinction from `fetch`: `rwv fetch` aligns local clones to the
*existing* lock without advancing it; `rwv update` fetches from the remote,
advances each checkout onto the new branch tip, and writes a new lock. Run
`update` when you want to consume upstream commits; run `fetch` (or `sync`)
when you want to converge to an already-recorded state.

### What "advance" does to the checkout's branch

`update` **fast-forwards the branch the checkout is on**; it does not check
out the tip by revision. Which branch it is willing to move depends on where
you are:

- **In the canonical clone**, it moves only the local counterpart of the
  branch `version:` declares. On any other branch — an operator's personal
  branch — it names both refs and refuses, rather than relocating a bookmark
  it cannot relate to the tracking declaration.
- **Inside a workweave**, it moves that workweave's own branch. When the tip
  is not a fast-forward of it, `update` points at `rwv sync` — the verb that
  reconciles a workweave with its parent — rather than at a flag.

A tip that is not a fast-forward refuses in the canonical too, naming two
exits: reconcile the branch with its tracking tip yourself (ordinary
`git rebase` / `git merge`) and re-run, or pass `--detach-checkouts` to
materialize the tip on a detached HEAD without moving your branch.

A repo whose HEAD is already detached stays detached at the new tip; no
consent is needed, because there is no branch to abandon. It refuses while
the repo is stopped mid-rebase, mid-merge or mid-bisect — a detached HEAD
cannot say which of those it is, and only one of them is rwv's to move.

## Invocation

```
rwv update [--dirty] [--commit] [--detach-checkouts] [--project <name>]
           [--json] [--role <role>]... [--repo <selector>]... [-j <n>]
```

- `--detach-checkouts` — advance a repo even where that changes what HEAD is
  attached to: materialize the tip on a detached HEAD instead of refusing.
  The branch it leaves behind is not moved.
- `--json` emits machine-readable output (see Output below).
- `-j N` runs per-repo advances in parallel. Under `--json -j N` with `N > 1`,
  output switches to NDJSON (one JSON record per repo, streamed as repos finish).

Run `rwv --help update` for the full clap surface.

## Output

Default text output: a fetch-progress line per repo (prefixed with
`[<repo>]` under `-j > 1`), followed by a summary line of the form
`rwv update: advanced N repo(s)` — `N` counts the repos whose SHA actually
changed, not the repos the run visited. The subsequent lock re-snapshot emits
`Wrote <path>` on stderr. Unchanged entries are not individually reported.

An unfiltered update also re-authors the enabled integrations' managed content
against the advanced tree, and then reports any **member incompatibility** the
advance exposed — a `[warning] <integration>: member-incompatibility: …` line
on stderr, naming the value and its two remedies. Advancing members can raise
what they require above a value rwv seeded once and stepped back from (a
go.work go directive below the members' `go.mod`), and `update` is where you
are standing when that happens. It is a report and nothing more: the advance is
valid, the lock is written, and the exit code is unaffected. `rwv doctor` is
the standing arm for the same finding; both run the same predicate. Because it
goes to stderr, `--json` stdout stays purely structured and the envelope below
is unchanged.

Under `--json -j 1` (or `--json` with a single-repo project), output is
the envelope:

```
{
  "$schema": "<url>",
  "repos": [ { "path": "...", "kind": "updated", ... }, ... ]
}
```

Under `--json -j N` with `N > 1`, output switches to NDJSON: one
self-describing JSON record per repo, streamed as each repo's advance
completes. No envelope wrapper is emitted.

Each record carries:
- `path` — manifest-relative repo path.
- `absolute_path` — fully resolved on-disk path.
- `branch` — branch name from the manifest `version:` field.
- `kind` — `updated` (old_sha ≠ new_sha), `up-to-date` (already at HEAD),
  or `failed`.
- `old_sha` — SHA before the advance (omitted if unreadable).
- `new_sha` — SHA after the advance (omitted when `kind = failed`).
- `error` — human-readable failure message (only when `kind = failed`).

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "UpdateJsonOutput",
  "description": "Top-level envelope for `rwv update --json` (serial / `-j 1` mode). `{ \"$schema\": \"<url>\", \"repos\": [<RepoUpdateRecord>, ...] }`",
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
      "type": "array",
      "items": {
        "$ref": "#/definitions/RepoUpdateRecord"
      }
    },
    "resolution": {
      "description": "Resolved workspace coordinates (workspace root, optional workweave identity, project). Absent when no project is resolved.",
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
    "RepoUpdateRecord": {
      "description": "Per-repo record in `rwv update --json` output.\n\n`old_sha` is the tip before the fetch; `new_sha` is the tip after checkout (the new branch HEAD). Both are `null` when the SHA could not be read (e.g. the repo was missing from disk before the advance). For `kind = failed`, `new_sha` is always `null`; `error` carries the human-readable failure message.",
      "type": "object",
      "required": [
        "absolute_path",
        "branch",
        "kind",
        "path"
      ],
      "properties": {
        "absolute_path": {
          "description": "Fully resolved absolute path.",
          "type": "string"
        },
        "branch": {
          "description": "Branch name from the manifest `version:` field.",
          "type": "string"
        },
        "error": {
          "description": "Human-readable error message, only present when `kind = failed`.",
          "type": [
            "string",
            "null"
          ]
        },
        "kind": {
          "description": "Outcome discriminant.",
          "allOf": [
            {
              "$ref": "#/definitions/UpdateKind"
            }
          ]
        },
        "new_sha": {
          "description": "Tip SHA after the advance (`null` when `kind = failed`).",
          "type": [
            "string",
            "null"
          ]
        },
        "old_sha": {
          "description": "Tip SHA before the advance (`null` if unreadable).",
          "type": [
            "string",
            "null"
          ]
        },
        "path": {
          "description": "Manifest-relative path.",
          "type": "string"
        }
      }
    },
    "Resolution": {
      "description": "Resolved workspace coordinates for `--json` output and (future) plugin env-var envelope.\n\nCarries exactly the three result fields — `workspace` (primary root abs path), `workweave` (the `<project>--<name>` identity the registry records, absent at primary and for an unregistered workweave), and `project` (resolved project name). No separate `kind` or `location` field.\n\nResults only — provenance (which chain step resolved the project, which flag addressed the workspace) is deliberately excluded: anything in default `--json` output becomes depended on, and the assertion use case needs the result, not the mechanism. Provenance appears only in the human-facing \"target:\" line printed to stderr.\n\nIsomorphic to the plugin env-var envelope (`RWV_WORKSPACE`/`RWV_WORKWEAVE`/`RWV_PROJECT`): both surfaces are pure projections of `WorkspaceContext::resolution`, never independently computed.",
      "type": "object",
      "required": [
        "project",
        "workspace"
      ],
      "properties": {
        "project": {
          "description": "Resolved project name.",
          "type": "string"
        },
        "workspace": {
          "description": "Primary workspace root (absolute path).",
          "type": "string"
        },
        "workweave": {
          "description": "Workweave identity (`<project>--<name>`), as the primary-side registry records it.\n\nAbsent at the primary, and absent for a workweave whose directory no registry entry names — identity is by record, so an unregistered workweave has no identity to report and rwv will not spell one from the directory name. `rwv doctor --fix` registers such a directory.",
          "type": [
            "string",
            "null"
          ]
        }
      }
    },
    "UpdateKind": {
      "description": "Per-repo outcome kind for `rwv update --json`.",
      "oneOf": [
        {
          "description": "Repo was advanced to a new SHA (old_sha != new_sha).",
          "type": "string",
          "enum": [
            "updated"
          ]
        },
        {
          "description": "Repo was already at the branch HEAD (old_sha == new_sha).",
          "type": "string",
          "enum": [
            "up-to-date"
          ]
        },
        {
          "description": "Advance failed; see `error` for the message.",
          "type": "string",
          "enum": [
            "failed"
          ]
        }
      ]
    }
  }
}
```

## Exit codes

- `0` — lock updated successfully (or no changes were needed).
- non-zero — workspace could not be resolved, manifest parse failure, or
  one or more repos couldn't be inspected.

## Examples

Update the lock for the active project:

```
rwv update
```

Update a specific project without changing `.rwv-active`:

```
rwv update --project web-app
```

## Common errors

- *workspace could not be resolved* — `cwd` is not inside a weave or
  workweave.
- *clone missing on disk* — a manifest entry references a repo that isn't
  cloned on disk; run `rwv fetch` (no SOURCE) from the workspace to
  re-materialize missing manifest members, then re-run `rwv update`.
- *could not resolve branch on role-conventional remote* — the branch named
  in the manifest's `version:` field doesn't exist on the upstream remote;
  verify the branch name and remote configuration.
- *git fetch failed* — network error or remote authentication problem;
  inspect the git output for details.
- *lock not written* — reported when one or more repos fail to advance; the
  lock is left unchanged so a partial update never lands.
