# How-to: Run a command across multiple repositories

There are three tiers of approach, ordered by installation cost and ergonomics. Pick
the tier that matches your situation.

## Tier 1 — Shell composition (zero install)

The foundation is `rwv status --json`, which emits the absolute path, role, and URL
of every repository in the project. Pipe that into `jq`, `xargs`, or `parallel` to
build any bulk operation you need.

```bash
# Run git fetch in every repo
rwv status --json | jq -r '.repos[].absolute_path' | xargs -I {} git -C {} fetch --all

# Pull every repo you own
rwv status --json | jq -r '.repos[] | select(.role == "owned") | .absolute_path' | xargs -I {} git -C {} pull

# Create a feature branch in every fork simultaneously
rwv status --json | jq -r '.repos[] | select(.role == "fork") | .absolute_path' | xargs -I {} git -C {} checkout -b feat/my-big-change

# Run tests in repos that have a Makefile
rwv status --json | jq -r '.repos[] | .absolute_path' | while read path; do
  if [ -f "$path/Makefile" ]; then
    echo "--- Testing $path ---"
    make -C "$path" test
  fi
done
```

For large projects, run in parallel:

```bash
# Fetch all repositories, 4 at a time
rwv status --json | jq -r '.repos[] | .absolute_path' | xargs -P 4 -I {} git -C {} fetch --all
```

The `$schema` field in the output (`rwv explain status` embeds the full JSON Schema)
tells you exactly what fields are available for filtering. Tier-1 composition can
express anything a finite verb set could cover, plus everything it couldn't.

**Why this is the right default:** the shell already has parallelism (`xargs -P`,
GNU `parallel`), filtering (`jq`), and error handling. `rwv` provides the metadata
source; the shell provides the runner. There is no reason to encode "run this command
across all repos" as a core verb — see the [verb-vs-composition](../explanation/joints/verb-vs-composition.md)
joint for the principle.

## Tier 2 — PATH plugin (packaged ergonomics)

If you find yourself writing the same `jq` pipeline repeatedly, package it as a
`rwv-<verb>` executable on `$PATH`. `rwv` will dispatch to it when you invoke
`rwv <verb>`, hand it the resolved workspace context through the environment envelope,
and propagate its exit code verbatim.

A minimal plugin that runs a command across repos, using the envelope:

```sh
#!/usr/bin/env bash
# rwv-each: run a command in every repo
# Usage: rwv each <cmd> [args...]
set -euo pipefail

if [ -z "${RWV_WORKSPACE:-}" ]; then
  echo "rwv-each: not inside a workspace" >&2
  exit 1
fi

cmd=("$@")
rwv -C "$RWV_WORKSPACE" --project "$RWV_PROJECT" status --json \
  | jq -r '.repos[].absolute_path' \
  | xargs -I {} bash -c '"${cmd[@]}"' _ {}
```

Install it anywhere on `$PATH` (e.g. `~/.local/bin/rwv-each`, then `chmod +x`).
After that, `rwv each git status` does what you expect, and `rwv --help` lists it
under "External commands."

The plugin inherits `rwv`'s addressing flags: `rwv -w foundations--fo-x7 each git status`
addresses the workweave once, in `rwv`, and the plugin receives the resolved
coordinates through `$RWV_WORKSPACE` and `$RWV_PROJECT`.

See [write-a-plugin](./write-a-plugin.md) for the full plugin-author guide, including
the envelope table, back-addressing conventions, and schema probing.

## Tier 3 — gita integration (lifecycle-managed CSV)

The [gita integration](../reference/integrations/gita.md) is an `rwv`-managed
integration that maintains a gita group CSV file (`.gita/groups`) in the project
repo, keeping it in sync with the `rwv.yaml` manifest. This lets you use `gita
super primary <cmd>` for cross-repo command dispatch with gita's summary output.

The gita integration is **opt-in**; see the integration docs for how to enable it.
It is the right choice when:

- You want gita's summary output (per-repo status glyphs, colorized column display).
- You are already using gita for other purposes and want the groups file managed
  automatically rather than hand-maintained.
- Lifecycle management matters: the CSV stays consistent when repos are added or
  removed via `rwv add` / `rwv remove`.

The gita integration does not replace tier-1 or tier-2 approaches. A `rwv-each`-style
plugin and the gita integration can coexist; they address different needs. The
integration does not prohibit a plugin from providing similar UX — that coexistence
is a positive test that the plugin boundary is correctly drawn.
