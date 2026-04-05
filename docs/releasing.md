# Releasing

Workspace wiring gives you a monorepo-style development experience: cross-repo imports resolve locally, no version bumps needed during development. `rwv lock` captures the exact cross-repo state — the lock file is the project's version.

For repos that are only consumed within the project, this is the whole story. The lock file pins exact revisions, semver is optional, and you never need to publish to a registry. `sha256sum rwv.lock` is the project fingerprint — two machines with the same checksum have identical source.

Some repos are also consumed outside the project — published as packages on npm, crates.io, PyPI, etc. These repos need tagged releases and semver. The workspace wiring still eliminates the development-time version dance, but at release time you tag, publish, and update downstream version pins. The decision is per-repo: a project can have repos on the light internal path alongside repos that publish with full semver.

## Locking

When you're ready to capture the current state:

```bash
rwv lock
```

Reads the HEAD revision from each repo and writes `projects/web-app/rwv.lock`. If a tag exists at HEAD, the lock records the tag name; otherwise the revision ID.

```bash
cd projects/web-app
git add rwv.lock && git commit -m "lock: payment feature"
git push
```

For the internal model, this is the whole release: the committed lock file pins every repo to an exact revision. Reproduce it anywhere with `rwv fetch --locked`.

## Publishing: what workspace wiring gives you

During development, workspace wiring eliminates the development-time version dance: edit B, test A — no bump, no publish, no install. The iteration loop is just edit and test, across repos, like a monorepo.

At release time, the steps are: bump version, tag, publish, install. But most of these are simpler than they seem, because ecosystem tools and version constraints do the heavy lifting.

### Constraint checking during development

When you bump a package version in the workspace, most ecosystem tools immediately tell you if consumers' constraints are incompatible:

| Ecosystem | Catches constraint mismatch in workspace? | Details |
|---|---|---|
| **Cargo** | **Yes** | `cargo check` fails: "failed to select a version for the requirement" |
| **Go** | **Yes** | Module path convention (`v2` = different path) forces explicit migration |
| **npm** | **Yes** | `npm install` fails: "No matching version found" |
| **uv/Python** | **No** | `workspace = true` silently overrides constraints — version mismatch only surfaces outside the workspace |

This means you discover constraint incompatibilities *during development*, before publishing — except in Python where you need to be more careful.

### The common case: compatible bumps

If B bumps from 1.0.0 to 1.1.0 and A depends on `B ^1.0`, no constraint update is needed. The release sequence is just:

```bash
rwv lock                                          # capture tested state
cd github/chatly/protocol
git tag v1.1.0 && git push origin v1.1.0          # tag and publish
cd ../server
git tag v2.1.0 && git push origin v2.1.0          # A picks up new B on next install
```

No reference edits. The ecosystem lock file (`Cargo.lock`, `package-lock.json`) resolves the range to the new version at install time.

### Breaking changes: constraint updates

If B bumps to 2.0.0 (major/breaking) and A depends on `B ^1.0`, the ecosystem tool catches this in the workspace. Update A's constraint, then release in dependency order:

```bash
# During development: bump B, ecosystem tool flags A's constraint
# Fix A's constraint to ^2.0
rwv lock
cd github/chatly/protocol
git tag v2.0.0 && git push origin v2.0.0
cd ../server
# Cargo.toml already updated to ^2.0 during development
git tag v3.0.0 && git push origin v3.0.0
```

### Per-ecosystem release sequences

Each ecosystem has its own publish command. The pattern is the same — tag, publish, move on — but the exact invocations differ:

**Cargo (crates.io):**

```bash
cd github/chatly/protocol
cargo publish                       # publishes from Cargo.toml version
git tag v1.1.0 && git push origin v1.1.0
```

**Go (module proxy):**

```bash
cd github/chatly/protocol
git tag v1.1.0 && git push origin v1.1.0
# consumers: go get github.com/chatly/protocol@v1.1.0
```

**npm:**

```bash
cd github/chatly/web
npm version 1.1.0                   # bumps package.json, creates git tag
npm publish
git push origin v1.1.0
```

**Python (PyPI):**

```bash
cd github/chatly/ml-service
# bump version in pyproject.toml
git tag v1.1.0 && git push origin v1.1.0
uv build && uv publish
```

## What the lock file tells you

The lock file format itself encodes release state: entries with tag names (e.g., `v1.5.0`) are already released; entries with revision IDs are unreleased. Read `rwv.lock` to see what needs attention:

```yaml
repositories:
  github/chatly/protocol:
    version: v1.5.0              # released
  github/chatly/server:
    version: e1f2a3b4c5d6...     # unreleased — needs a tag
```

| What | Who knows |
|------|-----------|
| Which repos need a release | `rwv.lock` — tag = released, revision ID = unreleased |
| Which repos depend on which | Ecosystem manifests (`Cargo.toml`, `package.json`, `go.mod`) |
| Whether a version bump is compatible | Ecosystem tools — Cargo, Go, npm catch mismatches in the workspace |
| How to update version pins | Ecosystem tools (`cargo update`, `npm install`, `go get`) |
| What was tested together | `rwv.lock` — the cross-ecosystem snapshot |

repoweave owns the first and last rows — which repos, and the cross-repo snapshot. The middle rows are the ecosystem's job. In a multi-ecosystem project — a Go service using protobufs that a TypeScript frontend also uses — no single ecosystem tool sees all the repos that were tested together. The lock file is the only artifact that captures that cross-ecosystem state.
