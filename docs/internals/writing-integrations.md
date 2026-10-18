# Writing integrations

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

Integrations ship with `rwv` as Rust modules in `src/integrations/`. They are
compiled in — there is no dynamic loading and no id-to-implementation lookup, so
adding one means editing this tree. This page covers implementing a new one.

For *enabling* an existing integration in a project, see
[add an integration](../how-to/add-an-integration.md). For per-integration docs
(generated file format, config), see
[reference/integrations](../reference/integrations/index.md).

## The `Integration` trait

```rust
pub trait Integration {
    fn name(&self) -> &str;
    fn default_enabled(&self) -> bool;
    fn detection_manifests(&self) -> &[&str] { &[] }
    fn settings_keys(&self) -> &'static [&'static str] { &[] }

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>>;
    fn deactivate(&self, root: &Path) -> anyhow::Result<()>;
    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>>;

    fn activate_hook(&self, _ctx: &IntegrationContext) -> anyhow::Result<HookOutcome> {
        Ok(HookOutcome::Done)
    }
    fn fit_to_host(&self, _ctx: &IntegrationContext) -> anyhow::Result<()> { Ok(()) }
    fn generated_files(&self, _ctx: &IntegrationContext) -> Vec<SurfacedFile> { Vec::new() }
    fn managed_files(&self, ctx: &IntegrationContext) -> Vec<SurfacedFile> {
        self.generated_files(ctx)
    }
    fn verify(&self, _ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> { Ok(Vec::new()) }
    fn owned_paths_on_disk(&self, _ctx: &IntegrationContext) -> Vec<OwnedPath> { Vec::new() }
    fn member_incompatibility(&self, _ctx: &IntegrationContext)
        -> anyhow::Result<Option<MemberIncompatibility>> { Ok(None) }
}
```

### Required methods

- **`name`** — unique identifier (e.g., `"npm-workspaces"`). Used as the config
  key in `rwv.toml`'s `[integrations]` table; parsing `rwv.toml` refuses a
  table whose name no built-in integration has.
- **`default_enabled`** — whether the integration runs without explicit opt-in.
- **`activate`** — author the managed content. Called on the authoring path
  only — the intent verbs, and `rwv doctor --fix` when it regenerates; see
  [the trigger model](../explanation/joints/file-ownership.md#the-trigger-model)
  for which verbs those are and why. Returns findings the way `check` and
  `verify` do: read settings through `ctx.settings_or_issue` and return the
  `MalformedSettings` issue it hands back rather than bailing.
- **`deactivate`** — remove what this integration owns from the project
  directory `root`. Called from two places: `rwv workweave delete`, for every
  enabled integration, on the workweave's project checkout before the
  directory is removed; and `rwv materialize`, for a *disabled* integration
  whose `owned_paths_on_disk` reports a `MarkedRegion`. On the second path
  materialize removes the `WholeFile` entries itself and calls `deactivate`
  only for the region strip, because `deactivate` deletes the files it knows
  about without asking whether rwv wrote them. Takes a bare `&Path`, not a
  context, so owned keys must be static per-integration constants.
- **`check`** — read-only inspection of preconditions: tool availability,
  settings, member topology. Called during `rwv doctor`. The install hooks are
  refused while any enabled integration's `check` returns a
  `MalformedSettings` issue, so an integration whose hook reads settings must
  have `check` deserialize them too; one that reports malformed settings only
  from `verify` leaves its hooks unguarded.

**A hook's `Err` carries no kind.** `for_each_enabled` captures it as
`Issue { kind: IssueKind::IntegrationFailed, .. }`, which is the only kind the
runner can mint, so a condition that has a published kind must be *returned* as
an `Issue` rather than bailed. The published set is
`docs/reference/doctor-findings.md`, and the vaguest row on that page is what an
operator gets for anything raised the other way. `activate`, `check` and
`verify` return `Vec<Issue>`. `activate_hook` returns a `HookOutcome` and
`fit_to_host` and `deactivate` return `()`, so none of the three has an `Issue`
channel: a condition any of them detects that an operator needs to look up has
to be reported by `check` or `verify` too — the bail there is a second line of
defense, never the only report.

### Provided methods

- **`detection_manifests`** — every filename the integration passes to
  `ctx.detect_repos_with_manifest`. The runner precomputes one detection list
  per declared filename before any integration runs; a filename detected by
  but not declared here misses the cache and falls back to a live filesystem
  scan on every call. Ecosystem-tool output the integration never scans for
  (`go.work.sum`, `Cargo.lock`) does not belong here.
- **`settings_keys`** — the keys the integration reads from its
  `[integrations.<name>]` table, besides the framework's own `enabled`.
  Parsing `rwv.toml` refuses any other key in that table, so an integration
  that reads settings returns `settings_struct_keys::<T>()` of the type it
  deserializes them into. The default, empty, means only `enabled` parses.
- **`activate_hook`** — run install commands (`npm install`, `uv sync`,
  `cargo fetch`) after the config files are in place. Run the tool through
  `run_install_tool` (`src/integrations/mod.rs`), which returns `None` when
  the program is not on `PATH`; return `HookOutcome::ToolMissing(<tool>)` for
  that, and the runner reports it as a `tool-missing` warning while the other
  integrations' hooks still run. A tool that runs and fails is a bail, which
  reaches the operator as `integration-failed` and fails the verb.
  `--no-materialize` suppresses the hook on every verb that takes the flag. The
  hook materializes what membership and the recorded pins imply and never moves
  a pin — an implementation that re-resolves an existing lock does not belong
  here.
- **`fit_to_host`** — bring content whose right value is a fact about this
  host, not about `rwv.toml`, to this host's answer (cargo-workspace's
  `rustc-wrapper` key, which exists exactly where the wrapper binary is on
  `PATH`). Runs at every materialization — wherever the install hooks are not
  suppressed — ahead of surfacing, so a file it creates is linked in the same
  pass. It owns only what a machine decides; content the manifest decides
  belongs to `activate`. An `Err` becomes `integration-failed` and stops the
  install hooks for that run.
