# File ownership

Every file rwv manages is governed by two independent axes: **surfacing** (how the file reaches
the weave root) and **content ownership** (who authors the bytes). Understanding both is a
prerequisite for writing a new integration, reading an existing one, or reasoning about what
`rwv activate` and `rwv deactivate` will do.

## Axis 1 — Surfacing (universal)

The committed file lives in the **project repo** (`projects/<project>/`). rwv creates a symlink
from the weave root to it so ecosystem tools find it where they expect (`Cargo.toml`, `go.work`,
`package.json`, …).

This is shared framework infrastructure that every integration uses. Nothing writes to the weave
root directly; the symlink is the delivery layer.

**Committability is what makes hybrid-merge possible.** User-authored content must be versioned.
The project repo is the versioned unit. A tool or rwv writing through the symlink writes into the
committed file — so a merge that preserves the user's `[profile.*]` blocks or `catalog:` section
keeps them in VCS automatically.

Consequences:

- Hybrid integrations must write to `output_dir = project_dir`, never the weave root. Reads obey
  the same binding: `output_dir` is *derived* as `<weave>/projects/<project>` rather than passed
  in, so no verb can point an integration's `check()` or `verify()` at the root view. That view
  answers a different question — it carries symlinks for the *active* project only, and only for
  files whose source exists — so an integration reading through it inspects another project's file,
  or names a path that exists in neither view for a file that is simply absent.
- A root symlink is removed only when its name is in the owning integration's declared file set
  **and** its `read_link` target resolves to `projects/<project>/<file>`. A name claimed by two
  integrations is a hard `Severity::Error` before any symlink mutation.
- Surfacing is checked **framework-side**, not per-integration. The same
  `generated_files() ∪ managed_files()` union that drives symlink creation has a second consumer:
  `rwv doctor` asserts every file in the union exists at the weave root as a symlink resolving to
  `projects/<project>/<file>`, and `rwv doctor --fix` re-surfaces any that are missing or
  mis-resolved. This catches divergence the per-integration `verify()` (Axis-2 content drift)
  cannot see — a manual `rm`, an interrupted create, a manifest that gained a file after a
  workweave was created, or an integration enabled in an existing workweave. The repair is a pure
  re-surface (symlink (re)creation bound to the current weave directory); it never writes
  `.rwv-active`, so it is valid inside a workweave where `rwv activate` is refused.
