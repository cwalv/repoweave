# rwv update

## Purpose

Two forms of one verb:

- **`rwv update`** brings the weave up to the state the project records. It
  clones the members missing on disk and aligns each member present to the
  commit `rwv.lock` pins for it. It never advances a pin and never writes the
  lock.
- **`rwv update --remote`** advances each repo to the latest HEAD of its branch
  on the remote and re-snapshots `rwv.lock` (network bump). It is
  semantically analogous to `cargo update` / `npm update` — the form that
  mutates the lock by pulling fresh tips from the network.

Run `update` to reproduce a recorded state, or to repair a weave after members
drifted; run `update --remote` when you want to consume upstream commits;
`rwv fetch` downloads without moving anything, and `rwv pull` takes in the
project branch and then runs `update`.

## `rwv update`

It resolves the workspace from CWD, iterates the manifest of the active project
(or of `--project <name>`), and clones any repo whose canonical clone directory
is missing. This is the settled repair verb for a dangling reference (a
manifest entry pointing at a missing clone — reported by `rwv doctor`, or
surfaced by `rwv update --remote` / `rwv push` when they hit the missing
directory). `doctor --fix` intentionally does not auto-clone; network side
effects stay behind this explicit verb.

When no project is active, `--project` is not given, and the weave holds
exactly one project, `update` updates that project and then activates it, as
`rwv activate` would; with zero or several projects it refuses
(`no-active-project`) and names `--project`.

Run in a workweave, `update` treats the canonical clones in the primary as the
store the workweave's worktrees share, not as checkouts to align. It moves no
branch and no checkout of theirs: where a canonical clone lacks a revision the
workweave's `rwv.lock` pins, it fetches from that clone's remote, and where one
is not on disk, it clones it at the revision the primary's own `rwv.lock` pins
(at the workweave's pin when the primary pins none). It then gives the workweave
its worktree for every member its `rwv.lock` pins and the workweave lacks —
nothing at the slot, a symlink there, which is replaced, or a checkout whose
worktree link dangles, which keeps its files; a symlinked member the lock does
not pin starts from the canonical clone's HEAD. The worktree goes on the
workweave's own branch at its own tip where that branch survives, and on that
branch created at the pinned revision where it does not; no branch is moved or
deleted. It does not write the workweave's `rwv.lock`, which records the
workweave's own worktrees; `rwv lock` does. It is the repair `rwv doctor` names
for a `workweave-drift` finding whose canonical clone does not hold the pinned
revision or is not on disk; one whose canonical clone holds what the worktree
needs, `rwv doctor --fix` gives back without the network.

### What happens to a clone that is already present

A present clone is not skipped. What `update` does to it depends on whether the
lock covers it:

- **The lock has an entry for the repo.** `update` realigns the clone: it
  resolves the locked revision *in that clone's own object store* and moves the
  checkout onto it, **without changing what HEAD is attached to**. When the
  clone is on the local counterpart of the branch the manifest's `branch`
  declares, that branch is fast-forwarded to the pin and the clone stays on it.
  When the pin is not a fast-forward of that branch, or the clone is on some
  other branch, `update` refuses — see below. A clone already at the pin is
  untouched; a clone whose HEAD is already detached stays detached, now at the
  pin.
- **The lock has no entry for the repo**, or there is no lock. The clone is left
  exactly as it is. The lock is not written; `rwv lock` records it.

A repo not on disk that the lock has no entry for, or pins at a commit no ref in
its new clone reaches, is cloned on the branch its entry declares at that
branch's tip.

At the primary, realignment is a *local* operation: nothing of the member's own
history is fetched over the network for a clone that is already present (its
submodules are another matter; see below). If the locked revision is not in that
clone's object store, the repo fails with `revision ... not found` rather than
being re-fetched — run `rwv fetch` and rerun, or re-lock to a reachable
revision (see the reconcile-repos how-to).

A newly cloned repo is *born* on that same branch, positioned at the lock
revision rather than at the remote tip — so bringing up a weave from a lock that
is behind its remote leaves every member attached, not detached.

### Submodules

For every member it clones or finds present, `update` gives the canonical clone
what the member's checkouts record for their submodules, from upstream: a
submodule whose directory in the canonical clone is empty is checked out there,
nested ones included, and a commit a checkout records that the canonical clone's
copy lacks is fetched into that copy without moving its checkout. A member whose
manifest entry says `recurse-submodules = false` is left alone, and so is a
submodule git's `submodule.<name>.update = none` declines. A submodule that
cannot be fetched is a warning, not a failure.

Run in a workweave, `update` also checks out the empty submodules of the
workweave's members from the canonical clone's copies, after fetching into them
the commits the workweave's checkouts record. This is the repair `rwv doctor`
names for `uninitialized-submodule` / `unfetched`, run in that workweave.

### Realignment refuses to change what HEAD is attached to

Realigning a present clone is gated. `update` moves a branch only when it can
relate that branch to the lock, and only in the direction the lock justifies:

- **Not a fast-forward.** The pin is not a descendant of the branch tip —
  materializing an older lock does this, and so does a branch carrying commits
  its remote has never seen. Taking the pin would either rewind the branch or
  abandon it, so `update` refuses.
- **On some other branch.** The checkout is on a branch that is not the local
  counterpart of `branch` — an operator's personal branch. `update` names both
  refs and refuses; it does not relocate a branch it cannot relate to the lock,
  even when doing so would be a fast-forward. Your bookmark is yours.

The refusal names the repo, and that repo is reported as a failure while the
rest of the run continues. It does not fire when the pin is a fast-forward, when
HEAD is already detached, or when the clone is already at the locked revision —
so the ordinary case, including a CI runner with a warm clone cache, realigns
without a flag.

`--detach-checkouts` waives both refusals by materializing the pin on a
**detached HEAD**. Nothing is discarded and no branch is moved: uncommitted
changes come along, and the branch ref keeps every commit it had.

A workspace whose members were detached this way sits on detached HEADs, and
`rwv sync-to` refuses to land onto a detached target. Put a member back on its
branch with `git checkout <branch>` in that repo; use `rwv update --remote` when
the intent is to move the *lock* forward to branch HEAD instead.

## `rwv update --remote`

For each repo in the active project's manifest it runs the pruning fetch of the
repo's object store (the one `rwv fetch` runs, under the store's fetch claim),
resolves the manifest's `branch` on the remote, advances the checkout onto it,
and then re-snapshots `rwv.lock` from the new tips.

