# rwv init

## Purpose

Create a new project in the workspace, or adopt an existing repo as a project.
`init` is the day-0 verb: it bootstraps the workspace (if needed), creates
`projects/<name>/`, writes a skeletal `rwv.toml`, authors the integration
files it implies, commits exactly the files it wrote as the project's first
commit, and auto-activates the project. It records the branch the
project repo is on as `declared` (`rwv branch list` shows it); `init --adopt`
first merges in the records the adopted repo's remote holds, so a branch someone
already recorded keeps its record.

`rwv init <name>` authors what an empty manifest implies — with the default
integrations, `<name>.code-workspace` — before it activates, so a new project
starts with nothing for `rwv doctor` to report. `init --adopt` **never authors
integration content**: it clones only the project repo, and its members are
not on disk until `rwv update`, so generating at that point would overwrite
the adopted project's committed workspace files from an empty member set. To
regenerate an adopted project, run `rwv update`, then `rwv doctor --fix`.

### Empty-directory bootstrap

When invoked in an empty directory that has no workspace markers, `rwv init`
creates the minimal workspace skeleton (`projects/`) before proceeding. This
makes the standard day-0 flow work immediately:

```
mkdir my-ws && cd my-ws
rwv init my-project        # bootstraps projects/ and initializes the project
rwv member add <url>              # works — workspace context resolves
```

Running `rwv init` in a non-empty, non-workspace directory is refused with a
clear error naming the state and the corrective action.

If CWD is already a workspace, `init` skips the bootstrap and creates the
project directory inside the existing workspace.

### New-project form (`rwv init <name>`)

1. Validates `<name>` against rwv's naming rules, before anything touches
   disk.
2. Bootstraps the workspace skeleton if CWD is empty.
3. Resolves the weave root from CWD.
4. Creates `projects/<name>/`.
5. Runs `git init` in the new directory.
6. Writes a skeletal `rwv.toml` (a bare `[repositories]` table).
7. Configures replay-exclusion for `rwv.lock` (`.gitattributes`) and
   plants the durable `merge.rwv-ours.*` git config so `rwv sync` rebases
   correctly from the first commit.
8. If `--provider` is given, adds a git remote using the registry's clone URL
   pattern and records it as the project repo's url remote, the remote `rwv
   push` publishes the project repo to.
9. Authors the integration files the empty manifest implies into
   `projects/<name>/`.
10. Commits `rwv.toml`, `.gitattributes` and the authored integration files
    as the project repo's first commit, and nothing else. It does not push.
11. Auto-activates the project (writes `.rwv-active`, surfaces symlinks).

Before anything is written, `init` refuses when no commit author is
configured, naming the setting. A commit refused after the files are written
(a commit hook that declines it) names the files left uncommitted, and the
project is not activated.

### Adopt form (`rwv init <source> --adopt`)

Clones an existing repo as a project instead of creating a new one. `source`
is a full clone URL (`https://`, `git@`, `file://`) or a shorthand naming its
registry (`<registry>/<owner>/<repo>`, such as `github/myorg/my-service`). A
two-segment `<owner>/<repo>` names no registry and is refused
(`unresolvable-repo-source`). The cloned repo is placed at `projects/<name>/`
where `<name>` is the repo name derived from the source.

If the cloned repo does not already contain `rwv.toml`, a skeletal one is
written. Replay-exclusion and merge driver config are applied (idempotent if
already present). Whichever of `rwv.toml` and `.gitattributes` this changed is
committed, and nothing else: one more commit on the adopted history, or the
repo's first commit when it had none. A repo that already carries both gets no
commit. The project is auto-activated after cloning.

## Invocation

```
rwv init <name> [--provider <registry/owner>]
rwv init <source> --adopt [--no-materialize]
```

- `<name>` — project name. Creates `projects/<name>/` with `git init` and a
  skeletal `rwv.toml`. Mutually exclusive with `--adopt`.
- `--provider <registry/owner>` — configure a git remote for the new project
  repo. Format: `<registry>/<owner>` (e.g., `github/myorg`). The remote URL
  is derived from the named registry. Known registries: `github`, `gitlab`,
  `bitbucket`.
- `--adopt` — adopt an existing repo. `<source>` must be a clone URL or a
  `<registry>/<owner>/<repo>` shorthand. The project name is derived from the
  repository name segment of the source.
- `--no-materialize` — with `--adopt`, skip the integration install hooks of
  the adopted project's activation (they run for any member whose clone the
  weave already holds); `rwv materialize` runs them later. Refused without
  `--adopt`: a new project has no members for a hook to act on.

Run `rwv init --help` for the full clap surface.

## Output

Progress messages to stderr:

- `Bootstrapped workspace at <path> (created projects/)` — emitted when an
  empty directory is converted into a workspace.
- `Initialized project '<name>' at <path>` — project created successfully.
- `Committed <files>` — the files `init` wrote, committed.
- `Cloning <url> into <path>` — emitted by `--adopt` before the clone.
- `Adopted project '<name>' at <path>` — `--adopt` completed successfully.

Integration activation output follows on stderr (symlinks created/updated).
With `--adopt --no-materialize` it ends with `[skipped] core: the install hooks
did not run (--no-materialize); `rwv materialize` runs them`, and a hook whose
tool is not on `PATH` is skipped with its `tool-missing` warning.

## Exit codes

- `0` — project created (or adopted), committed and activated successfully.
- non-zero — CWD is a non-empty non-workspace directory; project already
  exists at `projects/<name>/`; the provider registry is unknown; no commit
  author is configured; the clone or the commit failed; or activation returned
  an error.

## Examples

Bootstrap a fresh workspace and create the first project:

```
mkdir my-ws && cd my-ws
rwv init my-project
```

Create a project in an existing workspace:

```
rwv init another-project
```

Create a project and configure a GitHub remote:

```
rwv init my-lib --provider github/myorg
```

Adopt an existing GitHub repo as a project:

```
rwv init https://github.com/myorg/my-service --adopt
```

Adopt using a shorthand:

```
rwv init github/myorg/my-service --adopt
```

## Common errors

- *invalid project name* — `<name>` fails rwv's naming rules: an ambiguous
  `--` delimiter or an embedded `+` are `unrenderable-name`; anything else git
  won't accept as a ref component is `invalid-ref-name`. Refused before
  anything touches disk — see the token `rwv explain` prints for the exit.
- *`rwv init` requires either an existing workspace or an empty directory* —
  CWD contains files but is not a repoweave workspace. Run `rwv init` in a
  fresh empty directory, or `cd` into an existing workspace.
- *project '<name>' already exists at projects/<name>/* — a project directory
  with this name already exists. Choose a different name or remove the
  existing directory. A second `init` of the same name changes and commits
  nothing.
- *no author is configured for that commit* — `init` commits what it writes;
  configure the author name and email it prints, then run it again. Nothing
  was written.
- *'<source>' is neither a URL nor a registry shorthand*
  (`unresolvable-repo-source`, with `--adopt`) — spell the shorthand with its
  registry (`github/<owner>/<repo>`) or a local path as a `file://` URL.
- *invalid --provider format '...', expected 'registry/owner'* — the
  `--provider` argument must be `<registry>/<owner>` (e.g., `github/myorg`).
- *unknown registry '...'* — the registry name is not one of `github`,
  `gitlab`, `bitbucket`.
- *clone failed* (with `--adopt`) — network error or authentication problem;
  inspect git output.
