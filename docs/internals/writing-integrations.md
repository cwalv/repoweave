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

    fn activate(&self, ctx: &IntegrationContext) -> anyhow::Result<()>;
    fn deactivate(&self, root: &Path) -> anyhow::Result<()>;
    fn check(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>>;

    fn activate_hook(&self, ctx: &IntegrationContext) -> anyhow::Result<()> { Ok(()) }
    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> { Vec::new() }
    fn managed_files(&self, ctx: &IntegrationContext) -> Vec<String> { self.generated_files(ctx) }
    fn verify(&self, ctx: &IntegrationContext) -> anyhow::Result<Vec<Issue>> { Ok(Vec::new()) }
    fn member_incompatibility(&self, ctx: &IntegrationContext)
        -> anyhow::Result<Option<MemberIncompatibility>> { Ok(None) }
}
```

### Required methods

- **`name`** — unique identifier (e.g., `"npm-workspaces"`). Used as the config
  key in `rwv.yaml`'s `integrations:` block.
- **`default_enabled`** — whether the integration runs without explicit opt-in.
- **`activate`** — author the managed content. Called from the *intent* verbs
  only; see [the trigger model](../explanation/joints/file-ownership.md#the-trigger-model)
  for which verbs those are and why.
- **`deactivate`** — remove what this integration owns. Called during workweave
  deletion and before re-running `activate`. Takes a bare `&Path`, not a
  context, so owned keys must be static per-integration constants.
- **`check`** — read-only inspection. Return `Vec<Issue>` for problems detected.
  Called during `rwv doctor`.

### Provided methods

- **`activate_hook`** — run install commands that refresh ecosystem lock files
  (`npm install`, `uv sync`, `cargo generate-lockfile`) after `activate` has
  written config files. Fires whenever the workspace's set of active repos may
  have changed; users suppress it with `rwv activate --no-install`.
- **`generated_files`** — paths this integration *fully* owns, relative to
  `output_dir`: whole-write and whole-delete safe, and eligible for
  `.gitignore`. Used by the activation framework to detect orphaned files.
- **`managed_files`** — the wider set including hybrid files, which are
  surfaced by symlink, never gitignored, and only ever strip-edited. Defaults
  to `generated_files()`, so a fully-owned integration implements just the one.
- **`verify`** — drift detection, deliberately separate from `check`: `check`
  answers "is the environment able to do this", `verify` answers "has what we
  wrote been changed underneath us".
- **`member_incompatibility`** — report a member the integration's own tooling
  cannot build with. Results are collected in one place and shared by
  `rwv doctor` and `rwv update`.

## `IntegrationContext`

Each hook receives an `IntegrationContext` (`src/integration.rs`) with:

| Field | Description |
|---|---|
| `output_dir` | Where generated files are written — the primary root, or the workweave directory |
| `workspace_root` | Where repos live on disk. In a workweave this still points at the primary root, so detection works when clones are not duplicated |
| `project` | The active project name |
| `repos` | Repo entries from the project's `rwv.yaml`, as an ordered `(path, entry)` list |
| `config` | Per-integration config from the `integrations:` key in `rwv.yaml` |
| `all_repos_on_disk` | All repos found on disk under registry directories. Computed once, shared across integrations |
| `all_project_paths` | All project paths. Computed once, shared across integrations |
| `detection_cache` | Manifest filename → repo paths containing it. Populated once per activation/check cycle, before any integration runs |
| `workweave` | The project's `workweave:` config, if any. Present so an integration can detect a name claimed by two sections at once |

## Helper methods

- **`ctx.active_repos()`** — filters out `reference` repos, which are read-only
  and not part of the build graph.
- **`ctx.detect_repos_with_manifest(filename)`** — active repos containing a
  given manifest file (e.g. `"package.json"`), served from `detection_cache`
  when warm and falling back to a live scan under `workspace_root`.

Both are on-disk gated. A repo declared in `rwv.yaml` but not cloned is not
"pending" — it is simply absent from every list, which is why an intent verb
withholds authoring when the member set is incomplete rather than writing a
smaller-but-wrong file.

## Registration

New integrations are registered in `builtin_integrations()` in
`src/integrations/mod.rs`. Add a `Box::new(YourIntegration)` entry alongside the
existing built-ins; the rest of `rwv` picks it up automatically. The returned
order is the order everything runs in.

## Style guidelines

- **Auto-detection over explicit config.** Where possible, detect whether there
  is work to do (`npm-workspaces` checks for repos with `package.json`) rather
  than requiring the user to opt in. Reserve explicit opt-in for cases where
  auto-detection is not safe — `gita` is opt-in because it depends on an
  external tool the user may not have installed.
- **No-op on no-match.** If auto-detection finds nothing, generate nothing and
  raise no error. Activation should succeed.
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
