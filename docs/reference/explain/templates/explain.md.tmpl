# rwv explain

## Purpose

Per-verb JIT reflection. `rwv explain <verb>` prints a markdown bundle
describing the verb's purpose, invocation surface, output shape, exit
codes, examples, and (for `--json`-capable verbs) the embedded JSON Schema.
With no argument, prints an index of explainable verbs.

`explain` exists because agent context budget is the scarce resource for
reflection. Bulk-dumping every verb's schema into `rwv prime` would defeat
context economy; per-verb JIT pull respects it. `prime` advertises the
JSON surface and points here; `explain` answers per-verb questions on
demand.

The artifacts shipped with the binary are build-time generated from the
same Rust types that produce `--json` output. Schemars derives the JSON
Schemas; a generator binary (`generate-explain`) assembles the markdown
from hand-written templates + the derived schemas; the main `rwv` binary
embeds the result via `include_str!()` and dispatches with a trivial
match. CI fails on drift between Rust types and committed artifacts.

## Invocation

```
rwv explain [<verb>]
```

- With no argument, prints the index of explainable verbs.
- With a verb, prints that verb's markdown bundle.
- Unknown verbs return non-zero with a friendly pointer to the index.

Run `rwv --help explain` for the full clap surface.

## Output

Markdown to stdout. For JSON-capable verbs (`status`, `doctor`, `sync`),
the bundle includes a fenced ```json block with the JSON Schema. For
markdown-only verbs (`fetch`, `update`, `prime`, `explain`), no schema
block is included.

## Exit codes

- `0` — bundle (or index) printed successfully.
- non-zero — unknown verb requested.

## Examples

List all explainable verbs:

```
rwv explain
```

Get the JSON Schema for `sync` output:

```
rwv explain sync
```

Read the schema for `doctor --json` directly from the committed artifact:

```
cat docs/reference/schemas/doctor.json
```

## Common errors

- *no explain entry for '<verb>'; did you mean: <candidate>?* — the verb
  isn't in the explain registry, but its spelling is within edit distance
  of a known core verb. Try the suggestion, or run `rwv explain` (no args)
  for the full index.
- *external command; try `rwv <verb> --help`* — the verb isn't a core
  verb. `explain` reflects over `rwv`'s CI-checked surfaces only and never
  execs PATH content, so third-party `rwv-<verb>` executables document
  themselves through their own `--help`.
