use crate::app::{
    default_base_dir, prompt_for_branch_action, CreateWorkspaceRequest, RemoveBranchAction,
    RemoveWorkspaceRequest, WorkspaceManager,
};
use crate::git;
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use inquire::{InquireError, MultiSelect};
use serde::Serialize;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Parser)]
#[command(name = "spaces")]
#[command(about = "Create and manage coordinated multi-repo git workspaces")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Create(CreateArgs),
    #[command(alias = "ls")]
    List(ListArgs),
    Show(ShowArgs),
    Remove(RemoveArgs),
}

#[derive(Debug, Args)]
struct CreateArgs {
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
struct ShowArgs {
    workspace: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoPromptOption {
    label: String,
    repo_root: PathBuf,
}

impl fmt::Display for RepoPromptOption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
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
        Commands::Create(args) => {
            let repo_paths = resolve_create_repo_paths(&args, repo_selector)?;
            let base_dir = args.base_dir.unwrap_or(default_base_dir()?);
            let manager = WorkspaceManager::new(base_dir);
            let result = manager.create(CreateWorkspaceRequest {
                workspace_name: args.name,
                branch_name: args.branch,
                repo_paths,
            })?;
            render(output, args.json, &result)
        }
        Commands::List(args) => {
            let manager = WorkspaceManager::new(args.base_dir.unwrap_or(default_base_dir()?));
            let result = manager.list()?;
            render(output, args.json, &result)
        }
        Commands::Show(args) => {
            let manager = WorkspaceManager::new(args.base_dir.unwrap_or(default_base_dir()?));
            let result = manager.show(&args.workspace)?;
            render(output, args.json, &result)
        }
        Commands::Remove(args) => {
            let base_dir = args.base_dir.clone().unwrap_or(default_base_dir()?);
            let manager = WorkspaceManager::new(base_dir);
            let branch_action = resolve_branch_action(&args, input, output)?;
            let result = manager.remove(RemoveWorkspaceRequest {
                workspace_name: args.workspace,
                branch_action,
            })?;
            render(output, args.json, &result)
        }
    }
}

fn resolve_create_repo_paths<S>(args: &CreateArgs, repo_selector: &mut S) -> Result<Vec<PathBuf>>
where
    S: RepoSelector,
{
    if args.repos.len() != 1 {
        return Ok(args.repos.clone());
    }

    let requested_path = &args.repos[0];
    let Some(discovery_root) = resolve_discovery_root(requested_path)? else {
        return Ok(args.repos.clone());
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
    let mut walker = WalkDir::new(discovery_root).follow_links(false).into_iter();

    while let Some(entry) = walker.next() {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect directory tree under {}",
                discovery_root.display()
            )
        })?;

        if !entry.file_type().is_dir() {
            continue;
        }

        if entry.file_name() == ".git" {
            walker.skip_current_dir();
            continue;
        }

        let path = entry.path();
        let Some(repo_root) = git::repo_root_if_repo(path)? else {
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

        walker.skip_current_dir();
    }

    repo_roots.sort();
    Ok(repo_roots)
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

    let selected = MultiSelect::new("Select repositories for the workspace", options)
        .with_page_size(12)
        .prompt()
        .map_err(map_prompt_error)?;

    Ok(selected
        .into_iter()
        .map(|option| option.repo_root)
        .collect())
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

fn map_prompt_error(error: InquireError) -> anyhow::Error {
    match error {
        InquireError::OperationCanceled | InquireError::OperationInterrupted => {
            anyhow::anyhow!("interactive repo selection was canceled")
        }
        other => other.into(),
    }
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
    use super::{run_from, run_from_with_selector};
    use crate::registry::{Registry, RegistryStore, WorkspaceRecord};
    use anyhow::Context;
    use anyhow::Result;
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
                "--json",
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
                "--json",
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
                "create",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                "--name",
                "rapid-signal",
                "--json",
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
    fn create_directory_mode_uses_discovered_repos() -> Result<()> {
        let temp = tempdir()?;
        let base_dir = temp.path().join("spaces-home");
        let discovery_root = temp.path().join("repos");
        fs::create_dir_all(discovery_root.join("clients"))?;
        let discovery_root = fs::canonicalize(&discovery_root)?;
        let repo_one = fs::canonicalize(init_repo(&discovery_root, "alpha")?)?;
        let repo_two = fs::canonicalize(init_repo(&discovery_root.join("clients"), "beta")?)?;

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
                "create",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                "--name",
                "rapid-signal",
                "--json",
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
                "create",
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
                "create",
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
                "create",
                "--base-dir",
                base_dir.to_str().expect("utf-8 path"),
                "--name",
                "steady-trail",
                "--json",
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
    fn create_directory_mode_requires_a_terminal_for_real_prompt() -> Result<()> {
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
                "create",
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
