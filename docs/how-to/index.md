# How-to guides

Task-shaped recipes. Each page leads with the command; the conceptual model is in the [explanation](../explanation/index.md) section.

## Day-to-day

- [Create a project](./create-a-project.md) — `rwv init`, adding member repos, enabling integrations, and publishing `projects/<name>/`
- [Switch projects](./switch-projects.md) — `rwv activate` and when to use a workweave instead
- [Add a repo](./add-a-repo.md) — URL clones, brownfield adoption, `--new`, and `rwv remove`
- [Migrate from submodules](./migrate-from-submodules.md) — replacing `.gitmodules` or `git-repo` `default.xml` with a repoweave project
- [Run a command across repos](./run-a-command-across-repos.md) — `rwv each`, in parallel, by role or path

## Cross-repo features

- [Create a feature workweave](./create-feature-workweave.md) — isolated cross-repo workspace and ephemeral branches
- [Bring workweave work home](./bring-workweave-work-home.md) — `rwv sync-to --retire` and manual landing ceremony
- [Recover from a sync conflict](./recover-from-sync-conflict.md) — resolving conflicts and resuming with `--continue`
- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspecting op-state, `--continue`, and `rwv abort` savepoint rollback
- [Push a cross-repo feature](./push-cross-repo-feature.md) — `rwv push`, role policy, and lock-precondition recovery

## Review and agents

- [Review a PR with a workweave](./review-pr-with-workweave.md) — detached PR checkout, build, review fixes, and cleanup
- [Hand a task to an agent](./hand-task-to-agent.md) — `rwv setup claude`, `rwv prime`, and parallel workweave isolation
- [Cache builds across workweaves](./cache-builds-across-workweaves.md) — reflink/hardlink sharing, `sccache`, `pnpm` store, and `uv` cache

## Recovery

- [Reconcile repos with the lock](./reconcile-repos.md) — `rwv status`, `rwv doctor --locked`, and `rwv fetch` in-place repair
- [Regenerate ecosystem workspace files](./regenerate-ecosystem-files.md) — `rwv doctor --fix` to re-author, `rwv materialize` to settle drift and run hooks
- [Upgrade to a new rwv on an existing weave](./upgrade-rwv.md) — clearing marker, manifest, and lock refusals in order when upgrading `rwv`, and the health-floor refusal

## Release and integration

- [Release a package](./release-a-package.md) — per-ecosystem release recipes and tag workflows
- [Add an integration](./add-an-integration.md) — enabling and disabling integrations in `rwv.toml`
- [Write a plugin](./write-a-plugin.md) — building an `rwv-<verb>` executable that inherits `rwv`'s addressing environment
