# Upgrade to a new rwv on an existing weave

rwv writes three files to disk that an older binary shaped differently: the
workweave marker, the project lock, and the project manifest. Installing a
newer `rwv` over an existing weave does not touch any of them — they only
change the moment something *reads* one. So the first command you run after
upgrading is the first place you can hit a refusal, and which file it names
depends on which one you touch first.

Each of the three is handled differently, because each comes from a different
place:

| File | Where it comes from | Remedy |
|---|---|---|
| `.rwv-workweave` | Written by rwv itself | `rwv doctor --fix` migrates it |
| `rwv.lock` | Generated output | `rwv lock` regenerates it |
| `rwv.yaml` / `rwv.toml` | Hand-authored by you | You rewrite it; no `--fix` arm |

This page walks the fix in the order you're likely to hit them: from inside a
workweave, the marker refuses first, then the manifest, then the lock. Each
step below is a real refusal, quoted, with the command that clears it.

## Step 1 — a legacy workweave marker

If `.rwv-workweave` was written by an older rwv, every command run from
inside that workweave refuses immediately, including `rwv doctor` itself:

```text
Error: <workweave>/.rwv-workweave is a legacy workweave marker (YAML format, or missing the required `parent:` field). Run `rwv doctor --fix` from the primary weave, not from inside this workweave: resolving the marker precedes the repair, so a self-invoked `--fix` hits this same refusal and changes nothing. The file is still readable YAML, so its own `primary:` names the weave to run from — `rwv doctor --fix -C <that path>` needs no `cd`.
```

The reason a workweave cannot migrate its own marker is that resolving
*which* workspace the command is running in happens before doctor's repair
logic does, and that resolution is what refuses — so no verb, `--fix`
included, gets far enough to rewrite the file. The marker is plain YAML,
so its `primary:` field is readable without fixing anything first:

```bash
cat .rwv-workweave        # primary: <path> — still readable, unmigrated
rwv doctor --fix -C <path from primary:>
```

`rwv doctor --fix` scans every workweave from primary, not just the one you
name, so this is also the routine way to clear every stale marker across a
weave in one pass. Once it runs, the marker is JSON and the workweave
resolves again — including from inside itself.

## Step 2 — a legacy project manifest

With the marker resolved, the next thing a command loads is the project the
workweave holds. A `rwv.yaml` with no `rwv.toml` beside it is refused at
error severity, and there is no `--fix` arm for it. `rwv doctor` reports:

```text
[error] core: <project>: <dir>/rwv.yaml is a YAML manifest; rwv reads rwv.toml. Rewrite it as rwv.toml by hand and delete <dir>/rwv.yaml — rwv will not convert it, because the comments and key order you wrote cannot be carried across formats.
```

The manifest is yours — the comments and key order in it are things you
wrote, and neither has a mechanical translation into TOML. Rewrite it by
hand (see [reference/formats — `rwv.toml`](../reference/formats.md#rwvtoml--project-manifest)
for the shape) and delete `rwv.yaml`. A directory that ends up holding both
names briefly is not reported as a problem — rwv reads `rwv.toml` and
ignores the leftover — but the leftover is still yours to delete once you've
confirmed the rewrite.

## Step 3 — a stale-format lock

With the manifest loading, a `rwv.lock` left in its pre-JSON shape fails to
parse. `rwv doctor` reports:

```text
[error] core: <project>: failed to parse rwv.lock at <dir>/rwv.lock: rwv.lock could not be parsed; it is a generated file — run `rwv lock` to regenerate it: expected value at line 1 column 1
```

Unlike the manifest, nothing here is yours to preserve — the lock only ever
records resolved repo revisions, so the fix is to regenerate it:

```bash
rwv lock
```

`rwv doctor` (or `rwv status`) should now report the workweave clean.

## Step 4 — a leftover `go.sum` link on a Go weave

Earlier versions declared `go.sum` at the weave root of a Go weave. A Go
workspace root never carries that file — the checksum file Go writes there is
`go.work.sum`, which rwv now declares instead. After upgrading, the first
`rwv doctor` on a Go weave therefore reports the old `go.sum` symlink as a
weave-root link at a name the project no longer declares.

```bash
rwv materialize --remove-undeclared-links
```

That removes the link only — it is rwv's own artifact, and no file it
resolves to is touched.

## `.rwv-op` / `.rwv-op-lease` — nothing to do

These two carry in-flight sync state, and there is no old-format arm at all:
an in-flight operation from before the upgrade has to be resolved with
`rwv abort` on the *old* binary before you upgrade. Once the new binary is
running, it never reads an old-shaped `.rwv-op`, so there is no refusal to
walk here — a leftover old-format file just fails to parse like any other
malformed record, which is a sign the abort was missed, not something this
upgrade path repairs.

## Related

- [Formats](../reference/formats.md) — the shape of `rwv.toml`, `rwv.lock`, and `.rwv-workweave`
- [Doctor findings — `legacy-manifest-format`, `legacy-workweave-marker`, `unparseable-project`](../reference/doctor-findings.md)
- [Reconcile repos with the lock](./reconcile-repos.md) — the read-only detect step once a weave is on the new formats
