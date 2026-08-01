# Release a package

Workspace wiring eliminates the version dance *during* development: cross-repo imports resolve to local clones, so editing `protocol` and testing `server` needs no bump, no publish, no install. The version dance only kicks in *at release time* — when a repo publishes to npm, crates.io, PyPI, etc.

This page is the release-time recipe per ecosystem. For repos consumed only inside the project (no external publish), the lock file is the whole release — no per-repo tagging needed.

## Capture the cross-repo state first

```bash
rwv lock
cd projects/web-app
git add rwv.lock && git commit -m "lock: payment feature"
git push
```

`rwv.lock` pins every repo to an exact revision. `sha256sum rwv.lock` is the project fingerprint — two machines with the same checksum have identical source.

For internal-only repos this is the whole release. Reproduce anywhere with:

```bash
rwv fetch chatly/web-app --frozen
```

## Per-ecosystem publish

The pattern is the same — tag, publish — but the exact invocations differ.

### Cargo (crates.io)

```bash
cd github/chatly/protocol
cargo publish                          # publishes from Cargo.toml version
git tag v1.1.0 && git push origin v1.1.0
```

`cargo check` in the workspace catches version-constraint mismatches before publish.

### Go (module proxy)

```bash
cd github/chatly/protocol
git tag v1.1.0 && git push origin v1.1.0
# consumers: go get github.com/chatly/protocol@v1.1.0
```

Module path conventions (`v2` = different path) force explicit migration on majors.

### npm

```bash
cd github/chatly/web
npm version 1.1.0                      # bumps package.json, creates git tag
npm publish
git push origin v1.1.0
```

`npm install` in the workspace catches "no matching version" before publish.

### Python (PyPI)

```bash
cd github/chatly/ml-service
# bump version in pyproject.toml
git tag v1.1.0 && git push origin v1.1.0
uv build && uv publish
```

Watch out: uv's `workspace = true` silently overrides version constraints, so version-mismatch only surfaces *outside* the workspace. Test the published artifact in a separate environment.

## Compatible bumps

If `protocol` bumps from 1.0.0 to 1.1.0 and `server` depends on `protocol ^1.0`, no constraint update is needed:

```bash
rwv lock
cd github/chatly/protocol
git tag v1.1.0 && git push origin v1.1.0
cd ../server
git tag v2.1.0 && git push origin v2.1.0
```

The ecosystem lock file (`Cargo.lock`, `package-lock.json`) resolves the range to the new version at install time.

## Breaking bumps

If `protocol` bumps to 2.0.0 and `server` depends on `^1.0`:

1. The workspace's ecosystem tool catches it during development (`cargo check`, `npm install` fail).
2. Update `server`'s constraint to `^2.0`.
3. `rwv lock` to recapture state.
4. Release `protocol` first, then `server`.

```bash
# server/Cargo.toml already updated to ^2.0 during development
rwv lock
cd github/chatly/protocol
git tag v2.0.0 && git push origin v2.0.0
cd ../server
git tag v3.0.0 && git push origin v3.0.0
```

## Include the revision in package versions

A useful convention: include the git revision in your published version via semver build metadata (`2.1.0+7a3b2c1`). The `+` suffix doesn't affect version precedence but carries provenance.

```
$ my-server --version
my-server 2.1.0+7a3b2c1
```

When debugging across repos you can immediately tell which commit each binary was built from. Most ecosystems support this:

- **npm**: `version` in `package.json` supports semver build metadata
- **Cargo**: `version` in `Cargo.toml` supports `+` metadata
- **Python**: PEP 440 uses `+local` for local version identifiers
- **Go**: `runtime/debug.BuildInfo` embeds VCS info via build flags

## What the lock file tells you

`rwv.lock` encodes release state per repo: entries with tag names are released, entries with revision IDs are unreleased.

```json
{
  "repositories": {
    "github/chatly/protocol": { "version": "v1.5.0" },
    "github/chatly/server": { "version": "e1f2a3b4c5d6..." }
  }
}
```

Read the lock to see what needs attention. `rwv doctor` flags unreleased entries the same way.

## Related

- [lock-as-derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is the canonical release artifact
- [reference/formats](../reference/formats.md) — `rwv.lock` shape
- [monorepo lens](../explanation/lenses/monorepo.md) — the "zero-version change" rationale
