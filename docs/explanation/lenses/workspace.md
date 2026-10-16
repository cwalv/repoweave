# The workspace lens

> A project is the machine-readable answer to "what do I need to work on X?"

This lens is the foundation. Pick it up first if you've ever inherited a stale setup README, lost an afternoon to wrong-version clones, or stood in a folder of repositories not knowing which ones are the *right* ones for your current task.

repoweave moves the source of truth for a development environment out of human memory and out of stale documentation, and into a versioned **project repo**. A new machine goes from `git clone` to a fully-functional, correctly-versioned environment via one `rwv fetch`.

## The two kinds of directory

A weave holds two kinds of subdirectories, and the path tells you which is which:

| Kind | Path | Purpose |
|---|---|---|
| **Project** | `projects/{name}/` | Coordination. `rwv.toml`, `rwv.lock`, docs. No importable code. |
| **Manifest repo** | `{registry}/{owner}/{repo}/` | Code. Build tools look here. Other repos import from here. |

The project repo *governs* the workspace: it carries the manifest (which repos, with what roles), the lock (pinned revisions), and any cross-cutting docs that don't belong to any single repo (architecture, decision records, onboarding notes). It is a normal git repo with normal git history — but it doesn't contain importable code, which keeps its history pure: the project's coordination story isn't entangled with any one library's revision log.

The manifest repos are the work surfaces. They're regular clones — `cd github/chatly/server && git status` works. No bare repos, no `.git` file indirection, universal tool compatibility.

## Why split coordination from code?

Most multi-repo setups put the "what" of the project — which repos at which versions — in a wiki page, a setup README, or someone's head. The project repo formalizes it:

- **Discoverability.** `rwv fetch <url>` makes joining a project as easy as cloning a single repository.
- **History purity.** The project's coordination history (manifest changes, lock updates) is decoupled from any individual library's revision log. The "we adopted X dependency" decision lives where it belongs — alongside other project decisions — not in some library's commit history as noise.
- **A home for scraps.** Project-wide notes, design documents, and small coordination scripts can land in the project repo without forking some other repository or inventing a "contrib" folder.
- **Lightweight.** The project repo carries no code, so it stays fast to clone and move.

It is the *owner* of the workspace without being a *parent* in the filesystem sense — every manifest repo stays sovereign.

## Roles: change resistance, made explicit

Every repo in a project has a **role**. The role describes the repo's relationship to *this* project — not some intrinsic property of the repo itself.

| Role | Change resistance | Meaning |
|---|---|---|
| `owned` | None | Your code. Change freely. |
| `fork` | Low | Forked upstream. Ideally accept changes upstream. |
| `dependency` | Medium | You build against it. Changes need upstream acceptance. |
| `reference` | High | Cloned for reading/study. No local changes. |

Roles are *per-project*. The same repo can be `owned` in one project and `dependency` in another. The active project's `rwv.toml` determines which role applies.

The role is doing three jobs at once:

1. **Cognitive load**, for humans. A `reference` repo means "I don't need to understand how to build or test this; I just need to read it." `owned` means "this is my work surface." You can stop wondering whether the third-party library next to your work is something you're expected to be familiar with.

2. **Blast radius**, for agents. An LLM driving `rwv` sees the role and knows what it's *allowed* to mutate. A `reference` repo is read-only as far as the agent is concerned. A `dependency` is "can read, can't modify." `owned` is "go ahead." The role is a machine-readable safety boundary.

3. **Build-graph membership.** `reference` repos are excluded from generated ecosystem workspace files (`go.work`, `package.json` workspaces, Cargo workspace members). They're visible to humans browsing the tree, but invisible to build tools — so a study copy of someone else's code doesn't accidentally become a build dependency.

See [reference/roles](../../reference/roles.md) for the full definitions and the change-resistance semantics in detail.

## Activation: making the workspace coherent

A weave can hold multiple projects. Only one is active at a time. The active project is what's named in `.rwv-active` at the primary weave root.

Selection is primary-only. A workweave's project is fixed when the workweave is created and cannot be switched, so a workweave root carries a `.rwv-workweave` marker naming its project *instead of* a pointer — the two files are mutually exclusive, and `rwv doctor` reports a root carrying both as a `weave-root-identity-conflict`.

Intent verbs (`rwv add`, `rwv remove`, `rwv update`, `rwv role`, and `rwv doctor --fix`) author and merge managed keys into ecosystem workspace files in the project directory (`package.json` workspaces, `go.work`, `Cargo.toml` with `[workspace]`, `pyproject.toml` with `[tool.uv.workspace]`), preserving user-authored content in each file. Then `rwv activate <project>` makes that project live at the primary weave root:

1. Updates `.rwv-active`.
2. Verifies integration files in `projects/<project>/` and surfaces symlinks at the weave root so ecosystem tools see them where they expect.
3. Runs integration install hooks (`--no-materialize` skips hooks; `rwv materialize` runs hooks and settles drifted generated files inside any workspace without switching projects).

This is the antidote to manual wiring hell:

