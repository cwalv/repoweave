# How-to guides

Task-shaped recipes. Each page leads with the command; the conceptual material is in the [explanation](../explanation/index.md) section (joints and lenses).

## Day-to-day

- [Switch projects](./switch-projects.md) — `rwv activate` and when to use a workweave instead
- [Add a repo](./add-a-repo.md) — URL, brownfield mv, `--new`, removal
- [Run a command across repos](./run-a-command-across-repos.md) — `rwv status --json | jq | xargs` recipes

## Cross-repo features

- [Create a feature workweave](./create-feature-workweave.md) — isolated cross-repo branch
- [Bring workweave work home](./bring-workweave-work-home.md) — `rwv sync-to --retire` and manual ceremony
- [Recover from a sync conflict](./recover-from-sync-conflict.md) — fix-and-resume, `rwv abort`
- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspect op-state, `--continue`, `rwv abort`
- [Push a cross-repo feature](./push-cross-repo-feature.md) — `rwv push`, role policy, lock-precondition recovery

## Review and agents

- [Review a PR with a workweave](./review-pr-with-workweave.md) — isolated PR build
- [Hand a task to an agent](./hand-task-to-agent.md) — the landed agent surface
- [Cache builds across workweaves](./cache-builds-across-workweaves.md) — sccache, pnpm store, uv cache

## Recovery

- [Reconcile repos with the lock](./reconcile-repos.md) — `rwv status`, `rwv doctor --locked`, `rwv fetch` in-place repair
- [Regenerate ecosystem workspace files](./regenerate-ecosystem-files.md) — `rwv activate` after membership changes

## Release and integration

- [Release a package](./release-a-package.md) — per-ecosystem release recipes
- [Add an integration](./add-an-integration.md) — enable/disable integrations in `rwv.yaml`
