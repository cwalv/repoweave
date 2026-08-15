# rwv push

## Purpose

Publish a cross-repo feature to shared remotes. Pushes manifest repos first,
then the project repo last — the project repo carries the committed lock that
pins manifest SHAs, so collaborators' `rwv fetch` must never see a committed
lock referencing unpushed manifest commits.

Refuses if invoked from a workweave (workweave branches shouldn't leak to
shared remotes). Refuses if any manifest repo's HEAD disagrees with its lock
entry (lock-precondition check). Use `rwv lock` to snapshot local state before
pushing, or `git checkout` to align with the lock.

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

Agents call `rwv push --json` to get machine-readable per-repo outcomes so
they can react to partial failures without parsing human-readable text.

## Invocation

```
rwv push [--json] [-j <N>] [--dry-run] [--force] [--role <role>] [--repo <selector>] [--project <name>]
```

- `--json` emits machine-readable output (see Output below).
- `-j <N>` runs up to `N` manifest-repo pushes in parallel. Default is `1`
  (serial), so the `--json` envelope shape is the unsurprising default and
  the envelope/NDJSON switch only happens when the user opts in with `-j > 1`.
  The project-repo push always runs serially as the last step regardless of
  `-j`.
- `--dry-run` prints the push plan without executing any pushes.
- `--force` force-pushes every repo in the operation.
- `--role` / `--repo` narrow the manifest-repo push loop (union semantics)
  and override the default `[owned, fork]` scope. When either flag is
  present, the caller's selectors are used verbatim — pass `--role owned
  --role fork` to reproduce the bare default explicitly. The
  lock-precondition check always runs against the full manifest regardless
  of these filters.
- `--project <name>` operates on the named project without changing
  `.rwv-active`.

Run `rwv --help push` for the full clap surface.

## Output

Default text output is one line per repo and a summary.

Under `--json` with `-j 1` (default), output is the pretty-printed envelope:

```
{
  "$schema": "<url>",
  "outcomes": [ { "kind": "...", "path": "...", "absolute_path": "...", ... }, ... ]
}
```

Outcome `kind` tags for manifest repos: `pushed`, `skipped` (e.g. filtered
by selector), `failed`. The project-repo record is always the last entry in `outcomes` and
uses kind `project-repo-pushed` or `project-repo-failed` — its kind tag is the
only way to distinguish it from manifest-repo records in the flat array. Failed
records carry a `message` field with the free-form error from the git push.

Under `--json -j N` with `N > 1`, the envelope is dropped and output switches
to NDJSON — one JSON record per line, streamed to stdout as each repo's push
completes. Every line is self-describing: each record embeds its own `"$schema"`
URL alongside the per-repo fields. The project-repo record is appended last
(after all manifest outcomes). Lines are mutex-guarded so concurrent workers
cannot tear a single record's bytes.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "PushJsonOutput",
  "description": "Top-level envelope for `rwv push --json` (serial mode, `jobs == 1`).\n\nShape: `{ \"$schema\": \"<url>\", \"outcomes\": [<PushOutcomeOutput>, ...] }`. Manifest-repo records appear first, in manifest order; the project-repo record is appended last (reflecting push ordering). Consumers can distinguish the project-repo record by checking `kind` for `\"project-repo-pushed\"` or `\"project-repo-failed\"`.",
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
        "$ref": "#/definitions/PushOutcomeOutput"
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
    "PushOutcomeOutput": {
      "description": "One per-repo outcome record in `rwv push --json` output.\n\nManifest-repo records use `kind` values `pushed`, `skipped`, and `failed`. The project-repo record uses `kind` values `project-repo-pushed` and `project-repo-failed`, making it distinguishable from manifest-repo records in the same flat `outcomes` array.\n\nChoosing option (a) — a `kind` field — over option (b) (two separate arrays) because: a single flat array supports uniform streaming in NDJSON mode without requiring consumers to merge two streams; the `kind` tag already carries all the type information consumers need; and the kebab-case kind-tag convention matches sync/status/doctor precedent.",
      "oneOf": [
        {
          "description": "Manifest repo was pushed successfully.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
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
          "description": "Manifest repo was skipped (e.g. by a selector filter).",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
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
          "description": "Manifest repo push failed.",
          "type": "object",
          "required": [
            "absolute_path",
            "kind",
            "message",
            "path"
          ],
          "properties": {
            "absolute_path": {
              "type": "string"
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
              "type": "string"
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
              "description": "The project name (e.g. `\"my-app\"`). Distinguishes the project repo's path convention (`projects/<name>/`) from manifest-repo paths.",
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
              "type": "string"
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
    }
  }
}
```

## Exit codes

- `0` — all in-scope manifest repos pushed (non-writable roles skipped by
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

Parallel push, stream outcomes as NDJSON:

```
rwv push --json -j 4 | jq -c 'select(.kind == "failed" or .kind == "project-repo-failed")'
```

## Common errors

- *lock-state mismatch* — one or more manifest repos' HEAD differs from the
  recorded lock SHA. Run `rwv lock` to snapshot current state, or check out
  the locked SHA in each repo.
- *refusing to push from workweave* — run `rwv sync-to primary` (or
  `rwv sync-to`) to land changes on primary, then push from there.
- *project repo not on canonical branch* — check out the canonical branch
  before pushing.
- *manifest-repo push failures* — inspect the `failed` outcomes in `--json`
  output. The project repo is NOT pushed when any manifest push fails; retry
  after resolving the failures.
