# Upgrade to a new rwv on an existing weave

Before replacing the `rwv` binary, finish or abort any in-flight sync using the **old** binary; after upgrading, migrate workweave markers, manifests, and locks from the primary weave root:

```bash
# 1. Before upgrading (with the old rwv binary): clear any in-flight sync
rwv status
rwv abort                           # if an operation is in progress

# 2. After installing the new rwv binary (from the primary weave root):
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

## Related

- [File formats reference](../reference/formats.md) — `rwv.toml`, `rwv.lock`, and `.rwv-workweave` schemas
- [Doctor findings reference](../reference/doctor-findings.md) — `legacy-manifest-format`, `legacy-workweave-marker`, `workweave-drift`, and `unparseable-project`
- [Reconcile repos with the lock](./reconcile-repos.md) — verifying lock alignment once formats are migrated
