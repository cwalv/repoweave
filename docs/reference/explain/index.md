# rwv explain — index

Per-verb agent-oriented reflection. Each entry below has a markdown bundle describing the verb's purpose, invocation, output shape, exit codes, and examples. JSON-capable verbs additionally embed the JSON Schema for their `--json` output.

Usage:

```
rwv explain <verb>
```

## Verbs

- **status** — per-repo workspace state (branch, tip, lock, relation) (`--json` available)
- **doctor** — convention-violation checks (orphans, drift, stale locks) (`--json` available)
- **sync** — reconcile each repo with its locked SHA (`--json` available)
- **sync-to** — advance target workspace to CWD's tip (3-step orchestration: rebase, relock, FF-advance) (`--json` available)
- **push** — publish manifest repos then the project repo to shared remotes (`--json` available)
- **fetch** — clone or fetch every repo in the active project (`--json` available)
- **update** — advance the lock to current HEADs (`--json` available)
- **prime** — agent-oriented orientation context for the workspace
- **explain** — per-verb JIT reflection (this verb)
- **workweave** — create, delete, or list workweaves for a project
- **abort** — restore CWD workspace to its pre-sync state using savepoint refs
- **add** — clone a repo and register it in the active project manifest
- **remove** — remove a repo from the active project manifest
- **lock** — snapshot current repo HEADs into rwv.lock (pure local; no network)
- **activate** — set the active project, create symlinks, run integration install hooks
- **init** — create a new project (or adopt an existing repo) and auto-activate it

Committed schemas live under `docs/reference/schemas/`. CI fails on drift between Rust types and committed artifacts; do not hand-edit the assembled files — edit `docs/reference/explain/templates/<verb>.md.tmpl` and re-run `cargo run --bin generate-explain`.
