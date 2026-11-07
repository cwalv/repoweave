# Upgrade to a new rwv on an existing weave

Before replacing the binaries, finish or abort any in-flight sync using the **old** binary; after upgrading, migrate workweave markers, manifests, and locks from the primary weave root:

```bash
# 1. Before upgrading (with the old rwv binary): clear any in-flight sync,
#    then repair until a weave-wide doctor run is clean
rwv status
rwv abort                           # if an operation is in progress
rwv doctor --fix --all
rwv doctor --all                    # a clean run records the health floor

# 2. After installing the new binaries (from the primary weave root):
rwv doctor --fix                    # migrate .rwv-workweave markers, reference worktrees and rwv.toml member keys
rwv lock                            # only if rwv.lock is in an old format: it is written afresh, pinning nothing (Step 4)
rwv branch set-mode main declared   # record the trunk; each workweave's branch is recorded from inside it (Step 6)
rwv update                          # record what this workspace aligns its members for; run it once in every workweave too (Step 7)
rwv materialize --remove-undeclared-links   # optional: unlink retired go.sum root symlink
```

An in-flight `.rwv-op` or `.rwv-op-lease` file from an older release has no cross-version migration path — resolve it with `--continue` or `rwv abort` on the old binary before upgrading.

For resting state on disk, installing a newer `rwv` does not touch existing files until a command reads them. Each file is migrated according to who owns it:

| File | Where it comes from | Remedy |
|---|---|---|
| `.rwv-op` / `.rwv-op-lease` | In-flight sync state | Finish or run `rwv abort` on the **old** binary before upgrading |
| `.rwv-workweave` & member slots | Written by `rwv` itself | `rwv doctor --fix` from the primary weave migrates them |
| `rwv.yaml` → `rwv.toml` | Hand-authored by you | Rewrite as `rwv.toml` and delete `rwv.yaml` (no `--fix` arm) |
| `version` and `submodules` keys in `rwv.toml` | Hand-authored by you | `rwv doctor --fix` renames them to `branch` and `recurse-submodules` and commits only that rename |
| `rwv.lock` | Generated output | `rwv lock` writes a file that does not parse afresh, pinning nothing; `rwv member pin` gives each member its pin again |
| What a workspace last aligned its members for | Written by `rwv` itself | `rwv update`, once in the weave root and once in each workweave |
| A weave branch with no record | Written by `rwv` itself | `rwv branch set-mode main declared` records the trunk; `rwv branch set-mode <project>--<name> feature --target main` records a workweave's branch |

## Install the new binaries

