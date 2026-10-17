# cargo-workspace

Merges managed keys into `projects/<project>/Cargo.toml` — `[workspace].members`,
`[workspace].resolver`, and (optionally) `[workspace.package]` — while
preserving all other content byte-for-semantics, and surfaces `Cargo.toml`
(and `.cargo/config.toml` when present) at the weave or
workweave root via symlinks. Seeds resolver version `"2"` when absent.
All other tables (`[profile.*]`, `[workspace.dependencies]`,
`[workspace.lints.*]`, user-authored `[patch.*]`, etc.) are user policy and are
never written or stripped by `rwv`.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | repos with `Cargo.toml` (plus active repos configured under `members`) |
| Manages | `Cargo.toml` and optional `.cargo/config.toml` (hybrid merge — see below) |
| Install hook | `cargo fetch`, or `cargo generate-lockfile` when no valid lock exists yet |

## Managed keys

`rwv` owns only these keys inside `Cargo.toml` (and `.cargo/config.toml` when configured), marking each owned key with a `# managed by rwv` prefix comment:

| Key | File | Ownership | Condition |
|---|---|---|---|
| `[workspace].members` | `Cargo.toml` | `Author` | Always when integration is active |
| `# excluded: <path> (opted out)` | `Cargo.toml` | Attached to `members` prefix decor | When `exclude` matches an active Rust repo and `rwv` holds the pen on `members` |
| `[workspace].resolver` | `Cargo.toml` | `DefaultOnly` (`"2"`) | Seeded when absent; user-chosen values (e.g. `"3"`) are preserved |
| `[workspace.package]` | `Cargo.toml` | `Author` | Only when `workspace-package = true` is set |
| `[patch.<registry>].<crate>` (`# managed by rwv` entries only) | `Cargo.toml` **or** `.cargo/config.toml` | `Author` | Only when `patch` is `"committed-paths"` or `"derived"`. File chosen by `patch-surface` (see below) |
| `[build].rustc-wrapper` (`# managed by rwv` entry only) | `.cargo/config.toml` | `Author` | Only when `rustc-wrapper` is configured and found on `PATH` |

All other content — `[profile.*]`, `[workspace.dependencies]`,
`[workspace.lints.*]`, `[workspace.metadata.*]`, user-authored
`[patch.*]` entries (without `# managed by rwv` decor), `[package]`,
inline comments, and key ordering — is user policy and survives every
authoring and deactivation cycle unmodified. `rwv`-marked patch entries are
stripped on deactivation; user-authored ones survive.

## Example managed section

```toml
[workspace]
# managed by rwv
members = [
  "github/chatly/protocol",
  "github/chatly/server/client",
  "github/chatly/server/common",
  "github/chatly/server/daemon",
]
# managed by rwv
resolver = "2"
```

