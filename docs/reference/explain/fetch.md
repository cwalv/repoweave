# rwv fetch

## Purpose

Bootstrap a project into the workspace and align every repo to the lock
file. `fetch` is the lock-aligning, read-only counterpart to `rwv update`:
it never advances pinned revisions. Its job is to reproduce a known state,
not discover a new one.

Given a `source` (a URL — `file://` for a local remote — or `owner/repo`
shorthand), `fetch` clones the project repo into `projects/<name>/`, reads its `rwv.lock`, and then clones
or attaches every member repo at the revision the lock records. When the
lock is absent (first-time bootstrap), `fetch` clones repos at branch HEAD
and writes a new lock from those HEAD SHAs. When a manifest entry is missing
from an existing lock, it is added additively at branch HEAD — already-pinned
entries are never moved. When no project is active, `fetch` then activates the
project it cloned, as `rwv activate` would — under `--frozen` and a
`--role` / `--repo` filter as well.

A plain path is not a source: it names no remote, so `fetch` refuses
(`unresolvable-repo-source`) and names the two spellings that are — `rwv fetch
file://<absolute path>` to clone from a local remote, and `rwv fetch --project
<name>` for a project already at `projects/<name>/`.

With no `source` argument, `fetch` runs *in-place*: it resolves the workspace
from CWD, iterates the manifest of the active project (or of `--project
<name>`), and clones any repo whose canonical clone directory is missing.
This is the settled repair verb for a dangling reference (a manifest entry
pointing at a missing clone — reported by `rwv doctor`, or surfaced by
`rwv update` / `rwv push` when they hit the missing directory).
`doctor --fix` intentionally does not auto-clone; network side effects stay
behind this explicit verb.

When no project is active, `--project` is not given, and the weave holds
exactly one project, in-place `fetch` fetches that project and then activates
it, as `rwv activate` would; with zero or several projects it refuses
(`no-active-project`) and names `--project`.

Run in a workweave, in-place `fetch` aligns the canonical clones and then
creates the workweave's worktree for every member its `rwv.lock` pins and the
workweave lacks — nothing at the slot, or a symlink there, which is replaced.
It is the repair `rwv doctor` names for a `workweave-drift` finding whose
canonical clone is not on disk; one whose canonical clone holds the pinned
revision, `rwv doctor --fix` creates without the network.

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
member repo (an *incomplete* lock), the command errors instead of writing.
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
          [-j <N>] [--json] [--project <name>] [--no-materialize]
```

- `<source>` — optional. Full clone URL (`file://` for a local remote) or
  `owner/repo` / `registry/owner/repo` shorthand. When present, the project
  repo is cloned from this URL (bootstrap mode). When absent, `fetch` runs
  in-place against the active project of the current workspace (repair
  mode). In-place mode errors if CWD is not inside a workspace, or if no
  project is active, `--project` is not given, and the weave does not hold
  exactly one project.
- `--project <name>` — in-place mode only: operate on this project instead of
  the active project. Does not change `.rwv-active`. Refused alongside
  `<source>`, which names its own project (`inapplicable-flag`).
- `--frozen` — CI mode: error if the lock is missing or does not cover all
  member repos. Never writes the lock.
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
- `-j <N>` — run up to `N` clone/checkout operations in parallel. Default:
  `min(nproc, 8)`.
- `--json` — emit machine-readable output (see Output below).
- `--no-materialize` — a fetch that activates a project runs the integration
  install hooks: a bootstrap fetch when no project is active yet, and an
  in-place fetch that selects the weave's only project. This skips them, and
  `rwv materialize` runs them later. The activation itself still happens.

Run `rwv fetch --help` for the full clap surface.

## Output

Default text output is one line per repo on the standard text channel. Lines
report whether each repo was cloned fresh, aligned to a lock-pinned revision,
or skipped. Errors (including per-repo failures) are printed to
stderr. A final summary line reports how many repos are ready.

Under `--json`, the fetch emits a single JSON envelope after all repos
finish, whatever `-j` is:

```
{
  "$schema": "<url>",
  "outcomes": [ { "path": "...", ... }, ... ],
  "placements": [ { "path": "...", "absolute_path": "...", "files": { ... } }, ... ],
  "skipped": [ { "effect": "install-hooks", "reason": "no-materialize", "remedy": "rwv materialize" }, ... ]
}
```

`outcomes` lists the repos in manifest order, however the workers finished.
Each record has the shape: `path` (manifest-relative), `absolute_path` (on-disk), `status`
(`ok` / `skipped` / `failed`), and `message` (human-readable detail; absent
for `ok`, present for `skipped` and `failed`).

