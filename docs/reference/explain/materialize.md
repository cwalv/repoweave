# rwv materialize

## Purpose

Run the integration install hooks (`npm install`, `uv sync`, `cargo fetch`,
etc.) for the project this checkout already presents, and nothing else.

**Hooks materialize; they never move a pin.** A hook's mandate is to make the
ecosystem state implied by *current membership plus the versions already
recorded* real on disk: a lock file gains what a new member requires, tool
state directories are brought up to what the lock says, and a version the lock
already pins stays where it is. Advancing a dependency is something you ask for
with the ecosystem's own update command (`cargo update`, `npm update`,
`uv lock --upgrade`) — never a side effect of a repoweave verb.

That guarantee is what makes this verb safe to run at any time, and safe to
name as the remedy after a `rwv sync` delivers changes.

### Why this is a separate verb

`rwv activate` does two things: it **selects** a project (writes
`.rwv-active`, moves the weave root's shared names) and it **materializes**
that project's ecosystem state. Only a primary can express selection, and only
for one project at a time — which is why `rwv activate` is refused inside a
workweave, where the project is fixed at creation.

Materialization has no such restriction. It is meaningful wherever the project
identity is already settled: in a workweave always, at a primary for the
project it currently presents. `rwv materialize` is that half on its own.

It takes **no project argument**. Naming a project would be a selection, and
selection is the one thing this verb does not do — `.rwv-active` is never read
as an instruction and never written.

`rwv activate --no-materialize` is the mirror image: select without
materializing. One word names the operation on both sides.

### What it touches

1. **Disabled-integration cleanup.** An integration turned off in `rwv.toml`
   implies the absence of what it authored, so this is where that absence is
   made real (see below).
2. **Drift settlement.** Generated files rwv attests are compared against what
   it accepted, and content it never accepted stops the run (see below).
3. **Surfacing repair.** The weave root's symlinks onto the project's owned
   files are re-created if missing, scoped to this project's own files. The
   root's shared names are not moved — the root already presents this project,
   so there is nothing to move.
4. **Install hooks.** Each enabled integration's hook runs against the
   now-in-place symlinks.

It does **not** author managed content. If an integration's managed file is
missing, its hook refuses and names `rwv doctor --fix`, which is the verb that
authors.

### Removing what a disabled integration left

Disabling an integration withdraws the justification for its content, not the
content. `rwv doctor` reports every artifact still on disk under
`disabled-integration-artifact`, names this verb, and has no `--fix` arm for it
— a one-character edit to `rwv.toml` must not put deletion one flag away. This
verb is that deletion, said in as many words:

- a **marked region** in a file you co-own is stripped out; everything else in
  the file stays, and the file itself survives unless nothing was left in it;
- a file rwv wrote **whole** is removed, along with its entry in the
  accepted-generation record;
- the weave-root symlinks that surfaced them are unlinked.

Each removal is printed as it happens. Nothing you authored is touched:
attribution runs off rwv's own ownership marker, so a hand-authored workspace
file — and the lock beside it — is never named or removed. Re-enabling the
integration and running `rwv doctor --fix` regenerates everything except the
ecosystem lock, whose next generation belongs to the ecosystem tool.

### Re-deriving state whose inputs moved

rwv records the digests of the inputs each generation read — the project
manifest and `rwv.lock` — beside the digest of what it produced. When those
inputs have moved since, `rwv doctor` reports the generated file as
`derived-state-stale` and names this verb; running it re-derives the state and
records the inputs the new generation read.

An attestation for a file nothing enabled here generates any more is dropped
rather than carried: rwv will not redo that derivation, so it stops vouching for
the result. The file itself is left alone.

### Arriving at drift

rwv records a digest of each generated file at the moment it accepts that
file's generation, so a lock file whose content differs from that record is
content rwv never accepted — an ecosystem tool run by hand in this checkout, or
an attestation a workweave inherited from a source sitting on the same
difference.

`materialize` **refuses** on arriving there, because the two ways out destroy
opposite things and only the operator knows which applies:

| Flag | Effect |
|---|---|
| `--regenerate-drifted` | Discard the current content and regenerate it from the current inputs |
| `--adopt-drifted` | Record the current content as the accepted generation |

Regenerating throws away a deliberate edit; adopting attests an accident.
Passing both is refused — they are contradictory requests, not a stricter one.

## Links at names the project no longer declares

The weave root surfaces what the active project declares. Drop a name from
`rwv.toml` — or change a setting that stops an integration declaring one — and
the link stays: every candidate set rwv builds comes from the *current*
declarations, so a name that left them is in none of them.

`rwv doctor` reports each such link with the path it resolves to. It does not
repair it, and `rwv doctor --fix` will not either: on disk rwv cannot tell its
own leftover from a link you made by hand at the same shape, so the choice is
yours to make.

| Flag | Effect |
|---|---|
| `--remove-undeclared-links` | Unlink the weave-root symlinks `rwv doctor` reported at undeclared names |

**It unlinks; it does not delete.** The file each link pointed at is untouched,
and `rwv doctor` printed the target, so a link removed by mistake is one
`ln -s` away. Nothing else at the weave root is reachable: a real file is never
touched, nor is a symlink pointing anywhere other than this project's copy of
that same path.

Names belonging to a *disabled* integration are not included here — those have
their own finding and plain `rwv materialize` removes what rwv authored.

The refusal lists every path it would act on, so running once without a flag is
how you see what is at stake. **What `--regenerate-drifted` discards is not
recoverable through rwv**: the bytes are content rwv never accepted, so no
digest, savepoint or copy of them exists anywhere in the workspace. Copy the
file aside first if you might want it back. `--adopt-drifted` discards nothing.

Files with no difference never reach this fork, so the friction is paid only
where the ambiguity is real.

This is where the consent is given, and not the only verb it binds. The hooks
are what settles drift, so every verb that runs them withholds them while drift
stands, and says so on stderr: `rwv activate`, `rwv add`, `rwv remove`,
`rwv update` and `rwv doctor --fix`. None of those carries a flag to answer
with — the answer is given here, once, and then they go through.

## Invocation

```
rwv materialize [--regenerate-drifted | --adopt-drifted]
                [--remove-undeclared-links]
```

No arguments.

Run `rwv --help materialize` for the full clap surface.

## Output

Install hook output goes to stderr. On success there is no confirmation
message.

## Exit codes

- `0` — hooks ran successfully.
- non-zero — no project is presented by this checkout, a generated file rwv
  attests holds content it never accepted and no consent flag was passed, the
  workspace could not be resolved, the manifest failed to parse, or an
  integration hook returned an error.

## Examples

Refresh a workweave's ecosystem state after syncing source in:

```
rwv sync ../other-weave
rwv materialize
```

Select a project fast, materialize later:

```
rwv activate web-app --no-materialize
rwv materialize
```

## Common errors

- *nothing is materialized at `<path>`: no project is active here* — a primary
  weave with no `.rwv-active`. There is no project to materialize until one is
  selected; run `rwv activate <name>`.
- *managed file missing … run `rwv doctor --fix` to regenerate* — the project's
  ecosystem config was never authored (or was deleted). `materialize` never
  authors; `rwv doctor --fix` does.
- *materialize stopped: content rwv never accepted is on disk* — pick an exit
  with `--adopt-drifted` or `--regenerate-drifted`, per "Arriving at drift"
  above. `rwv doctor` reports the same files, and its finding names the same
  two flags.
- *integration activate-hook error* — an install command returned non-zero. Fix
  the underlying ecosystem problem and rerun.
- *`<name>` is a weave-root link into project `<p>` at a name `<p>` no longer
  declares* — not an error, a standing `rwv doctor` finding. Re-run this verb
  with `--remove-undeclared-links` to unlink exactly the links it named.
