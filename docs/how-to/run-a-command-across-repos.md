# Run a command across repos

`rwv each` runs a command in every repo of the checkout you are in, in
parallel, and tells you which repos failed. Every way of installing `rwv`
installs it too ([installation](../reference/rwv-each.md#installation)).

```bash
rwv each -- git status --short
```

Each line of output is prefixed with the repo it came from:

```text
[github/chatly/server]  M src/main.rs
[github/chatly/web] ?? notes.txt
```

The command runs inside each repo's checkout, with its arguments passed as
given — no shell in between, so a path or an argument with spaces or `$` in it
arrives intact. The exit status is `0` only if the command succeeded in every
repo; otherwise a summary on stderr names each repo that failed and how.

## Choose the repos

With no selector, `rwv each` runs in every repo of the project except
reference repos — read-only material you study rather than change. Select with
`--role` and `--repo`, the same flags `rwv status`, `fetch`, `update` and
`push` take:

```bash
# Create a feature branch in every fork
rwv each --role fork -- git checkout -b feat/my-big-change

# Pull every repo you own
rwv each --role owned -- git pull

# One repo by path, a set by glob, a set by regex
rwv each --repo github/chatly/server -- cargo test
rwv each --repo 'glob:github/chatly/*' -- npm test
rwv each --repo 're:^github/(chatly|acme)/' -- git fetch --all
```

Repeated flags, and `--role` with `--repo`, combine as a union. A selector
replaces the default scope, so `--role reference` includes reference repos. A
selection that matches nothing is an error, not a silent no-op.

## Work in a workweave

`rwv each` runs in whichever checkout `rwv` resolved, so address a workweave
the way you address any verb:

```bash
rwv -w myproj--hotfix each -- git log --oneline -3
```

That runs in the workweave's checkouts, never the primary's.

## Use shell syntax

To use pipes, `&&`, globbing or variables, pass one string to `--shell`; it
runs through `sh -c` (Unix) or `cmd /C` (Windows). Each command sees
`RWV_REPO` (the manifest path), `RWV_REPO_ROLE` and `RWV_REPO_PATH`:

```bash
# Run tests only where there is a Makefile
rwv each --shell '[ -f Makefile ] && make test || echo "no Makefile in $RWV_REPO"'
```

## Control the run

```bash
rwv each -j 4 -- git fetch --all         # at most 4 repos at once (default: min(nproc, 8))
rwv each -j 1 -- git log -p              # one at a time, attached to the terminal: pagers and prompts work
rwv each --group -- cargo test           # hold each repo's output and print it whole when it finishes
rwv each --fail-fast -- cargo test       # start no new repo after the first failure
rwv each --timeout 10m -- cargo test     # kill a repo's whole process tree after 10 minutes
rwv each --dry-run -- git push           # show which repos would run what, and run nothing
```

Ctrl-C reaches every running command and its children, and no new repo starts.

## Keep the results

```bash
rwv each --json -- git rev-parse HEAD    # one record per repo, output captured
rwv each --joblog run.tsv -- make test   # a tab-separated line per repo as it finishes
rwv each --results out/ -- make test     # out/<repo>/stdout, stderr and exit, per repo
```

`--json` records carry each repo's status, exit code, duration and captured
output, so an agent or script never parses prefixed text.

The full contract — every flag, the record fields, exit codes and interrupt
behavior — is in the [`rwv each` reference](../reference/rwv-each.md).

## When you need something else

- **Your own packaged command.** `rwv each` is a plugin like any other.
  [Write a plugin](./write-a-plugin.md) when a recurring operation deserves its
  own verb; it gets the same workspace envelope `rwv each` uses.
- **Raw data.** `rwv status --json` lists each repo's path, role and lock
  state, for a script that needs more than a command per repo; its schema is in
  `rwv explain status`.
- **gita.** The opt-in [gita integration](../reference/integrations/gita.md)
  keeps `gita/repos.csv` and `gita/groups.csv` (grouped by role) in step with
  the manifest, for `gita ll owned` and gita's tabular display.
