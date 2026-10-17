# rwv activate

## Purpose

Set the active project, symlink its ecosystem workspace files to the weave
root, and run integration install hooks (`npm install`, `uv sync`,
`cargo fetch`, etc.). The hooks materialize what current membership implies;
they never move a version an existing lock file pins.

`activate` **never authors integration content**: it surfaces the files
already committed in the project directory and wires up the workspace for
use. Regeneration belongs to the verbs that change what those files are
generated from; see [file-ownership](../../explanation/joints/file-ownership.md).
The one write it makes into them is a setting whose right value is a fact
about this machine rather than about `rwv.toml` — cargo-workspace's
`rustc-wrapper` key, present exactly where the wrapper is on `PATH` — which
every materialization brings to this host's answer.

### `.rwv-active`

`.rwv-active` is the single source of truth for the active project. It is a
plain text file at the weave root containing the project name. CWD does not
override it. Every verb that operates "on the active project" reads this file;
one that writes into its project refuses (`cwd-in-inactive-project`) when run
from inside a different project's `projects/<name>/`, naming `--project`.

`activate` writes `.rwv-active` as its last step, after all symlinks and
integration hooks have succeeded. If any integration hook fails (severity
`Error`), `.rwv-active` is not written and the workspace is left in the
previous state.

### Integration file generation and ownership

`activate` does not regenerate integration files — it surfaces them. Unless
`--no-materialize` is passed, the host-dependent settings are brought to this
host's answer first, so the verification pass does not report as drift what
this run settles. Then the flow:

1. **Verification pass:** the integrations' `verify()` is called to detect
   drift between on-disk generated content and what they would produce now
   (Axis-2 content drift). Drift is reported as a warning
   (`warning (drift): ...`), not an error — `activate` never authors
   content; the recovery hatch is `rwv doctor --fix`.
2. **Symlink removal:** any root-level symlinks from the previous activation
   that are owned by the integrations are removed (owner-scoped predicate:
   unlinked only if the symlink target resolves to
   `projects/<some-project>/<that-file>`). Before anything is removed, every
   path a link is wanted at is read: a real file, a directory, or a link of
   any other shape standing there refuses with every such path named and no
   link changed (`surfacing-path-occupied`). A link that already points where
   it should is left in place. A real file at a lock file's path is not
   refused: it is the tool's write that missed its link. `activate` names it
   ("not yet taken into …; run `rwv doctor --fix`") and leaves it, with its
   link left out — except the outgoing project's on a switch, which is taken
   into that project first. If that project's own copy differs and neither
   version control nor rwv's record holds it, the switch is refused with
   nothing changed (`conflicting-root-write`). `add`, `remove`, `update`,
   `materialize` and `rwv doctor --fix` take it in and restore the link.
3. **Symlink creation:** new symlinks are created at the weave root pointing
   to `projects/<project>/<file>` for each file in the active integrations'
   union of `generated_files()` and `managed_files()` (Axis-1 surfacing). If
   one cannot be created, every link steps 2 and 3 had removed or created is
   put back before the error is reported.
4. **Install hooks:** integration install commands run against the now-in-place
   symlinks. A hook whose tool is not on `PATH` is skipped with a
   `tool-missing` warning and the other hooks still run; a hook that runs and
   fails is an error. Suppressed by `--no-materialize`, and withheld while a generated
   file rwv attests holds content it never accepted — a hook re-runs its
   generator and records what it produces as accepted, which would answer that
   fork on the operator's behalf. Settle it with `rwv materialize
   --adopt-drifted` or `rwv materialize --regenerate-drifted`, then rerun.
5. **Write `.rwv-active`.**

The per-integration `verify()` pass (step 1) covers Axis-2 content drift
only — it does not assert that the surfacing symlinks themselves are present
and resolve correctly. The framework-level **Axis-1 surfacing check** is a
separate pass in `rwv doctor`: it asserts every file in the
`generated_files() ∪ managed_files()` union has a valid symlink at the weave
root. This distinction matters inside a workweave: `rwv activate` is refused
there (the project is fixed at creation), but `rwv doctor --fix` re-runs the
step-2 surfacing primitive (`surface_symlinks`) bound to the workweave
directory — it creates missing or mis-resolved symlinks without re-selecting
the project, so it is safe and valid in a workweave. Use `rwv doctor --fix`
as the in-workweave recourse for any surfacing-symlink drift (e.g. a file
added to the manifest after the workweave was created, or a manual `rm` of a
surfaced symlink).

### Workweave restriction

`rwv activate` refuses when CWD is inside a workweave. The project is fixed at
workweave-creation time (`rwv workweave <project> create <name>`); switching
projects inside a workweave would silently mutate primary's `.rwv-active` and
weave-root symlinks. Switch projects only from the primary weave.

## Invocation

```
rwv activate <project> [--no-materialize]
```

- `<project>` — project name to activate. Must have a corresponding directory
  at `projects/<project>/` with a valid `rwv.toml`.
- `--no-materialize` — skip integration install hooks (`npm install`, `uv sync`,
  etc.) for a fast context-switch. Useful when you are only switching to a
  different project's editors and tools, and the install state is already
  correct. `rwv materialize` runs them later, at primary or inside a
  workweave.

Run `rwv --help activate` for the full clap surface.

## Output

Integration verification drift (if any) is reported as warnings on stderr:

```
[warning (drift)] <integration>: <message>
```

Install hook output goes to stderr, the tools' own stdout included. A hook
skipped for a missing tool prints:

```
[warning] <integration>: <tool> is not on PATH, so this integration's install hook did not run; install <tool>, then run `rwv materialize`
```

With `--no-materialize`, the skipped hooks are said in one line naming the verb
that runs them:

```
[skipped] core: the install hooks did not run (--no-materialize); `rwv materialize` runs them
```

On success, `.rwv-active` is written (no confirmation message by default).

## Exit codes

- `0` — project activated successfully.
- non-zero — workspace could not be resolved, manifest parse failure, called
  from inside a workweave, or one or more integration install hooks returned
  an error. A hook skipped because its tool is not on `PATH` is a warning,
  not an error.

## Examples

Activate the `web-app` project:

```
rwv activate web-app
```

Activate without running install hooks (fast switch):

```
rwv activate web-app --no-materialize
```

Check what is currently active:

```
cat .rwv-active
```

## Common errors

- *surfacing a project and running its install hooks does not start while an
  operation is in flight in this workspace* — another sync or sync-to already
  holds op-state covering this workspace. See `rwv explain op-in-progress`
  for what unblocks it.
- *rwv activate has no effect in a workweave* — called from inside a
  workweave. The project is fixed at creation time; switching is not
  supported in a workweave. `cd` to the primary weave and rerun. If you
  need to repair missing or mis-resolved surfacing symlinks inside the
  workweave, use `rwv doctor --fix` instead — it re-runs the surfacing
  primitive scoped to the workweave directory without re-selecting the
  project.
- *project directory not found* — `projects/<project>/` does not exist or
  has no valid `rwv.toml`. Verify the project name.
- *integration activate-hook error* — an install command (`npm install`,
  `uv sync`, etc.) returned non-zero. Fix the integration issue, then rerun.
  `.rwv-active` is not written when any hook fails at error severity.
- *manifest parse failure* — `rwv.toml` could not be loaded; verify the file
  is valid TOML.
