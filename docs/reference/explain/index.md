# rwv explain — index

Per-verb agent-oriented reflection. Each entry below has a markdown bundle describing the verb's purpose, invocation, output shape, exit codes, and examples. JSON-capable verbs additionally embed the JSON Schema for their `--json` output.

Usage:

```
rwv explain <verb>
```

## Verbs

- **status** — per-repo workspace state (branch, tip, lock, relation) (`--json` available)
- **log** — the VCS's log per repo, or the commits beyond a weave branch's pins (`--json` available)
- **doctor** — convention-violation checks (orphans, drift, stale locks) (`--json` available)
- **sync** — bring another workspace's committed state into this one (`--json` available)
- **land** — land this feature into its record's target: compose here, then fast-forward the target (`--json` available)
- **cherry-pick** — port one project commit onto this feature: copy member ranges, carry dependency pins, commit the relock (`--json` available)
- **push** — publish member repos then the project repo to shared remotes (`--json` available)
- **clone** — clone a project and each member at the commit rwv.lock pins; exit status 3 when a member is left unpinned (`--json` available)
- **fetch** — the pruning fetch of every remote of every repo; moves no branch, checkout or lock (`--json` available)
- **pull** — fetch, fast-forward the project branch to its upstream, then update the members (`--json` available)
- **update** — clone missing members and align the rest to rwv.lock; with --remote, advance each repo to its branch HEAD and re-snapshot the lock (`--json` available)
- **prime** — agent-oriented orientation context for the workspace
- **explain** — per-verb JIT reflection (this verb)
- **workweave** — create, delete, or list workweaves for a project (`--json` available)
- **abort** — roll an in-flight sync or landing back to its savepoints, from either workspace it touched
- **backup** — list, drop or prune the backups kept before a step that would leave a commit or an edit unpreserved (`--json` available)
- **branch** — create a weave branch, record how a weave branch is used, list the weave branches with their records, rename one, or delete one (`--json` available)
- **switch** — put the project repo on a weave branch and align the members to it, or detach the workspace at a revision
- **member** — add a repo to the active project manifest, remove one, or change a member's role or submodule setting
- **lock** — snapshot current repo HEADs into rwv.lock (pure local; no network)
- **activate** — set the active project, create symlinks, run integration install hooks
- **materialize** — settle drift, clean up disabled integrations, repair weave-root symlinks, and run integration install hooks for the presented project, without selecting anything
- **init** — create a new project (or adopt an existing repo) and auto-activate it
- **each** — run one command in each repo of the checkout (bundled plugin) (`--json` available)

`rwv explain <token>` serves the entry for a refusal token, a `rwv doctor` finding kind, or a VCS error kind. `rwv explain <kind>/<sub-kind>` serves the entry for a finding's sub-kind.
