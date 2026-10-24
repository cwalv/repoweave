# rwv each

`rwv each` runs one command in each repo of the checkout `rwv` resolved, in parallel, and reports how every repo fared. It is a plugin — an executable named `rwv-each` that `rwv` dispatches like any other ([plugin protocol](./plugin-protocol.md)) — that ships with `rwv` in every release channel. For a guided tour, see [run a command across repos](../how-to/run-a-command-across-repos.md).

## Installation

Every release channel installs `rwv-each` in the same directory as `rwv`: the install script, the release archives, Homebrew, `pip`/`uv` (including a one-off `uvx`), and `cargo install --git https://github.com/cwalv/repoweave`. It is the same version as the `rwv` beside it.

`rwv` finds `rwv-each` the way it finds any plugin — on `PATH` — so if `rwv each` is refused as an [unknown verb](./refusals.md#unknown-verb), `rwv-each` is not on `PATH`: the directory it was installed to is missing from `PATH`, or `rwv` came from somewhere that installs it alone (a single binary copied out of an archive, `cargo install --git https://github.com/cwalv/repoweave --bin rwv`). Put the `rwv-each` from the same release on `PATH`, next to `rwv`.

## Synopsis

```text
rwv [-C PATH | -w PROJECT--NAME] each [--role ROLE]... [--repo SEL]...
                                      [-j N] [--group] [--fail-fast] [--dry-run] [--json]
                                      [--timeout DURATION] [--joblog FILE] [--results DIR]
                                      [--progress]
                                      [--] COMMAND [ARGS...]
rwv [-C PATH | -w PROJECT--NAME] each [options] --shell 'COMMAND STRING'
```

Options come before the command. The first word that is not an option starts the command, and everything after it is passed to the command unchanged; `--` is accepted but never required. `rwv each` with no command is a usage error (exit 2).

## Which checkout, which repos

`rwv each` has no addressing flags of its own. Address it the way you address any `rwv` verb — `rwv -w myproj--hotfix each …` runs in that workweave's checkouts, never in the primary's. A relative `--joblog FILE` or `--results DIR` resolves against the directory `rwv` was invoked from, or the `-C`/`-w` directory, as a relative path argument to any verb does.

| Flag | Selects |
|---|---|
| *(neither flag)* | Every member repo except role `reference` |
| `--role ROLE` | Repos with that role; repeatable |
| `--repo SEL` | Repos whose manifest path matches: bare = exact, `re:<pat>`, `glob:<pat>`; repeatable |

`--role` and `--repo` are the [selector grammar](./cli.md#selector-grammar) of `rwv status`, `fetch`, `update` and `push`, with the same union rule. A selector *replaces* the default scope rather than narrowing it: `--role reference` and a `--repo` naming a reference repo both include it. The project repo is not a manifest member and is never run in.

A selection that matches no repo is an error naming the selectors that matched nothing, and nothing runs.

## Running

| Flag | Effect |
|---|---|
| `-j N` | Repos run at once. Default `min(nproc, 8)`. `-j 1` runs one repo at a time |
| `--shell STRING` | Run STRING with `sh -c` (Unix) or `cmd /C` (Windows). Without it the command is executed directly and every argument is passed as a value — no shell, no word splitting, no expansion |
| `--fail-fast` | Start no new repo after the first one that does not succeed; repos already running finish. Repos never started are reported `not run`. Without it every selected repo runs |
| `--timeout DURATION` | Kill a repo's whole process tree once its command has run this long; the repo counts as failed (`timed-out`). DURATION is seconds, or numbers suffixed `s`, `m`, `h`, `d` (`90`, `1m30s`, `2h`). Unbounded by default |
| `--dry-run` | Print each selected repo and the command that would run there, [quoted for reading](#how-a-command-is-shown); run nothing; exit 0 |

The command runs with the repo's checkout as its working directory. Each command gets these variables on top of the environment `rwv each` itself received:

| Variable | Value |
|---|---|
| `RWV_REPO` | The repo's manifest path, e.g. `github/org/app` |
| `RWV_REPO_ROLE` | The repo's role |
| `RWV_REPO_PATH` | The absolute path of the repo's checkout |

A member repo whose checkout is absent from disk is not run; it is reported as not present and counts as a failure — `rwv fetch` restores it.

A bare command name is looked up on `PATH` once, before any repo runs, so every repo runs the same program and a typo fails once rather than once per repo. A relative `PATH` entry is read from the directory `rwv` was invoked from, or the `-C`/`-w` directory, never from a repo's checkout. A command spelled with a directory (`./scripts/test.sh`) is resolved inside each repo.

## Output

| Mode | What you see |
|---|---|
| `-j 1` | A `[<path>]` header line, then the command attached directly to this terminal: colors, prompts and pagers work, and it reads this process's stdin |
| `-j N` (`N > 1`) | Every line as it arrives, prefixed `[<path>] `, stdout to stdout and stderr to stderr. Lines are written whole: two repos' output never interleaves within a line |
| `--group` | Each repo's output held until it finishes, then printed whole under a `[<path>]` header |
| `--json` | Each command's stdout and stderr captured into its record instead of printed (below) |

Except under `-j 1` with the terminal attached, commands get no stdin. Under `-j 1`, `--results` and `--progress` also detach the terminal: output is read line by line, under the header, unprefixed.

When anything did not succeed, a summary on stderr lists every such repo, sorted by repo path, with its exit status (`exit 3`, `killed by signal 9`, `failed to run`, `timed out`, `not present on disk`, `not run`). `failed to run` is a command that could not be started. A clean run prints no summary.

`--progress` keeps a one-line count — done, running, failed, remaining, and which repos are running — at the bottom of stderr, erased before each output line and redrawn after it. When stderr is not a terminal it prints nothing.

## Records and logs

### `--json`

With `-j 1`, one envelope once every repo is done, outcomes sorted by repo path:

```json
{ "$schema": "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/each.json",
  "outcomes": [ { "path": "github/org/app", "role": "owned", "status": "ok", "exit_code": 0,
                  "signal": null, "duration_ms": 1234, "stdout": "…", "stderr": "" } ] }
```

With `-j N`, `N > 1`, one record per line as each repo finishes (repos never started come last), each carrying its own `$schema` (`each-record.json`). Output is emitted even when repos fail.

`status` is one of `ok`, `failed` (non-zero exit, killed by a signal, or could not be started), `timed-out`, `not-run`, `not-present`. `exit_code` and `signal` are `null` when they do not apply; `signal` is never set on Windows. The schemas are committed at `docs/reference/schemas/each.json` and `each-record.json`.

### `--joblog FILE`

One tab-separated line per repo, written as each finishes, after a header row. The columns follow GNU parallel's joblog, with `Repo` in place of `Host` and no byte counts:

```text
Seq	Repo	Starttime	JobRuntime	Exitval	Signal	Command
```

`Seq` is the repo's position in the selection sorted by repo path, from 1. `Starttime` is Unix seconds and `JobRuntime` seconds, both with millisecond precision. `Exitval` is `-1` when the command left no exit code. Repos that never started have no line. FILE is replaced if it exists. The joblog cannot go to stdout, which carries the commands' output: `--joblog -` is refused, and `--joblog ./-` names a file called `-`. The same holds for `--results -`.

### How a command is shown

A `--dry-run` line and the joblog's `Command` column show the command for reading, quoted the way POSIX `sh` reads words on every platform, Windows included: a word a shell would split or expand is wrapped in single quotes, and a `'` inside it is written `'\''`. `rwv each ./build.sh 'two words' '$HOME'` shows as `./build.sh 'two words' '$HOME'`. The program is shown as you spelled it, not as the path it was found at. A `--shell` STRING is shown exactly as given.

### `--results DIR`

Each repo's output is also written to `DIR/<path>/stdout` and `DIR/<path>/stderr`, and its outcome to `DIR/<path>/exit`: `ok`, the exit code, `signal N`, `timed-out`, or `failed`. Terminal output is unchanged. A repo's existing files are replaced; repos not run in this invocation are left alone.

## Exit status and interrupts

| Exit | When |
|---|---|
| `0` | Every selected repo's command exited 0 |
| `1` | Any repo did not succeed, or nothing matched, or the selection could not be resolved |
| `2` | Usage error |
| `130` | Interrupted |

A command's own exit code is reported in the summary and in `--json`, not propagated: N exit codes do not fold into one.

Ctrl-C reaches every running command's whole process tree — its process group on Unix, its job object on Windows — and no new repo starts. On Unix the first Ctrl-C delivers an interrupt each command can handle; a command that has exited has whatever it left running ended; a second Ctrl-C kills every tree outright. On Windows each tree is ended at the first. `rwv each` exits 130 once its commands have exited.

If `rwv each` itself is killed, on Windows every command it started is ended with it. A command that leaves something running after a normal exit keeps it.
