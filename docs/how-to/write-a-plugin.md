# Write a plugin

Extend `rwv` with a custom subcommand by placing an executable named `rwv-<verb>` on `$PATH`:

```sh
#!/usr/bin/env bash
# ~/.local/bin/rwv-check-mid-op — exit non-zero if any repo has an operation in progress.
set -euo pipefail

if [ -z "${RWV_WORKSPACE:-}" ]; then
  echo "rwv-check-mid-op: not inside a workspace" >&2
  exit 1
fi

if [ -n "${RWV_WORKWEAVE:-}" ]; then
  data=$(rwv -C "$RWV_WORKSPACE" -w "$RWV_WORKWEAVE" status --json)
else
  data=$(rwv -C "$RWV_WORKSPACE" status --project "$RWV_PROJECT" --json)
fi

if [ "$(echo "$data" | jq '[.repos[] | has("mid_op")] | all')" != "true" ]; then
  echo "rwv-check-mid-op: requires an rwv with repos[].mid_op in status --json" >&2
  exit 1
fi

stuck=$(echo "$data" | jq -r '.repos[] | select(.mid_op != null) | .absolute_path')
if [ -n "$stuck" ]; then
  echo "rwv-check-mid-op: repo(s) with an operation in progress:" >&2
  echo "$stuck" >&2
  exit 1
fi
```

Make the script executable (`chmod +x ~/.local/bin/rwv-check-mid-op`) and invoke it as `rwv check-mid-op`. Below is how each section of the plugin works.

## 1. Guard on being inside a workspace

`rwv` sets `$RWV_WORKSPACE` only when it resolves a workspace. Plugins that require a workspace check `$RWV_WORKSPACE` up front and exit non-zero with a clear message when invoked outside one:

```sh
if [ -z "${RWV_WORKSPACE:-}" ]; then
  echo "rwv-check-mid-op: not inside a workspace" >&2
  exit 1
fi
```

## 2. Re-address `rwv` using the environment envelope

Rather than re-implementing manifest or project resolution, call back into `rwv status --json` using the coordinates `rwv` resolved before dispatching the plugin.

`$RWV_WORKSPACE` is always the **primary** weave root, even when the plugin was invoked inside a workweave. Branch on `$RWV_WORKWEAVE` (and pass `--project "$RWV_PROJECT"` after the `status` subcommand when addressing the primary weave) so the callback inspects the exact checkout and project the caller addressed:

```sh
if [ -n "${RWV_WORKWEAVE:-}" ]; then
  data=$(rwv -C "$RWV_WORKSPACE" -w "$RWV_WORKWEAVE" status --json)
else
  data=$(rwv -C "$RWV_WORKSPACE" status --project "$RWV_PROJECT" --json)
fi
```

A relative path the plugin takes as an argument needs no such care. The plugin starts in the directory `rwv` was invoked from — or the `-C`/`-w` directory — so under `rwv -C ~/work/app`, a plugin that opens the argument `notes.txt` as given opens `~/work/app/notes.txt`. See [Working directory](../reference/plugin-protocol.md#working-directory).

## 3. Probe the JSON shape, not `$RWV_VERSION`

Check that the JSON fields your plugin depends on are present rather than comparing version strings:

```sh
if [ "$(echo "$data" | jq '[.repos[] | has("mid_op")] | all')" != "true" ]; then
  echo "rwv-check-mid-op: requires an rwv with repos[].mid_op in status --json" >&2
  exit 1
fi
```

See [Additive-schema guarantee](../reference/plugin-protocol.md#additive-schema-guarantee) for why structural probing is preferred over version checks.

## 4. Propagate the exit code and verify discovery

`rwv` forwards the plugin's exit code verbatim, so `rwv check-mid-op && rwv sync-to` composes in shell scripts. Confirm the executable is on `$PATH` and discoverable:

```sh
rwv --help            # lists check-mid-op under "External commands"
rwv check-mid-op      # runs ~/.local/bin/rwv-check-mid-op
```

If multiple `rwv-check-mid-op` executables exist on `$PATH`, the first one wins at exec time and shadowed duplicates are reported by `rwv doctor --json` (see [Discovery and naming](../reference/plugin-protocol.md#discovery-and-naming)).

## Related

- [Plugin protocol reference](../reference/plugin-protocol.md) — environment envelope variables, addressing rules, write prohibition, and schema compatibility
- [Plugin boundary](../explanation/joints/plugin-boundary.md) — why `rwv` dispatches external subcommands without sandboxing
- [Run a command across repos](./run-a-command-across-repos.md) — `rwv each`, the plugin that ships with `rwv` for running a command across a project's repos
