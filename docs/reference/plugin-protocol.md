# Plugin protocol

A plugin is an executable named `rwv-<verb>` on `$PATH`. When `rwv <verb>` is invoked and `<verb>` is not a core verb, `rwv` execs the matching `rwv-<verb>` binary, projects the resolved workspace context into its environment, and propagates its exit status back verbatim. This page is the wire contract: discovery, the environment envelope, addressing, exit codes, the write prohibition, and the compatibility guarantee.

For the design rationale — what earns a place in core versus a plugin, and why `rwv` doesn't sandbox plugins — see the [plugin-boundary](../explanation/joints/plugin-boundary.md) joint. For a worked example, see the [write-a-plugin](../how-to/write-a-plugin.md) how-to.

## Discovery and naming

- Name your executable `rwv-<verb>`; anything after the `rwv-` prefix is a valid verb. One word is conventional; hyphens are allowed for a multi-word verb (`rwv-check-deps`).
- **Core always wins.** Core verbs are matched before external fallthrough runs. Naming a plugin after an existing — or future — core verb makes it unreachable; `rwv <name>` always resolves to the built-in.
- **First-found on `$PATH` wins.** When the same name exists in more than one `$PATH` directory, the first one found in `$PATH` order is the one `rwv` execs. `rwv doctor --json` inventories every copy in its `plugins` array and marks every non-winning copy `shadowed: true` with `shadowed_by` pointing at the winner — that inventory is the audit surface for duplicates, not a dispatch-time warning. See [`rwv doctor`](./explain/doctor.md).
- `rwv --help` lists discovered plugin names under an "External commands" section — winners only; shadowed copies are omitted there and surface only in the doctor inventory.
- `rwv explain` does not reflect over plugins: `rwv explain <name>` on a plugin name errors with `external command; try \`rwv <name> --help\``. `explain` only reflects over `rwv`'s own CI-checked surfaces, never over `$PATH` content.
- **Two failure modes, no others.** Everything that can go wrong on the `rwv` side of dispatch collapses to exactly one of:
  - `unknown verb` — no core verb and no `rwv-<verb>` anywhere on `$PATH`.
  - `exec failure` — a `rwv-<verb>` binary was found but could not be spawned (permissions, `ENOEXEC`, …); the OS error is included.
- **Soft fallthrough.** With no addressing flags, if the `cwd` walk finds no workspace, the plugin is still exec'd — some plugins legitimately run outside any workspace (`--help`, generators, setup commands). An addressing flag that fails to resolve explicitly (`-C <bad-path>` or `-w <bad-name>`) errors before any spawn is attempted: a plugin cannot salvage a target that doesn't exist.

## Context envelope

`rwv` sets the following variables on every plugin spawn, before `exec`:

| Variable | Value | Unset when |
|---|---|---|
| `RWV_VERSION` | `rwv` semver | never |
| `RWV_WORKSPACE` | primary workspace root (absolute path) | no workspace resolved |
| `RWV_WORKWEAVE` | `<project>--<name>` | not in a workweave, or in one the registry does not name |
| `RWV_WORKWEAVE_UNREGISTERED` | `1` | anywhere else |
| `RWV_PROJECT` | resolved project name | no workspace resolved |

