# Cache builds across workweaves

Each workweave gets its own ecosystem tool state — per-workweave `target/`, `node_modules/`, `.venv/`. That isolation is the correct default: a PR's dependency changes cannot corrupt the primary weave's build. The cost is that N concurrent workweaves of the same repos mean N cold builds.

This page covers host-level cache strategies, per ecosystem, that preserve isolation while sharing safe build artifacts across workweaves. Each strategy is labelled for concurrency safety — whether it is safe to run simultaneously in multiple workweaves without corruption or lock contention.

## Summary

| Ecosystem | Strategy | Concurrency-safe? | Setup needed? |
|---|---|---|---|
| Rust / cargo | sccache via the `rustc-wrapper` integration knob | Yes | Install sccache; one line in `rwv.toml` |
| Rust / cargo | sccache via `RUSTC_WRAPPER` env var | Yes | Install sccache; export one env var per shell |
| Rust / cargo | Shared `CARGO_TARGET_DIR` | No — contended | Not recommended for concurrent workweaves |
| Node / pnpm | pnpm content-addressed store | Yes | None — already shared by default |
| Node / npm | (no equivalent) | — | No safe shared cache available |
| Python / uv | uv global cache | Yes | None — already shared by default |

---

## Rust / cargo: sccache (recommended)

[sccache](https://github.com/mozilla/sccache) wraps the Rust compiler. It intercepts compilation units, computes a content-addressed cache key over the inputs (source file, flags, target, host toolchain), and serves cached object files on a hit. Because the key includes all relevant inputs, concurrent workweaves building the same crate at the same compiler flags share compiled artifacts without interfering with each other.

**Install:**

```bash
cargo install sccache
# or via your OS package manager / prebuilt release
```

**Enable via the integration knob** (preferred — travels with the project):

```toml
# rwv.toml
[integrations.cargo-workspace]
rustc-wrapper = "sccache"
```

At each activation/materialization — including the one `rwv fetch` runs on
a fresh machine — rwv looks sccache up on `PATH` and, when present, writes
`[build] rustc-wrapper = "sccache"` into the generated
`.cargo/config.toml`. Machines without sccache get no key and build
unwrapped; installing sccache later takes effect at the next
materialization. No shell configuration anywhere. See
[reference/integrations/cargo-workspace](../reference/integrations/cargo-workspace.md)
for the ownership and drift semantics.

**Or enable globally via the environment** (host-level alternative) by
adding to your shell profile (`~/.zshenv`, `~/.bashrc`, etc.):

```bash
export RUSTC_WRAPPER=sccache
```

The env var wins over the config-file key when both are set, and covers
cargo invocations outside any weave. Each `cargo build` routes through
sccache automatically either way. The cache directory defaults to
`~/.cache/sccache` (Linux/macOS) and is shared across all workweaves and
all shells.

**Verify it is active:**

```bash
sccache --show-stats
```

The `requests` and `cache_hits` counters rise after a warm build. A second workweave building the same crate should show a high hit rate.

**How the cache key works:** sccache keys on the full compiler invocation — crate source, all flags, the compiler version, and the target triple. Two workweaves on different branches with different source files get different keys and build independently. Two workweaves that happen to build the same unmodified crate (common for dependencies that neither branch touched) share the cached object files.

**Per-workweave `target/` directories are kept.** sccache caches compiled object files, not the final linked artifacts or metadata Cargo stores in `target/`. Each workweave still has its own `target/`, so Cargo's incremental state, fingerprints, and linked binaries are isolated. sccache provides a compile-time shortcut, not a shared `target/`.

**Concurrency-safe:** yes.

---

## Rust / cargo: shared `CARGO_TARGET_DIR` (not recommended for concurrent workweaves)

An alternative sometimes considered is pointing all workweaves at a single `target/` directory:

```bash
export CARGO_TARGET_DIR=~/.cargo/shared-target   # not recommended
```

This saves disk space but introduces two problems under concurrent workweaves:

- **Build-lock contention.** Cargo serializes writes to `target/` using a file lock. Concurrent builds from separate workweaves queue behind the lock; the wall-clock win from a warm cache is offset by wait time.
- **Profile and feature cross-talk.** Cargo's fingerprinting in `target/` keys on the feature set and profile. If two workweaves build the same crate with different features (one with `--features foo`, another without), Cargo may thrash — each build invalidates the other's fingerprint and forces a rebuild. This is worse than cold builds under heavy concurrency.

If you run workweaves serially (not simultaneously), a shared `CARGO_TARGET_DIR` can reduce disk use and rebuild time. Under concurrent workweaves, prefer sccache.

**Concurrency-safe:** no.

---

## Node / pnpm: no extra setup needed

pnpm's package store is content-addressed and shared across all installs on the machine by default. When each workweave runs `pnpm install`, packages are hardlinked from the shared store (`~/.local/share/pnpm/store` on Linux, `~/Library/pnpm/store` on macOS) into the workweave's `node_modules/`. Only the hardlinks are per-workweave; the package content on disk is deduplicated automatically.

Nothing needs to be configured. The per-workweave `node_modules/` is still isolated — a dependency change in one workweave does not affect another — but the disk cost is close to a single copy, and the install step after `rwv workweave create` fills quickly from the shared store.

**Concurrency-safe:** yes — the pnpm store uses atomic content-addressed writes; concurrent installs from different workweaves do not race.

---

## Node / npm: no equivalent

npm does not have a content-addressed package store equivalent to pnpm's. Each workweave's `node_modules/` is an independent copy. There is no host-level cache mechanism that is safe under concurrent workweaves.

If build time under npm is a concern, consider switching the project to pnpm (see [add an integration](./add-an-integration.md)).

---

## Python / uv: no extra setup needed

uv maintains a global package cache at `~/.cache/uv` (Linux) / `~/Library/Caches/uv` (macOS). When each workweave runs `uv sync`, packages are populated into the workweave's `.venv/` from the shared cache. The `.venv/` itself is per-workweave and isolated; the download and wheel-build work is shared.

Nothing needs to be configured. The global cache is populated on first use and reused by every subsequent `uv sync` on the same machine, across all workweaves and all shells.

**Concurrency-safe:** yes — uv's cache uses content-addressed storage with atomic writes; concurrent `uv sync` calls do not corrupt each other.

---

## Persistent environment variables

For sccache, the `RUSTC_WRAPPER` variable must be set in every shell that runs `cargo`. The most reliable place is a shell init file that runs in non-interactive shells:

- **zsh:** `~/.zshenv` (sourced for all zsh invocations, including those spawned by agents)
- **bash:** `~/.bashrc` (sourced for interactive shells; add to `~/.bash_profile` or `~/.profile` for login shells)

Agent workweaves inherit the environment of the shell that spawned the agent process. If your agent harness starts from a login shell, `~/.zshenv` or `~/.profile` covers it. Check with:

```bash
printenv RUSTC_WRAPPER
```

---

## Related

- [hand a task to an agent](./hand-task-to-agent.md) — agent workweave setup and environment
- [create a feature workweave](./create-feature-workweave.md) — workweave lifecycle and per-workweave tool state
- [review a PR with a workweave](./review-pr-with-workweave.md) — concurrent workweave use case where build caching matters most
- [reference/integrations/cargo-workspace](../reference/integrations/cargo-workspace.md) — Rust workspace integration details
- [reference/integrations/pnpm-workspaces](../reference/integrations/pnpm-workspaces.md) — pnpm integration details
- [reference/integrations/uv-workspace](../reference/integrations/uv-workspace.md) — uv integration details
