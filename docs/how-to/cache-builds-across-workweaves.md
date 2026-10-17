# Cache builds across workweaves

Each workweave isolates its own `target/`, `node_modules/`, and `.venv/` so concurrent builds never corrupt each other. Share compiled artifacts and package downloads across workweaves without sacrificing isolation by enabling a content-addressed compiler wrapper (`sccache` for Rust) alongside `pnpm` and `uv`'s built-in global stores:

```bash
# Install sccache and wire it into projects/<project>/rwv.toml
cargo install sccache
```

```toml
# projects/<project>/rwv.toml
[integrations.cargo-workspace]
rustc-wrapper = "sccache"
```

```bash
# Regenerate .cargo/config.toml so cargo routes through sccache when installed
rwv doctor --fix
```

## Summary by ecosystem

| Ecosystem | Strategy | Concurrency-safe? | Setup needed? |
|---|---|---|---|
| Rust / cargo | `sccache` via `rustc-wrapper` integration key | Yes | Install `sccache`; set `rustc-wrapper = "sccache"` in `rwv.toml` |
| Rust / cargo | `sccache` via `RUSTC_WRAPPER` env var | Yes | Install `sccache`; export `RUSTC_WRAPPER=sccache` in shell profile |
| Rust / cargo | Shared `CARGO_TARGET_DIR` | No (lock contention & feature thrashing) | Not recommended for concurrent workweaves |
| Node / pnpm | Content-addressed pnpm store | Yes | None — shared by default |
| Node / npm | Per-workweave `node_modules/` | — | No concurrency-safe shared store; prefer `pnpm` |
| Python / uv | Global uv wheel/package cache | Yes | None — shared by default |

For tracked source files themselves, `rwv` already shares disk blocks across workweaves automatically when populating worktrees — using copy-on-write reflinks (`FICLONE` on Linux, `clonefile` on macOS, block cloning on Windows) for all roles and read-only hardlinks for `reference` repos where supported. For gitignored static assets (such as local test fixtures or model weights) that you want symlinked or copied into new workweaves, declare `link = [...]` or `copy = [...]` under `[workweave]` in `rwv.toml`. The strategies below cover the build outputs and package managers that run on top of those checkouts.

## Rust / cargo: `sccache` (recommended)

[sccache](https://github.com/mozilla/sccache) wraps `rustc`, keys on the full compiler invocation (crate source, flags, compiler version, and target triple), and serves cached object files from `~/.cache/sccache` (Linux/macOS) on a hit. Each workweave keeps its own `target/` directory for Cargo fingerprints and linked binaries while sharing compiled dependency objects across workweaves.

### Enable per project via `rwv.toml`

Setting `rustc-wrapper = "sccache"` under `[integrations.cargo-workspace]` in `rwv.toml` instructs `rwv` to check whether `sccache` is on `PATH` whenever the workspace is materialized (including `rwv fetch`, `rwv activate`, `rwv materialize`, `rwv workweave create`, and `rwv doctor --fix`). When `sccache` is present on the host, `rwv` writes `[build] rustc-wrapper = "sccache"` into the generated `.cargo/config.toml`; on machines without `sccache`, the key is omitted (and the file removed when nothing else is in it) so `cargo` still builds cleanly.

### Or enable host-wide via shell environment

To enable `sccache` across all Cargo invocations on the machine (including outside any weave), export `RUSTC_WRAPPER` in a shell startup file that runs for both interactive and agent-spawned non-interactive shells (`~/.zshenv` for zsh, `~/.bashrc` and `~/.profile` for bash):

```bash
export RUSTC_WRAPPER=sccache
```

Verify that `sccache` is intercepting builds:

```bash
printenv RUSTC_WRAPPER
sccache --show-stats
```

## Rust / cargo: shared `CARGO_TARGET_DIR` (not recommended)

Pointing concurrent workweaves at a single `CARGO_TARGET_DIR` causes two problems:

- **Build-lock contention:** Cargo serializes writes to `target/` behind a file lock, so parallel agents in separate workweaves block on each other.
- **Feature and profile thrashing:** Building the same crate with different feature flags across two workweaves invalidates fingerprints in the shared `target/` and forces repeated rebuilds.

Use `CARGO_TARGET_DIR` only if you run workweaves strictly one at a time; for concurrent workweaves, use `sccache`.

## Node / pnpm and npm

- **pnpm:** `pnpm install` hardlinks packages from the machine-wide content-addressed store (`~/.local/share/pnpm/store` on Linux, `~/Library/pnpm/store` on macOS) into each workweave's isolated `node_modules/`. Writes to the store are atomic and concurrency-safe with zero extra configuration.
- **npm:** `npm` writes an independent copy of `node_modules/` per workweave and has no concurrency-safe shared package store. If install time across workweaves matters, switch the project to `pnpm` (see [Add an integration](./add-an-integration.md)).

## Python / uv

`uv sync` populates each workweave's isolated `.venv/` from uv's global content-addressed cache (`~/.cache/uv` on Linux, `~/Library/Caches/uv` on macOS). Concurrent `uv sync` runs across workweaves share downloaded wheels and built artifacts safely with no extra configuration.

## Related

- [Hand a task to an agent](./hand-task-to-agent.md) — running parallel agents in isolated workweaves
- [Create a feature workweave](./create-feature-workweave.md) — workweave creation and per-workweave tool state
- [Review a PR with a workweave](./review-pr-with-workweave.md) — reviewing pull requests in disposable workweaves
- [Cargo workspace integration reference](../reference/integrations/cargo-workspace.md) — `rustc-wrapper` and `.cargo/config.toml` ownership
- [pnpm workspaces integration reference](../reference/integrations/pnpm-workspaces.md) — pnpm workspace configuration
- [uv workspace integration reference](../reference/integrations/uv-workspace.md) — uv workspace configuration
