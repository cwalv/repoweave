# Upgrade to a new rwv on an existing weave

Before replacing the binaries, finish or abort every in-flight operation using the **old** binary. After upgrading, migrate the primary weave root, end any operation an earlier release left stopped, record the primary's branch, then migrate each workweave from inside it. The block is the order the commands run in; `<primary>` is the primary weave root and `<workweave>` a workweave's directory:

```bash
# 1. Before upgrading (with the old rwv binary): in every workspace that
#    holds an operation, finish it or abort it; only then repair until a
#    weave-wide doctor run is clean
rwv status
rwv abort                           # in each workspace whose operation is in progress
rwv doctor --fix --all
rwv doctor --all

# 2. After installing the new binaries, in the primary:
rwv -C <primary> doctor --fix       # migrate every .rwv-workweave marker, the workweave index and its basedir, reference worktrees, and this checkout's rwv.toml member keys and lock merge driver (Steps 1, 3, 8 and 9)
rwv -C <primary> lock               # only if rwv.lock is in an old format: it is written afresh, pinning nothing (Step 4)

# 3. Only if an operation an earlier release left is still recorded: the
#    lines its refusal prints, in the workspace that holds it, then any
#    naming line step 2 printed (Step 1, and "Ending an operation an earlier release left")
rwv -C <workweave> doctor --fix
rwv -C <workweave> abort --discard-unreadable-record
rwv -C <primary> workweave create <name> --project <project> --dir <workweave>

# 4. The primary's records:
rwv -C <primary> branch set-mode main declared   # record the trunk (Step 6)
rwv -C <primary> branch set-history linear       # say how the trunk accepts landings: linear, merge or ff-only (Step 10)
rwv -C <primary> update                          # record what this workspace aligns its members for (Step 7)
rwv -C <primary> doctor --all                    # names each workweave still to migrate, with its line
rwv -C <primary> materialize --remove-undeclared-links   # optional: unlink retired go.sum root symlink (Step 5)

# 5. Each workweave:
rwv -C <workweave> doctor --fix                  # its own rwv.toml member keys and lock merge driver line (Steps 3 and 9)
rwv -C <workweave> branch set-mode feature --target main   # record its branch (Step 6)
rwv -C <workweave> branch set-history linear     # only a workweave that other workweaves land into: the trunk's value (Step 10)
rwv -C <workweave> update                        # (Step 7)

# 6. The primary again:
rwv -C <primary> doctor --fix       # drops the savepoints an ended operation left, keeping as a backup each tip no branch reaches
rwv -C <primary> doctor --all
```

