use crate::app::{
    default_base_dir, prompt_for_branch_action, AddWorkspaceReposRequest, CreateWorkspaceRequest,
    RemoveBranchAction, RemoveWorkspaceRequest, WorkspaceManager,
};
use crate::git;
use crate::repo_picker::{prompt_for_repo_selection as run_repo_picker, RepoPromptOption};
use anyhow::{bail, Context, Result};
use clap::{ArgGroup, Args, Parser, Subcommand};
use serde::Serialize;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(name = "spaces")]
#[command(about = "Create and manage coordinated multi-repo git workspaces")]
#[command(args_conflicts_with_subcommands = true)]
#[command(subcommand_negates_reqs = true)]
struct Cli {
    #[command(flatten)]
    create: CreateArgs,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Create(CreateArgs),
    Add(AddArgs),
    #[command(alias = "ls")]
    List(ListArgs),
    Cwd(CwdArgs),
    Show(ShowArgs),
    #[command(alias = "rm")]
    Remove(RemoveArgs),
}

#[derive(Debug, Args)]
struct CreateArgs {
    #[arg(short = 'i', long)]
    interactive: bool,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    branch: Option<String>,
    #[arg(long)]
    base_dir: Option<PathBuf>,
    #[arg(long)]
    json: bool,
    #[arg(required = true)]
    repos: Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[arg(long)]
    base_dir: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct AddArgs {
    workspace: String,
    #[arg(long)]
    base_dir: Option<PathBuf>,
    #[arg(long)]
    json: bool,
    #[arg(required = true)]
    repos: Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct ShowArgs {
    workspace: String,
    #[arg(long)]
    base_dir: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("workspace_selector")
        .required(true)
        .args(["workspace", "last"])
))]
struct CwdArgs {
    workspace: Option<String>,
    #[arg(long)]
    last: bool,
    #[arg(long)]
    base_dir: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct RemoveArgs {
    workspace: String,
    #[arg(long)]
    base_dir: Option<PathBuf>,
    #[arg(long)]
    yes: bool,
    #[arg(long, conflicts_with = "delete_branches")]
    keep_branches: bool,
    #[arg(long, conflicts_with = "keep_branches")]
    delete_branches: bool,
    #[arg(long)]
    json: bool,
}

trait RepoSelector {
    fn select_repos(
        &mut self,
        discovery_root: &Path,
        repo_roots: &[PathBuf],
    ) -> Result<Vec<PathBuf>>;
}

impl<F> RepoSelector for F
where
    F: FnMut(&Path, &[PathBuf]) -> Result<Vec<PathBuf>>,
{
    fn select_repos(
        &mut self,
        discovery_root: &Path,
        repo_roots: &[PathBuf],
    ) -> Result<Vec<PathBuf>> {
        self(discovery_root, repo_roots)
    }
}

pub fn run_from<I, T>(args: I, input: &mut dyn BufRead, output: &mut dyn Write) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let mut selector = prompt_for_repo_selection;
    run_from_with_selector(args, input, output, &mut selector)
}

fn run_from_with_selector<I, T, S>(
    args: I,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    repo_selector: &mut S,
) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    S: RepoSelector,
{
    let cli = Cli::parse_from(args);

    match cli.command {
        Some(Commands::Create(args)) => run_create(args, output, repo_selector),
        Some(Commands::Add(args)) => {
            let manager = WorkspaceManager::new(args.base_dir.unwrap_or(default_base_dir()?));
            let result = manager.add(AddWorkspaceReposRequest {
                workspace_name: args.workspace,
                repo_paths: args.repos,
            })?;
            let _ = args.json;
            render(output, true, &result)
        }
        Some(Commands::List(args)) => {
            let manager = WorkspaceManager::new(args.base_dir.unwrap_or(default_base_dir()?));
            let result = manager.list()?;
            let _ = args.json;
            render(output, true, &result)
        }
        Some(Commands::Cwd(args)) => {
            let manager = WorkspaceManager::new(args.base_dir.unwrap_or(default_base_dir()?));
            let cwd = if args.last {
                manager.cwd_last()?
            } else {
                manager.cwd(
                    args.workspace
                        .as_deref()
                        .expect("clap should require a selector"),
                )?
            };
            if args.json {
                render(output, true, &cwd)
            } else {
                writeln!(output, "{}", cwd.workspace_dir.display())
                    .context("failed to write cwd output")
            }
        }
        Some(Commands::Show(args)) => {
            let manager = WorkspaceManager::new(args.base_dir.unwrap_or(default_base_dir()?));
            let result = manager.show(&args.workspace)?;
            let _ = args.json;
            render(output, true, &result)
        }
        Some(Commands::Remove(args)) => {
            let base_dir = args.base_dir.clone().unwrap_or(default_base_dir()?);
            let manager = WorkspaceManager::new(base_dir);
            let branch_action = resolve_branch_action(&args, input, output)?;
            let result = manager.remove(RemoveWorkspaceRequest {
                workspace_name: args.workspace,
                branch_action,
            })?;
            let _ = args.json;
            render(output, true, &result)
        }
        None => run_create(cli.create, output, repo_selector),
    }
}

