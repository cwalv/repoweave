# cargo-workspace

Merges `[workspace].members` (and, when configured, `[patch.*]` entries) into
`projects/<project>/Cargo.toml` while preserving all other content
byte-for-semantics, seeds `[workspace].resolver = "2"` when absent, and
surfaces `Cargo.toml` (and `.cargo/config.toml` when rwv writes one) at the
weave or workweave root via symlinks. All other tables (`[profile.*]`,
`[workspace.dependencies]`, `[workspace.lints.*]`, user-authored `[patch.*]`,
etc.) are user policy and are never written or stripped by `rwv`.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | repos with `Cargo.toml` (plus active repos configured under `members`) |
| Manages | `Cargo.toml` and optional `.cargo/config.toml` (hybrid merge — see below) |
| Generates | `Cargo.lock`, written by Cargo ([commit policy](./index.md#committed-files-and-committability)) |
| Install hook | `cargo fetch`, or `cargo generate-lockfile` when no lock records a resolve yet |

## Configuration

Enabled by default. The `[integrations.cargo-workspace]` table takes:

| Key | Type | Default | Effect |
|---|---|---|---|
| `enabled` | bool | `true` | Turns the integration on or off. |
| `exclude` | list of repo paths | `[]` | Opts active Rust repos out of `[workspace].members` — see [Nested workspaces](#nested-workspaces). |
| `members` | table of `"<repo>" = { include, exclude }` | — | Contributes sub-paths of a repo instead of its root — see [Members sub-path config](#members-sub-path-config). |
| `patch` | `"off"`, `"committed-paths"`, `"derived"` | `"off"` | Which cross-repo `[patch]` entries rwv writes — see [Cross-repo path deps and `[patch]`](#cross-repo-path-deps-and-patch). |
| `patch-surface` | `"manifest"`, `"cargo-config"` | `"manifest"` | Which file those entries land in — see [Patch surface](#patch-surface-manifest-vs-cargo-config). |
| `rustc-wrapper` | string | unset | A compiler wrapper to route builds through where the host has it — see [Compiler wrapper](#compiler-wrapper-rustc-wrapper). |
| `workspace-package` | bool | `false` | See [`[workspace.package]`](#workspacepackage). |

Any other key in the table is refused when `rwv.toml` is parsed, naming the
accepted keys. An unknown key inside a `members."<repo>"` table, or a value of
the wrong type anywhere in the table, is reported as `malformed-settings`
instead.

### Members sub-path config

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

### Nested workspaces

Cargo refuses to nest workspaces. If a member repo declares its own `[workspace]` section in its root `Cargo.toml` — including a *virtual workspace* (a `Cargo.toml` with `[workspace]` and no `[package]`) — Cargo fails with `"multiple workspace roots found in the same workspace"`.

`rwv` detects this before invoking Cargo: authoring verbs refuse with an error naming the conflicting repo, and `rwv doctor` reports it as `config-rejected`. There are four ways to resolve the conflict:

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

### Cross-repo path deps and `[patch]`

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

`patch = true` is read as `"committed-paths"`, and `patch = false` as `"off"`.

Each generated entry is an inline table carrying the `# managed by rwv` marker:

```toml
[patch.crates-io]
# managed by rwv
chatly-core = { path = "github/chatly/core" }
```

Setting `patch` back to `"off"` leaves the entries rwv already wrote in place,
and `rwv doctor` does not report them; remove the marked entries by hand.

#### `derived` mode — details

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
4. Runs a version-compatibility precheck with Cargo's semver rules: if the
   in-weave crate's `[package].version` does not satisfy the member's
   requirement, the patch is *skipped* and a warning is printed to stderr
   naming the crate, in-weave version, and the member's requirement. This
   avoids Cargo's mismatch diagnostic, which points at crates.io rather than
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

### Patch surface (`manifest` vs `cargo-config`)

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

Both surfaces at once is structurally impossible — `patch-surface` is
an enum, not two booleans.

#### Why the config surface exists — the nesting-immune lens

Cargo hard-errors when a workspace member itself declares
`[workspace]` at its root (nested workspaces are illegal). Repos in this
shape (e.g. a standalone multi-crate service or tool repo) must be `exclude`-d from
the weave workspace, and once excluded the workspace-manifest `[patch]`
never applies to their builds. The `cargo-config` surface writes the
patch into `.cargo/config.toml` at the same level as `Cargo.toml`;
Cargo's config discovery walks *upward* from the current working directory (not by workspace
membership), so `cargo build` invoked from inside an opted-out repo
still sees the weave's patches.

#### Path resolution

Cargo resolves relative patch paths in `.cargo/config.toml` against
the **parent of `.cargo/`**.
Since `.cargo/` is surfaced directly at the weave root, member paths
in the generated config stay **weave-root-relative**
(`github/chatly/protocol`), the same shape the manifest surface writes. No
`../` prefix and no canonicalization are used, preserving symlinked `.cargo/`
surfacing at the weave root.

#### Same-key shadowing

A member's own `.cargo/config.toml` declaring the same
`[patch.<reg>].<crate>` key silently overrides the weave-level entry
(Cargo's closest-config-wins per key). The `cargo-patch-shadowing` scan
in `rwv doctor` detects this against both surfaces.
Generation time surfaces the same warning to stderr.

### Compiler wrapper (`rustc-wrapper`)

An opt-in knob that routes the weave's Cargo builds through a compiler
wrapper — `sccache` is the typical choice — **when the host has it**:

```toml
[integrations.cargo-workspace]
rustc-wrapper = "sccache"
```

At each authoring pass and each materialization — `rwv add`, `rwv remove`,
`rwv update`, `rwv role`, a `rwv doctor --fix` that regenerates,
`rwv materialize`, and, unless `--no-materialize` is passed, `rwv activate`,
`rwv workweave create`, and the activation a `rwv fetch` performs — the
named binary is looked up on `PATH`:

- **Present** — a `# managed by rwv`-marked `[build] rustc-wrapper` key is
  merged into `projects/<project>/.cargo/config.toml`, and the file is
  surfaced at the weave root as `.cargo/config.toml`. Cargo discovers that
  file upward from any member directory, so every in-weave build routes
  through the wrapper.
- **Absent** — the key is omitted, and a previously written marked key is
  stripped (the project's file and `.cargo/` directory are pruned when
  nothing else remains). The weave-root `.cargo/config.toml` link stays
  behind; `rwv doctor` reports it as a `surfacing` warning, and
  `rwv materialize --remove-undeclared-links` removes it.

```toml
[build]
# managed by rwv
rustc-wrapper = "sccache"
```

The conditionality keeps the setting portable across machines: Cargo
hard-errors on a wrapper it cannot spawn, so an unconditional key would
break machines without the tool. Installing or removing the wrapper
takes effect at the **next** such pass, not immediately; until then the
verify pass reports the difference as a finding `rwv doctor --fix` repairs
(`managed-file-missing` when the file is absent, `managed-file-drift` when
the key is stale).

When `.cargo` or `.cargo/config.toml` in the project directory is a symlink,
the key is not fitted: the verb prints a warning naming the link and
continues, because a write through it could land outside the weave.

Hybrid ownership is the same as for patch entries: an unmarked
user-authored `[build] rustc-wrapper` key holds the pen — `rwv` neither
overwrites nor strips it, and verify reports it as `managed-file-user-held`.

### `[workspace.package]`

`workspace-package = true` asks `rwv` to author an empty `[workspace.package]`
table in `Cargo.toml`, marked as its own:

```toml
# managed by rwv
[workspace.package]
```

`rwv` writes no keys into the table. An authoring verb (`add`, `remove`,
`role`, `update`) authors it as soon as the knob is set; `rwv doctor --fix`
authors it only when other drift already makes it regenerate. Disabling the
integration and running `rwv materialize` strips the table and its marker.
With the knob unset (the default), `[workspace.package]` is left entirely to
you.

## Managed and generated files

`rwv` owns only these keys inside `Cargo.toml` (and `.cargo/config.toml` when configured). Each `Author` key carries a `# managed by rwv` prefix comment:

| Key | File | Ownership | Condition |
|---|---|---|---|
| `[workspace].members` | `Cargo.toml` | `Author` | Always when integration is active |
| `# excluded: <path> (opted out)` | `Cargo.toml` | Attached to `members` prefix decor | When `exclude` matches an active Rust repo and `rwv` holds the pen on `members` |
| `[workspace].resolver` | `Cargo.toml` | `DefaultOnly` (`"2"`), unmarked | Seeded when absent; user-chosen values (e.g. `"3"`) are preserved |
| `[patch.<registry>].<crate>` (`# managed by rwv` entries only) | `Cargo.toml` **or** `.cargo/config.toml` | `Author` | Only when `patch` is `"committed-paths"` or `"derived"`. File chosen by `patch-surface` |
| `[build].rustc-wrapper` (`# managed by rwv` entry only) | `.cargo/config.toml` | `Author` | Only when `rustc-wrapper` is configured and found on `PATH` |

All other content — `[profile.*]`, `[workspace.dependencies]`,
`[workspace.lints.*]`, `[workspace.metadata.*]`, user-authored
`[patch.*]` entries (without `# managed by rwv` decor), `[package]`,
inline comments, and key ordering — is user policy and survives every
authoring and deactivation cycle unmodified. `rwv`-marked patch entries are
stripped on deactivation; user-authored ones survive.

```toml
[workspace]
# managed by rwv
members = ["github/chatly/protocol", "github/chatly/server/client", "github/chatly/server/common", "github/chatly/server/daemon"]
resolver = "2"
```

The file lives in `projects/<project>/Cargo.toml` and is symlinked to the weave
root. Build artifacts (`target/`) are gitignored tool state;
`Cargo.lock` is produced by Cargo in `projects/<project>/Cargo.lock`, and its
commit-vs-gitignore handling follows [Committed files and
committability](./index.md#committed-files-and-committability).

The generated `.cargo/config.toml` is hybrid, same as `Cargo.toml`:
`rwv` owns `# managed by rwv`-decorated `[patch.<registry>].<crate>` entries
(and `[build].rustc-wrapper` when configured), and
the user owns everything else (linker flags in `[target.*]`, Cargo
credentials, hand-authored unmarked `[patch]` entries). It lives in
`projects/<project>/` alongside `Cargo.toml`, `Cargo.lock`, and
`.rwv-owned-digests`; `rwv` does not manage a `.gitignore` for any of these
(see [Committed files and committability](./index.md#committed-files-and-committability)).

### Three Rust artifacts

A Cargo workspace weave produces three distinct lock/manifest artifacts with separate roles:

| Artifact | Produced by | Pins | Committable |
|---|---|---|---|
| `Cargo.toml` (managed) | Authoring verbs (`rwv add`, `remove`, `update`, `role`, `doctor --fix`) | Workspace shape — which repos are members | Yes |
| `Cargo.lock` | Install hook (`cargo generate-lockfile` for the first resolve, `cargo fetch` after) | External crate versions from crates.io | Operator's choice ([details](./index.md#committed-files-and-committability)) |
| `rwv.lock` | `rwv lock` | Internal repo revisions (git SHAs) | Yes |

`Cargo.lock` answers "which version of `serde`?" ([commit policy](./index.md#committed-files-and-committability)); `rwv.lock` answers "which commit of `github/chatly/protocol`?". They operate at different layers — external ecosystem vs. internal repo graph — and neither subsumes the other.

## Install hook

Runs at the weave root. Which verbs run it, and how `--no-materialize` skips
it, is the same for every integration: see [Hook points](./index.md#hook-points).

- **`cargo fetch`** when the `Cargo.lock` Cargo reads at the weave root
  already records a resolve: it keeps every version the lock pins and adds
  only what current membership requires.
- **`cargo generate-lockfile`** when no lock records a resolve yet.
- **Accepted generation** — after a run whose write reached
  `projects/<project>/Cargo.lock`, `rwv` records a digest of the lock and of
  the inputs it was generated from (`rwv.toml`, `rwv.lock`) in
  `projects/<project>/.rwv-owned-digests`. The verify pass compares later
  bytes against that record; see [Diagnostics](#diagnostics).
- **No managed `Cargo.toml`** — the hook refuses with an error naming
  `rwv doctor --fix`, which authors the file.
- **Members not on disk** — when a member `Cargo.toml` names is not cloned
  yet (directly after `rwv init --adopt`), the hook skips with a warning
  naming the missing members and `rwv fetch`.
- **Tool trouble** — with `cargo` absent from `PATH` the hook is skipped with
  a `tool-missing` warning and the verb continues. A Cargo run that fails is
  an error; its message names the two ways out of a duplicate crate name
  (`exclude`, or `members` sub-path config).

The `rustc-wrapper` key is fitted to the host before the hook runs.

## Deactivation

Strips only the `# managed by rwv`-marked keys (`[workspace].members` and its `# excluded:` prefix comments, any marked `[patch.<registry>].<crate>` entries from `"committed-paths"` or `"derived"` mode, and a marked `[build] rustc-wrapper` in `.cargo/config.toml`) from the files `rwv` authors in `projects/<project>/`. User-authored content — `[profile.*]`, `[workspace.dependencies]`, `[workspace.lints.*]`, and user-authored `[patch.*]` entries — survives. Emptied `[patch.<registry>]`, `[patch]`, and `[build]` tables are pruned. `.cargo/config.toml` is deleted when nothing user-authored remains after stripping.

`[workspace].resolver` is `DefaultOnly` and is not stripped, so a `Cargo.toml` `rwv` seeded keeps `[workspace]` with `resolver = "2"` and is not deleted. If the integration is enabled again later, `rwv doctor` reports that file as `managed-file-user-held` (an unmarked `[workspace]` key is present), `rwv doctor --fix` and `rwv materialize` leave it, and the next authoring verb writes `members` back under the marker.

- **Disabled** (`enabled = false`): `rwv materialize` strips the marked keys, removes `Cargo.lock`, and removes the weave-root links, including the weave-root `.cargo/` directory when that removal empties it. Its `[stripped]` line names each file it acted on.
- **Last Rust member removed**: the authoring verb strips the marked keys on the same rule and leaves `Cargo.lock` in place. The weave-root links stay; `rwv doctor` reports them as `surfacing` warnings, and `rwv materialize --remove-undeclared-links` removes them.

## Diagnostics

Reported by `rwv doctor`. Verify rows also print as warnings on `rwv activate`, `rwv materialize`, `rwv workweave create` and the activation a `rwv fetch` performs; see [Hook points](./index.md#hook-points).

| Kind | Severity | Condition | Repair |
|---|---|---|---|
| [`malformed-settings`](../doctor-findings.md#malformed-settings) | error | A value of the wrong type in `[integrations.cargo-workspace]`, or an unknown key inside a `members."<repo>"` table. | Correct the field the message names. |
| [`tool-missing`](../doctor-findings.md#tool-missing) | warning | Active Rust work is present, but `cargo` is not on `PATH`. | Install `cargo`. |
| [`config-rejected`](../doctor-findings.md#config-rejected) | error | An active Rust repo's root `Cargo.toml` declares `[workspace]` and the repo is not in `exclude`. | See [Nested workspaces](#nested-workspaces). |
| [`managed-file-missing`](../doctor-findings.md#managed-file-missing) | warning | `Cargo.toml` or `Cargo.lock` is absent from the project directory, or `.cargo/config.toml` is absent while rwv has entries to write there. | `rwv doctor --fix` |
| [`managed-file-drift`](../doctor-findings.md#managed-file-drift), `safe_to_fix: true` | warning | The marked `members` differ from what `rwv.toml` declares, or no Rust member remains and the marked region is still there; the marked keys in `.cargo/config.toml` differ from what `rwv.toml` and this host's wrapper detection imply; `Cargo.lock` is unreadable or not valid TOML. | `rwv doctor --fix` |
| [`managed-file-drift`](../doctor-findings.md#managed-file-drift), `safe_to_fix: false` | warning | `Cargo.lock` parses, but its bytes differ from the last generation `rwv` accepted. Plain `rwv materialize` refuses until you choose. A weave with no recorded generation skips this comparison. | `rwv materialize --adopt-drifted` records the current content as accepted; `rwv materialize --regenerate-drifted` discards it and regenerates; or restore the file. |
| [`managed-file-user-held`](../doctor-findings.md#managed-file-user-held) | warning | `Cargo.toml` has an unmarked `[workspace].members`. Authoring verbs, `rwv doctor --fix` and `rwv materialize` leave it as it is. | Add `# managed by rwv` on its own line directly above `members`, or delete `members` and `resolver` from `[workspace]`; then run `rwv doctor --fix`. |
| [`managed-file-user-held`](../doctor-findings.md#managed-file-user-held) | warning | `Cargo.toml` has an unmarked `[workspace].resolver` and no `members`, as [Deactivation](#deactivation) leaves it. `rwv doctor --fix` and `rwv materialize` leave the file. | The next authoring verb writes `members` under the marker and keeps `resolver`. Or delete `resolver` and run `rwv doctor --fix`, which writes both. |
| [`managed-file-user-held`](../doctor-findings.md#managed-file-user-held) | warning | `.cargo/config.toml` holds an unmarked key rwv would write, and no marked key. Authoring verbs, `rwv doctor --fix` and `rwv materialize` leave that key as it is. | Add `# managed by rwv` on its own line directly above the key and run `rwv doctor --fix`, or delete the key and run an authoring verb, which writes it under the marker. |
| [`managed-file-user-held`](../doctor-findings.md#managed-file-user-held) | warning | `.cargo/config.toml` exists, holds no marked key, and holds none of the keys rwv has to write there. `rwv doctor --fix` leaves the file. | The next authoring verb writes the keys under the marker. |
| [`disabled-integration-artifact`](../doctor-findings.md#disabled-integration-artifact) | warning | The integration is disabled, and the marked `Cargo.toml` region, `Cargo.lock`, or marked `.cargo/config.toml` entries are still on disk. | `rwv materialize` |
| [`derived-state-stale`](../doctor-findings.md#derived-state-stale) | warning | `rwv.toml` or `rwv.lock` moved since `Cargo.lock` was generated. Raised by `core`. | `rwv materialize` |
| [`cargo-version-skew`](../doctor-findings.md#cargo-version-skew) | warning | Two or more workspace members (nested workspaces included) require the same crate at different version strings. | Report-only. |
| [`cargo-patch-shadowing`](../doctor-findings.md#cargo-patch-shadowing) | warning | A member's `.cargo/config.toml` declares a `[patch.<registry>].<crate>` key that shadows a weave-level entry. | Report-only. |

## Related

- [uv-workspace: a compiled extension can go stale](./uv-workspace.md#polyglot-cargouv-a-compiled-extension-can-go-stale) — a repo carrying both `Cargo.toml` and `pyproject.toml` (a maturin/PyO3 bindings crate) is claimed by this integration and `uv-workspace` at once, with no conflict between the generated files
- [Integrations reference](./index.md) — hook points and committability
- [add an integration](../../how-to/add-an-integration.md) — enabling and disabling integrations
