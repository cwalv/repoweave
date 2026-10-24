# Plugin protocol

A plugin is an executable named `rwv-<verb>` on `$PATH`. When `rwv <verb>` is invoked and `<verb>` is not a core verb, `rwv` execs the matching `rwv-<verb>` binary, projects the resolved workspace context into its environment, and propagates its exit status back verbatim. This page is the wire contract: discovery, the environment envelope, the working directory, addressing, exit codes, the write prohibition, and the compatibility guarantee.

For the design rationale — what earns a place in core versus a plugin, and why `rwv` doesn't sandbox plugins — see the [plugin-boundary](../explanation/joints/plugin-boundary.md) joint. For a worked example, see the [write-a-plugin](../how-to/write-a-plugin.md) how-to.

## Discovery and naming

- Name your executable `rwv-<verb>`; anything after the `rwv-` prefix is a valid verb. One word is conventional; hyphens are allowed for a multi-word verb (`rwv-check-deps`).
- **The verb is the name you type.** `rwv` finds plugins the way the platform's own command lookup finds programs, and lists each one under the name that reaches it.
  - **Unix:** the file must carry the executable bit, and the whole name after `rwv-` is the verb. Nothing is stripped: an executable `rwv-lint.py` is the verb `lint.py`, run as `rwv lint.py`. Name the file `rwv-lint` to get `rwv lint`.
  - **Windows:** a file is executable when its extension is listed in `PATHEXT` (by default `.COM;.EXE;.BAT;.CMD;…`), and the verb drops that extension, in any letter case: `rwv-lint.exe`, `rwv-lint.CMD` and `rwv-lint.bat` are each the verb `lint`. Ship anything a Windows command prompt would run — a compiled `.exe`, or a `.cmd` shim around an interpreter. A `rwv-*` file whose extension is not in `PATHEXT` (`rwv-lint.py`, `rwv-notes.txt`) is not a plugin and is not listed.
- **Core always wins.** Core verbs are matched before external fallthrough runs. Naming a plugin after an existing — or future — core verb makes it unreachable; `rwv <name>` always resolves to the built-in.
- **First-found on `$PATH` wins.** When the same verb exists in more than one `$PATH` directory, the first one found in `$PATH` order is the one `rwv` execs. On Windows, two files for one verb in the same directory (`rwv-lint.exe` beside `rwv-lint.cmd`) are duplicates too, and the one whose extension comes first in `PATHEXT` wins; directory order is decided first. `rwv doctor --json` inventories every copy in its `plugins` array — `name` is the verb, `path` the file with its real name — and marks every non-winning copy `shadowed: true` with `shadowed_by` pointing at the winner. That inventory is the audit surface for duplicates, not a dispatch-time warning. It is not a finding: a plugin's presence never changes doctor's exit code. `rwv explain doctor` describes the array.
- `rwv --help` lists discovered plugin names under an "External commands" section — winners only; shadowed copies are omitted there and surface only in the doctor inventory.
- `rwv explain` only reflects over `rwv`'s own CI-checked surfaces, never over `$PATH` content. A bundled plugin is one of those surfaces: `rwv explain each` serves the page for [`rwv each`](./rwv-each.md). On the name of any other plugin found on `$PATH`, `rwv explain <name>` refuses (`no-explain-entry`) with ``external command; try `rwv <name> --help` ``, even when the name is a typo's distance from something `rwv explain` does serve.
- **Two failure modes, no others.** Everything that can go wrong on the `rwv` side of dispatch collapses to exactly one of:
  - `unknown verb` — no core verb and no `rwv-<verb>` anywhere on `$PATH`.
  - `exec failure` — a `rwv-<verb>` binary was found but could not be spawned (permissions, `ENOEXEC`, …); the OS error is included.
- **Soft fallthrough.** With no addressing flags, if the `cwd` walk finds no workspace, the plugin is still exec'd — some plugins legitimately run outside any workspace (`--help`, generators, setup commands). An addressing flag that fails to resolve explicitly (`-C <bad-path>` or `-w <bad-name>`) errors before any spawn is attempted: a plugin cannot salvage a target that doesn't exist.

## Context envelope

`rwv` sets the following variables on every plugin spawn, before `exec`:

| Variable | Value | Unset when |
|---|---|---|
| `RWV_VERSION` | `rwv` release version, semver (`0.24.0`); never a `git describe` string | never |
| `RWV_WORKSPACE` | primary weave root (absolute path) | no workspace resolved |
| `RWV_WORKWEAVE` | `<project>--<name>` | not in a workweave, or in one the registry does not name |
| `RWV_WORKWEAVE_UNREGISTERED` | `1` | anywhere else |
| `RWV_PROJECT` | resolved project name | no project resolved |