The file lives in `projects/<project>/Cargo.toml` and is symlinked to the weave
root. Build artifacts (`target/`) are gitignored tool state;
`Cargo.lock` is produced by Cargo in `projects/<project>/Cargo.lock`, and its
commit-vs-gitignore handling follows [Committed files and
committability](./index.md#committed-files-and-committability).

## Three Rust artifacts

A Cargo workspace weave produces three distinct lock/manifest artifacts with separate roles:

| Artifact | Produced by | Pins | Committable |
|---|---|---|---|
| `Cargo.toml` (managed) | Authoring verbs (`rwv add`, `remove`, `update`, `role`, `doctor --fix`) | Workspace shape — which repos are members | Yes |
| `Cargo.lock` | Install hook (`cargo generate-lockfile` for the first resolve, `cargo fetch` after) | External crate versions from crates.io | Operator's choice ([details](./index.md#committed-files-and-committability)) |
| `rwv.lock` | `rwv lock` | Internal repo revisions (git SHAs) | Yes |

`Cargo.lock` answers "which version of `serde`?" ([commit policy](./index.md#committed-files-and-committability)); `rwv.lock` answers "which commit of `github/chatly/protocol`?". They operate at different layers — external ecosystem vs. internal repo graph — and neither subsumes the other.

## Members sub-path config

By default `rwv` treats each active Rust repo's root as a single workspace
member. For repos whose packages live below the root (e.g. a repo with
`daemon/`, `client/`, `common/` sub-packages and no root `Cargo.toml`), use
the `members` config block:

```toml
[integrations.cargo-workspace.members."github/chatly/server"]
include = ["daemon", "client", "common"]
exclude = ["fuzz"]
```

This produces members `github/chatly/server/client`, `github/chatly/server/common`,
and `github/chatly/server/daemon` — the repo root is not treated as a member.

`include` lists sub-paths to add as workspace members; `exclude` removes
entries from the effective include set. An empty `include` means **no
members** from that repo. Both lists default to empty when omitted.

Repos not listed under `members` keep the root-as-member auto-behavior.

## `[workspace.package]` opt-in

Set `workspace-package = true` in `rwv.toml` to have `rwv` seed and manage the
`[workspace.package]` table in `Cargo.toml`:

```toml
[integrations.cargo-workspace]
workspace-package = true
```

`[workspace.package]` is written only when this flag is `true` (default
`false`). Any sub-keys you add inside `[workspace.package]` in `Cargo.toml`
(`license`, `authors`, `edition`, `repository`) are preserved across merges.
When `false`, `[workspace.package]` is left entirely to the user.
Not useful for multi-product weaves (Cargo's `[workspace.package]` is
workspace-wide with no per-member override) or for internal-only
`publish = false` crates.

## Cross-repo path deps and `[patch]`

The `patch` mode picks how cross-repo dependencies resolve
against in-weave sources. Three modes:

| Mode | TOML value(s) | Behavior |
|---|---|---|
| Off (default) | `"off"`, `false` | `rwv` writes no `[patch]` entries. Members declare committed relative `path = "..."` deps for cross-repo deps. Default for internal-only (`publish = false`) crates. |
| Committed-paths | `"committed-paths"`, `true` | `rwv` mirrors each member's committed cross-member `path = "..."` deps into `[patch.crates-io]` at the weave root. Useful when the weave's canonical shape is a set of `path`-consuming publishable crates. |
| Derived | `"derived"` | `rwv` matches each member's **registry** and **git** deps by crate name against the in-weave package-name index and emits patch entries — members declare `chatly-core = "0.3"` and resolve to the in-weave checkout automatically. |

```toml
[integrations.cargo-workspace]
patch = "derived"
```

The boolean aliases `patch = true` (→ `"committed-paths"`) and `patch = false`
(→ `"off"`) remain supported for backwards compatibility.

### `derived` mode — details

Under `patch = "derived"`, `rwv`:

1. Builds a package-name index over every in-weave crate — active
   workspace members *and* `reference`-role repos, which are read-only
   study material and never workspace members.
2. Scans each active member's `[dependencies]`, `[dev-dependencies]`,
   and `[build-dependencies]`, resolving `workspace = true` through the
   member's ancestor `[workspace.dependencies]` table.
3. For each dep whose crate name matches an in-weave entry:
   - **Git-source dep** (`foo = { git = "<url>", version = "..." }`) →
     emits under `[patch."<url>"].foo` (git deps are a distinct source
     from crates.io; a `[patch.crates-io]` entry would not apply).
   - **Registry dep** → emits under `[patch.crates-io].foo`.
   - **Self-patch** (member depending on itself under its own crate
     name) → skipped.
4. Runs a version-compatibility precheck using Cargo's `semver` crate
   (`VersionReq::parse` / `Version::parse`): if the in-weave crate's
   `[package].version` does not satisfy the member's requirement, the
   patch is *skipped* and a warning is printed to stderr naming the
   crate, in-weave version, and the member's requirement. This avoids
   Cargo's mismatch diagnostic, which points at crates.io rather than
   the local patch target.
5. Runs a same-key shadowing precheck: if a member's own
   `.cargo/config.toml` declares the same `[patch.<reg>].<crate>` key
   that `rwv` is about to write, Cargo's closest-config-wins-per-key rule
   silently voids the weave-level entry. `rwv` surfaces the shadowing as
   a stderr warning at generation time so the operator sees it
   *before* Cargo emits its diagnostic.

**Tier boundary:** a member depending on an
*unpublished* crate name resolves only inside the weave. Standalone
(outside the weave), the member fails with "no matching package". This
is the sovereignty tier boundary — `derived` mode gives sovereign
members live in-weave sources on top of registry deps, but does not
make unpublished names available outside the weave. If your intent is
weave-native crates, that is the expected shape; if you need
standalone-buildable members, keep the target's version published.

**No per-crate auto-detection:** `rwv` never reads or infers publishability
from member `[package].publish` fields. The mode is an operator decision
that applies to the entire weave.

## Patch surface (`manifest` vs `cargo-config`)

`patch-surface` decides **where** `rwv` writes the `[patch.*]` tables.
Orthogonal to `patch` (which decides *what* patches to compute).

| Surface | TOML value | Where entries land | Reaches |
|---|---|---|---|
| Manifest (default) | `"manifest"` | `[patch.*]` in the managed `Cargo.toml` | Every consumer of the workspace manifest. **Cannot** reach nested-workspace opt-outs. |
| Cargo config | `"cargo-config"` | `[patch.*]` in a generated `.cargo/config.toml` alongside the managed `Cargo.toml` | Every consumer above, PLUS builds run from *inside* a nested-workspace opt-out's directory (via Cargo's upward config discovery). |

```toml
[integrations.cargo-workspace]
patch = "derived"
patch-surface = "cargo-config"
```

### Why the config surface exists — the nesting-immune lens

Cargo hard-errors when a workspace member itself declares
`[workspace]` at its root (nested workspaces are illegal). Repos in this
shape (e.g. a standalone multi-crate service or tool repo) must be `exclude`-d from
the weave workspace, and once excluded the workspace-manifest `[patch]`
never applies to their builds. The `cargo-config` surface writes the
patch into `.cargo/config.toml` at the same level as `Cargo.toml`;
Cargo's config discovery walks *upward* from the current working directory (not by workspace
membership), so `cargo build` invoked from inside an opted-out repo
still sees the weave's patches.

Both surfaces at once is structurally impossible — `patch-surface` is
an enum, not two booleans.

### Hybrid ownership

The generated `.cargo/config.toml` is hybrid, same as `Cargo.toml`:
`rwv` owns `# managed by rwv`-decorated `[patch.<registry>].<crate>` entries
(and `[build].rustc-wrapper` when configured), and
the user owns everything else (linker flags in `[target.*]`, Cargo
credentials, hand-authored unmarked `[patch]` entries). Deactivation
strips only the marked entries; a config that carries only user keys
is left alone.

### Path resolution

Cargo resolves relative patch paths in `.cargo/config.toml` against
the **parent of `.cargo/`**.
Since `.cargo/` is surfaced directly at the weave root, member paths
in the generated config stay **weave-root-relative**
(`github/chatly/protocol`), the same shape the manifest surface writes. No
`../` prefix and no canonicalization are used, preserving symlinked `.cargo/`
surfacing at the weave root.

### Same-key shadowing

A member's own `.cargo/config.toml` declaring the same
`[patch.<reg>].<crate>` key silently overrides the weave-level entry
(Cargo's closest-config-wins per key). The patch-shadowing scan
in `rwv doctor` detects this against both surfaces.
Generation time surfaces the same warning to stderr.

### Housekeeping — untracked project state

The generated `.cargo/config.toml` lives in `projects/<project>/`
alongside `Cargo.toml`, `Cargo.lock`, and `.rwv-owned-digests`.
`rwv` does not auto-manage a `.gitignore` for any of these; see
[Committed files and committability](./index.md#committed-files-and-committability)
for how project-directory manifests, `Cargo.lock`, and `.rwv-owned-digests`
are handled in version control.

## Compiler wrapper (`rustc-wrapper`)

An opt-in knob that routes the weave's Cargo builds through a compiler
wrapper — `sccache` is the typical choice — **when the host has it**:

```toml
[integrations.cargo-workspace]
rustc-wrapper = "sccache"
```

At each authoring or materialization pass (`rwv add`, `rwv doctor --fix`,
`rwv materialize`, or `rwv fetch`), the named binary is looked up on `PATH`:

- **Present** — a `# managed by rwv`-marked `[build] rustc-wrapper` key is
  merged into `.cargo/config.toml`. Cargo discovers that file
  upward from any member directory, so every in-weave build routes through
  the wrapper.
- **Absent** — the key is omitted, and a previously written marked key is
  stripped (the file and `.cargo/` directory are pruned when nothing else
  remains).

The conditionality keeps the setting portable across machines: Cargo
hard-errors on a wrapper it cannot spawn, so an unconditional key would
break machines without the tool. Installing or removing the wrapper
takes effect at the **next** authoring or materialization pass
(`rwv materialize`, `rwv doctor --fix`), not immediately; until then the
verify pass reports the difference as auto-fixable drift.

Hybrid ownership is the same as for patch entries: an unmarked
user-authored `[build] rustc-wrapper` key holds the pen — `rwv` neither
overwrites nor strips it, and verify surfaces the conflict as `USER-HELD`.

## Nested workspaces

Cargo refuses to nest workspaces. If a member repo declares its own `[workspace]` section in its root `Cargo.toml` — including a *virtual workspace* (a `Cargo.toml` with `[workspace]` and no `[package]`) — Cargo fails with `"multiple workspace roots found in the same workspace"`.

`rwv` detects this before invoking Cargo and refuses with an actionable error naming the conflicting repo. There are four ways to resolve the conflict:

1. **Remove the nested `[workspace]`** from the repo's root `Cargo.toml` (usually only sensible for repos you own).
2. **Disable the `cargo-workspace` integration entirely** in `rwv.toml`:

    ```toml
    [integrations.cargo-workspace]
    enabled = false
    ```

3. **Opt the offending repo out** (per-repo escape hatch) by listing its path under `integrations.cargo-workspace.exclude`:

    ```toml
    [integrations.cargo-workspace]
    exclude = ["github/chatly/legacy-tools"]
    ```

4. **Use `members` sub-path config** if the repo has no root `Cargo.toml`
   (or its root `Cargo.toml` does not declare `[workspace]`) and its
   sub-packages have their own `Cargo.toml` manifests. If the repo's root
   `Cargo.toml` still declares `[workspace]`, `members` sub-path config
   does not bypass the conflict because Cargo's ancestor walk still binds
   the sub-packages to that root workspace.

When `rwv` holds the pen on `[workspace].members`, opted-out active Rust repos are omitted from `members` and recorded as `# excluded: <path> (opted out)` prefix comments on the `members` key in `Cargo.toml`. When `members` is stripped on deactivation, those prefix comments are removed with it.

**Tradeoff:** opted-out repos remain in the weave (their clones are still on disk, still reachable from the workweave) but are absent from the unified workspace. That means `cargo test --workspace`, `cargo build --workspace`, and similar workspace-scoped commands do not cover them. Unless `patch-surface = "cargo-config"` is enabled, builds inside an opted-out repo resolve cross-repo dependencies via the registry rather than local clones. Work inside the opted-out repo must be built and tested from its own directory.

Opt-out entries that do not match any active Rust repo (because the repo was removed from the manifest, or is not a Rust repo) are silently ignored.

## Deactivation

Strips only the `# managed by rwv`-marked keys (`[workspace].members` and its `# excluded:` prefix comments, `[workspace].resolver`, `[workspace.package]` when configured, any marked `[patch.<registry>].<crate>` entries from `"committed-paths"` or `"derived"` mode, and a marked `[build] rustc-wrapper` in `.cargo/config.toml`) from the files `rwv` authors in `projects/<project>/`. User-authored content — `[profile.*]`, `[workspace.dependencies]`, `[workspace.lints.*]`, and user-authored `[patch.*]` entries — survives. Emptied `[workspace]`, `[patch.<registry>]`, `[patch]`, and `[build]` tables are pruned. `Cargo.toml` or `.cargo/config.toml` is deleted **only** when nothing user-authored remains after stripping.

## Check

`rwv doctor` reports:

- `malformed-settings` (error) if `[integrations.cargo-workspace]` contains unknown keys or invalid values.
- `tool-missing` (warning) if `cargo` is not on `PATH` when active Rust work is present.
- `config-rejected` (error) for every active Rust repo whose root `Cargo.toml` declares `[workspace]` without an `exclude` opt-out.

## Verify (drift)

`rwv doctor` and context verbs (`rwv activate`, `rwv fetch`,
`rwv workweave create`) also run the read-only verify pass:

- **`Cargo.toml` and `.cargo/config.toml`** (hybrid): `MISSING` / `DRIFT` (both auto-fixable via `rwv doctor --fix`) / `USER-HELD` (surfaced, never auto-overwritten) / `CLEAN` — the shared four-state dispatch.
- **`Cargo.lock`** (fully owned; see [Committed files and committability](./index.md#committed-files-and-committability)): `MISSING` and unparseable-content `DRIFT` are auto-fixable (`rwv doctor --fix` regenerates via `cargo generate-lockfile` — neither state holds a resolve to preserve). A lock that is present and parseable in `projects/<project>/Cargo.lock` whose bytes differ from the **last `rwv`-accepted generation** is reported as a warning (`fully-owned-digest-mismatch`, not auto-fixed): the install hook records a SHA-256 of each accepted generation and input set in `.rwv-owned-digests`, and verify compares against it. The operator either re-runs `rwv materialize` (accepting and re-stamping the new content) or restores `Cargo.lock`. Workspaces without prior digest state skip this comparison silently.
- **Dependency version skew and patch shadowing**: `rwv doctor` also scans workspace members (and nested workspaces) for inconsistent dependency version requirements (`dep-version-skew`) and member `.cargo/config.toml` files that shadow weave-level patches (`patch-shadowed`).

## Notes

A repo detected by this integration is also claimed by `uv-workspace` when it carries a `pyproject.toml` alongside its `Cargo.toml` (a maturin/PyO3 bindings crate) — both generated files include it, with no conflict. See [uv-workspace: a compiled extension can go stale](./uv-workspace.md#polyglot-cargouv-a-compiled-extension-can-go-stale) for the install hook's interaction in that combination.
