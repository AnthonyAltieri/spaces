use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashInfo {
    pub stash_ref: String,
    pub stash_commit: String,
    pub stash_message: String,
}

fn trim_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

fn command_error_message(output: &Output) -> String {
    let stderr = trim_output(&output.stderr);
    if stderr.is_empty() {
        trim_output(&output.stdout)
    } else {
        stderr
    }
}

fn run_git_with_current_dir<F>(cwd: &Path, configure: F) -> Result<Output>
where
    F: FnOnce(&mut Command) -> &mut Command,
{
    let mut command = Command::new("git");
    configure(command.current_dir(cwd));
    command
        .output()
        .with_context(|| format!("failed to run git in {}", cwd.display()))
}

fn ensure_success(output: Output, context: &str) -> Result<Output> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(anyhow!("{context}: {}", command_error_message(&output)))
    }
}

pub fn resolve_repo_root(path: &Path) -> Result<PathBuf> {
    let output = run_git_with_current_dir(path, |command| {
        command.arg("rev-parse").arg("--show-toplevel")
    })?;
    let output = ensure_success(output, "failed to resolve git repository root")?;

    let repo_root = trim_output(&output.stdout);
    if repo_root.is_empty() {
        bail!(
            "git returned an empty repository root for {}",
            path.display()
        );
    }

    Ok(PathBuf::from(repo_root))
}

pub fn repo_root_if_repo(path: &Path) -> Result<Option<PathBuf>> {
    let output = run_git_with_current_dir(path, |command| {
        command.arg("rev-parse").arg("--show-toplevel")
    })?;

    if !output.status.success() {
        return Ok(None);
    }

    let repo_root = trim_output(&output.stdout);
    if repo_root.is_empty() {
        bail!(
            "git returned an empty repository root for {}",
            path.display()
        );
    }

    Ok(Some(PathBuf::from(repo_root)))
}

pub fn has_remote_origin(repo_root: &Path) -> Result<bool> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command.arg("remote").arg("get-url").arg("origin")
    })?;
    Ok(output.status.success())
}

fn status_output(repo_root: &Path) -> Result<String> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command
            .arg("status")
            .arg("--porcelain")
            .arg("--untracked-files=all")
    })?;
    let output = ensure_success(output, "failed to inspect repository status")?;
    Ok(trim_output(&output.stdout))
}

pub fn status_is_clean(repo_root: &Path) -> Result<bool> {
    Ok(status_output(repo_root)?.is_empty())
}

pub fn stash_if_dirty(repo_root: &Path, message: &str) -> Result<Option<StashInfo>> {
    if status_is_clean(repo_root)? {
        return Ok(None);
    }

    let previous_top_commit = latest_stash(repo_root)?.map(|stash| stash.stash_commit);
    let output = run_git_with_current_dir(repo_root, |command| {
        command
            .arg("stash")
            .arg("push")
            .arg("--include-untracked")
            .arg("--message")
            .arg(message)
    })?;
    ensure_success(
        output,
        &format!("failed to stash changes in {}", repo_root.display()),
    )?;

    let created = latest_stash(repo_root)?.ok_or_else(|| {
        anyhow!(
            "git stash did not create an entry in {}",
            repo_root.display()
        )
    })?;

    if previous_top_commit.as_deref() == Some(created.stash_commit.as_str()) {
        bail!(
            "git stash did not create a new entry in {}",
            repo_root.display()
        );
    }

    Ok(Some(created))
}

pub fn restore_stash(repo_root: &Path, stash_commit: &str) -> Result<()> {
    let stash_ref = find_stash_ref_by_commit(repo_root, stash_commit)?.ok_or_else(|| {
        anyhow!(
            "stash {} is no longer present in {}",
            stash_commit,
            repo_root.display()
        )
    })?;
    let output = run_git_with_current_dir(repo_root, |command| {
        command
            .arg("stash")
            .arg("pop")
            .arg("--index")
            .arg(&stash_ref)
    })?;
    ensure_success(
        output,
        &format!(
            "failed to restore stash {stash_ref} in {}",
            repo_root.display()
        ),
    )?;
    Ok(())
}

