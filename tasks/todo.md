# Todo

## Goal

Build a Rust CLI that creates and manages coordinated multi-repo Git workspaces under `~/.spaces`.

## Success Criteria

- `create` accepts multiple local repo paths and creates one named workspace directory with one worktree per repo.
- Worktrees are created from fetched `origin/main` by default.
- A memorable short workspace name is generated and reused as the default branch name.
- `list`, `show`, and `remove` work from persisted metadata.
- Fail-fast preflight prevents partial setup when repos are not safe to use, and rollback handles mid-create failures.

## Assumptions / Constraints

- The repository starts empty and will be bootstrapped as a new Rust binary crate.
- This machine currently has Git installed but does not have `cargo` or `rustc` available.
- Implementation should still be completed in-repo; local build/test verification may be limited by missing toolchain.
- Metadata will be stored centrally at `~/.spaces/registry.json`.

## Steps

- [x] Create task tracking documents for the repo.
- [x] Scaffold the Rust crate, module layout, and top-level docs.
- [x] Implement CLI parsing and typed command/config models.
- [x] Implement workspace naming, registry persistence, and path handling.
- [x] Implement Git preflight, fetch, worktree creation, rollback, and removal flows.
- [x] Implement `create`, `list`, `show`, and `remove` command execution.
- [x] Add unit and integration-style tests for core behavior.
- [x] Run available verification and capture any environment blockers.

## Risks / Edge Cases

- Dirty repos, missing remotes, or missing `origin/main` must fail before mutation.
- Branch or worktree path conflicts must be detected before creation.
- Partial creation/removal failures must leave the registry consistent enough to recover.
- Non-interactive `remove` must not hang waiting for branch deletion input.
- Workspace names must remain memorable while avoiding collisions.

## Verification Plan

- Inspect the final crate structure and public CLI surface.
- Run Rust formatting/build/test checks if the toolchain is available.
- If Rust tooling is unavailable, validate with targeted file review and document the gap.

## Review

- Implemented a greenfield Rust CLI with `create`, `list`, `show`, and `remove`.
- Added a central JSON registry at `~/.spaces/registry.json`.
- Added rollback-aware worktree creation and guarded removal flows.
- Added unit coverage for registry and name generation plus git-backed integration-style tests for create/remove flows.
- Fixed a CLI gap found during verification: `list`, `show`, and `remove` now honor `--base-dir`, and added regression tests for that path.
- Verified locally with `cargo test`: 11 tests passed.
- Verified live behavior against `~/code/dotfiles` plus a temporary throwaway repo under `~/code`: `create`, `list`, `show`, and `remove --delete-branches` all succeeded with a custom base dir.
- Verified the safety path against `~/code/bond`: creation failed fast with `repository /Users/ki/code/bond has uncommitted or untracked changes`, and no workspace directory was created for that attempt.
- `/Users/ki/code/space` is still not a Git repository, so there is no repo-local `git status` for this project itself.
