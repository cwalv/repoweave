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
job; one under which promised behavior silently stops is not a fallback.

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

## Why 2.43

Every merge the backend computes as refs (`merge_commit_as_refs` in
`src/git/merge.rs`) runs `merge-tree --write-tree`, which arrived in 2.38.
Under a derived-content policy that merge reads attributes as of the commit it
keeps, not from any checkout's files: it passes that commit as git's global
`--attr-source`, so no checkout's `.gitattributes` is read and a kept commit
holding none contributes none.
`--attr-source` arrived in 2.41, and `merge-tree --write-tree` given it
segfaults on 2.41 and 2.42; 2.43 fixed it (its release notes: "git merge-tree"
used to segfault when the "--attr-source" option is used). 2.43 is therefore
the first release on which the merge runs.

`git hook run` (2.36), which finishing
a workspace whose files rwv placed itself runs (`WorkspacePopulation::finish_checkout`,
see [member-population.md](member-population.md)), is below the floor.

## Detected capabilities

None. A command newer than the floor is either a detected capability, a
`Vcs` method returning an `Option` answered from the one `git version` reading
with a fallback that does the same work, or it raises the floor.

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
| `merge-tree --write-tree` | 2.38 | the floor |
| `--attr-source` given to `merge-tree --write-tree` | 2.43 (2.41 for the option; `merge-tree` crashes on it before 2.43) | the floor |

The lock merge driver reads the heads a merge names from the `GITHEAD_<id>`
variables `git merge` and `git pull` export to a merge driver; 2.43 exports
them as the newest releases do, and with none exported the driver takes no
side and leaves git's conflict.

The suite itself also sets git up with `GIT_CONFIG_COUNT` (2.31) and
`GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` (2.32), all below the floor. A test
fixture is held to the floor as production is: `worktree add --orphan` (2.42),
for one, is not available to it.

## Testing at the floor

`scripts/ci-local.sh --stages=git-floor` builds the floor release with
`scripts/git-floor.sh` into a cache outside the repository (once per machine:
it needs a C compiler, make and zlib's headers) and runs the test stage with
that git first on `PATH`. No default run reaches it, because it downloads;
`--tier=release` runs it, and CI runs it as its own job in
`.github/workflows/ci-checks.yml`. The build leaves out
message translations, so a test that switches git to German runs in English
there and says so.
