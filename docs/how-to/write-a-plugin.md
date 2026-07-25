# How-to: Write a plugin

This walks through building `rwv-check-mid-op`, a small plugin that exits
non-zero and lists any repo with an operation in progress — useful as a
preflight guard before scripting a `sync` or `sync-to`. It touches every part
of the plugin contract along the way: reading the envelope, addressing back
into `rwv`, probing the JSON shape, and exit codes.

For the full contract behind each step — the exact envelope variables,
what "addressing" resolves to and why, exit-code semantics, the write
prohibition, the compatibility guarantee — see the
[plugin-protocol](../reference/plugin-protocol.md) reference. This page only
walks the happy path of using it.

## 1. Scaffold the executable

A plugin is any executable named `rwv-<verb>` on `$PATH`; `rwv <verb>` dispatches
to it whenever `<verb>` isn't a core verb. Start the script and make it
executable — it doesn't need to be on `$PATH` yet, that's the last step:

```sh
#!/usr/bin/env bash
# rwv-check-mid-op: exit non-zero if any repo has an operation in progress.
set -euo pipefail
```

```sh
chmod +x rwv-check-mid-op
```

## 2. Guard on being inside a workspace

`rwv` sets `$RWV_WORKSPACE` only when it resolved a workspace. Some plugins
legitimately run without one (`--help`, generators); `rwv-check-mid-op` isn't
one of them, so it checks and fails with an actionable message rather than
proceeding against absent context:

```sh
if [ -z "${RWV_WORKSPACE:-}" ]; then
  echo "rwv-check-mid-op: not inside a workspace" >&2
  exit 1
fi
```

## 3. Call back into `rwv` for data

The repo list already exists via `rwv status --json` — re-invoke `rwv` rather
than re-implementing project resolution. Address it explicitly with the
envelope values instead of relying on the plugin's own `cwd`, which may not
match where `rwv` itself was invoked from.

`$RWV_WORKSPACE` is always the **primary** workspace root, even when the
plugin is running inside a workweave — so on its own, `-C "$RWV_WORKSPACE"`
reaches primary, not "wherever this plugin happens to be." To stay inside the
*same* workweave the plugin was dispatched from, add `-w`:

```sh
if [ -n "${RWV_WORKWEAVE:-}" ]; then
  data=$(rwv -C "$RWV_WORKSPACE" -w "$RWV_WORKWEAVE" status --json)
else
  data=$(rwv -C "$RWV_WORKSPACE" --project "$RWV_PROJECT" status --json)
fi
```

`rwv-check-mid-op` wants to check wherever it was dispatched, so it branches
on `$RWV_WORKWEAVE` rather than always addressing primary.

## 4. Probe the shape, not the version

Before depending on a field, check it's actually there instead of assuming
based on `$RWV_VERSION` — a structural probe degrades gracefully on an older
`rwv`, where a version-number check produces false negatives on every newer
compatible release you didn't explicitly account for:

```sh
if [ "$(echo "$data" | jq '[.repos[] | has("mid_op")] | all')" != "true" ]; then
  echo "rwv-check-mid-op: requires an rwv with repos[].mid_op in status --json" >&2
  exit 1
fi
```

See [additive-schema guarantee](../reference/plugin-protocol.md#additive-schema-guarantee)
for why this is the preferred technique.

## 5. Do the work and set the exit code

```sh
stuck=$(echo "$data" | jq -r '.repos[] | select(.mid_op != null) | .absolute_path')
if [ -n "$stuck" ]; then
  echo "rwv-check-mid-op: repo(s) with an operation in progress:" >&2
  echo "$stuck" >&2
  exit 1
fi

exit 0
```

`rwv` propagates this exit code verbatim as its own, so `rwv check-mid-op &&
rwv sync-to` composes normally in a script.

## 6. Install and verify

Put the finished script anywhere on `$PATH` (`~/.local/bin/rwv-check-mid-op`,
for example) and confirm it's discoverable:

```sh
rwv --help            # lists it under "External commands"
rwv check-mid-op      # runs it
```

If two copies of the same name end up on `$PATH`, the first one found wins at
exec time and the rest are shadowed for audit in `rwv doctor --json` — see
[discovery and naming](../reference/plugin-protocol.md#discovery-and-naming)
in the reference.

## Next steps

- [plugin-protocol](../reference/plugin-protocol.md) — the full contract: every
  envelope variable, the two dispatch-failure shapes, the write prohibition,
  and the compatibility guarantee for `--json` output.
- [plugin-boundary](../explanation/joints/plugin-boundary.md) — the rationale for
  where the plugin/core line sits, and why `rwv` doesn't sandbox plugins.
- [run a command across repos](./run-a-command-across-repos.md) — packaging a
  plugin specifically for fanning a command out across every repo in a project.
