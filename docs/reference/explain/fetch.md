# rwv fetch

## Purpose

Bootstrap a project into the workspace and align every repo to the lock
file. `fetch` is the lock-aligning, read-only counterpart to `rwv update`:
it never advances pinned revisions. Its job is to reproduce a known state,
not discover a new one.

Given a `source` (full URL or `owner/repo` shorthand), `fetch` clones the
project repo into `projects/<name>/`, reads its `rwv.lock`, and then clones
or attaches every manifest repo at the revision the lock records. When the
lock is absent (first-time bootstrap), `fetch` clones repos at branch HEAD
and writes a new lock from those HEAD SHAs. When a manifest entry is missing
from an existing lock, it is added additively at branch HEAD — already-pinned
entries are never moved.

With no `source` argument, `fetch` runs *in-place*: it resolves the workspace
from CWD, iterates the active project's manifest, and clones any repo whose
canonical clone directory is missing. This is the settled repair verb for a
dangling reference (a manifest entry pointing at a missing clone — reported
by `rwv doctor`, or surfaced by `rwv update` / `rwv push` when they hit the
missing directory).
`doctor --fix` intentionally does not auto-clone; network side effects stay
behind this explicit verb.

### What happens to a clone that is already present

A present clone is not skipped. What `fetch` does to it depends on whether
the lock covers it:

- **The lock has an entry for the repo.** `fetch` realigns the clone: it
  resolves the locked revision *in that clone's own object store* and moves
  the checkout onto it, **without changing what HEAD is attached to**. When
  the clone is on the local counterpart of the branch `version:` declares,
  that branch is fast-forwarded to the pin and the clone stays on it. When
  the pin is not a fast-forward of that branch, or the clone is on some
  other branch, `fetch` refuses — see below. A clone already at the pin is
  untouched; a clone whose HEAD is already detached stays detached, now at
  the pin.
- **The lock exists but has no entry for the repo** (an *incomplete* lock).
  The clone is left exactly as it is; the repo is recorded in the lock at
  whatever the clone's HEAD is now. Coverage is additive — pinned entries are
  never moved.
- **There is no lock at all** (first-time bootstrap). The clone is left
  exactly as it is, and the lock is written from the on-disk HEADs.

Realignment is a *local* operation: nothing is fetched over the network for a
clone that is already present. If the locked revision is not in that clone's
object store, the repo fails with `revision ... not found` rather than being
re-fetched — run `git fetch` in that repo and re-run, or re-lock to a
reachable revision (see the reconcile-repos how-to).

A newly cloned repo is *born* on that same branch, positioned at the lock
revision rather than at the remote tip — so bootstrapping a weave from a
lock that is behind `origin` leaves every member attached, not detached.

### Realignment refuses to change what HEAD is attached to

Realigning a present clone is gated. `fetch` moves a branch only when it can
relate that branch to the lock, and only in the direction the lock justifies:

- **Not a fast-forward.** The pin is not a descendant of the branch tip —
  materializing an older lock does this, and so does a branch carrying
  commits `origin` has never seen. Taking the pin would either rewind the
  branch or abandon it, so `fetch` refuses.
- **On some other branch.** The checkout is on a branch that is not the local
  counterpart of `version:` — an operator's personal branch. `fetch` names
  both refs and refuses; it does not relocate a branch it cannot relate to
  the lock, even when doing so would be a fast-forward. Your bookmark is
  yours.

The refusal names the repo, and that repo is reported as a failure while the
rest of the run continues. It does not fire when the pin is a fast-forward,
when HEAD is already detached, or when the clone is already at the locked
revision — so the ordinary case, including a CI runner with a warm clone
cache, realigns without a flag.

`--detach-checkouts` waives both refusals by materializing the pin on a
**detached HEAD**. Nothing is discarded and no branch is moved: uncommitted
changes come along, and the branch ref keeps every commit it had.

A workspace whose members were detached this way sits on detached HEADs, and
`rwv sync-to` refuses to land onto a detached target. Put a member back on
its branch with `git checkout <branch>` in that repo; use `rwv update` when
the intent is to move the *lock* forward to branch HEAD instead.

