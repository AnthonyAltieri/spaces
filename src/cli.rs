use crate::app::{
    default_base_dir, prompt_for_branch_action, CreateWorkspaceRequest, RemoveBranchAction,
    RemoveWorkspaceRequest, WorkspaceManager,
};
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;

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

pub fn run_from<I, T>(
    args: I,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);

    match cli.command {
        Commands::Create(args) => {
            let base_dir = args.base_dir.unwrap_or(default_base_dir()?);
            let manager = WorkspaceManager::new(base_dir);
            let result = manager.create(CreateWorkspaceRequest {
                workspace_name: args.name,
                branch_name: args.branch,
                repo_paths: args.repos,
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
    use super::run_from;
    use crate::registry::{Registry, RegistryStore, WorkspaceRecord};
    use anyhow::Result;
    use serde_json::Value;
    use std::io::Cursor;
    use std::path::PathBuf;
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

    fn path_to_string(path: PathBuf) -> String {
        path.to_str().expect("utf-8 path").to_owned()
    }
}
