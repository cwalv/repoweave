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
rwv update [--dirty] [--commit [-m <MSG>]] [--detach-checkouts] [--project <name>]
           [--json] [--role <role>]... [--repo <selector>]... [-j <n>]
           [--no-materialize]
```

- `--commit` — commit the new `rwv.lock` in the project repo, together with the
  integration files regenerated against the new tips and with `rwv.toml` when it
  has uncommitted changes, under an auto-generated message listing which repos
  advanced (and naming `rwv.toml` when it is in the commit). Refuses if the
  project repo has uncommitted changes outside that set.
- `-m, --message <MSG>` — the commit message for `--commit`, in place of the
  generated one. Without `--commit` it is a usage error (exit 2).

- `--detach-checkouts` — advance a repo even where that changes what HEAD is
  attached to: materialize the tip on a detached HEAD instead of refusing.
  The branch it leaves behind is not moved.
- `--json` emits machine-readable output (see Output below).
- `-j N` runs per-repo advances in parallel. Default: `min(nproc, 8)`.
- `--no-materialize` — skip the integration install hooks (`npm install`,
  `uv sync`, `cargo fetch`, …) of the regeneration this runs at primary;
  `rwv materialize` runs them later. Inside a workweave the hooks are skipped
  either way.

Run `rwv update --help` for the full clap surface.

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

A side effect the update skipped is said on stderr, one line each, naming the
command that performs it, and nothing is said when it skipped none:

- narrowed by `--role` or `--repo`, it regenerates no integration files
  (`[skipped] core: the integration files were not regenerated (the update was
  narrowed by --role or --repo); `rwv doctor --fix` regenerates them`) and runs
  no install hooks (the same line for hooks, naming `rwv materialize`);
- with `--no-materialize`, or inside a workweave, it runs no install hooks
  (`[skipped] core: the install hooks did not run (--no-materialize); `rwv
  materialize` runs them`, or `(an authoring verb runs none inside a
  workweave)`);
- a hook whose tool is not on `PATH` is skipped with a `tool-missing` warning.

The envelope records the same facts in `skipped`.

Under `--json`, output is the envelope, whatever `-j` is:

```
{
  "$schema": "<url>",
  "repos": [ { "path": "...", "kind": "updated", ... }, ... ],
  "placements": [ { "path": "...", "absolute_path": "...", "files": { ... } }, ... ],
  "skipped": [ { "effect": "install-hooks", "reason": "no-materialize", "remedy": "rwv materialize" }, ... ]
}
```

`repos` lists the repos in manifest order, however the workers finished.
Each record carries:
- `path` — manifest-relative repo path.
- `absolute_path` — fully resolved on-disk path.
- `branch` — branch name from the manifest `version:` field.
- `kind` — `updated` (old_sha ≠ new_sha), `up-to-date` (already at HEAD),
  or `failed`.
- `old_sha` — SHA before the advance (omitted if unreadable).
- `new_sha` — SHA after the advance (omitted when `kind = failed`).
- `error` — human-readable failure message (only when `kind = failed`).

File counts are never on a record. Each checkout whose files the advance
rewrote gets a placement record: `path`, `absolute_path`, and `files`
(`reflinked`, `hardlinked`, `written`). The envelope lists them under
`placements`, empty when nothing was placed.

Each record in `skipped` carries `effect` (`install-hooks`,
`integration-files`, `weave-root-links` or `activation`), `integration`
(present only when one integration's part was skipped), `reason`
(`no-materialize`, `inside-workweave` or `narrowed`, or an object naming what
it concerns: `{"tool-missing": {"tool": …}}`, `{"not-presented": {"project":
…}}`, `{"project-already-active": {"project": …}}`) and `remedy`, the command
that performs the effect.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "UpdateJsonOutput",
  "description": "Top-level envelope for `rwv update --json`. `{ \"$schema\": \"<url>\", \"repos\": [<RepoUpdateRecord>, ...] }`",
  "type": "object",
  "required": [
    "$schema",
    "placements",
    "repos"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "placements": {
      "description": "How the files of each checkout the advance moved were placed. Empty, not absent, when it moved none.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/PlacementOutput"
      }
    },
    "repos": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/RepoUpdateRecord"
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
    },
    "skipped": {
      "description": "The materialization side effects this update skipped, each naming the command that performs it. Empty, not absent, when it skipped none.",
      "default": [],
      "type": "array",
      "items": {
        "$ref": "#/definitions/SkippedEffect"
      }
    }
  },
  "definitions": {
    "Effect": {
      "description": "The side effect that did not happen.",
      "oneOf": [
        {
          "description": "The integrations' install hooks (`npm install`, `uv sync`, `cargo fetch`, …).",
          "type": "string",
          "enum": [
            "install-hooks"
          ]
        },
        {
          "description": "Regenerating the integrations' managed and generated files.",
          "type": "string",
          "enum": [
            "integration-files"
          ]
        },
        {
          "description": "Refreshing the weave root's links onto the project's files.",
          "type": "string",
          "enum": [
            "weave-root-links"
          ]
        },
        {
          "description": "Activating a fetched project: selecting it, surfacing its links and running its install hooks.",
          "type": "string",
          "enum": [
            "activation"
          ]
        }
      ]
    },
    "FilePlacement": {
      "description": "How a member's tracked files reached a workspace.",
      "type": "object",
      "required": [
        "hardlinked",
        "reflinked",
        "written"
      ],
      "properties": {
        "hardlinked": {
          "description": "Linked to another workspace's read-only copy.",
          "type": "integer",
          "format": "uint",
          "minimum": 0.0
        },
        "reflinked": {
          "description": "Cloned from another workspace's copy, sharing its storage until either is written.",
          "type": "integer",
          "format": "uint",
          "minimum": 0.0
        },
        "written": {
          "description": "Written by the version control system.",
          "type": "integer",
          "format": "uint",
          "minimum": 0.0
        }
      }
    },
    "PlacementOutput": {
      "description": "How the files of one checkout were placed when a verb created or moved it.",
      "type": "object",
      "required": [
        "absolute_path",
        "files",
        "path"
      ],
      "properties": {
        "absolute_path": {
          "description": "The checkout whose files were placed.",
          "allOf": [
            {
              "$ref": "#/definitions/WirePath"
            }
          ]
        },
        "files": {
          "description": "How many files were cloned, linked or written.",
          "allOf": [
            {
              "$ref": "#/definitions/FilePlacement"
            }
          ]
        },
        "path": {
          "description": "Manifest-relative path of the repository.",
          "type": "string"
        }
      }
    },
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
          "allOf": [
            {
              "$ref": "#/definitions/WirePath"
            }
          ]
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
    "SkipReason": {
      "description": "Why the effect did not happen.",
      "oneOf": [
        {
          "description": "`--no-materialize` was passed.",
          "type": "string",
          "enum": [
            "no-materialize"
          ]
        },
        {
          "description": "An authoring verb ran inside a workweave, where it runs no hooks.",
          "type": "string",
          "enum": [
            "inside-workweave"
          ]
        },
        {
          "description": "The CLI the integration's hook drives is not on `PATH`.",
          "type": "object",
          "required": [
            "tool-missing"
          ],
          "properties": {
            "tool-missing": {
              "type": "object",
              "required": [
                "tool"
              ],
              "properties": {
                "tool": {
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "The verb acted on a project the weave root does not present.",
          "type": "object",
          "required": [
            "not-presented"
          ],
          "properties": {
            "not-presented": {
              "type": "object",
              "required": [
                "project"
              ],
              "properties": {
                "project": {
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "description": "`rwv update` was narrowed by `--role` or `--repo`.",
          "type": "string",
          "enum": [
            "narrowed"
          ]
        },
        {
          "description": "A project was already active, so the fetch selected none.",
          "type": "object",
          "required": [
            "project-already-active"
          ],
          "properties": {
            "project-already-active": {
              "type": "object",
              "required": [
                "project"
              ],
              "properties": {
                "project": {
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        }
      ]
    },
    "SkippedEffect": {
      "description": "One materialization side effect a verb skipped on this run.",
      "type": "object",
      "required": [
        "effect",
        "reason",
        "remedy"
      ],
      "properties": {
        "effect": {
          "$ref": "#/definitions/Effect"
        },
        "integration": {
          "description": "The integration whose part of `effect` was skipped; absent when the whole effect was.",
          "type": [
            "string",
            "null"
          ]
        },
        "reason": {
          "$ref": "#/definitions/SkipReason"
        },
        "remedy": {
          "description": "The command that performs the effect.",
          "type": "string"
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
    },
    "WirePath": {
      "description": "Absolute path, `/`-separated on every platform.",
      "type": "string"
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
- *could not resolve branch on the remote* — the branch named
  in the manifest's `version:` field doesn't exist on the upstream remote;
  verify the branch name and remote configuration.
- *git fetch failed* — network error or remote authentication problem;
  inspect the git output for details.
- *lock not written* — reported when one or more repos fail to advance; the
  lock is left unchanged so a partial update never lands.