- **`generated_files`** — paths this integration *fully* owns, relative to
  `output_dir`: whole-write and whole-delete safe, and eligible for
  `.gitignore`. Each is a `SurfacedFile`, constructed with
  `SurfacedFile::written_through_link` (an ecosystem tool writes the path at
  the weave root, through a link that may precede the file — lockfiles) or
  `SurfacedFile::written_at_source` (the file is in the project directory
  before it is linked); `.from_primary()` makes the link read the primary's
  checkout. Declaring a file here means "surface this", not "rwv wrote this".
- **`managed_files`** — the wider set including hybrid files, which are
  surfaced by symlink, never gitignored, and only ever strip-edited. Defaults
  to `generated_files()`, so a fully-owned integration implements just the one.
  The union of the two is what surfacing links; where the two disagree about a
  path's `SurfacedSource`, `WrittenThroughLink` wins.
- **`verify`** — drift detection, separate from `check`: `check`
  answers "is the environment able to do this", `verify` answers "has what we
  wrote been changed underneath us". Must not author; the recovery verb is
  `rwv doctor --fix`, which re-runs `activate` when a finding is
  `safe_to_fix` and its kind's `IssueKind::fix_scope` is `PerFinding`
  (`Issue::fix_permitted`; `--json` publishes the same answer). An enabled integration whose files are not on disk yet
  reports them here as `ManagedFileMissing` — that finding is what makes
  `rwv doctor --fix` author a newly enabled integration.
