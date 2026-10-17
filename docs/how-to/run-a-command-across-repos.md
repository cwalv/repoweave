# Run a command across repos

Pipe `rwv status --json` into `jq` and `xargs` to filter member repositories by role or path and run any command across them:

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

For large projects, add `-P <N>` to run in parallel:

```bash
# Fetch all repositories, 4 at a time
rwv status --json | jq -r '.repos[] | .absolute_path' | xargs -P 4 -I {} git -C {} fetch --all
```

`absolute_path` is emitted forward-separated on every platform, so these pipelines carry no byte that bare `xargs` interprets as an escape. If `git` reports `cannot change to '<path>?'` on Windows, a `jq` build is emitting CRLF line endings; insert `tr -d '\r'` between `jq` and `xargs`.

Run `rwv explain status` to inspect the full JSON Schema of fields available for filtering.

## Package a recurring pipeline as a PATH plugin

If you run the same pipeline repeatedly, save it as an `rwv-<verb>` executable on `$PATH`. `rwv <verb>` dispatches to it, passes the resolved workspace context in environment variables, and propagates its exit code:

```sh
#!/usr/bin/env bash
# rwv-each: run a command in every repo
# Usage: rwv each <cmd> [args...]
set -euo pipefail

if [ -z "${RWV_WORKSPACE:-}" ]; then
  echo "rwv-each: not inside a workspace" >&2
  exit 1
fi

if [ -n "${RWV_WORKWEAVE:-}" ]; then
  paths=$(rwv -C "$RWV_WORKSPACE" -w "$RWV_WORKWEAVE" status --json | jq -r '.repos[].absolute_path')
else
  paths=$(rwv -C "$RWV_WORKSPACE" status --project "$RWV_PROJECT" --json | jq -r '.repos[].absolute_path')
fi
while IFS= read -r path; do
  (cd "$path" && "$@") < /dev/null
done <<< "$paths"
```

Install the script on `$PATH` (for example `~/.local/bin/rwv-each` with `chmod +x`). Invoking `rwv each git status` or `rwv -w myproj--hotfix each git status` then runs the command across every member repo of the addressed workspace, stopping at the first repo where it fails and exiting with its status, and `rwv --help` lists `each` under "External commands."

## Optional: `gita` integration

The opt-in [gita integration](../reference/integrations/gita.md) maintains `gita/repos.csv` and `gita/groups.csv` (grouped by role, such as `owned` and `fork`) from `rwv.toml`. Enable it when you want `gita ll owned` or `gita super owned <cmd>` with gita's tabular output:

```toml
[integrations.gita]
enabled = true
```

`rwv doctor --fix` writes both files and surfaces `gita/` at the weave root. gita reads its configuration from `$GITA_PROJECT_HOME/gita/`, so point `GITA_PROJECT_HOME` at the weave root and run gita from there:

```bash
rwv doctor --fix
export GITA_PROJECT_HOME="$PWD"
gita ll owned
gita super owned status --short
```

## Related

- [Verb vs. composition](../explanation/joints/verb-vs-composition.md) — why ad-hoc cross-repo execution composes with `jq` and `xargs`
- [Write a plugin](./write-a-plugin.md) — step-by-step walkthrough for building an `rwv-<verb>` plugin
- [Plugin protocol reference](../reference/plugin-protocol.md) — environment envelope, addressing, and exit code contract
- [Plugin boundary](../explanation/joints/plugin-boundary.md) — where core verbs end and PATH plugins begin
- [Gita integration reference](../reference/integrations/gita.md) — `gita/repos.csv` and `gita/groups.csv` layout
