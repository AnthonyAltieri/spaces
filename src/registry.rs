use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoRecord {
    pub repo_name: String,
    pub source_repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub remote_name: String,
    pub base_ref: String,
    pub base_commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceRecord {
    pub name: String,
    pub branch_name: String,
    pub created_at_epoch_seconds: u64,
    pub workspace_dir: PathBuf,
    pub repos: Vec<RepoRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Registry {
    pub version: u32,
    pub workspaces: Vec<WorkspaceRecord>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            version: 1,
            workspaces: Vec::new(),
        }
    }
}

impl Registry {
    pub fn get(&self, workspace_name: &str) -> Option<&WorkspaceRecord> {
        self.workspaces.iter().find(|workspace| workspace.name == workspace_name)
    }

    pub fn contains_workspace(&self, workspace_name: &str) -> bool {
        self.get(workspace_name).is_some()
    }

    pub fn upsert(&mut self, workspace: WorkspaceRecord) {
        self.remove(&workspace.name);
        self.workspaces.push(workspace);
        self.workspaces.sort_by(|left, right| left.name.cmp(&right.name));
    }

    pub fn remove(&mut self, workspace_name: &str) -> Option<WorkspaceRecord> {
        let index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.name == workspace_name)?;
        Some(self.workspaces.remove(index))
    }
}

#[derive(Debug, Clone)]
pub struct RegistryStore {
    base_dir: PathBuf,
    registry_path: PathBuf,
}

impl RegistryStore {
    pub fn new(base_dir: PathBuf) -> Self {
        let registry_path = base_dir.join("registry.json");
        Self {
            base_dir,
            registry_path,
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn registry_path(&self) -> &Path {
        &self.registry_path
    }

    pub fn load(&self) -> Result<Registry> {
        if !self.registry_path.exists() {
            return Ok(Registry::default());
        }

        let contents = fs::read_to_string(&self.registry_path).with_context(|| {
            format!(
                "failed to read registry from {}",
                self.registry_path.display()
            )
        })?;

        let registry: Registry = serde_json::from_str(&contents).with_context(|| {
            format!(
                "failed to parse registry from {}",
                self.registry_path.display()
            )
        })?;

        Ok(registry)
    }

    pub fn save(&self, registry: &Registry) -> Result<()> {
        fs::create_dir_all(&self.base_dir).with_context(|| {
            format!("failed to create base directory {}", self.base_dir.display())
        })?;

        let temp_path = self.registry_path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(registry).context("failed to serialize registry")?;
        fs::write(&temp_path, bytes)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        fs::rename(&temp_path, &self.registry_path).with_context(|| {
            format!(
                "failed to replace registry at {}",
                self.registry_path.display()
            )
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Registry, RegistryStore, WorkspaceRecord};
    use anyhow::Result;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn loads_empty_registry_when_missing() -> Result<()> {
        let dir = tempdir()?;
        let store = RegistryStore::new(dir.path().join(".spaces"));
        let registry = store.load()?;

        assert_eq!(registry, Registry::default());
        Ok(())
    }

    #[test]
    fn saves_and_loads_registry() -> Result<()> {
        let dir = tempdir()?;
        let store = RegistryStore::new(dir.path().join(".spaces"));
        let mut registry = Registry::default();
        registry.upsert(WorkspaceRecord {
            name: "amber-anchor".into(),
            branch_name: "amber-anchor".into(),
            created_at_epoch_seconds: 123,
            workspace_dir: PathBuf::from("/tmp/example"),
            repos: Vec::new(),
        });

        store.save(&registry)?;
        let loaded = store.load()?;

        assert_eq!(loaded, registry);
        Ok(())
    }
}
