mod support;

use anyhow::Result;
use expectrl::Expect;
use serde_json::Value;
use std::fs;
use tempfile::tempdir;

use support::{drain_session, extract_json, init_repo, spawn_spaces, strip_ansi};

#[test]
fn create_directory_mode_supports_ratatui_repo_picker() -> Result<()> {
    let temp = tempdir()?;
    let base_dir = temp.path().join("spaces-home");
    let discovery_root = temp.path().join("repos");
    fs::create_dir_all(&discovery_root)?;

    init_repo(&discovery_root, "alpha")?;
    let selected_repo = fs::canonicalize(init_repo(&discovery_root, "beta")?)?;
    init_repo(&discovery_root, "gamma")?;

    let mut session = spawn_spaces(&[
        "-i",
        "--base-dir",
        base_dir.to_str().expect("utf-8 path"),
        "--name",
        "tidy-trail",
        discovery_root.to_str().expect("utf-8 path"),
    ])?;

    session.expect("Select repositories for the workspace")?;
    session.send("\u{1b}[B")?;
    session.send("bt")?;
    session.send(" ")?;
    session.send("\r")?;

    let transcript = strip_ansi(&drain_session(&mut session)?);
    let json = extract_json(&transcript)?;
    let repos = json["repos"].as_array().expect("repos array");

    assert_eq!(json["workspace_name"], Value::String("tidy-trail".into()));
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["repo_name"], Value::String("beta".into()));
    assert_eq!(
        repos[0]["source_repo_path"],
        Value::String(path_to_string(selected_repo))
    );
    assert!(base_dir.join("tidy-trail").join("beta").exists());
    assert!(!base_dir.join("tidy-trail").join("alpha").exists());
    assert!(!base_dir.join("tidy-trail").join("gamma").exists());

    Ok(())
}

#[test]
fn create_directory_mode_escape_cancels_without_creating_workspace() -> Result<()> {
    let temp = tempdir()?;
    let base_dir = temp.path().join("spaces-home");
    let discovery_root = temp.path().join("repos");
    fs::create_dir_all(&discovery_root)?;
    init_repo(&discovery_root, "alpha")?;

    let mut session = spawn_spaces(&[
        "-i",
        "--base-dir",
        base_dir.to_str().expect("utf-8 path"),
        "--name",
        "tidy-trail",
        discovery_root.to_str().expect("utf-8 path"),
    ])?;

    session.expect("Select repositories for the workspace")?;
    session.send("\u{1b}")?;

    let transcript = strip_ansi(&drain_session(&mut session)?);
    assert!(transcript.contains("interactive repo selection was canceled"));
    assert!(!base_dir.join("registry.json").exists());
    assert!(!base_dir.join("tidy-trail").exists());

    Ok(())
}

fn path_to_string(path: std::path::PathBuf) -> String {
    path.to_str().expect("utf-8 path").to_owned()
}
