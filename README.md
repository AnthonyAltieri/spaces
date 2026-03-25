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
- supports a shell-friendly `cwd` lookup command for optional `cd` wrappers

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

## Install With Homebrew

`spaces` is published through the `AnthonyAltieri/tap` Homebrew tap.

Prerequisites:

- Homebrew installed
- Git available on `PATH`

Install:

```bash
brew tap AnthonyAltieri/tap
brew install spaces
```

`spaces` shells out to `git` at runtime, so Git still needs to be installed even when the CLI itself comes from Homebrew.

## Build And Run

Build the CLI:

```bash
cargo build
```

Run it directly with Cargo:

```bash
cargo run -- ~/code/dotfiles ~/code/firecrawl
```

Use interactive discovery from a parent directory:

```bash
cargo run -- -i ~/code
```

Or run the built binary:

```bash
./target/debug/spaces ~/code/dotfiles ~/code/firecrawl
```

Run tests:

```bash
cargo test
```

## Release Process

Homebrew publishing is driven by version changes in `Cargo.toml` on `main`. When a push to `main` changes the package version:

1. the GitHub Actions workflow compares the previous `Cargo.toml` version to the current one
2. if the version did not change, the workflow exits cleanly without publishing
3. if the new version already has a `vX.Y.Z` tag on another commit, the workflow fails with a clear version-conflict error
4. if the tag does not exist yet, the workflow creates and pushes `vX.Y.Z`
5. the workflow runs `cargo test`
6. the workflow downloads the tagged source archive and computes its SHA256
7. the workflow renders `Formula/spaces.rb` in `AnthonyAltieri/homebrew-tap`
8. the workflow runs `brew audit`, `brew install --build-from-source`, and `brew test`
9. the workflow commits the updated formula to the tap repo

Before the workflow can publish formula updates, configure the `HOMEBREW_TAP_GITHUB_TOKEN` repository secret with a token that can push to `AnthonyAltieri/homebrew-tap`.

If a publish attempt fails after the version bump lands, you can rerun the workflow manually with `workflow_dispatch` on the same commit instead of bumping the version again.
## Commands

### Create

Create a new coordinated workspace:

```bash
spaces ~/code/dotfiles ~/code/firecrawl
```

Use an explicit workspace name, branch name, and base directory:

```bash
spaces \
  --name spring-rollout \
  --branch spring-rollout \
  --base-dir /tmp/spaces-home \
  ~/code/dotfiles \
  ~/code/firecrawl
```

JSON output is the default:

```bash
spaces ~/code/dotfiles ~/code/firecrawl
```

If you want to discover repos from the immediate children of a parent directory, use `-i`. That opens the interactive `ratatui` picker with explicit checkboxes. Type to fuzzy filter, use the arrow keys to move, press `Space` to toggle repos, `Enter` to continue with the selected set, and `Esc` to cancel.

```bash
spaces -i ~/code
```

Interactive discovery requires a terminal. Without `-i`, a single path is treated as a normal repo path and produces a single-repo workspace.

### List

List tracked workspaces:

```bash
spaces list
```

Short alias:

```bash
spaces ls
```

List from a custom base directory:

```bash
spaces list --base-dir /tmp/spaces-home --json
```

### Cwd

Print the on-disk workspace directory for a tracked workspace:

```bash
spaces cwd spring-rollout
```

Use a custom base directory when the workspace registry lives somewhere else:

```bash
spaces cwd spring-rollout --base-dir /tmp/spaces-home
```

Look up the most recently created tracked workspace instead of naming one explicitly:

```bash
spaces cwd --last
```

`cwd` prints only the directory path, which makes it suitable for shell wrappers. See [Shell Integration](#shell-integration) below for a `zsh` wrapper that turns `spaces cwd <name>` or `spaces cwd --last` into an actual shell `cd`. It fails if the requested workspace is not in the registry, if no workspaces are tracked for `--last`, or if the recorded workspace directory is missing on disk.

If you need machine-readable output instead, `cwd` also supports `--json`:

```bash
spaces cwd spring-rollout --json
```

### Add

Add new repos into an existing workspace using that workspace's branch name:

```bash
spaces add spring-rollout ~/code/new-repo
```

Add multiple repos into a workspace stored under a custom base directory:

```bash
spaces add spring-rollout \
  --base-dir /tmp/spaces-home \
  ~/code/new-repo \
  ~/code/another-repo
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

### 3. Clean The Source Repo Before Mutating

`create` validates all selected repos before making worktrees. For dirty repos, it creates an auto-stash in the source repo and then builds the workspace from a freshly fetched `origin/main`.

It still rejects repos that:

- are not valid Git repos
- do not have `origin`
- do not have `origin/main` after fetch
- already contain the requested local branch
- would collide on worktree directory name

This keeps the new workspace clean while still preserving source-repo changes for later manual recovery.

### 4. Roll Back On Mid-Create Failure

If worktree creation succeeds for some repos and then fails for a later repo, the tool removes any worktrees and branches it already created before returning an error.

If registry persistence fails after worktrees are created, it also rolls back those created worktrees and branches.

If failure happens after an auto-stash was created, the tool also restores those source-repo changes before returning the error.

### 5. Shared Branch Name By Default

The default branch name is the workspace name. This keeps the multi-repo change aligned and easy to reason about when one agent is working in the parent directory.

### 6. `origin/main` Is The Default Base

The tool intentionally fetches `origin main` and creates worktrees from `refs/remotes/origin/main`. This matches the intended default workflow and keeps the v1 interface simple.

### 7. JSON Output Is First-Class

Every command supports `--json`. Human-readable debug-style output is useful for manual use, but machine-readable output is required for agent-driven workflows.

## Operational Notes

- `add` creates new child worktrees inside an existing workspace directory and updates the recorded repo list
- `cwd` prints the workspace directory as a plain path for shell integration
- `list`, `show`, and `remove` all support `--base-dir`, not just `create`
- `ls` is an alias for `list`
- JSON output is the default for all commands except `cwd`, which defaults to a plain path for shell integration
- `spaces -i <dir>` inspects only the immediate child directories under `<dir>` when `<dir>` is not itself a repo and then opens a filtered checkbox picker in the terminal
- `add` uses the existing workspace branch name and validates new repos against the current child directory names in that workspace
- duplicate repo paths are deduplicated by canonical repo root
- repos with the same basename are rejected because each repo gets its own directory under the workspace
- `show` and `list` surface stale state if worktrees or workspace directories are missing on disk
- `remove` fails if it cannot fully remove the recorded worktrees

## Shell Integration

The `spaces` binary cannot change the current directory of the parent shell by itself. If you want `spaces cwd <name>` to actually move your shell, add a wrapper in `zsh`:

```bash
spaces() {
  if [[ "$1" == "cwd" ]]; then
    shift

    local dir
    dir=$(command spaces cwd "$@") || return
    builtin cd -- "$dir"
    return
  fi

  command spaces "$@"
}
```

With that wrapper in place:

```bash
spaces cwd spring-rollout
```

changes the shell into the workspace directory, while every other `spaces` subcommand still delegates directly to the compiled binary.

## Verified Behavior

The current implementation has been verified in two ways:

- `cargo test` passes
- live verification succeeded against `~/code/dotfiles` plus a temporary throwaway repo under `~/code`

The automated test coverage also confirms the dirty-repo path: dirty repos are auto-stashed, fresh worktrees are created from `origin/main`, and rollback restores source-repo state if create later fails.