An in-flight `.rwv-op` or `.rwv-op-lease` file from an older release has no cross-version migration path — resolve it with `--continue` or `rwv abort` on the old binary before upgrading. One left in progress is refused as `unreadable-op-record`, and the refusal prints the lines that end it, in the order they run (see [Ending an operation an earlier release left](#ending-an-operation-an-earlier-release-left)).

For resting state on disk, installing a newer `rwv` does not touch existing files until a command reads them. Each file is migrated according to who owns it:

| File | Where it comes from | Remedy |
|---|---|---|
| `.rwv-op` / `.rwv-op-lease` | In-flight sync state | Finish or run `rwv abort` on the **old** binary before upgrading; left in progress, the lines the refusal prints end it |
| `.rwv-workweave` & member slots | Written by `rwv` itself | `rwv doctor --fix` from the primary weave migrates them |
| `projects/<project>/.rwv-workweave-index` | Written by `rwv` itself | `rwv doctor --fix` rebuilds the index in the project clone from the workweave markers, records the container it named as the basedir, and removes the old file (Step 8) |
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

A `.rwv-workweave` written by an older `rwv` lacks the workweave's `name` (and, older still, `parent`, or is YAML). Every command run from inside that workweave refuses immediately, including `rwv doctor` itself, and names the command that migrates it:

```text
Error: <workweave>/.rwv-workweave is a legacy workweave marker, written by an earlier release (YAML, or missing the required `parent` or `name` field). Run `rwv doctor --fix` from the primary weave, not from inside this workweave: resolving the marker precedes the repair, so a self-invoked `--fix` hits this same refusal and changes nothing:
  rwv -C <primary> doctor --fix
```

Resolving which workspace the command is running in happens before `doctor`'s repair logic runs, so a self-invoked `--fix` inside the workweave cannot rewrite its own marker. Run it from the primary weave:

```bash
rwv -C <primary> doctor --fix
```

One run migrates every marker a checkout of the project clone sits in, wherever its directory is, `--dir` workweaves included, and every marker in a workweave container. Each takes the name the project's workweave index records for its directory, read from the file an earlier release kept at `projects/<project>/.rwv-workweave-index` while it stands; the same run then rebuilds the index (Step 8) and removes that file. A workweave the index does not list takes the name its directory name `<project>--<name>` spells. One whose name nothing records is reported with the command that names it, run in the primary as printed:

```bash
rwv -C <primary> workweave create <name> --project <project> --dir <workweave>
```

It writes the name into the marker and records the workweave, changing nothing else there. While an operation another workspace holds leases the primary, as one an earlier release left stopped does, `workweave create` refuses there, so the finding prints the lines that end that operation first and the naming line last; run them in that order. The same pass replaces any legacy symlinked `reference` member slots with read-only git worktrees and settles file permissions.

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

Run the repair from the project's checkout: in the primary weave root, then inside each workweave, whose checkout carries its own `rwv.toml`; the primary's `rwv doctor --all` names each workweave still to do as [`unmigrated-workweave`](../reference/doctor-findings.md#unmigrated-workweave), with its line:

```bash
rwv doctor --fix
```

It renames `version` to `branch` and `submodules = "skip"` to `recurse-submodules = false`, and drops `submodules = "local"`, which is what an absent `recurse-submodules` means. Comments and key order stay. It makes one commit holding only that rename, and leaves every other uncommitted edit, to `rwv.toml` and to other files, where it was. It declines, changing nothing, while another file is staged or a rebase, merge or cherry-pick is in progress in the project repo; deal with that, then run it again.

A commit other than the checked-out one that still names the old keys declares nothing to the commands that read it, such as `rwv sync` reading the target's `rwv.toml`. Its `rwv.lock`, whose format did not change, is still read, so its pins still count.

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

A member of a workweave that still carries the branch shape an earlier release used, `<project>--<name>/<segment>`, makes `rwv workweave create` and `rwv update` refuse with `pre-flat-branch-shape` and name the bridge version to step through, as the next section describes.

## Step 8 — a workweave index at the old path

An earlier release kept each project's workweave index at `projects/<project>/.rwv-workweave-index`. The index now lives in the project clone, in the directory its worktrees share, so the primary and every workweave reach one copy and a clean of the project's working tree cannot remove it. A file left at the old path is read by nothing as the index: every verb that reads it refuses as `unreadable-workweave-index`, whatever shape the file is in, and names the command that rebuilds it.

```bash
rwv -C <primary> doctor --fix
```

The rebuild runs after the markers are migrated (Step 1), so it lists the workweave each checkout of the project clone sits in, by the name its `.rwv-workweave` marker records, writes the index into the project clone with the container the old file recorded as the basedir, so `rwv workweave create` keeps placing new workweaves where the earlier release did, and removes the old file. Where no file records a container, the fix says where new workweaves will be created and prints, for each other directory the existing workweaves stand in, the line that records it:

```bash
rwv -C <primary> workweave set-default-create-basedir <dir> --project <project>
```

If the old file was committed to the project repo, the removal leaves the deletion for you to commit.

## Step 9 — the lock merge driver an earlier release set up

An earlier release assigned `rwv.lock` the `rwv-ours` merge driver (`rwv.lock merge=rwv-ours`, or `merge=ours`, in `.gitattributes`, with `merge.rwv-ours.driver = true` in the clone's config), which kept one side of every lock merge silently. `rwv.lock` now merges through the lock merge driver: `rwv.lock merge=rwv-lock`, with `merge.rwv-lock.driver` naming `rwv lock --merge-driver %O %A %B`. Every verb that writes the project repo or its lock refuses the old setup, or none, as `missing-lock-merge-driver`, before it writes anything; `rwv explain missing-lock-merge-driver` serves the entry.

Run the repair in each checkout of the project repo whose branch carries the old line:

```bash
rwv doctor --fix
```

It writes the line last in the checked-out commit's `.gitattributes`, where no other line of the file outranks it, committing only that, and in the working file with its other edits in place, and replaces the clone's config. A definition or attribute from outside the project repo's files that still outranks rwv's is named, and the finding stays until you remove it. The same run that rewrites the checkout's `rwv.toml` keys (Step 3) does this too. Every workweave shares its clone's config; another branch carries its own `.gitattributes` and is migrated where it is checked out.

## Step 10 — the trunk's history

An earlier release recorded no top-level `history`. The first `rwv sync` or `rwv land` that does not fast-forward refuses `history unset`, naming `rwv -C <primary> branch set-history <h>`. Set it in the primary once its records are in place, before any workweave lands:

```bash
rwv -C <primary> branch set-history linear
```

A workweave made by the earlier release shares a merge base with the trunk whose `rwv.toml` still names the earlier keys; that base is read as `rwv doctor --fix` rewrites it, so a workweave whose branch never set `history` lands without changing the trunk's.

A workweave that other workweaves land into, such as a long-lived seat that task seats were forked from, records no `history` of its own either. `rwv land` from a seat that lands into it refuses `history unset`, and so does `rwv sync` in a seat under it. Run `rwv branch set-history <h>` in each such workweave, with the same value as the trunk, before anything lands into it:

```bash
rwv -C <workweave> branch set-history linear
```

## Ending an operation an earlier release left

A `.rwv-op` an earlier release wrote carries no record schema, so this release neither resumes nor rolls it back. Every verb that reads it refuses with `unreadable-op-record`, and `rwv status` reports it, in every workspace whose marker this release reads; inside a workweave whose marker is unmigrated, the marker refusal of Step 1 comes first. The record's refusal lists the lines that end the operation, in the order they run:

```text
  rwv -C <primary> doctor --fix
  rwv -C <workspace> doctor --fix
  rwv -C <workspace> abort --discard-unreadable-record
```

The first is printed while the marker of the workspace holding the record is unmigrated, and the second while that workspace's `rwv.toml` names keys an earlier release wrote and its project repo is stopped in no rebase or merge. The last removes the record and releases the lease and claims the operation held; it moves nothing back, since without its record which tips are the operation's own cannot be told, and lists what it leaves: each repo stopped in a rebase or merge, and each savepoint. While the record stands, no `rwv doctor --fix` anywhere in the weave drops its savepoints. Finish or cancel a stopped rebase with the VCS, as the discard prints (for git, `git -C <checkout> rebase --abort`); the primary's `rwv doctor --fix` then keeps each savepoint tip no branch reaches as a backup and drops the savepoints.

## Branches in the pre-flat shape

A release drops the repair arms for on-disk states older than some version. The one state whose absence of repair is not silent is a workweave's branch name occupied by branches of the shape `<project>--<name>/<segment>`: `rwv workweave create` and `rwv update` refuse with `pre-flat-branch-shape` and name the bridge version, the newest release that still migrates them. Install it, run `rwv doctor --fix --all` until `rwv doctor --all` reports clean, then install the newer rwv again.

## Related

- [File formats reference](../reference/formats.md) — `rwv.toml`, `rwv.lock`, and `.rwv-workweave` schemas
- [Doctor findings reference](../reference/doctor-findings.md) — `legacy-manifest-format`, `legacy-workweave-marker`, `workweave-drift`, and `unparseable-project`
- [Reconcile repos with the lock](./reconcile-repos.md) — verifying lock alignment once formats are migrated