File counts are never on an outcome. Each checkout whose files the fetch
placed gets a placement record: `path`, `absolute_path` (the checkout the
files are in), and `files` (`reflinked`, `hardlinked`, `written`). A clone
the fetch realigned gets one, and so, from a workweave, does each worktree
the fetch creates there for a member the workweave lacks, under the
workweave's path. The envelope lists them under `placements`, empty when
nothing was placed.

A fetch that does not activate a project, or activates one without running its
hooks, says so in one line naming the command that does it:

- a bootstrap fetch while another project is active: `rwv fetch: skipping
  auto-activate (project '<active>' already active); `rwv activate <fetched>`
  selects it and runs its install hooks` (on stdout in text mode, stderr under
  `--json`);
- an in-place fetch that activates nothing but created or moved a checkout
  (at primary a member clone; in a workweave a member worktree it created):
  `[skipped] core: the install hooks did not run (project `<project>` was
  already active, so this fetch activated nothing); `rwv materialize` runs
  them` — or, for a project the weave root does not present, `(`<project>` is
  not the project this weave root presents); `rwv activate <project>` runs
  them`. An in-place fetch that changed no checkout says nothing;
- an activation run with `--no-materialize`: `[skipped] core: the install hooks
  did not run (--no-materialize); `rwv materialize` runs them`;
- a hook whose tool is not on `PATH`: its `tool-missing` warning.

The envelope records the same facts in `skipped`. Each record carries
`effect` (`install-hooks`, `integration-files`, `weave-root-links` or
`activation`), `integration` (present only when one integration's part was
skipped), `reason` (`no-materialize`, `inside-workweave` or `narrowed`, or an
object naming what it concerns: `{"tool-missing": {"tool": …}}`,
`{"not-presented": {"project": …}}`, `{"project-already-active": {"project":
…}}`) and `remedy`, the command that performs the effect.

The `$schema` URL points to the committed schema artifact. See the Schema
section below.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "FetchJsonOutput",
  "description": "Top-level envelope for `rwv fetch --json`.\n\nShape: `{ \"$schema\": \"<url>\", \"outcomes\": [<FetchOutcomeOutput>, ...] }`.",
  "type": "object",
  "required": [
    "$schema",
    "outcomes",
    "placements"
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
    "placements": {
      "description": "How the files of each checkout the fetch moved or created were placed: a canonical clone it realigned, and from a workweave each worktree it created there. Empty, not absent, when it placed none.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/PlacementOutput"
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
      "description": "The materialization side effects this fetch skipped, each naming the command that performs it. Empty, not absent, when it skipped none.",
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
          "$ref": "#/definitions/WirePath"
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
    "WirePath": {
      "description": "Absolute path, `/`-separated on every platform.",
      "type": "string"
    }
  }
}
```

## Exit codes

- `0` — the project repo and every non-reference member repo cloned or
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
  the envelope is emitted before exit even on failure, so
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

Fetch with 4 workers and emit the JSON envelope:

```
rwv fetch owner/myproject -j 4 --json
```

Get absolute paths for every successfully fetched repo:

```
rwv fetch owner/myproject --json | jq -r '.outcomes[] | select(.status == "ok") | .absolute_path'
```

Re-materialize missing manifest members of the active project (repair verb
for `rwv doctor`'s `DanglingReference` finding — no `<source>`):

```
rwv fetch
```

The same for a named project, leaving `.rwv-active` as it is:

```
rwv fetch --frozen --project web-app
```

Realign every present clone to the lock even where that means detaching it
from the branch it is on:

```
rwv fetch --detach-checkouts
```

## Common errors

- *cannot fetch project 'X': projects/X already exists* — if that is the
  project you meant, `rwv fetch --project X` fetches its members in place;
  otherwise run `rwv fetch <source>` in an empty directory, which becomes a
  weave of its own.
- *no repoweave workspace found and … is not empty* — the current directory
  is not a workspace; use `--allow-non-empty-dir` to bootstrap here anyway.
- *no SOURCE and no repoweave workspace found above …* — in-place mode
  (no `<source>`) requires an existing workspace; either `cd` into one or
  pass a `<source>` to bootstrap a new project.
- *--allow-non-empty-dir has no effect without SOURCE* — the flag is only
  meaningful in bootstrap mode; drop it to re-materialize missing members in
  place.
- *--project has no effect with SOURCE* — a source names the project it
  clones; drop one of the two.
- *is a plain path, which names no remote* — spell a local remote as
  `file://<absolute path>`, or drop `<source>` and pass `--project <name>`
  for a project already in the weave.
- *no active project* (in-place mode) — the weave holds zero or several
  projects and none is active; pass `--project <name>` or run `rwv activate
  <name>`.
- *lock file does not exist* (with `--frozen`) — the project has no
  `rwv.lock`; run without `--frozen` to bootstrap it.
- *lock file is incomplete* (with `--frozen`) — the lock does not cover all
  member repos; update the lock with `rwv update` first.
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
