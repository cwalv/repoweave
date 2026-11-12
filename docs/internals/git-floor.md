# The oldest git rwv runs on

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

**Audience:** someone adding a git command or option to the backend, or moving
the floor.

## The rule

The floor is the oldest release in which every command the backend runs
outside a detected capability exists. A command newer than the floor is
either a detected capability with a fallback that does the same work, or it
raises the floor. A fallback is right only when an older form does the same
job; one under which promised behaviour silently stops is not a fallback.

The floor is one constant, `MINIMUM_RELEASE` in `src/git.rs`. The refusal
names it through `Vcs::minimum_release`; `scripts/git-floor.sh` reads it from
that line to build the release it names; `git::floor_tests` holds the README,
the install page and the CLI reference to it.

## The check

`git version` runs once per process (`git_reading` in `src/git.rs`). The
dispatcher asks `Vcs::tool_readiness` before any verb that touches a
repository does anything (`drives_repositories` in `src/cli/dispatch.rs`) and
refuses with `unsupported-vcs-tool` for a release below the floor or an answer
naming no release, and with `missing-vcs-tool` where no `git` can be started.
Every detected capability answers from the same reading, so the check costs
the one spawn the first capability question would have made.

## Why 2.36

`git hook run` arrived in 2.36. Finishing a workspace whose files rwv placed
itself (`WorkspacePopulation::finish_checkout`, see
[member-population.md](member-population.md)) runs the `post-checkout` hook
that `worktree add --no-checkout` skipped, and `git hook run` is the only way to
have git run a hook as it would itself: the hooks directory, `core.hooksPath`,
and how a hook script is started on each platform are git's. Population was
once a capability detected at 2.36, and without it the backend answered no
population questions; measured against 2.31.0, that silently stopped making
`reference` members read-only, stopped checking out submodules in workweaves,
and stopped reporting file placement. That is promised behaviour disappearing,
not a fallback, so the floor rose to 2.36 instead.

Below that, the suite run against git built from the release tarballs:

- 2.30.0: `rev-parse --path-format=absolute` (2.31) is unknown and echoed back
  as text, so every path the backend reads through it is wrong, and nearly
  every test that drives a repository fails.
- 2.31.0: the population tests and every test of read-only `reference`
  members, workweave submodules and file placement fail, as above.

## Detected capabilities

| Capability | Release | Seam | Fallback |
|---|---|---|---|
| `merge-tree --write-tree` | 2.38 | `Vcs::tree_merges` | the merge runs in a checkout stopped before its commit (`Vcs::start_merge`), which the caller commits |

A capability is a `Vcs` method returning an `Option`, answered from the one
`git version` reading. Its fallback is tested on a new git through the scenario
harness's release shim (`Seed::old_git` in
`tests/scenario_harness_test/sandbox.rs`): a `git` earlier on `PATH` that
answers as the release before the capability and refuses the command, then
runs the real git. A new capability extends that shim.

## Commands newer than 2.25, and how each is held

Every git invocation in production goes through `GitCommand`
(`src/git/argv.rs`), so this list is drawn from the argv the backend builds.
"Release" is from git's release notes; each is covered by the git-floor stage
passing at the floor.

| Command or option | Release | Held by |
|---|---|---|
| `rebase --empty=drop` | 2.26 | the floor |
| `init --initial-branch` | 2.28 | the floor |
| `worktree repair` | 2.29 | the floor |
| `worktree list --porcelain` reporting `locked` | 2.30 | the floor |
| `rev-parse --path-format=absolute` | 2.31 | the floor (measured: 2.30 echoes it) |
| `worktree list --porcelain` reporting `prunable` | 2.31 | the floor |
| `git hook run` | 2.36 | the floor |
| `merge-tree --write-tree` | 2.38 | detected capability |

The suite itself also sets git up with `GIT_CONFIG_COUNT` (2.31) and
`GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` (2.32), all below the floor.

## Testing at the floor

`scripts/ci-local.sh --stages=git-floor` builds the floor release with
`scripts/git-floor.sh` into a cache outside the repository (once per machine:
it needs a C compiler, make and zlib's headers) and runs the test stage with
that git first on `PATH`. It is opt-in locally, because it downloads; CI runs
it as its own job in `.github/workflows/ci-checks.yml`. The build leaves out
message translations, so a test that switches git to German runs in English
there and says so.