`--frozen` is the CI mode: if the lock is absent or does not cover every
manifest repo (an *incomplete* lock), the command errors instead of writing.
It changes lock validation only — present clones are realigned exactly as
they are without it, and the realignment gate still applies. Reference repos
that fail to clone are skipped without failing the overall run.

A `--role` / `--repo` filter narrows which repos are fetched; the selected
ones are cloned and realigned as usual, but the whole lock-write step is
skipped, so neither the bootstrap write nor the additive coverage write
happens under a filter.

## Invocation

```
rwv fetch [<source>] [--frozen] [--allow-non-empty-dir] [--no-reference]
          [--detach-checkouts] [--role <role>...] [--repo <selector>...]
          [-j <N>] [--json]
```

- `<source>` — optional. Full clone URL or `owner/repo` /
  `registry/owner/repo` shorthand. When present, the project repo is
  cloned from this URL (bootstrap mode). When absent, `fetch` runs
  in-place against the active project of the current workspace (repair
  mode). In-place mode requires an active project — it errors if CWD is
  not inside a workspace or no project is active.
- `--frozen` — CI mode: error if the lock is missing or does not cover all
  manifest repos. Never writes the lock.
- `--allow-non-empty-dir` — bootstrap into a non-empty directory that is not
  already a workspace (normally an error). Only meaningful in bootstrap mode;
  rejected when `<source>` is absent.
- `--no-reference` — skip repos with `role: reference` (useful when mirrors
  are offline).
- `--detach-checkouts` — realign a present clone even where that changes
  what HEAD is attached to: materialize the pin on a detached HEAD instead
  of refusing. Without it, those repos refuse.
- `--role <role>` / `--repo <selector>` — limit the repo fetch to a subset of
  the manifest. Repeat for union. Bare strings match exactly; `re:<pat>` is
  regex; `glob:<pat>` is glob. A filtered fetch does not write the lock.
- `-j <N>` — run up to `N` clone/checkout operations in parallel.
- `--json` — emit machine-readable output (see Output below).

Run `rwv --help fetch` for the full clap surface.

## Output

Default text output is one line per repo on the standard text channel. Lines
report whether each repo was cloned fresh, aligned to a lock-pinned revision,
or skipped. Errors (including per-repo failures) are printed to
stderr. A final summary line reports how many repos are ready.

Under `--json`, output switches to machine-readable format:

- **`-j 1` or no `-j`** (serial / envelope mode): emits a single JSON envelope
  after all repos finish:

  ```
  {
    "$schema": "<url>",
    "outcomes": [ { "path": "...", ... }, ... ]
  }
  ```

- **`-j N` with `N > 1`** (NDJSON streaming mode): emits one self-describing
  JSON line per repo as workers finish, with no envelope wrapper. Each line
  carries its own `$schema` URL.

Each per-repo record in the `outcomes` array (or in each NDJSON line) has the
shape: `path` (manifest-relative), `absolute_path` (on-disk), `status`
(`ok` / `skipped` / `failed`), and `message` (human-readable detail; absent
for `ok`, present for `skipped` and `failed`).

