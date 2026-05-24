# How-to: Run a command across multiple repositories

Repoweave does not include a built-in "each" runner. Instead, it follows the Unix philosophy by exposing repository metadata in a scriptable JSON format, allowing you to compose bulk operations using tools you already know like `jq`, `xargs`, and `parallel`.

## The Primitive: `rwv status --json`

The foundation for all bulk operations is the enriched JSON output from the status command. It includes the absolute path, role, and URL for every repository in the project.

```bash
rwv status --json | jq .
```

## Canonical Recipes

### 1. Pull all owned repositories
To update every repository that you "own" (role: primary), pipe the filtered paths to `xargs`.

```bash
rwv status --json | jq -r '.repos[] | select(.role == "primary") | .absolute_path' | xargs -I {} git -C {} pull
```

### 2. Create a feature branch across all forks
When starting a cross-cutting feature, you can create a branch in every fork simultaneously.

```bash
rwv status --json | jq -r '.repos[] | select(.role == "fork") | .absolute_path' | xargs -I {} git -C {} checkout -b feat/my-big-change
```

### 3. Run tests in repos with a Makefile
You can combine `jq` filtering with shell existence checks.

```bash
rwv status --json | jq -r '.repos[] | .absolute_path' | while read path; do
  if [ -f "$path/Makefile" ]; then
    echo "--- Testing $path ---"
    make -C "$path" test
  fi
done
```

## Advanced Usage: Parallelism

For large projects, running commands sequentially can be slow. Use `parallel` (GNU Parallel) or `xargs -P` to speed things up.

```bash
# Fetch all repositories in parallel (4 at a time)
rwv status --json | jq -r '.repos[] | .absolute_path' | xargs -P 4 -I {} git -C {} fetch --all
```

## Alternative: Gita

For users who prefer a dedicated tool with "summary sugar" and built-in bulk commands, repoweave still supports the [Gita integration](../integrations.md#gita). Gita is now **opt-in**; see the integration docs for how to enable it.