- **No `npm link`.** You don't have to manually link packages every time you open a terminal. Intent verbs keep `package.json` workspaces (or `go.work`, or `Cargo.toml`) aligned with `rwv.toml`, and `rwv activate` surfaces them at the weave root.
- **Deterministic paths.** Build tools see a consistent view. Two projects that use different versions of a shared library can't accidentally cross-contaminate.
- **Low overhead.** Symlinks are cheap and ephemeral — losing one costs nothing, because the source of truth always remains in the project repo. `rwv activate` re-creates them at the weave root; inside a workweave, where `activate` is refused, `rwv doctor --fix` re-surfaces them and `rwv materialize` runs install hooks.

`.rwv-active` is the single source of truth for what's active in a workspace. There is no CWD-based override: cd-ing into `projects/<name>/` does not switch the active project. Action verbs read `.rwv-active`; if you want to operate on a non-active project, use `--project <name>` for a one-shot or `rwv activate <name>` to switch.

See [switch projects](../../how-to/switch-projects.md) for the operational recipe.

## Reproducibility: `rwv.lock`

The lock file pins each manifest repo to an exact 40-character commit SHA — never a tag or branch name, even when a tag points at `HEAD` (tags and branches can move; a commit SHA cannot):

`projects/web-app/rwv.lock`, with the entry fields not under discussion elided:

```json
{
  "repositories": {
    "github/chatly/protocol": { "version": "7a3b2c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b" },
    "github/chatly/server":   { "version": "c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3" }
  }
}
```

Release channels and milestones are represented at the project layer by branches and tags on the project repo (`projects/<name>/`), each carrying its own committed `rwv.lock` (see [pyramid of stability](../joints/pyramid-of-stability.md)).

`sha256sum rwv.lock` is the project fingerprint. Two machines with the same checksum have identical source for every repo in the project. This is the multi-repo analog of `git rev-parse HEAD` on a monorepo.

The lock file is *derived state*: produced by `rwv lock`, `rwv update`, or `rwv sync`, not edited by hand. It is committed to the project repo so it has a history (you can trace exactly when which repo was at which revision), and regenerated whenever the cross-repo state changes. See [lock-as-derived](../joints/lock-as-derived.md) for why the lock is always derived.

## Provenance: the registry/owner/repo path

Manifest repos live at `{registry}/{owner}/{repo}/`. The first segment is a short name for where the repo came from — `github` for `github.com`, `gitlab` for `gitlab.com`, custom names for self-hosted hosts.

This follows Go's GOPATH precedent (`$GOPATH/src/github.com/owner/repo`), shortened. It does several jobs:

- **Provenance.** The on-disk path tells you where the repo came from, without consulting any metadata.
- **No collisions.** Two `web-app` repos owned by different organizations can coexist on disk without renaming.
- **Discovery.** `find github/chatly/` lists every chatly repo on disk; `find . -name '.git' -type d` lists everything across all owners.

Project paths are simpler — `projects/{name}/`. They use short names because there's typically one of each per workspace; if names collide across organizations, `rwv` prompts for a scoped path (`projects/{owner}/{name}/`). The asymmetry is intentional: manifest repos are physical clones whose path encodes their origin; projects are coordination entities referred to by short name.

## What ecosystem tools see

Build tools don't know there are multiple repos. They see a directory with workspace files at the root. npm sees a `package.json` with workspace globs; Go sees a `go.work` listing modules; Cargo sees a `Cargo.toml` with `[workspace]` members.

This is the [monorepo lens](./monorepo.md)'s payoff: ecosystem ergonomics on top of polyrepo sovereignty. The repos stay independent, but the build experience is the same as a monorepo. `cargo test --workspace`, `npm test --workspaces`, `go test ./...` — all work across repos, no manual wiring.

Integrations (`npm-workspaces`, `go-work`, `cargo-workspace`, `uv-workspace`, `pnpm-workspaces`, ...) translate between the project manifest and the ecosystem's workspace format. They auto-detect — a repo with `package.json` triggers `npm-workspaces`, a repo with `go.mod` triggers `go-work`. See [reference/integrations](../../reference/integrations/index.md).

## The shape, in one paragraph

A project repo lives at `projects/{name}/` and carries `rwv.toml` (which repos, with what roles), `rwv.lock` (pinned 40-character commit SHAs), and any cross-cutting docs. Manifest repos live at `{registry}/{owner}/{repo}/` as regular clones. Intent verbs (`add`, `remove`, `update`, `role`) merge managed keys into ecosystem workspace files in `projects/{name}/`, preserving user-authored content; one project is *active* in a primary weave at a time (`.rwv-active`), and `rwv activate` surfaces those workspace files to the weave root via symlinks and runs install hooks. The lock is derived state — output of `rwv lock`, never an input to merge. Roles tag each repo's change resistance, doing triple duty as a human cognitive aid, an agent safety boundary, and a build-graph membership flag.

## Related

- [Pyramid of stability](../joints/pyramid-of-stability.md) — what "canonical tip" means across a project
- [Lock-as-derived](../joints/lock-as-derived.md) — why the lock is output-only
- [Workweave hierarchy](../joints/workweave-hierarchy.md) — when one workspace isn't enough
- [Monorepo lens](./monorepo.md) — what the workspace model gives you ergonomically
- [Agent lens](./agent.md) — what the workspace model gives you for automation
