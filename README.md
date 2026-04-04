# repoweave (`rwv`)

**Monorepo ergonomics for independent repositories.**

You already use `go.work`, Cargo workspaces, pnpm workspaces, or uv workspaces?  
repoweave builds directly on top of them — at the repo layer.

### Why it exists

Your code lives across multiple independent git repositories. Changing a shared internal library forces the version-bump / publish / update dance. Full monorepos eliminate that but require vendoring everything.

repoweave gives you the two biggest practical wins of a monorepo while keeping every repo sovereign:

- **No version-bump dance** for internal dependencies — imports resolve locally, tests run end-to-end, you commit and `rwv lock`
- **Ephemeral isolated workspaces** — spin up clean copies for agents, PRs, or parallel work via git worktrees on ephemeral branches

### The weave metaphor

“Weave” comes from weaving fabric: independent **threads** (your git repositories) are interwoven into a single, usable **fabric** — a unified workspace. The threads keep their identity and history; they simply work better together.

### Quick start

```bash
cargo install repoweave
mkdir my-workspace && cd my-workspace
rwv fetch myproject/web-app
cd projects/web-app
# edit across repos freely, test immediately
rwv lock          # update pinned SHAs
git add rwv.lock && git commit -m "chore: update lock"