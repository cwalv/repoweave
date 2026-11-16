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
rwv explain <verb> <sub-verb>
rwv explain <token>
rwv explain <kind>/<sub-kind>
```

- With no argument, prints the index of explainable verbs.
- With a verb, prints that verb's markdown bundle.
- With a verb and one of its sub-verbs (`rwv explain member add`), prints the
  sub-verb's section of the verb's bundle, or the whole bundle when the verb's
  page has no section of that name. A second word that is not a sub-verb of
  the verb is refused, naming the sub-verbs it has.
- With a refusal kind or token, a `rwv doctor` finding kind or sub-kind, or a
  VCS error kind, prints that token's entry. A kind's entry lists its states
  and the choices each prints, generated from the table refusals are printed
  from.
- With a finding's `<kind>/<sub-kind>`, prints the sub-kind's entry, or the
  kind's entry where one entry covers all its sub-kinds.
- A name that is no verb, token or plugin returns non-zero and says there is
  no entry for it, lists the verbs, and points at `rwv explain <token>` for
  tokens.

Run `rwv explain --help` for the full clap surface.

## Output

Every invocation (index or bundle) opens with a `rwv <version>` line — the
same string `rwv --version` prints — then a blank line, then markdown to
stdout. For JSON-capable verbs (`status`, `doctor`, `sync`, `fetch`,
`update`), the bundle includes a fenced ```json block with the JSON Schema.
For markdown-only verbs (`prime`, `explain`), no schema block is included.

## Exit codes

- `0` — bundle, entry or index printed successfully.
- non-zero — the name reaches nothing, or is a sub-kind more than one finding
  kind carries.

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

- *no explain entry for `<name>`; did you mean: <candidate>?* — the verb
  isn't in the explain registry, but its spelling is within edit distance
  of a known core verb. Try the suggestion, or run `rwv explain` (no args)
  for the full index.
- *`<sub-kind>` is a sub-kind of more than one finding kind* — the
  refusal lists `rwv explain <kind>/<sub-kind>` for each kind; run the one
  for the finding you met.
- *external command; try `rwv <verb> --help`* — the name is a plugin on
  `$PATH`, not a core verb. `explain` reflects over `rwv`'s CI-checked surfaces only and never
  execs PATH content, so third-party `rwv-<verb>` executables document
  themselves through their own `--help`.