- **Surfaced names come in two classes, and only one may cross projects.** A *per-project name* —
  `<project>.code-workspace` — cannot collide with another project's, so two projects' can sit at
  the weave root at once and either may be surfaced at any time. Every other surfaced name is
  *shared*: more than one project produces `Cargo.toml`, so the root shows exactly one project's,
  and which one is what `.rwv-active` (a workweave's `.rwv-workweave`) answers. Since `--project X`
  scopes a verb to a project without selecting it, a repair for a project the weave root does not
  present surfaces that project's per-project names and leaves every shared name where it is.
  Not writing the pointer is not sufficient: moving the files the pointer governs performs the
  switch and hides it, which is how a `--fix` scoped to a non-active project once re-pointed seven
  shared root names out from under a concurrent seat. The check is symmetric — it expects only
  per-project names for a project the root does not present, and for the project it does present
  it flags any shared name resolving into another project, including names no enabled integration
  of the presented project declares. Without that last part a stomp survives its own repair: the
  presented project cannot reclaim a name nothing it declares can reach.

See [sync-semantics](./sync-semantics.md) for how the project-repo commit structure interacts with workweave syncing.
See [vcs-as-seam](./vcs-as-seam.md) for the VCS-layer abstraction that makes symlink mechanics portable.

## Axis 2 — Content ownership

Who authors the bytes of the file in the project repo? Every integration declares one of three
positions:

| Content ownership | Meaning | Integrations |
|---|---|---|
| **Fully rwv-owned** | rwv owns the entire artifact; whole-write and whole-delete are safe; file is gitignore-eligible | gita CSVs; ecosystem lockfiles |
| **Hybrid** | a sentinel marker proves ownership; rwv writes only *managed keys*; format-aware parse preserves all foreign content byte-for-semantics; deactivate strips managed keys and deletes the file only if nothing user-authored remains | cargo, uv, pnpm, go-work, npm, vscode |
| **Fully user-owned** | rwv authors no content bytes; it only surfaces the user's file via Axis 1 | static-files |

**static-files is not a third regime.** It is the empty-managed-set corner of hybrid: an
integration whose managed key set is empty. It rides 100% on the universal surfacing substrate.
The destruction risk for it is an Axis-1 surfacing-ownership collision (rwv-c5h), not content
loss.

## The hybrid-merge contract

For any integration in the hybrid position, the following invariants hold. They are extracted
from the npm and vscode precedents and applied uniformly across all six hybrid integrations.

### 1. Ownership is proven by an explicit marker

A single, position-independent sentinel establishes ownership before any mutation:

- **JSON:** `x-repoweave` key (`{"managed": true, "keys": [...]}`)
- **vscode:** `rwv.generated` key (kept from the existing precedent; value records managed keys)
- **TOML:** key decoration — `# managed by rwv` on the managed key (e.g. on `[tool.uv.workspace].members`)
- **YAML / go.work:** a `# managed by repoweave` comment line above the managed block

Name-squatting real semantic fields (e.g. `"name"` in `package.json`) is rejected as the
ownership marker. A sentinel key keeps "what is this file called" and "who manages this file"
separate.

### 2. Managed keys only

Only the declared `managed_keys` set is written or stripped. A managed key may name a sub-path —
a sub-array inside a map, a sub-table — not only a top-level key. rwv owns keys *within* a
managed map, never the whole map (unless the map itself is a managed key).

Examples: npm owns `workspaces.packages` inside an object-form `workspaces`; vscode owns
specific keys within `settings.files.exclude`; uv owns `[tool.uv.workspace]` inside
`pyproject.toml`.

### 3. All other content survives byte-for-semantics

Foreign content — `[profile.*]`, `catalog:`, `overrides:`, `replace (…)`, `[tool.ruff.*]`,
user-added `files.exclude` keys, `extensions`/`launch`/`tasks` blocks — is never touched.
Format-preserving parsers (`toml_edit` for TOML, `preserve_order` for JSON, line-region editors
for YAML/go.work) keep comments and key ordering where the format allows; serde round-trips
normalize whitespace but preserve keys and order.

### 4. `activate()` is read-or-empty → merge → write

- File absent: start from an empty document.
- File present and well-formed: parse, merge managed keys and marker, write.
- File present and **malformed**: bail loudly, naming the file. Do not silently overwrite
  or zero a file rwv does not fully own.

This makes `activate()` idempotent: re-activating over an already-activated file produces the
same result as the first activation.

A member set that has emptied — the integration detects no relevant repo, so it has no managed
keys to write — is the degenerate case of the same rule, not an exemption from it. `activate()`
strips its region there, by the invariant #5 states. Skipping the file instead would make the
region a function of the membership *history*: a project that never had a Rust member and one
that lost its last are the same manifest, and must reach the same file.

### 5. `deactivate()` is strip-not-delete

Gate on the marker. Strip exactly the managed keys. Prune now-empty parent tables. **Delete the
file only when nothing user-authored remains.** Otherwise rewrite the stripped document without
the marker — leaving it as a hand-owned file the user can edit freely.

This means a standalone `rwv deactivate` path cannot destroy user content, even on integrations
that previously used whole-file deletion.

### 6. Lockfiles are fully owned

Ecosystem lockfiles (`package-lock.json`, `Cargo.lock`, `uv.lock`, `pnpm-lock.yaml`) are fully
rwv-owned artifacts: deactivate removes them, gated on the same ownership marker that guards the
workspace config file.

## The marker as generate-vs-verify switch

The marker does double duty as an ownership signal and a **generate-vs-verify switch** — the
same mechanism for all six hybrid integrations:

- **Marker present, or managed key absent:** rwv *owns* the key and *authors* it on intent verbs.
  Managed key absent → rwv creates the key and the marker; manages from that point forward.
- **Managed key present but unmarked:** the user took the pen (e.g. replaced rwv's list with a
  native glob `members = ["github/*/*"]`). rwv *does not author* the key; it only
  **verifies and warns** on drift. This is the same as the merge model's "never touch unmarked
  content" applied at whole-key granularity.

This makes generate-merge vs check-and-warn an emergent **per-file property**, not a
per-integration mode. There is no per-project config for this — the marker state in the file is
the switch.

## The trigger model

The hybrid-merge contract governs *how* rwv authors managed keys. A separate, orthogonal question
is *when* it authors them. In brief:

| Verb class | Verbs | Action on managed region |
|---|---|---|
| **Intent** | `add`, `remove`, `update` | Regenerate, for the operator to commit alongside the `rwv.toml`/`rwv.lock` change |
| **Context** | `activate`, `fetch`, workweave-create, `lock`, `init`, `init --adopt` | Surface (symlink, always) + verify-and-warn; never author |
| **Recovery** | `rwv doctor` / `rwv doctor --fix` | Report Axis-2 content drift **and** Axis-1 surfacing gaps; `--fix` regenerates content and re-surfaces symlinks |

rwv writes the regenerated files into the project directory; it never commits them. An intent verb
also **withholds** regeneration when a repo the manifest declares active has no directory on disk —
authoring from a partial member set would drop the absent repos from every managed file. The
manifest change still lands; `rwv fetch` then `rwv doctor --fix` regenerates once the member set is
whole.

`activate` creates the symlink unconditionally (Axis-1 surfacing). It never authors the committed
file. The committed file is always consistent with committed `rwv.toml` + `rwv.lock` by
construction, because regeneration is tied to the intent verb that changed them.

### Install hooks at context verbs: lockfiles may be rewritten

"Never author" is a claim about the **authoring path** — the managed regions and rwv-computed
artifacts that intent verbs regenerate. It is not a claim that a context verb leaves every byte on
disk untouched. The context verbs that run activation (`activate`, `init`, `init --adopt`, and
`fetch`'s first-fetch auto-activate — not workweave-create, which suppresses hooks, and not `lock`,
which runs no activation) still fire the integration **install hooks**, and the ecosystem tools
those hooks invoke rewrite the lockfiles they own: `Cargo.lock`, `package-lock.json`,
`pnpm-lock.yaml`, `uv.lock`.

This holds at `init --adopt` even when the adopted repo **commits** its lock. A generated lock is
fully rwv-owned derived state with no user author (§"Lockfiles are fully owned",
[lock-as-derived](./lock-as-derived.md)); committing one does not transfer the pen, so an adopt
regenerating it is a refresh of derived state, not re-authoring of committed content. (A refresh
is not a re-resolve: an existing `Cargo.lock` is materialized with `cargo fetch`, which honours
the versions it already records. `cargo generate-lockfile`, which rebuilds a lock with the latest
compatible version of every package, runs only where no lock exists.) The adopt-time no-clobber guarantee is
exactly the authoring-path guarantee: managed regions, rwv-computed generated artifacts (a
`.code-workspace`, gita CSVs — written only by intent-mode `activate()`), and static-files entries
(surfaced, never written) all survive an adopt byte-for-byte.

The license is keyed on **ownership**, not on `generated_files()` membership: static-files entries
ride `generated_files()` for Axis-1 surfacing while remaining fully user-owned, and no verb —
context or intent — may write them. Conversely `go.sum` is hook-owned in principle, but rwv runs
no go install hook today, so nothing rewrites it at a context verb.

A license to rewrite is not an obligation to. A hook whose inputs are not on disk yet **defers**
rather than failing: `init --adopt` clones only the project repo, so an adopted manifest that
already names its members names paths that do not exist, and cargo cannot resolve a lockfile
against them. The cargo hook detects that before shelling out, skips the generation, and names the
absent members plus the commands that resolve them (`rwv fetch`, then `rwv activate`) — the adopt
completes, and the lock first appears at the activation that follows the members arriving. Nothing
is lost by deferring, because membership change is the lockfile-refresh trigger in the first place.
The deferral is scoped to inputs that are **absent**: a member that is on disk but whose manifest
is broken is a genuine hook failure and still bails.

## The shared helper API

The hybrid-merge invariants live once, in `src/integrations/merge.rs`, rather than duplicated
per integration. The trait:

```rust
type KeyPath = Vec<String>;

trait ManagedDoc: Sized {
    fn parse(text: &str) -> anyhow::Result<Self>;    // bail on malformed
    fn empty() -> Self;
    fn has_marker(&self, owned_keys: &[KeyPath]) -> bool;
    fn set_marker(&mut self, owned_keys: &[KeyPath]);
    fn remove_marker(&mut self, owned_keys: &[KeyPath]);
    fn key_present(&self, key: &KeyPath) -> bool;
    fn set_owned(&mut self, key: &KeyPath, value: &OwnedValue);
    fn remove_owned(&mut self, key: &KeyPath);
    fn is_empty(&self) -> bool;
    fn serialize(&self) -> anyhow::Result<String>;
}
fn merge_activate<D: ManagedDoc>(
    path: &Path,
    owned: &[(KeyPath, Ownership, OwnedValue)],
) -> anyhow::Result<MergeResult>;
fn strip_deactivate<D: ManagedDoc>(
    path: &Path,
    owned_keys: &[KeyPath],
) -> anyhow::Result<StripOutcome>;
```

Three shapes in that signature list carry contract, not convenience:

- **A key is a path of segments, not a dotted string.** `KeyPath` is
  `Vec<String>`, so vscode's `settings."files.exclude"` is unambiguously
  `["settings", "files.exclude"]` — two segments, the second containing a
  literal dot. A dotted `&str` cannot express that.
- **The marker methods take the owned keys.** TOML places the marker as a
  per-key decoration rather than a document header, so the trait does not
  commit to a single sentinel location; JSON ignores the slice and uses its
  top-level marker key. This is what lets one trait cover both.
- **`key_present` is the generate-vs-verify switch.** A managed key on disk
  with no marker means the user took the pen, and `merge_activate` defers
  rather than authoring. Without this method the switch would have to be
  re-derived per format.

`merge_activate` also takes each key's `Ownership`. `Author` keys rwv writes
and strips; `DefaultOnly` keys rwv writes only when absent and never
overwrites or strips, because the value is the operator's to adjust once
seeded — a divergent `DefaultOnly` value is CLEAN, not drift. The returned
`MergeResult` names which `Author` keys were authored and which were deferred,
so the caller can raise the USER-HELD warning for the deferred ones;
`DefaultOnly` keys appear in neither list.

`strip_deactivate` returns a `StripOutcome` — `Absent`, `UserHeld`, or
`Stripped` — so a caller that must know whether the strip happened (to gate
removing a co-requisite generated file, say) learns it without re-reading and
re-parsing the file to redo the marker check.

Implementations: `JsonDoc` (serde_json — npm and vscode), `TomlDoc` (toml_edit — cargo and uv,
with sub-table scoping), `YamlDoc` / line-editor (pnpm), `GoWorkDoc` (use-block merger). The
delete-if-empty and strip-only-owned invariants live here once.

This is the hybrid axis only. Attesting a fully-owned generated file (rule 6)
is a separate mechanism with no overlap — see
[lock-as-derived](./lock-as-derived.md).

`deactivate(root: &Path)` receives no `IntegrationContext`, so owned keys are static
per-integration constants — the helper does not infer them from the manifest at deactivate time.

## Author guidance for new integrations

A new integration declares its content ownership regime. For **fully-rwv-owned**, implement
`activate` as a whole-write, `deactivate` as a whole-delete, and list the file in
`generated_files()`. For **hybrid**, implement via `ManagedDoc`, declare `managed_keys`, and:

- Never whole-write or whole-delete a user file.
- Implement `activate` as read-merge-write (idempotent).
- Implement `deactivate` as strip-and-delete-if-empty; never delete if user content remains.
- Add the three-regression-test shape: activate-preserves, deactivate-strips-keeps,
  deactivate-deletes-if-empty.

Both halves (merge-activate and strip-not-delete-deactivate) must land **in the same change**;
shipping merge-activate while leaving the old whole-delete deactivate would let the standalone
deactivate path destroy content the merge protects.

See [lock-as-derived](./lock-as-derived.md) for how lockfiles fit into the fully-owned category.

## Related joints

- [weave-root](./weave-root.md) — why the root the symlinks land in is not a repo, and why
  jurisdiction over root links is rwv's while authorship stays with the files.
- [sync-semantics](./sync-semantics.md) — how project-repo commits and workweave syncing interact with the files
  hybrid integrations write.
- [lock-as-derived](./lock-as-derived.md) — why lockfiles are fully owned, not hybrid.
- [vcs-as-seam](./vcs-as-seam.md) — the VCS-layer abstraction that owns symlink mechanics and replay exclusion.
