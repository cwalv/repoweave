# rwv fetch

## Purpose

Bootstrap a project into the workspace and align every repo to the lock
file. `fetch` is the lock-aligning, read-only counterpart to `rwv update`:
it never advances pinned revisions. Its job is to reproduce a known state,
not discover a new one.

Given a `source` (full URL or `owner/repo` shorthand), `fetch` clones the
project repo into `projects/<name>/`, reads its `rwv.lock`, and then clones
or checks out every manifest repo at the revision the lock records. When the
lock is absent (first-time bootstrap), `fetch` clones repos at branch HEAD
and writes a new lock from those HEAD SHAs. When a manifest entry is missing
from an existing lock, it is added additively at branch HEAD — already-pinned
entries are never moved.

With no `source` argument, `fetch` runs *in-place*: it resolves the workspace
from CWD, iterates the active project's manifest, and clones any repo whose
canonical clone directory is missing. Repos already present on disk are
untouched. This is the settled repair verb for a dangling reference (a
manifest entry pointing at a missing clone — reported by `rwv doctor`, or
surfaced by `rwv update` / `rwv push` when they hit the missing directory).
`doctor --fix` intentionally does not auto-clone; network side effects stay
behind this explicit verb.

`--frozen` is the CI mode: if the lock is absent or does not cover every
manifest repo, the command errors instead of writing. Reference repos that
fail to clone are skipped without failing the overall run.

## Invocation

```
rwv fetch [<source>] [--frozen] [--force] [--no-reference]
          [--role <role>...] [--repo <selector>...] [-j <N>] [--json]
```

- `<source>` — optional. Full clone URL or `owner/repo` /
  `registry/owner/repo` shorthand. When present, the project repo is
  cloned from this URL (bootstrap mode). When absent, `fetch` runs
  in-place against the active project of the current workspace (repair
  mode). In-place mode requires an active project — it errors if CWD is
  not inside a workspace or no project is active.
- `--frozen` — CI mode: error if the lock is missing or does not cover all
  manifest repos. Never writes the lock.
- `--force` — bootstrap into a non-empty directory that is not already a
  workspace (normally an error). Only meaningful in bootstrap mode;
  rejected when `<source>` is absent.
- `--no-reference` — skip repos with `role: reference` (useful when mirrors
  are offline).
- `--role <role>` / `--repo <selector>` — limit the repo fetch to a subset of
  the manifest. Repeat for union. Bare strings match exactly; `re:<pat>` is
  regex; `glob:<pat>` is glob. A filtered fetch does not write the lock.
- `-j <N>` — run up to `N` clone/checkout operations in parallel.
- `--json` — emit machine-readable output (see Output below).

Run `rwv --help fetch` for the full clap surface.

## Output

Default text output is one line per repo on the standard text channel. Lines
report whether each repo was cloned fresh, checked out at a lock-pinned
revision, or skipped. Errors (including per-repo failures) are printed to
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
  repo failed, `--frozen` detected a missing or stale lock, or the current
  directory is not a workspace and `--force` was not passed. Under `--json`,
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

CI mode — error if the lock is missing or stale:

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

## Common errors

- *project 'X' already exists at projects/X/* — run `rwv fetch` from a
  fresh directory, or use a scoped path hint printed in the error.
- *no repoweave workspace found and … is not empty* — the current directory
  is not a workspace; use `--force` to bootstrap here anyway.
- *no SOURCE and no repoweave workspace found above …* — in-place mode
  (no `<source>`) requires an existing workspace; either `cd` into one or
  pass a `<source>` to bootstrap a new project.
- *--force has no effect without SOURCE* — `--force` is only meaningful in
  bootstrap mode; drop it to re-materialize missing members in place.
- *lock file does not exist* (with `--frozen`) — the project has no
  `rwv.lock`; run without `--frozen` to bootstrap it.
- *lock file is stale* (with `--frozen`) — the lock does not cover all
  manifest repos; update the lock with `rwv update` first.
- *repository not found* — the source URL is wrong or the remote is private
  and credentials aren't configured.
- *network-error* — connectivity issue; retry.