fn resolve_create_repo_paths<S>(args: &CreateArgs, repo_selector: &mut S) -> Result<Vec<PathBuf>>
where
    S: RepoSelector,
{
    if !args.interactive {
        return Ok(args.repos.clone());
    }

    if args.repos.len() != 1 {
        bail!("interactive repo selection requires exactly one directory path");
    }

    let requested_path = &args.repos[0];
    let Some(discovery_root) = resolve_discovery_root(requested_path)? else {
        bail!("interactive repo selection requires a non-repository directory path");
    };

    let repo_roots = discover_repo_roots(&discovery_root)?;
    if repo_roots.is_empty() {
        bail!(
            "no git repositories were found under {}",
            discovery_root.display()
        );
    }

    let selected = repo_selector.select_repos(&discovery_root, &repo_roots)?;
    if selected.is_empty() {
        bail!("no repositories were selected");
    }

    Ok(selected)
}

fn run_create<S>(args: CreateArgs, output: &mut dyn Write, repo_selector: &mut S) -> Result<()>
where
    S: RepoSelector,
{
    let repo_paths = resolve_create_repo_paths(&args, repo_selector)?;
    let base_dir = args.base_dir.unwrap_or(default_base_dir()?);
    let manager = WorkspaceManager::new(base_dir);
    let result = manager.create(CreateWorkspaceRequest {
        workspace_name: args.name,
        branch_name: args.branch,
        repo_paths,
    })?;
    let _ = args.json;
    render(output, true, &result)
}

fn resolve_discovery_root(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() || !path.is_dir() {
        return Ok(None);
    }

    if git::repo_root_if_repo(path)?.is_some() {
        return Ok(None);
    }

    fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize {}", path.display()))
        .map(Some)
}

fn discover_repo_roots(discovery_root: &Path) -> Result<Vec<PathBuf>> {
    let mut repo_roots = Vec::new();
    let mut seen = HashSet::new();

    for entry in fs::read_dir(discovery_root).with_context(|| {
        format!(
            "failed to inspect immediate child directories under {}",
            discovery_root.display()
        )
    })? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let path = entry.path();
        if !contains_git_dir(&path)? {
            continue;
        }

        let Some(repo_root) = git::repo_root_if_repo(&path)? else {
            continue;
        };

        let repo_root = fs::canonicalize(&repo_root)
            .with_context(|| format!("failed to canonicalize {}", repo_root.display()))?;

        if !repo_root.starts_with(discovery_root) {
            continue;
        }

        if seen.insert(repo_root.clone()) {
            repo_roots.push(repo_root);
        }
    }

    repo_roots.sort();
    Ok(repo_roots)
}

fn contains_git_dir(path: &Path) -> Result<bool> {
    let git_path = path.join(".git");
    match fs::symlink_metadata(&git_path) {
        Ok(metadata) => Ok(metadata.is_dir() || metadata.file_type().is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect {}", git_path.display()))
        }
    }
}

fn prompt_for_repo_selection(
    discovery_root: &Path,
    repo_roots: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        bail!("interactive repo selection requires a terminal; pass explicit repo paths instead");
    }

    let options = repo_roots
        .iter()
        .map(|repo_root| build_repo_prompt_option(discovery_root, repo_root))
        .collect::<Result<Vec<_>>>()?;

    run_repo_picker(options)
}

fn build_repo_prompt_option(discovery_root: &Path, repo_root: &Path) -> Result<RepoPromptOption> {
    let repo_name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("failed to derive repo name for {}", repo_root.display()))?;
    let relative_path = repo_root.strip_prefix(discovery_root).unwrap_or(repo_root);
    let label = if relative_path == Path::new(repo_name) {
        repo_name.to_owned()
    } else {
        format!("{repo_name} ({})", relative_path.display())
    };

    Ok(RepoPromptOption {
        label,
        repo_root: repo_root.to_path_buf(),
    })
}