The `$schema` URL points to the committed schema artifact. See the Schema
section below.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "FetchJsonOutput",
  "description": "Top-level envelope for `rwv fetch --json` (serial / `-j 1` mode).\n\nShape: `{ \"$schema\": \"<url>\", \"outcomes\": [<FetchOutcomeOutput>, ...] }`.",
  "type": "object",
  "required": [
    "$schema",
    "outcomes"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "outcomes": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/FetchOutcomeOutput"
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
    "FetchOutcomeOutput": {
      "description": "Per-repo outcome record for `rwv fetch --json`.\n\n`status` is one of `\"ok\"`, `\"skipped\"`, or `\"failed\"`. `message` carries a human-readable description of the outcome (always present for `\"failed\"`; present for `\"skipped\"` to say why; `null` for `\"ok\"`).",
      "type": "object",
      "required": [
        "absolute_path",
        "path",
        "status"
      ],
      "properties": {
        "absolute_path": {
          "type": "string"
        },
        "message": {
          "type": [
            "string",
            "null"
          ]
        },
        "path": {
          "type": "string"
        },
        "status": {
          "$ref": "#/definitions/FetchOutcomeStatus"
        }
      }
    },
    "FetchOutcomeStatus": {
      "description": "Status discriminant for `FetchOutcomeOutput`.",
      "type": "string",
      "enum": [
        "ok",
        "skipped",
        "failed"
      ]
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
    }
  }
}
```

## Exit codes

- `0` — the project repo and every non-reference manifest repo cloned or
  checked out successfully (reference repos that failed are warnings, not
  errors). Under `--json`, exit `0` means every outcome has `"status": "ok"`
  or `"skipped"`.
- non-zero — the project repo failed to clone, at least one non-reference
  repo failed (including a repo whose realignment was refused because it
  would change what HEAD is attached to), `--frozen` detected a missing or
  incomplete
  lock, or the current directory is not a workspace and
  `--allow-non-empty-dir` was not passed.
  Under `--json`,
  the envelope (or NDJSON stream) is emitted before exit even on failure, so
  consumers always get parseable output.

## Examples

Bootstrap a project from a GitHub shorthand:

```
rwv fetch owner/myproject
```

Same, with a full URL:

```
rwv fetch https://github.com/owner/myproject.git
```

CI mode — error if the lock is missing or incomplete:

```
rwv fetch owner/myproject --frozen
```

Skip reference repos (faster when mirrors are offline):

```
rwv fetch owner/myproject --no-reference
```

Parallel clone/checkout across 8 workers:

```
rwv fetch owner/myproject -j 8
```

Fetch and emit JSON envelope (serial; guarantees envelope output):

```
rwv fetch owner/myproject -j 1 --json
```

Fetch with 4 workers and stream NDJSON as repos finish:

```
rwv fetch owner/myproject -j 4 --json
```

Get absolute paths for every successfully fetched repo:

```
rwv fetch owner/myproject -j 1 --json | jq -r '.outcomes[] | select(.status == "ok") | .absolute_path'
```

Re-materialize missing manifest members of the active project (repair verb
for `rwv doctor`'s `DanglingReference` finding — no `<source>`):

```
rwv fetch
```

Realign every present clone to the lock even where that means detaching it
from the branch it is on:

```
rwv fetch --detach-checkouts
```

## Common errors

- *project 'X' already exists at projects/X/* — run `rwv fetch` from a
  fresh directory, or use a scoped path hint printed in the error.
- *no repoweave workspace found and … is not empty* — the current directory
  is not a workspace; use `--allow-non-empty-dir` to bootstrap here anyway.
- *no SOURCE and no repoweave workspace found above …* — in-place mode
  (no `<source>`) requires an existing workspace; either `cd` into one or
  pass a `<source>` to bootstrap a new project.
- *--allow-non-empty-dir has no effect without SOURCE* — the flag is only
  meaningful in bootstrap mode; drop it to re-materialize missing members in
  place.
- *lock file does not exist* (with `--frozen`) — the project has no
  `rwv.lock`; run without `--frozen` to bootstrap it.
- *lock file is incomplete* (with `--frozen`) — the lock does not cover all
  manifest repos; update the lock with `rwv update` first.
- *aligning … is not a fast-forward* — the pin is not a descendant of the
  branch tip (an older lock, or a branch carrying commits `origin` does not
  have). Reconcile the branch with the pin yourself and re-run, or pass
  `--detach-checkouts` to materialize the pin on a detached HEAD.
- *is on branch 'X', which is not the local counterpart of …* — the checkout
  is on a branch the manifest does not declare. Switch to the declared
  branch and re-run, or pass `--detach-checkouts`.
- *repo is mid-…* — an already-detached repo is stopped mid-rebase, mid-merge
  or mid-bisect. Finish or abort that operation and re-run; rwv will not move
  a HEAD that is carrying an in-flight operation's state.
- *revision … not found in …* — the clone is present but its object store
  does not have the locked revision, and a present clone is realigned without
  a network fetch. `git fetch` in that repo, or re-lock to a reachable
  revision.
- *repository not found* — the source URL is wrong or the remote is private
  and credentials aren't configured.
- *network-error* — connectivity issue; retry.
