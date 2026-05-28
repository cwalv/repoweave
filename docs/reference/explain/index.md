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
- **fetch** — clone or fetch every repo in the active project
- **update** — advance the lock to current HEADs
- **prime** — agent-oriented orientation context for the workspace
- **explain** — per-verb JIT reflection (this verb)

Committed schemas live under `docs/reference/schemas/`. CI fails on drift between Rust types and committed artifacts; do not hand-edit the assembled files — edit `docs/reference/explain/templates/<verb>.md.tmpl` and re-run `cargo run --bin generate-explain`.