### What "advance" does to the checkout's branch

`update --remote` **fast-forwards the branch the checkout is on**; it does not
check out the tip by revision. Which branch it is willing to move depends on
where you are:

- **In the canonical clone**, it moves only the local counterpart of the branch
  the manifest's `branch` declares. On any other branch — an operator's personal
  branch — it names both refs and refuses, rather than relocating a bookmark it
  cannot relate to the tracking declaration.
- **Inside a workweave**, it moves that workweave's own branch. When the tip is
  not a fast-forward of it, `update --remote` points at `rwv sync` — the verb
  that reconciles a workweave with its parent — rather than at a flag.

A tip that is not a fast-forward refuses in the canonical too, naming two exits:
reconcile the branch with its tracking tip yourself (ordinary `git rebase` /
`git merge`) and re-run, or pass `--detach-checkouts` to materialize the tip on
a detached HEAD without moving your branch.

A repo whose HEAD is already detached stays detached at the new tip; no consent
is needed, because there is no branch to abandon. It refuses while the repo is
stopped mid-rebase, mid-merge or mid-bisect — a detached HEAD cannot say which of
those it is, and only one of them is rwv's to move.

With `--commit`, the project repo is judged before anything moves: uncommitted
changes to tracked files outside `rwv.toml`, `rwv.lock` and the integration files
the run regenerates refuse (`edits` `dirty`) with the lock and the members as
they were.

## Invocation

```
rwv update [--detach-checkouts] [--project <name>] [--role <role>]...
           [--repo <selector>]... [-j <n>] [--json] [--no-materialize]
rwv update --remote [--dirty] [--commit [-m <MSG>]] [--detach-checkouts]
           [--project <name>] [--role <role>]... [--repo <selector>]...
           [-j <n>] [--json] [--no-materialize]
```

- `--remote` — advance each repo to its branch HEAD on the remote and
  re-snapshot the lock, instead of aligning to the lock.
- `--dirty` — with `--remote`: allow the lock re-snapshot with uncommitted
  changes in repos.