- **Outputs only.** `rwv` sets these; it never reads them back. A plugin that needs to hand a value back to `rwv` passes it as an explicit argument — see addressing, below.
- **Presence encodes kind, across two variables.** `RWV_WORKWEAVE` being set means the invocation addressed a workweave and the registry names it. The checkout is one of three states, though, and the third — a workweave whose directory no registry entry names — has no identity to put in `RWV_WORKWEAVE`, so it is unset there exactly as it is at the primary. `RWV_WORKWEAVE_UNREGISTERED=1` is what distinguishes the two, and it is set in that one case only. A plugin that wants "am I in a workweave" tests both; one that never looks at the second variable sees the environment it always saw. Run `rwv doctor --fix` to register such a directory, after which the ordinary two-state reading holds again.
- **`RWV_WORKSPACE` is always the primary root**, not "wherever this checkout is" — the value is identical whether the plugin is running at primary or inside a workweave. See addressing, below, for why this matters.
- **One projection, two surfaces.** The envelope is a pure projection of the same resolved value that the `resolution` block in `--json` output serializes. A plugin that reads both never sees them disagree.
- **Version floors are a last resort.** `RWV_VERSION` is always set, but prefer probing the shape of `--json` output over gating on it — see [additive-schema guarantee](#additive-schema-guarantee) below.

## Addressing back into `rwv`

A plugin that needs to invoke `rwv` — to read status, consume `--json` output, or trigger a core verb — should address it explicitly with `-C` / `-w` / `--project` rather than rely on its own `cwd`, which may not match where `rwv` itself was invoked from:

```sh
rwv -C "$RWV_WORKSPACE" --project "$RWV_PROJECT" status --json
```

`$RWV_WORKSPACE` is always the **primary** workspace root. This form is correct for reaching primary — and a plugin never needs a separate lookup to find it; it is already sitting in the envelope. `rwv resolve` has no flag for this; the envelope is the only lookup you need.

**Reaching primary is not the same as reaching the checkout the plugin is running in.** If the plugin should operate on the *same* workweave it was dispatched from, add `-w` — `$RWV_WORKWEAVE` is already in the `<project>--<name>` shape `-w` expects, and it subsumes `--project` (the workweave name's own prefix resolves the project, so drop `--project` when you pass it):

```sh
rwv -C "$RWV_WORKSPACE" -w "$RWV_WORKWEAVE" status --json
```

`-C` pins the search to the primary root explicitly, robust against wherever the plugin's own `cwd` happens to be; `-w` then selects the workweave by name within it. Use the first form when `$RWV_WORKWEAVE` is unset — there is no workweave to select, and it already addresses the right place.

## Exit codes and output

- **Exit propagates verbatim.** A normal plugin exit passes its status code straight through as `rwv`'s own exit code.
- **Signal death maps to `128 + N`** — Unix only; Windows has no POSIX signals, so this mapping never applies on a Windows build — reported on stderr as `rwv-<verb> terminated by signal N`.
- **No wrapping.** The plugin inherits stdin, stdout, and stderr directly; `rwv` never buffers or translates them. JSON output, terminal-control sequences, and streaming progress all pass through unchanged.

## Write prohibition

A plugin must not write:

- `rwv.toml`, `rwv.lock`
- `.rwv-active`, `.rwv-workweave`, `.rwv-workweave-index`
- Ecosystem workspace files managed by an integration (`Cargo.toml`, `go.work`, `package.json`, …)
- The `refs/rwv/*` git-ref namespace `rwv` uses for its own bookkeeping — for example `refs/rwv/pre-op/<op-id>`, the sync-abort savepoint created by `rwv sync` / `rwv sync-to` before replay. This is a ref namespace, not a working-tree file: "must not write" means no ref-update against it, not just filesystem I/O.

This is a documented rule, not a dispatch-time guard — nothing stops a plugin from writing these, and `rwv` does not sandbox its children. `rwv doctor` is the audit surface: it can notice an rwv-owned file changed outside a core-verb write and report it, after the fact, not before.

For what each file records, see [reference/formats](./formats.md) (`rwv.toml`, `rwv.lock`, `.rwv-active`, `.rwv-workweave`, `.rwv-op`). A plugin may keep its own state anywhere else — its own dotfiles, a side-database, per-repo metadata it owns — under one rule: claim a namespace that does not overlap `rwv`'s.

## Additive-schema guarantee

Within a major `rwv` version, committed `--json` schemas only gain fields; they never remove or re-type an existing one. A plugin that reads `repos[].absolute_path` from `rwv status --json` keeps working across every patch and minor release; it breaks only at a major-version boundary, and that break is called out in the migration guide.

Pre-1.0 (`0.x`) does not carry this guarantee: schema breaks are permitted with changelog notice, and a plugin author targeting `0.x` accepts that.

**Probe the shape, don't gate on the version.** Testing whether a field exists degrades gracefully — the check keeps passing as fields are added — where a version ceiling produces false negatives on every compatible release above it:

```sh
# Does this rwv expose the field this plugin depends on? (`all` is vacuously
# true on an empty `repos` array, so this doesn't need a length check first.)
if [ "$(rwv status --json | jq '[.repos[] | has("absolute_path")] | all')" != "true" ]; then
  echo "rwv-myverb: requires an rwv with repos[].absolute_path in status --json" >&2
  exit 1
fi
```

Reach for `$RWV_VERSION` only to gate a behavioral change that has no structural signal in the JSON — rare; most plugin needs are field-shaped.

## Related

- [plugin-boundary](../explanation/joints/plugin-boundary.md) — design rationale: what earns a core verb, the security posture, why doctor doesn't give plugins a check-registration hook.
- [write-a-plugin](../how-to/write-a-plugin.md) — a worked walkthrough building a plugin end to end.
- [reference/formats](./formats.md) — `rwv.toml`, `rwv.lock`, `.rwv-active`, `.rwv-workweave`, `.rwv-op`: what each owned file records.
- [reference/cli](./cli.md) — the `-C`, `-w`, and `--project` global flags in full; the `--json` envelope convention.
- [`rwv doctor`](./explain/doctor.md) — the `plugins` inventory array (`shadowed` / `shadowed_by`).
