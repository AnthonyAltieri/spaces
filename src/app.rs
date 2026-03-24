use crate::git;
use crate::names;
use crate::registry::{Registry, RegistryStore, RepoRecord, WorkspaceRecord};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const BASE_REF: &str = "refs/remotes/origin/main";
const DEFAULT_REMOTE: &str = "origin";

#[derive(Debug, Clone)]
pub struct CreateWorkspaceRequest {
    pub workspace_name: Option<String>,
    pub branch_name: Option<String>,
    pub repo_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct AddWorkspaceReposRequest {
    pub workspace_name: String,
    pub repo_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoveBranchAction {
    Keep,
    Delete,
}

#[derive(Debug, Clone)]
pub struct RemoveWorkspaceRequest {
    pub workspace_name: String,
    pub branch_action: RemoveBranchAction,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkspaceRepoView {
    pub repo_name: String,
    pub source_repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub base_commit: String,
    pub exists_on_disk: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceHealth {
    Healthy,
    MissingWorkspaceDir,
    MissingWorktrees,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CreateWorkspaceResult {
    pub workspace_name: String,
    pub branch_name: String,
    pub workspace_dir: PathBuf,
    pub registry_path: PathBuf,
    pub stashed_source_repos: Vec<StashedSourceRepoView>,
    pub repos: Vec<WorkspaceRepoView>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AddWorkspaceReposResult {
    pub workspace_name: String,
    pub branch_name: String,
    pub workspace_dir: PathBuf,
    pub registry_path: PathBuf,
    pub stashed_source_repos: Vec<StashedSourceRepoView>,
    pub added_repos: Vec<WorkspaceRepoView>,
    pub repos: Vec<WorkspaceRepoView>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StashedSourceRepoView {
    pub source_repo_path: PathBuf,
    pub stash_commit: String,
    pub stash_message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub workspace_name: String,
    pub branch_name: String,
    pub workspace_dir: PathBuf,
    pub repo_count: usize,
    pub health: WorkspaceHealth,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ListWorkspacesResult {
    pub registry_path: PathBuf,
    pub workspaces: Vec<WorkspaceSummary>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ShowWorkspaceResult {
    pub workspace_name: String,
    pub branch_name: String,
    pub workspace_dir: PathBuf,
    pub workspace_exists_on_disk: bool,
    pub created_at_epoch_seconds: u64,
    pub health: WorkspaceHealth,
    pub repos: Vec<WorkspaceRepoView>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoveWorkspaceResult {
    pub workspace_name: String,
    pub branch_name: String,
    pub workspace_dir: PathBuf,
    pub branch_action: RemoveBranchAction,
    pub removed_worktree_count: usize,
}

#[derive(Debug, Clone)]
pub struct WorkspaceManager {
    store: RegistryStore,
}

#[derive(Debug, Clone)]
struct ResolvedRepo {
    repo_name: String,
    repo_root: PathBuf,
    worktree_path: PathBuf,
    base_commit: String,
}

#[derive(Debug, Clone)]
struct AutoStashedRepo {
    source_repo_path: PathBuf,
    stash_commit: String,
    stash_message: String,
}

impl WorkspaceManager {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            store: RegistryStore::new(base_dir),
        }
    }

    pub fn base_dir(&self) -> &Path {
        self.store.base_dir()
    }

    pub fn registry_path(&self) -> &Path {
        self.store.registry_path()
    }

    pub fn create(&self, request: CreateWorkspaceRequest) -> Result<CreateWorkspaceResult> {
        if request.repo_paths.is_empty() {
            bail!("at least one repository path is required");
        }

        fs::create_dir_all(self.base_dir()).with_context(|| {
            format!(
                "failed to create base directory {}",
                self.base_dir().display()
            )
        })?;

        let mut registry = self.store.load()?;
        let workspace_name = resolve_workspace_name(
            request.workspace_name.as_deref(),
            &registry,
            self.base_dir(),
        )?;
        validate_workspace_name(&workspace_name)?;

        if registry.contains_workspace(&workspace_name) {
            bail!("workspace `{workspace_name}` already exists in the registry");
        }

        let branch_name = request
            .branch_name
            .unwrap_or_else(|| workspace_name.clone());
        validate_branch_name(&branch_name)?;

        let workspace_dir = self.base_dir().join(&workspace_name);
        if workspace_dir.exists() {
            bail!(
                "workspace directory already exists at {}",
                workspace_dir.display()
            );
        }

        let mut repos = resolve_repos(&request.repo_paths, &branch_name, &workspace_dir, &[])?;
        let stashed_repos = auto_stash_repos(&repos, &workspace_name)?;

        if let Err(error) = populate_base_commits(&mut repos) {
            return Err(rollback_auto_stashes(error, &stashed_repos));
        }

        if let Err(error) = fs::create_dir_all(&workspace_dir).with_context(|| {
            format!(
                "failed to create workspace directory {}",
                workspace_dir.display()
            )
        }) {
            return Err(rollback_auto_stashes(error, &stashed_repos));
        }

        let created_repos = match self.create_repo_records(&workspace_name, &branch_name, repos) {
            Ok(created_repos) => created_repos,
            Err(error) => {
                let _ = fs::remove_dir_all(&workspace_dir);
                return Err(rollback_auto_stashes(error, &stashed_repos));
            }
        };

        let created_record = WorkspaceRecord {
            name: workspace_name.clone(),
            branch_name: branch_name.clone(),
            created_at_epoch_seconds: current_epoch_seconds()?,
            workspace_dir: workspace_dir.clone(),
            repos: created_repos,
        };

        registry.upsert(created_record.clone());
        if let Err(error) = self.store.save(&registry) {
            rollback_workspace_creation(&created_record.repos, &created_record.branch_name);
            let _ = fs::remove_dir_all(&workspace_dir);
            let error = error.context("failed to persist registry after creating worktrees");
            return Err(rollback_auto_stashes(error, &stashed_repos));
        }

        Ok(build_create_result(
            &created_record,
            self.registry_path().to_path_buf(),
            &stashed_repos,
        ))
    }

    pub fn add(&self, request: AddWorkspaceReposRequest) -> Result<AddWorkspaceReposResult> {
        if request.repo_paths.is_empty() {
            bail!("at least one repository path is required");
        }

        let mut registry = self.store.load()?;
        let workspace = registry
            .get(&request.workspace_name)
            .cloned()
            .ok_or_else(|| anyhow!("workspace `{}` was not found", request.workspace_name))?;

        if !workspace.workspace_dir.exists() {
            bail!(
                "workspace directory is missing at {}",
                workspace.workspace_dir.display()
            );
        }

        let mut repos = resolve_repos(
            &request.repo_paths,
            &workspace.branch_name,
            &workspace.workspace_dir,
            &workspace.repos,
        )?;
        let stashed_repos = auto_stash_repos(&repos, &workspace.name)?;

        if let Err(error) = populate_base_commits(&mut repos) {
            return Err(rollback_auto_stashes(error, &stashed_repos));
        }

        let added_repos =
            match self.create_repo_records(&workspace.name, &workspace.branch_name, repos) {
                Ok(added_repos) => added_repos,
                Err(error) => return Err(rollback_auto_stashes(error, &stashed_repos)),
            };

        let mut updated_workspace = workspace.clone();
        updated_workspace.repos.extend(added_repos.clone());

        registry.upsert(updated_workspace.clone());
        if let Err(error) = self.store.save(&registry) {
            rollback_workspace_creation(&added_repos, &workspace.branch_name);
            let error = error.context("failed to persist registry after adding worktrees");
            return Err(rollback_auto_stashes(error, &stashed_repos));
        }

        Ok(build_add_result(
            &updated_workspace,
            self.registry_path().to_path_buf(),
            &stashed_repos,
            &added_repos,
        ))
    }

    pub fn list(&self) -> Result<ListWorkspacesResult> {
        let registry = self.store.load()?;
        let mut workspaces = registry
            .workspaces
            .iter()
            .map(|workspace| WorkspaceSummary {
                workspace_name: workspace.name.clone(),
                branch_name: workspace.branch_name.clone(),
                workspace_dir: workspace.workspace_dir.clone(),
                repo_count: workspace.repos.len(),
                health: determine_health(workspace),
            })
            .collect::<Vec<_>>();

        workspaces.sort_by(|left, right| left.workspace_name.cmp(&right.workspace_name));

        Ok(ListWorkspacesResult {
            registry_path: self.registry_path().to_path_buf(),
            workspaces,
        })
    }

    pub fn show(&self, workspace_name: &str) -> Result<ShowWorkspaceResult> {
        let registry = self.store.load()?;
        let workspace = registry
            .get(workspace_name)
            .cloned()
            .ok_or_else(|| anyhow!("workspace `{workspace_name}` was not found"))?;

        Ok(ShowWorkspaceResult {
            workspace_name: workspace.name.clone(),
            branch_name: workspace.branch_name.clone(),
            workspace_dir: workspace.workspace_dir.clone(),
            workspace_exists_on_disk: workspace.workspace_dir.exists(),
            created_at_epoch_seconds: workspace.created_at_epoch_seconds,
            health: determine_health(&workspace),
            repos: workspace
                .repos
                .iter()
                .map(|repo| WorkspaceRepoView {
                    repo_name: repo.repo_name.clone(),
                    source_repo_path: repo.source_repo_path.clone(),
                    worktree_path: repo.worktree_path.clone(),
                    base_commit: repo.base_commit.clone(),
                    exists_on_disk: repo.worktree_path.exists(),
                })
                .collect(),
        })
    }

    pub fn remove(&self, request: RemoveWorkspaceRequest) -> Result<RemoveWorkspaceResult> {
        let mut registry = self.store.load()?;
        let workspace = registry
            .get(&request.workspace_name)
            .cloned()
            .ok_or_else(|| anyhow!("workspace `{}` was not found", request.workspace_name))?;

        let mut errors = Vec::new();
        let mut removed_count = 0_usize;

        for repo in &workspace.repos {
            if !repo.worktree_path.exists() {
                errors.push(format!(
                    "worktree path is missing for {}: {}",
                    repo.repo_name,
                    repo.worktree_path.display()
                ));
                continue;
            }

            if let Err(error) = git::remove_worktree(&repo.source_repo_path, &repo.worktree_path) {
                errors.push(error.to_string());
                continue;
            }

            removed_count += 1;
        }

        if errors.is_empty() && request.branch_action == RemoveBranchAction::Delete {
            for repo in &workspace.repos {
                if let Err(error) =
                    git::delete_local_branch(&repo.source_repo_path, &workspace.branch_name)
                {
                    errors.push(error.to_string());
                }
            }
        }

        if !errors.is_empty() {
            bail!(
                "failed to fully remove workspace `{}`:\n{}",
                workspace.name,
                errors.join("\n")
            );
        }

        if workspace.workspace_dir.exists() {
            fs::remove_dir_all(&workspace.workspace_dir).with_context(|| {
                format!(
                    "failed to remove workspace directory {}",
                    workspace.workspace_dir.display()
                )
            })?;
        }

        registry.remove(&workspace.name);
        self.store.save(&registry)?;

        Ok(RemoveWorkspaceResult {
            workspace_name: workspace.name,
            branch_name: workspace.branch_name,
            workspace_dir: workspace.workspace_dir,
            branch_action: request.branch_action,
            removed_worktree_count: removed_count,
        })
    }

    fn create_repo_records(
        &self,
        workspace_name: &str,
        branch_name: &str,
        repos: Vec<ResolvedRepo>,
    ) -> Result<Vec<RepoRecord>> {
        let mut created = Vec::new();

        for repo in &repos {
            if let Err(error) =
                git::create_worktree(&repo.repo_root, &repo.worktree_path, branch_name, BASE_REF)
            {
                let _ = git::remove_worktree(&repo.repo_root, &repo.worktree_path);
                let _ = git::delete_local_branch(&repo.repo_root, branch_name);
                rollback_created_worktrees(&created, branch_name);
                return Err(anyhow!(
                    "failed to create workspace `{workspace_name}`: {error}"
                ));
            }
            created.push((repo.repo_root.clone(), repo.worktree_path.clone()));
        }

        Ok(repos
            .into_iter()
            .map(|repo| RepoRecord {
                repo_name: repo.repo_name,
                source_repo_path: repo.repo_root,
                worktree_path: repo.worktree_path,
                remote_name: DEFAULT_REMOTE.to_owned(),
                base_ref: "origin/main".to_owned(),
                base_commit: repo.base_commit,
            })
            .collect())
    }
}

pub fn default_base_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("failed to resolve the current user's home directory")?;
    Ok(home.join(".spaces"))
}

pub fn prompt_for_branch_action(
    workspace_name: &str,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<RemoveBranchAction> {
    write!(
        output,
        "Delete local branches for workspace `{workspace_name}` after removing the worktrees? [y/N]: "
    )?;
    output.flush()?;

    let mut answer = String::new();
    input.read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();

    Ok(match answer.as_str() {
        "y" | "yes" => RemoveBranchAction::Delete,
        _ => RemoveBranchAction::Keep,
    })
}

fn resolve_workspace_name(
    requested_name: Option<&str>,
    registry: &Registry,
    base_dir: &Path,
) -> Result<String> {
    if let Some(name) = requested_name {
        return Ok(name.to_owned());
    }

    let mut existing = registry
        .workspaces
        .iter()
        .map(|workspace| workspace.name.clone())
        .collect::<HashSet<_>>();

    if base_dir.exists() {
        for entry in fs::read_dir(base_dir)
            .with_context(|| format!("failed to inspect base directory {}", base_dir.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                existing.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }

    Ok(names::generate_workspace_name(&existing))
}

fn validate_workspace_name(workspace_name: &str) -> Result<()> {
    if workspace_name.is_empty() {
        bail!("workspace name must not be empty");
    }

    if workspace_name == "." || workspace_name == ".." {
        bail!("workspace name `{workspace_name}` is reserved");
    }

    if !workspace_name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        bail!(
            "workspace name `{workspace_name}` may only contain ASCII letters, digits, '-' and '_'"
        );
    }

    Ok(())
}

fn validate_branch_name(branch_name: &str) -> Result<()> {
    if branch_name.trim().is_empty() {
        bail!("branch name must not be empty");
    }

    if branch_name.contains('\n') || branch_name.contains('\r') {
        bail!("branch name must not contain line breaks");
    }

    Ok(())
}

fn resolve_repos(
    requested_paths: &[PathBuf],
    branch_name: &str,
    workspace_dir: &Path,
    existing_repos: &[RepoRecord],
) -> Result<Vec<ResolvedRepo>> {
    let existing_roots = existing_repos
        .iter()
        .map(|repo| repo.source_repo_path.clone())
        .collect::<HashSet<_>>();
    let mut seen_roots = HashSet::new();
    let mut repos = Vec::new();
    let mut seen_repo_names = existing_repos
        .iter()
        .map(|repo| repo.repo_name.clone())
        .collect::<HashSet<_>>();

    for requested_path in requested_paths {
        let repo_root = git::resolve_repo_root(requested_path).with_context(|| {
            format!(
                "failed to treat {} as a local git repository",
                requested_path.display()
            )
        })?;
        let repo_root = fs::canonicalize(&repo_root)
            .with_context(|| format!("failed to canonicalize {}", repo_root.display()))?;

        if !seen_roots.insert(repo_root.clone()) {
            continue;
        }

        if existing_roots.contains(&repo_root) {
            bail!(
                "repository {} is already part of the workspace",
                repo_root.display()
            );
        }

        if !git::has_remote_origin(&repo_root)? {
            bail!(
                "repository {} does not have an `origin` remote",
                repo_root.display()
            );
        }

        if git::local_branch_exists(&repo_root, branch_name)? {
            bail!(
                "repository {} already has a local branch named `{branch_name}`",
                repo_root.display()
            );
        }

        let repo_name = repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                anyhow!(
                    "failed to determine a repo name for {}",
                    repo_root.display()
                )
            })?;

        if !seen_repo_names.insert(repo_name.clone()) {
            bail!("multiple selected repositories resolve to the same basename `{repo_name}`");
        }

        let worktree_path = workspace_dir.join(&repo_name);
        if worktree_path.exists() {
            bail!(
                "worktree path already exists for {}: {}",
                repo_name,
                worktree_path.display()
            );
        }

        repos.push(ResolvedRepo {
            repo_name,
            repo_root,
            worktree_path,
            base_commit: String::new(),
        });
    }

    Ok(repos)
}

fn auto_stash_repos(repos: &[ResolvedRepo], workspace_name: &str) -> Result<Vec<AutoStashedRepo>> {
    let mut stashed_repos = Vec::new();

    for repo in repos {
        let stash_message = format!("spaces auto-stash {workspace_name}/{}", repo.repo_name);
        let stash = match git::stash_if_dirty(&repo.repo_root, &stash_message) {
            Ok(stash) => stash,
            Err(error) => {
                let error = anyhow!(
                    "failed to auto-stash {}: {}",
                    repo.repo_root.display(),
                    error
                );
                return Err(rollback_auto_stashes(error, &stashed_repos));
            }
        };

        match stash {
            Some(stash) => stashed_repos.push(AutoStashedRepo {
                source_repo_path: repo.repo_root.clone(),
                stash_commit: stash.stash_commit,
                stash_message: stash.stash_message,
            }),
            None => {}
        }
    }

    Ok(stashed_repos)
}

fn populate_base_commits(repos: &mut [ResolvedRepo]) -> Result<()> {
    for repo in repos {
        git::fetch_origin_main(&repo.repo_root)?;
        if !git::remote_main_exists(&repo.repo_root)? {
            bail!(
                "repository {} does not have refs/remotes/origin/main after fetch",
                repo.repo_root.display()
            );
        }
        repo.base_commit = git::remote_main_commit(&repo.repo_root)?;
    }

    Ok(())
}

fn rollback_auto_stashes(error: anyhow::Error, stashed_repos: &[AutoStashedRepo]) -> anyhow::Error {
    if stashed_repos.is_empty() {
        return error;
    }

    match restore_auto_stashes(stashed_repos) {
        Ok(()) => error,
        Err(restore_error) => anyhow!("{error}\n{restore_error}"),
    }
}

fn restore_auto_stashes(stashed_repos: &[AutoStashedRepo]) -> Result<()> {
    let mut errors = Vec::new();

    for stash in stashed_repos.iter().rev() {
        if let Err(error) = git::restore_stash(&stash.source_repo_path, &stash.stash_commit) {
            errors.push(format!(
                "failed to restore auto-stash for {} ({}) [{}]: {}",
                stash.source_repo_path.display(),
                stash.stash_commit,
                stash.stash_message,
                error
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        bail!(
            "failed to restore auto-stashed source repos:\n{}",
            errors.join("\n")
        )
    }
}

fn rollback_workspace_creation(repos: &[RepoRecord], branch_name: &str) {
    let created = repos
        .iter()
        .map(|repo| (repo.source_repo_path.clone(), repo.worktree_path.clone()))
        .collect::<Vec<_>>();
    rollback_created_worktrees(&created, branch_name);
}

fn rollback_created_worktrees(created: &[(PathBuf, PathBuf)], branch_name: &str) {
    for (repo_root, worktree_path) in created.iter().rev() {
        let _ = git::remove_worktree(repo_root, worktree_path);
        let _ = git::delete_local_branch(repo_root, branch_name);
    }
}

fn build_create_result(
    record: &WorkspaceRecord,
    registry_path: PathBuf,
    stashed_repos: &[AutoStashedRepo],
) -> CreateWorkspaceResult {
    CreateWorkspaceResult {
        workspace_name: record.name.clone(),
        branch_name: record.branch_name.clone(),
        workspace_dir: record.workspace_dir.clone(),
        registry_path,
        stashed_source_repos: build_stashed_source_repo_views(stashed_repos),
        repos: build_workspace_repo_views(&record.repos),
    }
}

fn build_add_result(
    record: &WorkspaceRecord,
    registry_path: PathBuf,
    stashed_repos: &[AutoStashedRepo],
    added_repos: &[RepoRecord],
) -> AddWorkspaceReposResult {
    AddWorkspaceReposResult {
        workspace_name: record.name.clone(),
        branch_name: record.branch_name.clone(),
        workspace_dir: record.workspace_dir.clone(),
        registry_path,
        stashed_source_repos: build_stashed_source_repo_views(stashed_repos),
        added_repos: build_workspace_repo_views(added_repos),
        repos: build_workspace_repo_views(&record.repos),
    }
}

fn build_stashed_source_repo_views(
    stashed_repos: &[AutoStashedRepo],
) -> Vec<StashedSourceRepoView> {
    stashed_repos
        .iter()
        .map(|stash| StashedSourceRepoView {
            source_repo_path: stash.source_repo_path.clone(),
            stash_commit: stash.stash_commit.clone(),
            stash_message: stash.stash_message.clone(),
        })
        .collect()
}

fn build_workspace_repo_views(repos: &[RepoRecord]) -> Vec<WorkspaceRepoView> {
    repos
        .iter()
        .map(|repo| WorkspaceRepoView {
            repo_name: repo.repo_name.clone(),
            source_repo_path: repo.source_repo_path.clone(),
            worktree_path: repo.worktree_path.clone(),
            base_commit: repo.base_commit.clone(),
            exists_on_disk: repo.worktree_path.exists(),
        })
        .collect()
}

fn determine_health(workspace: &WorkspaceRecord) -> WorkspaceHealth {
    if !workspace.workspace_dir.exists() {
        return WorkspaceHealth::MissingWorkspaceDir;
    }

    if workspace
        .repos
        .iter()
        .any(|repo| !repo.worktree_path.exists())
    {
        return WorkspaceHealth::MissingWorktrees;
    }

    WorkspaceHealth::Healthy
}

fn current_epoch_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::{
        default_base_dir, AddWorkspaceReposRequest, CreateWorkspaceRequest, RemoveBranchAction,
        RemoveWorkspaceRequest, WorkspaceHealth, WorkspaceManager,
    };
    use crate::git;
    use anyhow::{Context, Result};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn default_base_dir_ends_in_spaces() -> Result<()> {
        let base_dir = default_base_dir()?;
        assert!(base_dir.ends_with(".spaces"));
        Ok(())
    }

    #[test]
    fn create_workspace_records_registry_and_worktrees() -> Result<()> {
        let sandbox = tempdir()?;
        let repo_one = init_repo(sandbox.path(), "alpha")?;
        let repo_two = init_repo(sandbox.path(), "beta")?;
        let manager = WorkspaceManager::new(sandbox.path().join("spaces-home"));

        let result = manager.create(CreateWorkspaceRequest {
            workspace_name: Some("steady-trail".into()),
            branch_name: None,
            repo_paths: vec![repo_one.clone(), repo_two.clone()],
        })?;

        assert_eq!(result.workspace_name, "steady-trail");
        assert_eq!(result.branch_name, "steady-trail");
        assert_eq!(result.repos.len(), 2);
        assert!(result.workspace_dir.exists());
        assert!(manager.registry_path().exists());
        assert_eq!(
            git::current_branch(&result.repos[0].worktree_path)?,
            "steady-trail"
        );
        assert_eq!(
            git::current_branch(&result.repos[1].worktree_path)?,
            "steady-trail"
        );

        let listed = manager.list()?;
        assert_eq!(listed.workspaces.len(), 1);
        assert_eq!(listed.workspaces[0].health, WorkspaceHealth::Healthy);

        let shown = manager.show("steady-trail")?;
        assert_eq!(shown.repos.len(), 2);
        assert!(shown.workspace_exists_on_disk);
        assert_eq!(shown.health, WorkspaceHealth::Healthy);

        Ok(())
    }

    #[test]
    fn create_auto_stashes_dirty_repo_and_records_metadata() -> Result<()> {
        let sandbox = tempdir()?;
        let repo_one = init_repo(sandbox.path(), "alpha")?;
        let repo_two = init_repo(sandbox.path(), "beta")?;
        let repo_two = fs::canonicalize(repo_two)?;
        fs::write(repo_two.join("DIRTY.txt"), "dirty\n")?;

        let manager = WorkspaceManager::new(sandbox.path().join("spaces-home"));
        let result = manager.create(CreateWorkspaceRequest {
            workspace_name: Some("rapid-signal".into()),
            branch_name: None,
            repo_paths: vec![repo_one, repo_two.clone()],
        })?;

        assert_eq!(result.stashed_source_repos.len(), 1);
        assert_eq!(result.stashed_source_repos[0].source_repo_path, repo_two);
        assert!(result.stashed_source_repos[0]
            .stash_message
            .contains("rapid-signal/beta"));
        assert!(git::status_is_clean(&repo_two)?);
        assert_eq!(git::list_stashes(&repo_two)?.len(), 1);

        let beta_worktree = result
            .repos
            .iter()
            .find(|repo| repo.repo_name == "beta")
            .expect("beta worktree");
        assert!(!beta_worktree.worktree_path.join("DIRTY.txt").exists());

        Ok(())
    }

    #[test]
    fn add_workspace_records_new_worktrees_and_registry_entries() -> Result<()> {
        let sandbox = tempdir()?;
        let repo_one = init_repo(sandbox.path(), "alpha")?;
        let repo_two = init_repo(sandbox.path(), "beta")?;
        let manager = WorkspaceManager::new(sandbox.path().join("spaces-home"));

        manager.create(CreateWorkspaceRequest {
            workspace_name: Some("steady-trail".into()),
            branch_name: None,
            repo_paths: vec![repo_one.clone()],
        })?;

        let result = manager.add(AddWorkspaceReposRequest {
            workspace_name: "steady-trail".into(),
            repo_paths: vec![repo_two.clone()],
        })?;

        assert_eq!(result.workspace_name, "steady-trail");
        assert_eq!(result.branch_name, "steady-trail");
        assert_eq!(result.added_repos.len(), 1);
        assert_eq!(result.added_repos[0].repo_name, "beta");
        assert_eq!(result.repos.len(), 2);
        assert_eq!(
            git::current_branch(&result.added_repos[0].worktree_path)?,
            "steady-trail"
        );

        let shown = manager.show("steady-trail")?;
        assert_eq!(shown.repos.len(), 2);
        assert_eq!(shown.health, WorkspaceHealth::Healthy);

        Ok(())
    }

    #[test]
    fn add_auto_stashes_dirty_repo_and_records_metadata() -> Result<()> {
        let sandbox = tempdir()?;
        let repo_one = init_repo(sandbox.path(), "alpha")?;
        let repo_two = init_repo(sandbox.path(), "beta")?;
        let repo_two = fs::canonicalize(repo_two)?;
        fs::write(repo_two.join("DIRTY.txt"), "dirty\n")?;

        let manager = WorkspaceManager::new(sandbox.path().join("spaces-home"));
        manager.create(CreateWorkspaceRequest {
            workspace_name: Some("steady-trail".into()),
            branch_name: None,
            repo_paths: vec![repo_one],
        })?;

        let result = manager.add(AddWorkspaceReposRequest {
            workspace_name: "steady-trail".into(),
            repo_paths: vec![repo_two.clone()],
        })?;

        assert_eq!(result.stashed_source_repos.len(), 1);
        assert_eq!(result.stashed_source_repos[0].source_repo_path, repo_two);
        assert!(result.stashed_source_repos[0]
            .stash_message
            .contains("steady-trail/beta"));
        assert!(git::status_is_clean(&repo_two)?);
        assert_eq!(git::list_stashes(&repo_two)?.len(), 1);
        assert!(!result.added_repos[0]
            .worktree_path
            .join("DIRTY.txt")
            .exists());

        Ok(())
    }

    #[test]
    fn add_rejects_repositories_already_in_the_workspace() -> Result<()> {
        let sandbox = tempdir()?;
        let repo_one = init_repo(sandbox.path(), "alpha")?;
        let manager = WorkspaceManager::new(sandbox.path().join("spaces-home"));

        manager.create(CreateWorkspaceRequest {
            workspace_name: Some("steady-trail".into()),
            branch_name: None,
            repo_paths: vec![repo_one.clone()],
        })?;

        let error = manager
            .add(AddWorkspaceReposRequest {
                workspace_name: "steady-trail".into(),
                repo_paths: vec![repo_one],
            })
            .expect_err("adding the same repo twice should fail");

        assert!(error
            .to_string()
            .contains("is already part of the workspace"));

        Ok(())
    }

    #[test]
    fn create_restores_staged_changes_when_fetch_fails_after_auto_stash() -> Result<()> {
        let sandbox = tempdir()?;
        let repo_one = init_repo(sandbox.path(), "alpha")?;
        let repo_two = init_repo(sandbox.path(), "beta")?;
        let repo_two = fs::canonicalize(repo_two)?;
        fs::write(repo_two.join("STAGED.txt"), "staged\n")?;
        run(Command::new("git")
            .current_dir(&repo_two)
            .arg("add")
            .arg("STAGED.txt"))?;
        run(Command::new("git")
            .current_dir(&repo_two)
            .arg("remote")
            .arg("set-url")
            .arg("origin")
            .arg(sandbox.path().join("missing-origin.git")))?;

        let manager = WorkspaceManager::new(sandbox.path().join("spaces-home"));
        let error = manager
            .create(CreateWorkspaceRequest {
                workspace_name: Some("broken-flight".into()),
                branch_name: None,
                repo_paths: vec![repo_one, repo_two.clone()],
            })
            .expect_err("fetch should fail after auto-stash");

        assert!(error.to_string().contains("failed to fetch origin/main"));
        assert!(!manager.registry_path().exists());
        assert!(!manager.base_dir().join("broken-flight").exists());
        assert!(git::list_stashes(&repo_two)?.is_empty());
        assert!(git::status_entries(&repo_two)?
            .iter()
            .any(|entry| entry == "A  STAGED.txt"));

        Ok(())
    }

    #[test]
    fn remove_workspace_keeps_branches_when_requested() -> Result<()> {
        let sandbox = tempdir()?;
        let repo_one = init_repo(sandbox.path(), "alpha")?;
        let repo_two = init_repo(sandbox.path(), "beta")?;
        let manager = WorkspaceManager::new(sandbox.path().join("spaces-home"));

        manager.create(CreateWorkspaceRequest {
            workspace_name: Some("merry-forest".into()),
            branch_name: None,
            repo_paths: vec![repo_one.clone(), repo_two.clone()],
        })?;

        let result = manager.remove(RemoveWorkspaceRequest {
            workspace_name: "merry-forest".into(),
            branch_action: RemoveBranchAction::Keep,
        })?;

        assert_eq!(result.removed_worktree_count, 2);
        assert!(git::local_branch_exists(&repo_one, "merry-forest")?);
        assert!(git::local_branch_exists(&repo_two, "merry-forest")?);
        assert!(manager.list()?.workspaces.is_empty());
        assert!(!manager.base_dir().join("merry-forest").exists());

        Ok(())
    }

    #[test]
    fn remove_workspace_can_delete_branches() -> Result<()> {
        let sandbox = tempdir()?;
        let repo_one = init_repo(sandbox.path(), "alpha")?;
        let repo_two = init_repo(sandbox.path(), "beta")?;
        let manager = WorkspaceManager::new(sandbox.path().join("spaces-home"));

        manager.create(CreateWorkspaceRequest {
            workspace_name: Some("tidy-voyage".into()),
            branch_name: None,
            repo_paths: vec![repo_one.clone(), repo_two.clone()],
        })?;

        manager.remove(RemoveWorkspaceRequest {
            workspace_name: "tidy-voyage".into(),
            branch_action: RemoveBranchAction::Delete,
        })?;

        assert!(!git::local_branch_exists(&repo_one, "tidy-voyage")?);
        assert!(!git::local_branch_exists(&repo_two, "tidy-voyage")?);
        Ok(())
    }

    fn init_repo(base_dir: &Path, name: &str) -> Result<PathBuf> {
        let remote_path = base_dir.join(format!("{name}-origin.git"));
        let repo_path = base_dir.join(name);

        run(Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&remote_path))?;
        run(Command::new("git").arg("init").arg(&repo_path))?;
        run(Command::new("git")
            .current_dir(&repo_path)
            .arg("config")
            .arg("user.name")
            .arg("Spaces Test"))?;
        run(Command::new("git")
            .current_dir(&repo_path)
            .arg("config")
            .arg("user.email")
            .arg("spaces@example.com"))?;
        run(Command::new("git")
            .current_dir(&repo_path)
            .arg("checkout")
            .arg("-b")
            .arg("main"))?;

        fs::write(repo_path.join("README.md"), format!("# {name}\n"))?;
        run(Command::new("git")
            .current_dir(&repo_path)
            .arg("add")
            .arg("README.md"))?;
        run(Command::new("git")
            .current_dir(&repo_path)
            .arg("commit")
            .arg("-m")
            .arg("initial"))?;
        run(Command::new("git")
            .current_dir(&repo_path)
            .arg("remote")
            .arg("add")
            .arg("origin")
            .arg(&remote_path))?;
        run(Command::new("git")
            .current_dir(&repo_path)
            .arg("push")
            .arg("-u")
            .arg("origin")
            .arg("main"))?;
        run(Command::new("git")
            .current_dir(&repo_path)
            .arg("fetch")
            .arg("origin")
            .arg("main"))?;

        Ok(repo_path)
    }

    fn run(command: &mut Command) -> Result<()> {
        let output = command.output().context("failed to run git test command")?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git test command failed: {}", stderr.trim());
        }
    }
}
