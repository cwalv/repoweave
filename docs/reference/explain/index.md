# rwv explain — index

Per-verb agent-oriented reflection. Each entry below has a markdown bundle describing the verb's purpose, invocation, output shape, exit codes, and examples. JSON-capable verbs additionally embed the JSON Schema for their `--json` output.

Usage:

```
rwv explain <verb>
```

## Verbs

- **status** — per-repo workspace state (branch, tip, lock, relation) (`--json` available)
- **doctor** — convention-violation checks (orphans, drift, stale locks) (`--json` available)
- **sync** — bring another workspace's committed state into this one (`--json` available)
- **sync-to** — advance target workspace to CWD's tip (3-step orchestration: rebase, relock, FF-advance) (`--json` available)
- **push** — publish member repos then the project repo to shared remotes (`--json` available)
- **fetch** — clone a project, or align its repos to rwv.lock without advancing pins (`--json` available)
- **update** — advance each repo to its branch HEAD on the remote and re-snapshot the lock (`--json` available)
- **prime** — agent-oriented orientation context for the workspace
- **explain** — per-verb JIT reflection (this verb)
- **workweave** — create, delete, or list workweaves for a project
- **abort** — roll an in-flight sync or sync-to back to its savepoints, from either workspace it touched
- **add** — clone a repo and register it in the active project manifest
- **remove** — remove a repo from the active project manifest
- **role** — change a repo's role in the active project manifest and settle its files
- **lock** — snapshot current repo HEADs into rwv.lock (pure local; no network)
- **activate** — set the active project, create symlinks, run integration install hooks
- **materialize** — settle drift, clean up disabled integrations, repair weave-root symlinks, and run integration install hooks for the presented project, without selecting anything
- **init** — create a new project (or adopt an existing repo) and auto-activate it

`rwv explain <token>` serves the entry for a refusal token, a `rwv doctor` finding kind, or a VCS error kind. `rwv explain <kind>/<sub-kind>` serves the entry for a finding's sub-kind.