- **Outputs only.** `rwv` sets these; it never reads them back. A plugin that needs to hand a value back to `rwv` passes it as an explicit argument — see addressing, below.
- **Presence encodes checkout kind across `RWV_WORKWEAVE` and `RWV_WORKWEAVE_UNREGISTERED`:**

  | Checkout kind | `RWV_WORKWEAVE` | `RWV_WORKWEAVE_UNREGISTERED` |
  |---|---|---|
  | Primary weave | *(unset)* | *(unset)* |
  | Registered workweave | `<project>--<name>` | *(unset)* |
  | Unregistered workweave (register with `rwv doctor --fix`) | *(unset)* | `1` |

- **A workspace can resolve without a project.** At a primary with no project selected, `RWV_WORKSPACE` is set and `RWV_PROJECT` is not; `rwv activate <project>` selects one.
- **`RWV_WORKSPACE` is always the primary root**, not "wherever this checkout is" — the value is identical whether the plugin is running at primary or inside a workweave. See addressing, below, for why this matters.
- **One projection, two surfaces.** The envelope is a pure projection of the same resolved value that the `resolution` block in `--json` output serializes. A plugin that reads both never sees them disagree.
- **Version floors are a last resort.** `RWV_VERSION` is always set, but prefer probing the shape of `--json` output over gating on it — see [additive-schema guarantee](#additive-schema-guarantee) below.

## Working directory

The plugin starts in the **invocation directory**: the `-C` directory when `-C` is given, the workweave `-w` names when `-w` is given, and otherwise the directory `rwv` was run from. It is the directory core verbs resolve relative path arguments against, so a relative path a plugin receives as an argument names the file it would name for a core verb. `rwv -C ~/work/app lint src/main.rs` starts `rwv-lint` in `~/work/app` with the argument `src/main.rs`, as `git -C` would. A plugin resolves relative arguments against its own working directory and needs nothing from `rwv` to do so.

- **The working directory is where the invocation was addressed, not where the workspace is.** `rwv -C ~/work/app/github/org/lib lint` starts the plugin in `github/org/lib`, while `RWV_WORKSPACE` still names the primary weave root.
- **Without `-C` or `-w` nothing moves.** The plugin starts in the directory `rwv` was run from, the same directory it would have inherited.
- **A relative `$PATH` entry is read from the directory `rwv` was run from.** The plugin it finds is the one the shell would have found, wherever the plugin then starts.

## Addressing back into `rwv`

A plugin that needs to invoke `rwv` — to read status, consume `--json` output, or trigger a core verb — should address it explicitly with `-C`, `-w` (global flags before the subcommand), and `--project` (on the subcommand) rather than rely on its working directory. A bare `rwv` run from the plugin repeats the containment walk from wherever the plugin is when it runs; the envelope already holds the walk's answer:

```sh
rwv -C "$RWV_WORKSPACE" status --project "$RWV_PROJECT" --json
```

`$RWV_WORKSPACE` is always the **primary** weave root. This form is correct for reaching primary — and a plugin never needs a separate lookup to find it; it is already sitting in the envelope. `rwv resolve` has no flag for this; the envelope is the only lookup you need.

**Reaching primary is not the same as reaching the checkout the plugin is running in.** If the plugin should operate on the *same* workweave it was dispatched from, add `-w` — `$RWV_WORKWEAVE` is already in the `<project>--<name>` shape `-w` expects, and it subsumes `--project` (the workweave name's own prefix resolves the project, so drop `--project` when you pass it). `-C` and `-w` are global flags and go before the verb; `--project` belongs to the verb and goes after it:

```sh
rwv -C "$RWV_WORKSPACE" -w "$RWV_WORKWEAVE" status --json
```

`-C` pins the search to the primary root explicitly, robust against wherever the plugin's own `cwd` happens to be; `-w` then selects the workweave by name within it. Use the first form when `$RWV_WORKWEAVE` is unset — there is no workweave to select, and it already addresses the right place.

## Exit codes and output

- **Exit propagates verbatim.** A normal plugin exit passes its status code straight through as `rwv`'s own exit code.
- **Signal death maps to `128 + N`** — Unix only; Windows has no POSIX signals, so this mapping never applies on a Windows build — reported on stderr as `rwv-<verb> terminated by signal N`.
- **No wrapping.** The plugin inherits stdin, stdout, and stderr directly; `rwv` never buffers or translates them. JSON output, terminal-control sequences, and streaming progress all pass through unchanged.
- **Interrupts belong to the plugin.** A Ctrl-C at the terminal reaches the plugin as it would any program; `rwv` itself waits it out, so the plugin can wind down and its exit status — `130`, say — is the one the shell sees.

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
- [reference/cli](./cli.md) — the `-C` and `-w` global flags, per-verb `--project`, and the `--json` envelope convention.
- [Refusals](./refusals.md) — `unknown-verb` and `no-explain-entry`, the dispatch-side refusals.