Every release channel installs two executables side by side: `rwv`, and `rwv-each`, the bundled [`rwv each`](../reference/rwv-each.md) plugin. An upgrade by hand from a release archive copies both into the same directory; copying `rwv` alone leaves `rwv each` an [unknown verb](../reference/refusals.md#unknown-verb). Releases before 0.25.0 also installed an executable named generate-explain beside `rwv`, which only maintains this project's own documentation; `rm` it.

If you wrote your own `rwv-each`, as earlier versions of [run a command across repos](./run-a-command-across-repos.md) showed at `~/.local/bin/rwv-each`, rename it to another verb before upgrading:

```bash
mv ~/.local/bin/rwv-each ~/.local/bin/rwv-myeach    # now run as: rwv myeach
```

What each channel does with a file you wrote at the path it installs `rwv-each` to:

| Channel | Your file |
|---|---|
| Install script | Refuses before installing anything, and names the file |
| `pip install`, `pip install --user` | **Replaced without a word** |
| Release archive, copied by hand | **Replaced**, as any copy over an existing file is |
| `uv tool install` | Refuses (`Executable already exists`) and links neither executable |
| `cargo install --git` | Refuses (`binary already exists in destination`) and installs neither executable |
| Homebrew | Installs into its own prefix, never `~/.local/bin`; `brew link` refuses a file already at its own target |

A script you keep in a directory earlier on `PATH` than the new install shadows the bundled plugin, so `rwv each` keeps running yours. `rwv doctor --json` lists every `rwv-each` it finds in its `plugins` array and marks each copy that loses `shadowed: true`.

## Migrate files from an older release

From inside a workweave, the marker is checked first, then the manifest, then the lock. Each step below quotes the refusal you will see and the command that clears it.

## Step 1 — a legacy workweave marker

If `.rwv-workweave` was written by an older `rwv`, every command run from inside that workweave refuses immediately, including `rwv doctor` itself:

```text
Error: <workweave>/.rwv-workweave is a legacy workweave marker (YAML format, or missing the required `parent:` field). Run `rwv doctor --fix` from the primary weave, not from inside this workweave: resolving the marker precedes the repair, so a self-invoked `--fix` hits this same refusal and changes nothing. The file is still readable YAML, so its own `primary:` names the weave to run from — `rwv doctor --fix -C <that path>` needs no `cd`.
```

Resolving which workspace the command is running in happens before `doctor`'s repair logic runs, so a self-invoked `--fix` inside the workweave cannot rewrite its own marker. Because the legacy marker is plain YAML, its `primary:` field is still readable:

```bash
cat .rwv-workweave        # primary: <path> — still readable, unmigrated
rwv doctor --fix -C <path from primary:>
```

`rwv doctor --fix` scans every workweave from the primary weave in one pass, migrating legacy `.rwv-workweave` markers to JSON, replacing any legacy symlinked `reference` member slots with read-only git worktrees, and settling file permissions.

## Step 2 — a legacy project manifest

A `rwv.yaml` with no `rwv.toml` beside it is refused at error severity, and there is no `--fix` arm for it. `rwv doctor` reports:

```text
[error: legacy-manifest-format] core: <project>: <dir>/rwv.yaml is a YAML manifest; rwv reads rwv.toml. Rewrite it as rwv.toml by hand and delete <dir>/rwv.yaml — rwv will not convert it, because the comments and key order you wrote cannot be carried across formats.
```

Rewrite the manifest by hand (see [File formats reference — `rwv.toml`](../reference/formats.md#rwvtoml--project-manifest) for the schema) and delete `rwv.yaml`.

## Step 3 — member-entry keys an earlier release wrote

An `rwv.toml` whose member entries name `version` for the tracked branch, or `submodules` for the submodule setting, does not load. Every verb that reads the manifest at the checked-out tip refuses with `unparseable-project`, and `rwv doctor` reports it as [`unparseable-project`](../reference/doctor-findings.md#unparseable-project) / `pre-schema`:

```text
[error: unparseable-project/pre-schema] core: <project>: failed to parse rwv.toml at <dir>/rwv.toml: rwv.toml names member-entry keys an earlier release wrote: `version` is now `branch` in <member>; `rwv doctor --fix` rewrites them and commits only that rewrite: TOML parse error at line 4, column 1
```

A verb that reads the manifest refuses instead, naming the same repair:

```text
rwv lock: projects/<project>: unparseable-project here
    <member>: `version` is now `branch`
choices:
  rewrite the keys an earlier release wrote, committing only that
    rwv doctor --fix

rwv explain unparseable-project
```

Run the repair from the project's checkout:

```bash
rwv doctor --fix
```

It renames `version` to `branch` and `submodules = "skip"` to `recurse-submodules = false`, and drops `submodules = "local"`, which is what an absent `recurse-submodules` means. Comments and key order stay. It makes one commit holding only that rename, and leaves every other uncommitted edit, to `rwv.toml` and to other files, where it was. It declines, changing nothing, while another file is staged or a rebase, merge or cherry-pick is in progress in the project repo; deal with that, then run it again.

A commit other than the checked-out one that still names the old keys declares nothing to the commands that read it, such as `rwv sync` reading the other workspace's `rwv.toml`. Its `rwv.lock`, whose format did not change, is still read, so its pins still count.

## Step 4 — a stale-format lock

With the manifest loading, a `rwv.lock` left in its pre-JSON shape fails to parse. `rwv doctor` reports:

```text
[error: unparseable-project/malformed] core: <project>: failed to parse rwv.lock at <dir>/rwv.lock: rwv.lock could not be parsed; restore it from version control, or run `rwv lock` to write it afresh — a lock that does not parse pins nothing, so each member then needs its pin back from `rwv member pin` or `rwv update --remote`: expected value at line 1 column 1
```

Write a fresh file, then pin each member at the commit its checkout is at. A lock that does not parse pins nothing, so `rwv lock` alone leaves every member without a pin:

```bash
rwv lock
rwv member pin <repo> HEAD    # once per member
```

`rwv doctor` (or `rwv status`) will now report the workspace clean.

## Step 5 — a leftover `go.sum` link on a Go weave

Earlier versions declared `go.sum` at the weave root of a Go weave instead of `go.work.sum`. After upgrading, `rwv doctor` reports the old `go.sum` symlink as a weave-root link at a name the project no longer declares. Remove the symlink with:

```bash
rwv materialize --remove-undeclared-links
```

That unlinks only the undeclared symlink without touching any target file.

## Step 6 — a weave branch with no record

Every weave branch has a record that says how it is used, kept on the project repo's records branch. A branch an earlier release made has none, and the verbs that align, lock, land or publish refuse it, naming `rwv fetch` (the record may be published but not yet fetched) and `rwv branch set-mode`:

```text
rwv lock: projects/<project>: target unrecorded (main)
choices:
  look for what may be published but not yet fetched
    rwv fetch
  say how this branch is used: feature work onto `<T>`, or declared
    rwv branch set-mode main feature --target <T>
    rwv branch set-mode main declared
```

Record the trunk, `main` or whatever branch the project repo is on in the primary weave, as `declared`, from the primary:

```bash
rwv branch set-mode main declared
```

Then record each existing workweave's branch, `<project>--<name>`, as a feature onto the branch it was made from, from inside that workweave:

```bash
rwv branch set-mode feature --target main
```

`rwv branch list` shows every weave branch with its record, and `unrecorded` for the ones still to do. `rwv init` and `rwv clone` record the trunk themselves, and `rwv workweave create` records the branch it makes, so a weave made by the new release has nothing to migrate.

## Step 7 — every seat runs `rwv update` once

Each workspace keeps the branch it last aligned its members for, in state of its own beside the project repo's. A workspace an earlier release made has none, so `rwv status` lists the project as `project unaligned` and the members as they stand, and nothing is moved. Run `rwv update` once in the weave root, and once from inside each workweave:

```bash
rwv update
```

It records the state, and puts each member where the checked-out project branch expects it: a `dependency` or `reference` member of a feature whose branch is checked out in another workspace detached at its pin, and any member on a branch other than the expected one back on it, where every commit it holds stays on some other ref. A member that would leave commits behind stays where it is and is reported with them and the commands that keep them. `rwv update` never writes `rwv.lock`. A workweave made by the new release records its state when it is created, so it needs no run.

A member of a workweave that still carries the branch shape an earlier release used, `<project>--<name>/<segment>`, makes `rwv workweave create` and `rwv update` refuse with `health-floor-too-low` and name the bridge version to step through, as the next section describes.

## The health floor and bridge versions

A release can drop the repair arms for on-disk states older than some version. Before it does, a weave has to record that it was clean under a version that still had them: that record is the health floor. A text-mode `rwv doctor --all` run with no `--kind` filter records it when the run is clean, meaning no error and nothing left that `rwv doctor --fix` would repair. `rwv doctor` without `--all`, with `--kind`, or with `--json` records nothing. The record names the rwv version that made the run, and a run by an older binary never lowers it.

A binary whose release dropped such arms refuses `rwv doctor` (`health-floor-too-low`, exit 1) when the weave's floor is below the version it requires, or when no floor was ever recorded, and names a bridge version:

```text
Error: this weave's health floor records version <recorded>, below the <minimum> this rwv requires: migratory repair arms for older states were removed in this release. Step through version <bridge>: install rwv <bridge>, run `rwv doctor --fix --all` until `rwv doctor --all` reports clean (which records the floor), then upgrade again
```

Follow it: install the bridge version, run `rwv doctor --fix --all` until `rwv doctor --all` reports clean, then install the newer rwv again. Running the pre-upgrade commands at the top of this page, which end with a clean `rwv doctor --all` under the old binary, keeps the floor current so the refusal does not arise.

## Related

- [File formats reference](../reference/formats.md) — `rwv.toml`, `rwv.lock`, and `.rwv-workweave` schemas
- [Doctor findings reference](../reference/doctor-findings.md) — `legacy-manifest-format`, `legacy-workweave-marker`, `workweave-drift`, and `unparseable-project`
- [Reconcile repos with the lock](./reconcile-repos.md) — verifying lock alignment once formats are migrated
