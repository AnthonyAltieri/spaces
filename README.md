# spaces

`spaces` is a Rust CLI for creating and managing coordinated Git workspaces across multiple local repositories.

It is built for the case where one agent or one shell session needs to work across several repos at once under a single parent directory.

## What It Does

- creates a workspace under `~/.spaces` by default
- creates one Git worktree per selected repo inside that workspace
- fetches `origin main` before creating worktrees
- uses `origin/main` as the default base for every repo
- generates a memorable short workspace name when one is not provided
- uses the workspace name as the default branch name across repos
- stores workspace metadata in `registry.json`
- supports machine-readable `--json` output on every command

Example workspace layout:

```text
~/.spaces/
  amber-trail/
    dotfiles/
    firecrawl/
  registry.json
```

## Prerequisites

- Rust toolchain with `cargo` and `rustc`
- Git available on `PATH`
- local repositories already cloned on disk
- each selected repo must have:
  - a clean working tree
  - an `origin` remote
  - `origin/main`

## Build And Run

Build the CLI:

```bash
cargo build
```

Run it directly with Cargo:

```bash
cargo run -- create ~/code/dotfiles ~/code/firecrawl
```

Or run the built binary:

```bash
./target/debug/spaces create ~/code/dotfiles ~/code/firecrawl
```

Run tests:

```bash
cargo test
```

## Commands

### Create

Create a new coordinated workspace:

```bash
spaces create ~/code/dotfiles ~/code/firecrawl
```

Use an explicit workspace name, branch name, and base directory:

```bash
spaces create \
  --name spring-rollout \
  --branch spring-rollout \
  --base-dir /tmp/spaces-home \
  ~/code/dotfiles \
  ~/code/firecrawl
```

JSON output for automation:

```bash
spaces create --json ~/code/dotfiles ~/code/firecrawl
```

### List

List tracked workspaces:

```bash
spaces list
```

List from a custom base directory:

```bash
spaces list --base-dir /tmp/spaces-home --json
```

### Show

Inspect one workspace:

```bash
spaces show spring-rollout
```

### Remove

Remove a workspace and keep the created local branches:

```bash
spaces remove spring-rollout --yes --keep-branches
```

Remove a workspace and delete the created local branches:

```bash
spaces remove spring-rollout --yes --delete-branches
```

If neither branch flag is provided, `remove` prompts interactively.

## Architecture Decisions

### 1. Shell Out To Git Instead Of Reimplementing Git Behavior

The tool uses the `git` CLI for fetch, branch checks, worktree creation, and removal. That keeps behavior aligned with the user's installed Git and avoids carrying a second interpretation of Git state in Rust.

### 2. Central Registry Instead Of Per-Workspace Manifests

Workspace metadata is stored in `~/.spaces/registry.json` by default. The registry records:

- workspace name
- branch name
- created timestamp
- workspace directory
- per-repo source path
- per-repo worktree path
- base ref and resolved commit

This makes `list`, `show`, and `remove` straightforward without scanning arbitrary directories.

### 3. Fail Fast Before Mutating

`create` validates all selected repos before making worktrees. It rejects repos that:

- are not valid Git repos
- are dirty
- do not have `origin`
- do not have `origin/main` after fetch
- already contain the requested local branch
- would collide on worktree directory name

This avoids partially-created multi-repo workspaces caused by obvious preflight failures.

### 4. Roll Back On Mid-Create Failure

If worktree creation succeeds for some repos and then fails for a later repo, the tool removes any worktrees and branches it already created before returning an error.

If registry persistence fails after worktrees are created, it also rolls back those created worktrees and branches.

### 5. Shared Branch Name By Default

The default branch name is the workspace name. This keeps the multi-repo change aligned and easy to reason about when one agent is working in the parent directory.

### 6. `origin/main` Is The Default Base

The tool intentionally fetches `origin main` and creates worktrees from `refs/remotes/origin/main`. This matches the intended default workflow and keeps the v1 interface simple.

### 7. JSON Output Is First-Class

Every command supports `--json`. Human-readable debug-style output is useful for manual use, but machine-readable output is required for agent-driven workflows.

## Operational Notes

- `list`, `show`, and `remove` all support `--base-dir`, not just `create`
- duplicate repo paths are deduplicated by canonical repo root
- repos with the same basename are rejected because each repo gets its own directory under the workspace
- `show` and `list` surface stale state if worktrees or workspace directories are missing on disk
- `remove` fails if it cannot fully remove the recorded worktrees

## Verified Behavior

The current implementation has been verified in two ways:

- `cargo test` passes
- live verification succeeded against `~/code/dotfiles` plus a temporary throwaway repo under `~/code`

The live verification also confirmed the safety path: attempting to include dirty `~/code/bond` fails before any workspace is created.