- **`owned_paths_on_disk`** — where this integration's ownership is written
  under `output_dir` right now, each path tagged with its cleanup shape:
  `OwnedPath::WholeFile` (rwv wrote all of it; cleanup is removal) or
  `OwnedPath::MarkedRegion` (cleanup is `deactivate`'s strip). The one hook
  meaningful for a *disabled* integration: it drives the
  `disabled-integration-artifact` finding and what `rwv materialize` strips.
  Attribute by ownership evidence on disk — the marker (`holds_owned_region`),
  a byte-exact comparison against what `activate` would write now, or the
  marker on the workspace file a lock was resolved from — never by presence at
  the declared path. **The default is empty**, which is safe and inert: a
  disabled integration that does not override it is never reported and its
  files are never cleaned up.
- **`member_incompatibility`** — report a member the integration's own tooling
  cannot build with. Results are collected in one place and shared by
  `rwv doctor` and `rwv update`.

## `IntegrationContext`

Each hook receives an `IntegrationContext` (`src/integration.rs`) with:

| Field | Description |
|---|---|
| `output_dir` | Where managed and generated files live: `<workspace_root>/projects/<project>`, in every weave and for every verb |
| `workspace_root` | The weave root the verb is acting on — the primary root at the primary, the workweave directory inside one. Repo detection scans under it |
| `container_kind` | `ContainerKind::Primary` or `ContainerKind::Workweave`, already resolved by the verb. Read it here rather than probing `workspace_root` for a marker file |
| `project` | The active project name |
| `repos` | Repo entries from the project's `rwv.toml`, as an ordered `(path, entry)` list |
| `config` | Per-integration config from the `[integrations.<name>]` table in `rwv.toml` |
| `all_repos_on_disk` | All repos found on disk under registry directories. Computed once, shared across integrations |
| `all_project_paths` | All project paths. Computed once, shared across integrations |
| `detection_cache` | Manifest filename → repo paths containing it, for every filename the integrations about to run declare in `detection_manifests`. Populated once per cycle, before any integration runs |
| `workweave` | The project's `[workweave]` config, if any. Present so an integration can detect a name claimed by two sections at once |

## Helper methods

- **`ctx.active_repos()`** — filters out `reference` repos, which are read-only
  and not part of the build graph.
- **`ctx.detect_repos_with_manifest(filename)`** — active repos containing a
  given manifest file (e.g. `"package.json"`), served from `detection_cache`
  when warm and falling back to a live scan under `workspace_root`.
- **`ctx.settings_or_issue::<T>(name)`** — the typed settings, or the
  `MalformedSettings` issue to return when the table does not deserialize into
  `T`. The issue names the field and is `safe_to_fix: false`.

The two detection helpers are on-disk gated. A repo declared in `rwv.toml` but
not cloned is not "pending" — it is simply absent from every list, which is why
an intent verb withholds authoring when the member set is incomplete rather
than writing a smaller-but-wrong file.

## Hybrid merge helper (`ManagedDoc`)

The hybrid-merge invariants ([file-ownership](../explanation/joints/file-ownership.md))
live once in `src/integrations/merge.rs` rather than being duplicated per integration:

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

Key design details of that signature:

- **`KeyPath` is `Vec<String>`, not a dotted `&str`:** VS Code's `settings."files.exclude"` is `["settings", "files.exclude"]`, where the second segment contains a literal dot.
- **Marker methods take `owned_keys`:** TOML places `# managed by rwv` as a per-key decoration rather than a document header, whereas JSON ignores the slice and uses its top-level marker key (`x-repoweave` or `rwv.generated`).
- **`key_present` drives the generate-vs-verify switch:** when an `Author` key is present on disk with no marker, `merge_activate` defers rather than overwriting and lists the key in `MergeResult::deferred`. `MergeResult` is `{ authored, deferred }`; the operator-facing report of that state comes from `verify`, through `drift_issues`, as `IssueKind::ManagedFileUserHeld`.
- **`Ownership::Author` vs. `Ownership::DefaultOnly`:** `Author` keys are written and stripped by `rwv`; `DefaultOnly` keys are written only when absent and never overwritten or stripped.
- **`strip_deactivate` returns `StripOutcome` (`Absent`, `UserHeld`, `Stripped`):** callers that need to gate removing a co-requisite generated file (such as an ecosystem lockfile) learn whether the strip happened without re-reading the file. `StripOutcome::UserHeld` is the strip-side name for the unmarked state. `holds_owned_region` answers the same marker question read-only, for `owned_paths_on_disk`.
- **Implementations:** `JsonDoc` (`serde_json` — npm and vscode), `TomlDoc` (`toml_edit` — cargo and uv), `YamlDoc` / line-editor (pnpm), and `GoWorkDoc` (`go.work` `use` block merger). Because `deactivate(root: &Path)` receives no `IntegrationContext`, owned keys are static per-integration constants.

The verify side has shared helpers in the same file: `drift_issues` (the USER-HELD / DRIFT / CLEAN dispatch), `missing_issue`, `orphaned_region_issues` (a marked region no membership justifies), and `fully_owned_drift_issue`. Use them so every integration's findings carry the same kinds and message templates.

For a new integration:

- **Fully rwv-owned:** implement `activate` as a whole-write, `deactivate` as a whole-delete, list the file in `generated_files()`, report it from `verify` (`missing_issue` / `fully_owned_drift_issue`), and return it from `owned_paths_on_disk()` as `OwnedPath::WholeFile` only where its bytes are what `activate` would write now.
- **Hybrid:** implement via `ManagedDoc`, never whole-write or whole-delete a user file, land both `merge_activate` and `strip_deactivate` in the same change, return the file from `owned_paths_on_disk()` as `OwnedPath::MarkedRegion` where `holds_owned_region` finds the marker, and cover the three regression test shapes (`activate-preserves`, `deactivate-strips-keeps`, `deactivate-deletes-if-empty`).
- **Either:** if the integration reads settings, return `settings_struct_keys::<T>()` from `settings_keys()` and deserialize them in `check` through `ctx.settings_or_issue`.

## Registration

New integrations are registered in `builtin_integrations()` in
`src/integrations/mod.rs`. Add a `Box::new(YourIntegration)` entry alongside the
existing built-ins; the rest of `rwv` picks it up automatically, including the
manifest parser, which builds the accepted `[integrations.<name>]` names and
keys from this list and each entry's `settings_keys()`. The returned order is
the order everything runs in.

## Style guidelines

- **Auto-detection over explicit config.** Where possible, detect whether there
  is work to do (`npm-workspaces` checks for repos with `package.json`) rather
  than requiring the user to opt in. Reserve explicit opt-in for cases where
  auto-detection is not safe — `gita` is opt-in because it depends on an
  external tool the user may not have installed.
- **No-op on no-match.** If auto-detection finds nothing, generate nothing and
  raise no error. Activation should succeed.

  This governs `generated_files` and `managed_files` as much as `activate`, and
  those two are where it gets forgotten. Enablement and detection are separate
  questions and the framework only answers the first: an integration is enabled
  by its `default_enabled()` and the operator's config, never by whether a
  member repo carries its manifest. Nothing downstream re-asks. So an ungated
  declaration is a claim on a weave-root name in every workspace that leaves
  the integration on its default, ecosystem present or not — and the surfacing
  step will symlink it, at a source the integration knows cannot be there. Gate
  both methods on the same `detect_repos_with_manifest` call, and gate them
  alike: `go-work` gated `managed_files` and not `generated_files`, and every
  Go-free weave grew a `go.sum` symlink pointing at nothing.
- **Idempotent.** Running `activate` twice produces the same result. Running
  `deactivate` then `activate` is a clean re-installation.
- **Filter `reference` repos.** Use `ctx.active_repos()` rather than
  `ctx.repos.iter()` when generating workspace member lists.
- **Surfacing integrity is framework-level — not a per-integration `check()`
  responsibility.** `rwv doctor` runs a framework-side check asserting that
  every file in the `generated_files() ∪ managed_files()` union has a valid
  symlink at the weave root. Do not duplicate that logic inside your
  integration. Per-integration `check()` belongs to environment and config
  preconditions.
- **Do not restate the ownership contract; implement it.** Whether your
  integration is fully-owned or hybrid, what marker proves ownership, how
  `activate` merges and `deactivate` strips, and the three regression shapes a
  hybrid integration must test, are all normative and published in
  [file-ownership](../explanation/joints/file-ownership.md). Hybrid
  integrations implement it through `ManagedDoc` in
  `src/integrations/merge.rs`, which holds the delete-if-empty and
  strip-only-owned invariants once.

## Related

- [file-ownership](../explanation/joints/file-ownership.md) — the normative
  ownership and trigger contracts.
- [reference/integrations](../reference/integrations/index.md) —
  per-integration documentation.
- [add an integration](../how-to/add-an-integration.md) — enabling existing
  integrations.
- [`ARCHITECTURE.md`](../../ARCHITECTURE.md) §6.2 — where this trait sits
  relative to `integration_runner` and the merge engine.