- `--commit` — with `--remote`: commit the new `rwv.lock` in the project repo,
  together with the integration files regenerated against the new tips and with
  `rwv.toml` when it has uncommitted changes, under an auto-generated message
  listing which repos advanced (and naming `rwv.toml` when it is in the
  commit). Refuses before anything moves if the project repo has uncommitted
  changes outside that set.
- `-m, --message <MSG>` — the commit message for `--commit`, in place of the
  generated one. Without `--commit` it is a usage error (exit 2).
- `--detach-checkouts` — align or advance a repo even where that changes what
  HEAD is attached to: materialize the pin, or the tip, on a detached HEAD
  instead of refusing. The branch it leaves behind is not moved.
- `--role <role>` / `--repo <selector>` — limit the update to a subset of the
  manifest. Repeat for union. Bare strings match exactly; `re:<pat>` is regex;
  `glob:<pat>` is glob.
- `--project <name>` — operate on this project instead of the active project.
  Does not change `.rwv-active`.
- `--json` emits machine-readable output (see Output below).
- `-j N` runs per-repo work in parallel. Default: `min(nproc, 8)`.
- `--no-materialize` — skip the integration install hooks (`npm install`,
  `uv sync`, `cargo fetch`, …) of the activation a plain update performs when it
  selects the weave's only project, or of the regeneration `--remote` runs at
  primary; `rwv materialize` runs them later. Inside a workweave the
  regeneration's hooks are skipped either way.

Run `rwv update --help` for the full clap surface.

## Output

### `rwv update`

Default text output is one line per repo on the standard text channel. Lines
report whether each repo was cloned fresh, aligned to a lock-pinned revision, or
left where it is. Errors (including per-repo failures) are printed to stderr. A
final summary line reports how many repos are ready.

Under `--json`, `update` emits a single JSON envelope after all repos finish,
whatever `-j` is:

```
{
  "$schema": "<url>",
  "outcomes": [ { "path": "...", ... }, ... ],
  "placements": [ { "path": "...", "absolute_path": "...", "files": { ... } }, ... ],
  "skipped": [ { "effect": "install-hooks", "reason": "no-materialize", "remedy": "rwv materialize" }, ... ]
}
```

