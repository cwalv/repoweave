# Local development

How to edit rwv's source, rebuild, install locally, and verify the new binary is picked up.

This is the **dogfooding** path — distinct from the end-user install ([README](../README.md#install)) and the release path ([releasing.md](./releasing.md)). End users get pre-built wheels from GitHub Releases; releasers tag, build wheels, and publish. Contributors iterate with the loop below.

## Build

From the repo root:

```bash
cargo build --release
```

Use `--release`, not debug. Hooks (`WorktreeCreate`, `WorktreeRemove`, Gas City `pre_start`) invoke rwv per event, and debug builds are noticeably slower to start.

The binary lands at `target/release/rwv`.

## Install

Cargo has no `cargo install -e` analog of `uv tool install -e`. The practical equivalent is a symlink that points at `target/release/rwv`; every subsequent `cargo build --release` updates what's on `PATH` in place, no re-install needed.

Two ways to wire it up. Pick one — they achieve the same thing.

### Option A — symlink in `~/.local/bin` (simplest)

One-time setup:

```bash
cd github/cwalv/repoweave
cargo build --release
ln -sf "$(pwd)/target/release/rwv" ~/.local/bin/rwv
```

After any source change:

```bash
cargo build --release
```

This is also where `install.sh` drops its binary, so the dev symlink and a released install occupy the same path — the dev symlink wins. To return to the released version, delete the symlink and re-run `install.sh` (or `uv tool install repoweave`).

### Option B — `uv tool install --editable`

Mirrors the shape of the released `uv tool install repoweave` path. Uses the editable-install fallback in the Python wrapper (`python/repoweave/src/repoweave/__init__.py`'s `_find_binary()` checks `<pkg_dir>/bin/rwv` before the installed scripts dir).

One-time setup:

```bash
cd github/cwalv/repoweave
cargo build --release
mkdir -p python/repoweave/src/repoweave/bin
ln -sf "$(pwd)/target/release/rwv" python/repoweave/src/repoweave/bin/rwv
uv tool install --editable ./python/repoweave --force
```

After any source change:

```bash
cargo build --release
```

The editable Python wrapper resolves through the symlink on each invocation, so the fresh binary is picked up immediately.

## Verify

```bash
which rwv           # resolves to ~/.local/bin/rwv (either option)
rwv --version       # matches the freshly built version
uv tool list        # option B: shows repoweave (editable)
```

`rwv --version` is the ground truth — if it reports the expected version and your source changes are visible in behavior, the loop is working.

## Hook interaction

`rwv` is invoked fresh per event — there's no daemon. Any hook that shells out to `rwv` (the built-in `WorktreeCreate` / `WorktreeRemove` hooks, Gas City `pre_start`, editor integrations) picks up the new binary on the next invocation. No restart required.

## Going back to the released version

**Option A:**

```bash
rm ~/.local/bin/rwv
curl -fsSL https://cwalv.github.io/repoweave/install.sh | sh
# or: uv tool install repoweave
```

**Option B:**

```bash
uv tool uninstall repoweave
uv tool install repoweave
```

## When to cut a release

Local development is local-only — contributors and agents iterate against source. External consumers (`uv tool install repoweave`, `install.sh`) get whatever is on GitHub Releases. When a change needs to reach them, follow [releasing.md](./releasing.md) to tag, build platform wheels, and publish.
