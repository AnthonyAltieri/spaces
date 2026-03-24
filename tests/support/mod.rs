use anyhow::{bail, Context, Result};
use expectrl::session::OsSession;
use expectrl::Session;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub fn cargo_bin_path() -> PathBuf {
    PathBuf::from(
        std::env::var_os("CARGO_BIN_EXE_spaces").expect("cargo should provide the spaces binary"),
    )
}

pub fn init_repo(base_dir: &Path, name: &str) -> Result<PathBuf> {
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

pub fn spawn_spaces(args: &[&str]) -> Result<OsSession> {
    let mut command = Command::new(cargo_bin_path());
    command.args(args);
    let mut session = Session::spawn(command).context("failed to spawn spaces in a PTY")?;
    session.set_expect_timeout(Some(Duration::from_secs(20)));
    Ok(session)
}

pub fn drain_session(session: &mut OsSession) -> Result<String> {
    let mut output = String::new();
    session
        .read_to_string(&mut output)
        .context("failed to read PTY output")?;
    Ok(output)
}

pub fn strip_ansi(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();

    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    while let Some(next) = chars.next() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.next_if_eq(&'\\').is_some() {
                            break;
                        }
                    }
                }
                Some(_) | None => {}
            }
            continue;
        }

        if character != '\r' {
            output.push(character);
        }
    }

    output
}

pub fn extract_json(text: &str) -> Result<Value> {
    for (index, character) in text.char_indices() {
        if character != '{' {
            continue;
        }

        if let Ok(value) = serde_json::from_str::<Value>(&text[index..]) {
            return Ok(value);
        }
    }

    bail!("failed to find JSON in output: {text}");
}

fn run(command: &mut Command) -> Result<()> {
    let output = command.output().context("failed to run git test command")?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git test command failed: {}", stderr.trim());
    }
}