`outcomes` lists the repos sorted by repo path, however the workers finished.
Each record has the shape: `path` (manifest-relative), `absolute_path` (on-disk),
`status` (`ok` / `skipped` / `failed`), `message` (human-readable detail; absent
for `ok`, present for `skipped` and `failed`), and, on a `failed` record whose
cause is a refusal, `refusal`: its token, which `rwv explain <token>` resolves.
In text mode the same token follows the repo's error line as `rwv explain
<token>`.

File counts are never on an outcome. Each checkout whose files the update placed
gets a placement record: `path`, `absolute_path` (the checkout the files are in),
and `files` (`reflinked`, `hardlinked`, `written`). A clone the update realigned
gets one, and so, from a workweave, does each worktree the update creates there
for a member the workweave lacks, under the workweave's path. The envelope lists
them under `placements`, empty when nothing was placed.

An update that does not activate a project, or activates one without running its
hooks, says so in one line naming the command that does it:

- an update that activates nothing but created or moved a checkout (at primary a
  member clone; in a workweave a member worktree it created): `[skipped] core:
  the install hooks did not run (project `<project>` was already active, so this
  update activated nothing); `rwv materialize` runs them` — or, for a project the
  weave root does not present, `(`<project>` is not the project this weave root
  presents); `rwv activate <project>` runs them`. An update that changed no
  checkout says nothing;
- an activation run with `--no-materialize`: `[skipped] core: the install hooks
  did not run (--no-materialize); `rwv materialize` runs them`;
- a hook whose tool is not on `PATH`: its `tool-missing` warning.

The envelope records the same facts in `skipped`. Each record carries `effect`
(`install-hooks`, `integration-files`, `weave-root-links` or `activation`),
`integration` (present only when one integration's part was skipped), `reason`
(`no-materialize`, `inside-workweave` or `narrowed`, or an object naming what it
concerns: `{"tool-missing": {"tool": …}}`, `{"not-presented": {"project": …}}`,
`{"project-already-active": {"project": …}}`) and `remedy`, the command that
performs the effect.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "UpdateJsonOutput",
  "description": "Top-level envelope for `rwv update --json`.\n\nShape: `{ \"$schema\": \"<url>\", \"outcomes\": [<MemberOutcomeOutput>, ...] }`.",
  "type": "object",
  "required": [
    "$schema",
    "judged",
    "outcomes",
    "placements"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "judged": {
      "description": "Each member the update left where it was, judged on the `alignment` axis (and `edits`, where uncommitted changes kept it), with that state's choices. Empty, not absent, when it left none.",
      "type": "array",
      "items": {
        "$ref": "#/definitions/JudgedOutput"
      }
    },
    "outcomes": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/MemberOutcomeOutput"
      }
    },
    "placements": {
      "description": "How the files of each checkout the update moved or created were placed: a canonical clone it realigned, and from a workweave each worktree it created there. Empty, not absent, when it placed none.",
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
      "description": "The materialization side effects this update skipped, each naming the command that performs it. Empty, not absent, when it skipped none.",
      "default": [],
      "type": "array",
      "items": {
        "$ref": "#/definitions/SkippedEffect"
      }
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
          "description": "Activating a cloned project: selecting it, surfacing its links and running its install hooks.",
          "type": "string",
          "enum": [
            "activation"
          ]
        }
      ]
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
    "MemberOutcomeOutput": {
      "description": "Per-member outcome record for `rwv update --json` and `rwv clone --json`.\n\n`status` is one of `\"ok\"`, `\"skipped\"`, or `\"failed\"`. `message` carries a human-readable description of the outcome (always present for `\"failed\"`; present for `\"skipped\"` to say why; `null` for `\"ok\"`).",
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
        "facts": {
          "description": "What holds for the member in its expected state, such as the branch it would sit on being checked out in another workspace.",
          "type": "array",
          "items": {
            "$ref": "#/definitions/FactOutput"
          }
        },
        "message": {
          "type": [
            "string",
            "null"
          ]
        },
        "moved_from": {
          "description": "The branch the checkout was moved off, when it was neither a declared branch nor named after a recorded weave branch.",
          "type": [
            "string",
            "null"
          ]
        },
        "path": {
          "type": "string"
        },
        "refusal": {
          "description": "The refusal token of a `failed` repo, resolvable with `rwv explain <token>`. Absent when the failure carries no classified refusal.",
          "type": [
            "string",
            "null"
          ]
        },
        "status": {
          "$ref": "#/definitions/MemberOutcomeStatus"
        }
      }
    },
    "MemberOutcomeStatus": {
      "description": "Status discriminant for `MemberOutcomeOutput`.",
      "type": "string",
      "enum": [
        "ok",
        "skipped",
        "failed"
      ]
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
          "description": "`rwv update --remote` was narrowed by `--role` or `--repo`.",
          "type": "string",
          "enum": [
            "narrowed"
          ]
        },
        {
          "description": "A project was already active, so the verb selected none.",
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

### `rwv update --remote`

Default text output: a fetch-progress line per repo (prefixed with `[<repo>]`
under `-j > 1`), followed by a summary line of the form `rwv update --remote:
advanced N repo(s)` — `N` counts the repos whose SHA actually changed, not the
repos the run visited. The subsequent lock re-snapshot emits `Wrote <path>` on
stderr. Unchanged entries are not individually reported.

An unfiltered `--remote` also re-authors the enabled integrations' managed
content against the advanced tree, and then reports any **member
incompatibility** the advance exposed — a `[warning] <integration>:
member-incompatibility: …` line on stderr, naming the value and its two
remedies. Advancing members can raise what they require above a value rwv seeded
once and stepped back from (a go.work go directive below the members' `go.mod`),
and `update --remote` is where you are standing when that happens. It is a
report and nothing more: the advance is valid, the lock is written, and the exit
code is unaffected. `rwv doctor` is the standing arm for the same finding; both
run the same predicate. Because it goes to stderr, `--json` stdout stays purely
structured and the envelope below is unchanged.

A side effect the run skipped is said on stderr, one line each, naming the
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

`repos` lists the repos sorted by repo path, however the workers finished. Each
record carries:
- `path` — manifest-relative repo path.
- `absolute_path` — fully resolved on-disk path.
- `branch` — branch name from the manifest `branch` field.
- `kind` — `updated` (old_sha ≠ new_sha), `up-to-date` (already at HEAD), or
  `failed`.
- `old_sha` — SHA before the advance (omitted if unreadable).
- `new_sha` — SHA after the advance (omitted when `kind = failed`).
- `error` — human-readable failure message (only when `kind = failed`).

File counts are never on a record. Each checkout whose files the advance
rewrote gets a placement record: `path`, `absolute_path`, and `files`
(`reflinked`, `hardlinked`, `written`). The envelope lists them under
`placements`, empty when nothing was placed. Each record in `skipped` carries the
fields described above.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "UpdateRemoteJsonOutput",
  "description": "Top-level envelope for `rwv update --remote --json`. `{ \"$schema\": \"<url>\", \"repos\": [<RepoUpdateRecord>, ...] }`",
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
          "description": "Activating a cloned project: selecting it, surfacing its links and running its install hooks.",
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
      "description": "Per-repo record in `rwv update --remote --json` output.\n\n`old_sha` is the tip before the fetch; `new_sha` is the tip after checkout (the new branch HEAD). Both are `null` when the SHA could not be read (e.g. the repo was missing from disk before the advance). For `kind = failed`, `new_sha` is always `null`; `error` carries the human-readable failure message.",
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
          "description": "Branch name from the manifest `branch` field.",
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
          "description": "`rwv update --remote` was narrowed by `--role` or `--repo`.",
          "type": "string",
          "enum": [
            "narrowed"
          ]
        },
        {
          "description": "A project was already active, so the verb selected none.",
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
      "description": "Per-repo outcome kind for `rwv update --remote --json`.",
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

- `0` — every repo was aligned (or, with `--remote`, advanced and the lock
  written), or no changes were needed.
- non-zero — workspace could not be resolved, manifest parse failure, or one or
  more repos failed (with `--remote` the lock is then not written). Under
  `--json` the envelope is emitted before exit even on failure.

## Examples

Bring up the members of the active project at the commits its lock pins:

```
rwv update
```

The same for a named project, leaving `.rwv-active` as it is:

```
rwv update --project web-app
```

Realign every present clone to the lock even where that means detaching it from
the branch it is on:

```
rwv update --detach-checkouts
```

Advance the lock to the latest upstream tips:

```
rwv update --remote
```

Advance one repo and commit the new lock:

```
rwv update --remote --repo github/acme/server --commit
```

## Common errors

- *no repoweave workspace found above …* — `rwv update` requires an existing
  workspace; either `cd` into one or run `rwv clone <source>` to bring up a new
  project.
- *no active project* — the weave holds zero or several projects and none is
  active; pass `--project <name>` or run `rwv activate <name>`.
- *aligning … is not a fast-forward* — the pin is not a descendant of the branch
  tip (an older lock, or a branch carrying commits its remote does not have).
  Reconcile the branch with the pin yourself and re-run, or pass
  `--detach-checkouts` to materialize the pin on a detached HEAD.
- *is on branch 'X', which is not the local counterpart of …* — the checkout is
  on a branch the manifest does not declare. Switch to the declared branch and
  re-run, or pass `--detach-checkouts`.
- *repo is mid-…* — an already-detached repo is stopped mid-rebase, mid-merge or
  mid-bisect. Finish or abort that operation and re-run; rwv will not move a HEAD
  that is carrying an in-flight operation's state.
- *revision … not found in …* — at the primary, the clone is present but its
  object store does not have the locked revision, and a present clone is
  realigned without a network fetch. Run `rwv fetch`, or re-lock to a reachable
  revision.
- *clone missing on disk* (with `--remote`) — a manifest entry references a repo
  that isn't cloned on disk; run `rwv update` to re-materialize missing manifest
  members, then re-run `rwv update --remote`.
- *could not resolve branch on the remote* (with `--remote`) — the branch named
  in the manifest's `branch` field doesn't exist on the upstream remote; verify
  the branch name and remote configuration.
- *fetch failed* (with `--remote`) — network error or remote authentication
  problem; inspect the git output for details.
- *lock not written* (with `--remote`) — reported when one or more repos fail to
  advance; the lock is left unchanged so a partial update never lands.
- *target unrecorded* — the checked-out branch of the project repo has no record
  of how it is used, so this verb, which aligns members (with `--remote`, and locks), refuses before changing
  anything. The record may be published but not yet fetched (`rwv fetch`);
  otherwise say how the branch is used with `rwv branch set-mode`.
