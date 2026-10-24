# rwv each

## Purpose

Run one command in each repo of the checkout `rwv` resolved, in parallel,
and report how every repo fared. `each` is a bundled plugin: an executable
named `rwv-each` that every release channel installs beside `rwv`, and that
`rwv` dispatches the way it dispatches any plugin. If `rwv each` is refused
as `unknown-verb`, `rwv-each` is not on `PATH`; put the one from the same
release next to `rwv`.

The full reference is `docs/reference/rwv-each.md` in the published book.

## Invocation

```
rwv [-C <path> | -w <project>--<name>] each [--role <role>]... [--repo <selector>]...
    [-j <N>] [--group] [--fail-fast] [--dry-run] [--json] [--timeout <duration>]
    [--joblog <file>] [--results <dir>] [--progress] [--] <command> [<args>...]
rwv [-C <path> | -w <project>--<name>] each [options] --shell '<command string>'
```

- Options come before the command. The first word that is not an option
  starts the command; everything after it is passed through unchanged.
- With neither `--role` nor `--repo`, every member repo except role
  `reference` runs. Either selector replaces that default scope, with the
  union semantics of `rwv status`, `fetch`, `update` and `push`. The project
  repo never runs.
- `-j <N>` runs up to `N` repos at once (default `min(nproc, 8)`). `-j 1`
  hands the terminal to each command in turn.
- `--shell` runs the string through `sh -c` (Unix) or `cmd /C` (Windows).
  Without it the command is executed directly, with no shell between.
- `--fail-fast` starts no new repo after the first failure.
- `--timeout <duration>` kills a repo's process tree after `<duration>`
  (`90`, `1m30s`, `2h`) and counts the repo as `timed-out`.
- `--dry-run` prints each selected repo and the command it would run, and
  runs nothing.
- Each command runs in the repo's checkout with `RWV_REPO`,
  `RWV_REPO_ROLE` and `RWV_REPO_PATH` set.

Run `rwv each --help` for the full clap surface.

## Output

Default text output streams each line prefixed `[<path>] `; `--group` holds
a repo's output and prints it whole under a `[<path>]` header. When any repo
did not succeed, a summary on stderr lists each one with its exit status.

Under `--json`, whatever `-j` is, output is one envelope once every repo is
done, outcomes sorted by repo path, each command's stdout and stderr captured
into its record. For a record of each repo as it finishes, use `--joblog` and
`--results`.

`status` is one of `ok`, `failed`, `timed-out`, `not-run`, `not-present`.

Schema:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "EachJsonOutput",
  "description": "The envelope `rwv each --json` prints once every repo is done, whatever `-j` is: `{ \"$schema\": \"<url>\", \"outcomes\": [<Outcome>, ...] }`, sorted by repo path.",
  "type": "object",
  "required": [
    "$schema",
    "outcomes"
  ],
  "properties": {
    "$schema": {
      "type": "string"
    },
    "outcomes": {
      "type": "array",
      "items": {
        "$ref": "#/definitions/Outcome"
      }
    }
  },
  "definitions": {
    "EachOutcomeStatus": {
      "oneOf": [
        {
          "description": "The command exited 0.",
          "type": "string",
          "enum": [
            "ok"
          ]
        },
        {
          "description": "The command exited non-zero, was ended by a signal, or could not be started.",
          "type": "string",
          "enum": [
            "failed"
          ]
        },
        {
          "description": "`--timeout` elapsed and the command's process tree was killed.",
          "type": "string",
          "enum": [
            "timed-out"
          ]
        },
        {
          "description": "Never started: `--fail-fast` or an interrupt stopped new repos first.",
          "type": "string",
          "enum": [
            "not-run"
          ]
        },
        {
          "description": "The repo's checkout is absent from disk; `rwv fetch` restores it.",
          "type": "string",
          "enum": [
            "not-present"
          ]
        }
      ]
    },
    "Outcome": {
      "description": "What happened in one repo.",
      "type": "object",
      "required": [
        "path",
        "role",
        "status",
        "stderr",
        "stdout"
      ],
      "properties": {
        "duration_ms": {
          "description": "Wall-clock run time. Absent when the command did not run.",
          "type": [
            "integer",
            "null"
          ],
          "format": "uint64",
          "minimum": 0.0
        },
        "exit_code": {
          "description": "The command's exit code. Absent when it did not run, could not be started, or was ended by a signal.",
          "type": [
            "integer",
            "null"
          ],
          "format": "int32"
        },
        "path": {
          "description": "The repo's manifest path.",
          "type": "string"
        },
        "role": {
          "$ref": "#/definitions/Role"
        },
        "signal": {
          "description": "The signal that ended the command (Unix only).",
          "type": [
            "integer",
            "null"
          ],
          "format": "int32"
        },
        "status": {
          "$ref": "#/definitions/EachOutcomeStatus"
        },
        "stderr": {
          "description": "The command's standard error, decoded lossily as UTF-8. Holds the reason when the command could not be started.",
          "type": "string"
        },
        "stdout": {
          "description": "The command's standard output, decoded lossily as UTF-8.",
          "type": "string"
        }
      }
    },
    "Role": {
      "description": "A repo's role.",
      "type": "string",
      "enum": [
        "owned",
        "fork",
        "dependency",
        "reference"
      ]
    }
  }
}
```

## Exit codes

- `0` — every selected repo's command exited 0.
- `1` — any repo did not succeed, nothing matched the selectors, or the
  selection could not be resolved.
- `2` — usage error.
- `130` — interrupted.

A command's own exit code is reported in the summary and under `--json`,
never propagated.

## Examples

Run the tests in every owned repo, four at a time:

```
rwv each --role owned -j 4 -- cargo test
```

List the repos whose command failed:

```
rwv each --json -j 1 -- make lint | jq -r '.outcomes[] | select(.status != "ok") | .path'
```
