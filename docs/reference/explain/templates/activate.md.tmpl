# rwv activate

## Purpose

Set the active project, regenerate ecosystem workspace files in the project
directory, symlink them to the weave root, and run integration install hooks
(`npm install`, `uv sync`, `cargo generate-lockfile`, etc.).

`activate` is a **context verb**: it surfaces the existing on-disk artifacts
authored by prior intent verbs (`rwv add`, `rwv remove`, `rwv update`) and
wires up the workspace for use. It does **not** regenerate integration content
— the source of truth for generated files is the last intent verb that ran.

### `.rwv-active`

`.rwv-active` is the single source of truth for the active project. It is a
plain text file at the weave root containing the project name. CWD does not
override it. Every verb that operates "on the active project" reads this file.

`activate` writes `.rwv-active` as its last step, after all symlinks and
integration hooks have succeeded. If any integration hook fails (severity
`Error`), `.rwv-active` is not written and the workspace is left in the
previous state.

### Integration file generation and ownership

`activate` does not regenerate integration files — it surfaces them. The flow:

1. **Verification pass:** the integrations' `verify()` is called to detect
   drift between on-disk generated content and what the intent verb produced.
   Drift is reported as a warning (`warning (drift): ...`), not an error —
   context verbs never author content; the recovery hatch is `rwv doctor --fix`.
2. **Symlink removal:** any root-level symlinks from the previous activation
   that are owned by the integrations are removed (owner-scoped predicate:
   unlinked only if the symlink target resolves to
   `projects/<some-project>/<that-file>`).
3. **Symlink creation:** new symlinks are created at the weave root pointing
   to `projects/<project>/<file>` for each file in the active integrations'
   union of `generated_files()` and `managed_files()`.
4. **Install hooks:** integration install commands run against the now-in-place
   symlinks. Suppressed by `--no-install`.
5. **Write `.rwv-active`.**

### Workweave restriction

`rwv activate` refuses when CWD is inside a workweave. The project is fixed at
workweave-creation time (`rwv workweave <project> create <name>`); switching
projects inside a workweave would silently mutate primary's `.rwv-active` and
weave-root symlinks. Switch projects only from the primary weave.

## Invocation

```
rwv activate <project> [--no-install]
```

- `<project>` — project name to activate. Must have a corresponding directory
  at `projects/<project>/` with a valid `rwv.yaml`.
- `--no-install` — skip integration install hooks (`npm install`, `uv sync`,
  etc.) for a fast context-switch. Useful when you are only switching to a
  different project's editors and tools, and the install state is already
  correct.

Run `rwv --help activate` for the full clap surface.

## Output

Integration verification drift (if any) is reported as warnings on stderr:

```
[warning (drift)] <integration>: <message>
```

Install hook output goes to stderr. On success, `.rwv-active` is written (no
confirmation message by default).

## Exit codes

- `0` — project activated successfully.
- non-zero — workspace could not be resolved, manifest parse failure, called
  from inside a workweave, or one or more integration install hooks returned
  an error.

## Examples

Activate the `web-app` project:

```
rwv activate web-app
```

Activate without running install hooks (fast switch):

```
rwv activate web-app --no-install
```

Check what is currently active:

```
cat .rwv-active
```

## Common errors

- *rwv activate has no effect in a workweave* — called from inside a
  workweave. `cd` to the primary weave and rerun.
- *project directory not found* — `projects/<project>/` does not exist or
  has no valid `rwv.yaml`. Verify the project name.
- *integration activate-hook error* — an install command (`npm install`,
  `uv sync`, etc.) returned non-zero. Fix the integration issue, then rerun.
  `.rwv-active` is not written when any hook fails at error severity.
- *manifest parse failure* — `rwv.yaml` could not be loaded; verify the file
  is valid YAML.