fn resolve_branch_action(
    args: &RemoveArgs,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<RemoveBranchAction> {
    if args.keep_branches {
        return Ok(RemoveBranchAction::Keep);
    }

    if args.delete_branches {
        return Ok(RemoveBranchAction::Delete);
    }

    if args.yes {
        bail!("`spaces remove --yes` requires either --keep-branches or --delete-branches");
    }

    prompt_for_branch_action(&args.workspace, input, output)
}

fn render<T>(output: &mut dyn Write, json: bool, value: &T) -> Result<()>
where
    T: Serialize + std::fmt::Debug,
{
    if json {
        serde_json::to_writer_pretty(&mut *output, value).context("failed to write JSON output")?;
        writeln!(output)?;
    } else {
        writeln!(output, "{value:#?}")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run_from, run_from_with_selector, Cli};
    use crate::app::{CreateWorkspaceRequest, WorkspaceManager};
    use crate::registry::{Registry, RegistryStore, WorkspaceRecord};
    use anyhow::Context;
    use anyhow::Result;
    use clap::error::ErrorKind;
    use clap::Parser;
    use serde_json::Value;
    use std::fs;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn list_uses_custom_base_dir() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let store = RegistryStore::new(base_dir.clone());
        let mut registry = Registry::default();
        registry.upsert(WorkspaceRecord {
            name: "steady-trail".into(),
            branch_name: "steady-trail".into(),
            created_at_epoch_seconds: 123,
            workspace_dir: base_dir.join("steady-trail"),
            repos: Vec::new(),
        });
        store.save(&registry)?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "list",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )?;

        let value: Value = serde_json::from_slice(&output)?;
        let workspaces = value["workspaces"].as_array().expect("workspaces array");
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["workspace_name"], "steady-trail");

        Ok(())
    }

    #[test]
    fn ls_alias_uses_custom_base_dir() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let store = RegistryStore::new(base_dir.clone());
        let mut registry = Registry::default();
        registry.upsert(WorkspaceRecord {
            name: "steady-trail".into(),
            branch_name: "steady-trail".into(),
            created_at_epoch_seconds: 123,
            workspace_dir: base_dir.join("steady-trail"),
            repos: Vec::new(),
        });
        store.save(&registry)?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "ls",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )?;

        let value: Value = serde_json::from_slice(&output)?;
        let workspaces = value["workspaces"].as_array().expect("workspaces array");
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["workspace_name"], "steady-trail");

        Ok(())
    }

    #[test]
    fn rm_alias_removes_a_workspace() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let repo_path = init_repo(temp.path(), "alpha")?;
        let manager = WorkspaceManager::new(base_dir.clone());

        let created = manager.create(CreateWorkspaceRequest {
            workspace_name: Some("steady-trail".into()),
            branch_name: None,
            repo_paths: vec![repo_path],
        })?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "rm",
                "steady-trail",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                "--yes",
                "--keep-branches",
            ],
            &mut input,
            &mut output,
        )?;

        let value: Value = serde_json::from_slice(&output)?;
        assert_eq!(value["workspace_name"], "steady-trail");
        assert_eq!(value["branch_action"], "keep");
        assert!(!created.workspace_dir.exists());
        assert!(manager.list()?.workspaces.is_empty());

        Ok(())
    }

    #[test]
    fn show_uses_custom_base_dir() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let workspace_dir = base_dir.join("steady-trail");
        std::fs::create_dir_all(&workspace_dir)?;
        let store = RegistryStore::new(base_dir.clone());
        let mut registry = Registry::default();
        registry.upsert(WorkspaceRecord {
            name: "steady-trail".into(),
            branch_name: "steady-trail".into(),
            created_at_epoch_seconds: 123,
            workspace_dir: workspace_dir.clone(),
            repos: Vec::new(),
        });
        store.save(&registry)?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "show",
                "steady-trail",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )?;

        let value: Value = serde_json::from_slice(&output)?;
        assert_eq!(value["workspace_name"], "steady-trail");
        assert_eq!(
            value["workspace_dir"],
            Value::String(path_to_string(workspace_dir))
        );

        Ok(())
    }

    #[test]
    fn cwd_uses_custom_base_dir() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let workspace_dir = base_dir.join("steady-trail");
        std::fs::create_dir_all(&workspace_dir)?;
        let store = RegistryStore::new(base_dir.clone());
        let mut registry = Registry::default();
        registry.upsert(WorkspaceRecord {
            name: "steady-trail".into(),
            branch_name: "steady-trail".into(),
            created_at_epoch_seconds: 123,
            workspace_dir: workspace_dir.clone(),
            repos: Vec::new(),
        });
        store.save(&registry)?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "cwd",
                "steady-trail",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )?;

        assert_eq!(
            String::from_utf8(output)?,
            format!("{}\n", workspace_dir.display())
        );

        Ok(())
    }

    #[test]
    fn cwd_supports_json_output() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let workspace_dir = base_dir.join("steady-trail");
        std::fs::create_dir_all(&workspace_dir)?;
        let store = RegistryStore::new(base_dir.clone());
        let mut registry = Registry::default();
        registry.upsert(WorkspaceRecord {
            name: "steady-trail".into(),
            branch_name: "steady-trail".into(),
            created_at_epoch_seconds: 123,
            workspace_dir: workspace_dir.clone(),
            repos: Vec::new(),
        });
        store.save(&registry)?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "cwd",
                "steady-trail",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                "--json",
            ],
            &mut input,
            &mut output,
        )?;

        let value: Value = serde_json::from_slice(&output)?;
        assert_eq!(value["workspace_name"], "steady-trail");
        assert_eq!(
            value["workspace_dir"],
            Value::String(path_to_string(workspace_dir))
        );

        Ok(())
    }

    #[test]
    fn cwd_last_uses_most_recent_workspace() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let older_workspace_dir = base_dir.join("amber-anchor");
        let newer_workspace_dir = base_dir.join("steady-trail");
        fs::create_dir_all(&older_workspace_dir)?;
        fs::create_dir_all(&newer_workspace_dir)?;
        let store = RegistryStore::new(base_dir.clone());
        let mut registry = Registry::default();
        registry.upsert(WorkspaceRecord {
            name: "amber-anchor".into(),
            branch_name: "amber-anchor".into(),
            created_at_epoch_seconds: 100,
            workspace_dir: older_workspace_dir,
            repos: Vec::new(),
        });
        registry.upsert(WorkspaceRecord {
            name: "steady-trail".into(),
            branch_name: "steady-trail".into(),
            created_at_epoch_seconds: 200,
            workspace_dir: newer_workspace_dir.clone(),
            repos: Vec::new(),
        });
        store.save(&registry)?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "cwd",
                "--last",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )?;

        assert_eq!(
            String::from_utf8(output)?,
            format!("{}\n", newer_workspace_dir.display())
        );

        Ok(())
    }

    #[test]
    fn cwd_last_supports_json_output() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let older_workspace_dir = base_dir.join("amber-anchor");
        let newer_workspace_dir = base_dir.join("steady-trail");
        fs::create_dir_all(&older_workspace_dir)?;
        fs::create_dir_all(&newer_workspace_dir)?;
        let store = RegistryStore::new(base_dir.clone());
        let mut registry = Registry::default();
        registry.upsert(WorkspaceRecord {
            name: "amber-anchor".into(),
            branch_name: "amber-anchor".into(),
            created_at_epoch_seconds: 100,
            workspace_dir: older_workspace_dir,
            repos: Vec::new(),
        });
        registry.upsert(WorkspaceRecord {
            name: "steady-trail".into(),
            branch_name: "steady-trail".into(),
            created_at_epoch_seconds: 200,
            workspace_dir: newer_workspace_dir.clone(),
            repos: Vec::new(),
        });
        store.save(&registry)?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "cwd",
                "--last",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                "--json",
            ],
            &mut input,
            &mut output,
        )?;

        let value: Value = serde_json::from_slice(&output)?;
        assert_eq!(value["workspace_name"], "steady-trail");
        assert_eq!(
            value["workspace_dir"],
            Value::String(path_to_string(newer_workspace_dir))
        );

        Ok(())
    }

    #[test]
    fn cwd_errors_when_workspace_dir_is_missing() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let workspace_dir = base_dir.join("steady-trail");
        let store = RegistryStore::new(base_dir.clone());
        let mut registry = Registry::default();
        registry.upsert(WorkspaceRecord {
            name: "steady-trail".into(),
            branch_name: "steady-trail".into(),
            created_at_epoch_seconds: 123,
            workspace_dir: workspace_dir.clone(),
            repos: Vec::new(),
        });
        store.save(&registry)?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = run_from(
            [
                "spaces",
                "cwd",
                "steady-trail",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )
        .expect_err("missing workspace dir should fail");

        assert!(error
            .to_string()
            .contains("workspace directory is missing at"));

        Ok(())
    }

    #[test]
    fn cwd_errors_for_unknown_workspace() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = run_from(
            [
                "spaces",
                "cwd",
                "steady-trail",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )
        .expect_err("unknown workspace should fail");

        assert!(error
            .to_string()
            .contains("workspace `steady-trail` was not found"));

        Ok(())
    }

    #[test]
    fn cwd_last_errors_when_no_workspaces_are_tracked() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = run_from(
            [
                "spaces",
                "cwd",
                "--last",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )
        .expect_err("empty registry should fail");

        assert!(error.to_string().contains("no workspaces are tracked"));

        Ok(())
    }

    #[test]
    fn cwd_parser_rejects_combining_workspace_and_last() {
        let error = Cli::try_parse_from(["spaces", "cwd", "steady-trail", "--last"])
            .expect_err("workspace name and --last should conflict");

        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn cwd_parser_requires_workspace_or_last() {
        let error =
            Cli::try_parse_from(["spaces", "cwd"]).expect_err("cwd should require a selector");

        assert!(error
            .to_string()
            .contains("required arguments were not provided"));
    }

    #[test]
    fn create_json_reports_auto_stashed_source_repos() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let repo_path = init_repo(temp.path(), "alpha")?;
        let repo_path = fs::canonicalize(repo_path)?;
        fs::write(repo_path.join("DIRTY.txt"), "dirty\n")?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                "--name",
                "rapid-signal",
                repo_path.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )?;

        let value: Value = serde_json::from_slice(&output)?;
        let stashed = value["stashed_source_repos"]
            .as_array()
            .expect("stashed source repos array");
        assert_eq!(stashed.len(), 1);
        assert_eq!(
            stashed[0]["source_repo_path"],
            Value::String(path_to_string(repo_path))
        );
        assert!(stashed[0]["stash_message"]
            .as_str()
            .expect("stash message")
            .contains("rapid-signal/alpha"));

        Ok(())
    }

    #[test]
    fn create_subcommand_remains_supported() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let repo_path = init_repo(temp.path(), "alpha")?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "create",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                "--name",
                "steady-trail",
                repo_path.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )?;

        let value: Value = serde_json::from_slice(&output)?;
        assert_eq!(value["workspace_name"], "steady-trail");

        Ok(())
    }

    #[test]
    fn add_subcommand_updates_an_existing_workspace() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let repo_one = init_repo(temp.path(), "alpha")?;
        let repo_two = init_repo(temp.path(), "beta")?;
        let manager = WorkspaceManager::new(base_dir.clone());

        manager.create(CreateWorkspaceRequest {
            workspace_name: Some("steady-trail".into()),
            branch_name: None,
            repo_paths: vec![repo_one],
        })?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from(
            [
                "spaces",
                "add",
                "steady-trail",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                repo_two.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )?;

        let value: Value = serde_json::from_slice(&output)?;
        assert_eq!(value["workspace_name"], "steady-trail");
        assert_eq!(
            value["added_repos"]
                .as_array()
                .expect("added repos array")
                .len(),
            1
        );
        assert_eq!(value["repos"].as_array().expect("repos array").len(), 2);

        Ok(())
    }

    #[test]
    fn create_directory_mode_uses_discovered_repos() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let discovery_root = temp.path().join("repos");
        fs::create_dir_all(&discovery_root)?;
        let discovery_root = fs::canonicalize(&discovery_root)?;
        let repo_one = fs::canonicalize(init_repo(&discovery_root, "alpha")?)?;
        let repo_two = fs::canonicalize(init_repo(&discovery_root, "beta")?)?;

        let expected = vec![repo_one.clone(), repo_two.clone()];
        let mut selector_called = false;
        let mut selector = |root: &Path, repo_roots: &[PathBuf]| -> Result<Vec<PathBuf>> {
            selector_called = true;
            assert_eq!(root, discovery_root.as_path());
            assert_eq!(repo_roots, expected.as_slice());
            Ok(repo_roots.to_vec())
        };

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from_with_selector(
            [
                "spaces",
                "-i",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                "--name",
                "rapid-signal",
                discovery_root.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
            &mut selector,
        )?;

        assert!(selector_called);

        let value: Value = serde_json::from_slice(&output)?;
        let repos = value["repos"].as_array().expect("repos array");
        assert_eq!(repos.len(), 2);
        assert_eq!(value["workspace_name"], "rapid-signal");

        Ok(())
    }

    #[test]
    fn create_directory_mode_ignores_nested_repositories() -> Result<()> {
        let temp = tempdir()?;
        let discovery_root = temp.path().join("repos");
        fs::create_dir_all(discovery_root.join("clients"))?;
        let discovery_root = fs::canonicalize(&discovery_root)?;
        let repo_one = fs::canonicalize(init_repo(&discovery_root, "alpha")?)?;
        init_repo(&discovery_root.join("clients"), "beta")?;

        let discovered = super::discover_repo_roots(&discovery_root)?;
        assert_eq!(discovered, vec![repo_one]);

        Ok(())
    }

    #[test]
    fn create_directory_mode_fails_when_no_repos_are_found() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let discovery_root = temp.path().join("empty");
        fs::create_dir_all(&discovery_root)?;

        let mut selector = |_: &Path, _: &[PathBuf]| -> Result<Vec<PathBuf>> {
            panic!("selector should not be called")
        };
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = run_from_with_selector(
            [
                "spaces",
                "-i",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                discovery_root.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
            &mut selector,
        )
        .expect_err("directory mode should fail without repos");

        assert!(error
            .to_string()
            .contains("no git repositories were found under"));

        Ok(())
    }

    #[test]
    fn create_directory_mode_fails_when_selection_is_empty() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let discovery_root = temp.path().join("repos");
        fs::create_dir_all(&discovery_root)?;
        init_repo(&discovery_root, "alpha")?;

        let mut selector = |_: &Path, _: &[PathBuf]| -> Result<Vec<PathBuf>> { Ok(Vec::new()) };
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = run_from_with_selector(
            [
                "spaces",
                "-i",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                discovery_root.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
            &mut selector,
        )
        .expect_err("empty selections should fail");

        assert!(error.to_string().contains("no repositories were selected"));

        Ok(())
    }

    #[test]
    fn create_single_repo_path_bypasses_directory_mode() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let repo_path = init_repo(temp.path(), "alpha")?;
        let mut selector_called = false;
        let mut selector = |_: &Path, _: &[PathBuf]| -> Result<Vec<PathBuf>> {
            selector_called = true;
            Ok(Vec::new())
        };

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        run_from_with_selector(
            [
                "spaces",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                "--name",
                "steady-trail",
                repo_path.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
            &mut selector,
        )?;

        assert!(!selector_called);

        let value: Value = serde_json::from_slice(&output)?;
        assert_eq!(value["workspace_name"], "steady-trail");

        Ok(())
    }

    #[test]
    fn interactive_mode_requires_a_terminal_for_real_prompt() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let discovery_root = temp.path().join("repos");
        fs::create_dir_all(&discovery_root)?;
        init_repo(&discovery_root, "alpha")?;

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = run_from(
            [
                "spaces",
                "-i",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                discovery_root.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
        )
        .expect_err("tests should not have a tty");

        assert!(error
            .to_string()
            .contains("interactive repo selection requires a terminal"));

        Ok(())
    }

    #[test]
    fn directory_path_without_interactive_flag_bypasses_selector() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let discovery_root = temp.path().join("repos");
        fs::create_dir_all(&discovery_root)?;
        init_repo(&discovery_root, "alpha")?;

        let mut selector_called = false;
        let mut selector = |_: &Path, _: &[PathBuf]| -> Result<Vec<PathBuf>> {
            selector_called = true;
            Ok(Vec::new())
        };

        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = run_from_with_selector(
            [
                "spaces",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                discovery_root.to_str().expect("utf-8 path"),
            ],
            &mut input,
            &mut output,
            &mut selector,
        )
        .expect_err("directory path should be treated as a repo path without -i");

        assert!(!selector_called);
        assert!(error.to_string().contains("failed to treat"));

        Ok(())
    }

    fn path_to_string(path: PathBuf) -> String {
        path.to_str().expect("utf-8 path").to_owned()
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
