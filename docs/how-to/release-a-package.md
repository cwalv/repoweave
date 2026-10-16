# Release a package

Snapshot every member repository's 40-character commit SHA into `rwv.lock`, publish the cross-repo state, and tag any packages that publish to external registries:

```bash
rwv lock --commit
rwv push
```

`rwv.lock` records the exact 40-character commit SHA of every member repository unconditionally. For internal-only repositories that do not publish to an external package registry, committing `rwv.lock` (and tagging `projects/<project>/` if you cut release tags at the project level) is the entire release. Any machine or CI job reproduces that exact source tree with:

```bash
rwv fetch chatly/web-app --frozen
```

## Publish to external registries

During development, workspace integrations resolve cross-repo imports to local checkouts without version bumps or registry publishes. When a member repository is published externally (crates.io, npm, PyPI, Go module proxy), tag and publish in dependency order after locking.

### Cargo (crates.io)

```bash
cd github/chatly/protocol
cargo publish                          # publishes the version in Cargo.toml
git tag v1.1.0 && git push origin v1.1.0
```

`cargo check` at the weave root catches version-constraint mismatches across workspace members before publish.

### Go (module proxy)

```bash
cd github/chatly/protocol
git tag v1.1.0 && git push origin v1.1.0
# consumers: go get github.com/chatly/protocol@v1.1.0
```

### npm

```bash
cd github/chatly/web
npm version 1.1.0                      # bumps package.json and creates a git commit + tag
npm publish
cd ../..
rwv lock --commit
rwv push && git -C github/chatly/web push origin v1.1.0
```

### Python (PyPI)

```bash
cd github/chatly/ml-service
# bump version in pyproject.toml and commit
git tag v1.1.0 && git push origin v1.1.0
uv build && uv publish
cd ../..
rwv lock --commit
```

Note that `uv`'s `[tool.uv.sources]` `workspace = true` overrides version constraints inside the weave; test the published wheel in a clean environment to verify external version ranges.

## Compatible vs. breaking version bumps

- **Compatible bump (`protocol` `1.0.0` → `1.1.0`, `server` requires `^1.0`):** Bump `protocol`, commit, tag `v1.1.0`, and run `rwv lock --commit` to record the new commit SHAs in `rwv.lock` alongside the updated ecosystem lockfile (`Cargo.lock`, `package-lock.json`, `uv.lock`).
- **Breaking bump (`protocol` `2.0.0`, `server` requires `^1.0`):**
  1. Update `protocol` to `2.0.0` and `server`'s dependency constraint to `^2.0`, and commit both.
  2. Run `rwv lock --commit` to snapshot both repositories' commit SHAs into `rwv.lock`.
  3. Tag and publish `protocol` `v2.0.0` first, then `server`.

## Include the commit SHA in binary versions

Embedding the git revision as semver build metadata (`2.1.0+7a3b2c1`) makes it immediately clear which commit a deployed binary was built from:

- **Cargo / npm:** Both `Cargo.toml` and `package.json` accept `+` build metadata in `version`.
- **Python:** PEP 440 supports `+local` version identifiers.
- **Go:** `runtime/debug.BuildInfo` embeds VCS revision metadata automatically during `go build`.

## Related

- [Lock as derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` records exact commit SHAs derived from member checkouts
- [Pyramid of stability](../explanation/joints/pyramid-of-stability.md) — project branches and tags as release channels
- [File formats reference](../reference/formats.md) — `rwv.lock` schema
- [Monorepo lens](../explanation/lenses/monorepo.md) — developing across repositories without intermediate registry publishes
