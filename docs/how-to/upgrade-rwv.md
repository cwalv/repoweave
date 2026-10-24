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
rwv doctor --fix                    # migrate .rwv-workweave markers and reference worktrees
rwv lock                            # regenerate rwv.lock in the current JSON format
rwv materialize --remove-undeclared-links   # optional: unlink retired go.sum root symlink
```

An in-flight `.rwv-op` or `.rwv-op-lease` file from an older release has no cross-version migration path — resolve it with `--continue` or `rwv abort` on the old binary before upgrading.

For resting state on disk, installing a newer `rwv` does not touch existing files until a command reads them. Each file is migrated according to who owns it:

| File | Where it comes from | Remedy |
|---|---|---|
| `.rwv-op` / `.rwv-op-lease` | In-flight sync state | Finish or run `rwv abort` on the **old** binary before upgrading |
| `.rwv-workweave` & member slots | Written by `rwv` itself | `rwv doctor --fix` from the primary weave migrates them |
| `rwv.yaml` → `rwv.toml` | Hand-authored by you | Rewrite as `rwv.toml` and delete `rwv.yaml` (no `--fix` arm) |
| `rwv.lock` | Generated output | `rwv lock` regenerates it |

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
[error] core: <project>: <dir>/rwv.yaml is a YAML manifest; rwv reads rwv.toml. Rewrite it as rwv.toml by hand and delete <dir>/rwv.yaml — rwv will not convert it, because the comments and key order you wrote cannot be carried across formats.
```

Rewrite the manifest by hand (see [File formats reference — `rwv.toml`](../reference/formats.md#rwvtoml--project-manifest) for the schema) and delete `rwv.yaml`.

## Step 3 — a stale-format lock

With the manifest loading, a `rwv.lock` left in its pre-JSON shape fails to parse. `rwv doctor` reports:

```text
[error] core: <project>: failed to parse rwv.lock at <dir>/rwv.lock: rwv.lock could not be parsed; it is a generated file — run `rwv lock` to regenerate it: expected value at line 1 column 1
```

Regenerate it from the current checkouts:

```bash
rwv lock
```

`rwv doctor` (or `rwv status`) will now report the workspace clean.

## Step 4 — a leftover `go.sum` link on a Go weave

Earlier versions declared `go.sum` at the weave root of a Go weave instead of `go.work.sum`. After upgrading, `rwv doctor` reports the old `go.sum` symlink as a weave-root link at a name the project no longer declares. Remove the symlink with:

```bash
rwv materialize --remove-undeclared-links
```

That unlinks only the undeclared symlink without touching any target file.

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