pub(crate) fn list_stashes(repo_root: &Path) -> Result<Vec<StashInfo>> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command
            .arg("stash")
            .arg("list")
            .arg("--format=%gd%x00%H%x00%gs")
    })?;
    let output = ensure_success(output, "failed to inspect stash list")?;

    let text = trim_output(&output.stdout);
    if text.is_empty() {
        return Ok(Vec::new());
    }

    text.lines()
        .map(|line| {
            let mut parts = line.split('\0');
            let stash_ref = parts
                .next()
                .ok_or_else(|| anyhow!("stash list omitted the stash reference"))?;
            let stash_commit = parts
                .next()
                .ok_or_else(|| anyhow!("stash list omitted the stash commit"))?;
            let stash_message = parts
                .next()
                .ok_or_else(|| anyhow!("stash list omitted the stash message"))?;

            Ok(StashInfo {
                stash_ref: stash_ref.to_owned(),
                stash_commit: stash_commit.to_owned(),
                stash_message: stash_message.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn status_entries(repo_root: &Path) -> Result<Vec<String>> {
    let output = status_output(repo_root)?;
    if output.is_empty() {
        return Ok(Vec::new());
    }

    Ok(output.lines().map(str::to_owned).collect())
}

pub fn local_branch_exists(repo_root: &Path, branch_name: &str) -> Result<bool> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command
            .arg("show-ref")
            .arg("--verify")
            .arg("--quiet")
            .arg(format!("refs/heads/{branch_name}"))
    })?;
    Ok(output.status.success())
}

pub fn fetch_origin_main(repo_root: &Path) -> Result<()> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command.arg("fetch").arg("origin").arg("main")
    })?;
    ensure_success(output, "failed to fetch origin/main")?;
    Ok(())
}

pub fn remote_main_exists(repo_root: &Path) -> Result<bool> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command
            .arg("rev-parse")
            .arg("--verify")
            .arg("--quiet")
            .arg("refs/remotes/origin/main")
    })?;
    Ok(output.status.success())
}

pub fn remote_main_commit(repo_root: &Path) -> Result<String> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command
            .arg("rev-parse")
            .arg("--verify")
            .arg("refs/remotes/origin/main")
    })?;
    let output = ensure_success(output, "failed to resolve origin/main commit")?;
    Ok(trim_output(&output.stdout))
}

pub fn create_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    branch_name: &str,
    base_ref: &str,
) -> Result<()> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command
            .arg("worktree")
            .arg("add")
            .arg("-b")
            .arg(branch_name)
            .arg(worktree_path)
            .arg(base_ref)
    })?;
    ensure_success(
        output,
        &format!(
            "failed to create worktree {} for {}",
            worktree_path.display(),
            repo_root.display()
        ),
    )?;
    Ok(())
}

pub fn remove_worktree(repo_root: &Path, worktree_path: &Path) -> Result<()> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command.arg("worktree").arg("remove").arg(worktree_path)
    })?;
    ensure_success(
        output,
        &format!(
            "failed to remove worktree {} from {}",
            worktree_path.display(),
            repo_root.display()
        ),
    )?;
    Ok(())
}

pub fn delete_local_branch(repo_root: &Path, branch_name: &str) -> Result<()> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command.arg("branch").arg("-D").arg(branch_name)
    })?;
    ensure_success(
        output,
        &format!(
            "failed to delete branch {branch_name} in {}",
            repo_root.display()
        ),
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn current_branch(repo_root: &Path) -> Result<String> {
    let output = run_git_with_current_dir(repo_root, |command| {
        command.arg("rev-parse").arg("--abbrev-ref").arg("HEAD")
    })?;
    let output = ensure_success(output, "failed to read current branch")?;
    Ok(trim_output(&output.stdout))
}

fn latest_stash(repo_root: &Path) -> Result<Option<StashInfo>> {
    Ok(list_stashes(repo_root)?.into_iter().next())
}

fn find_stash_ref_by_commit(repo_root: &Path, stash_commit: &str) -> Result<Option<String>> {
    Ok(list_stashes(repo_root)?
        .into_iter()
        .find(|stash| stash.stash_commit == stash_commit)
        .map(|stash| stash.stash_ref))
}
