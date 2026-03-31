use anyhow::{Context, Result};
use git2::{Repository, BranchType};
use std::path::{Path, PathBuf};

pub struct WorktreeManager {
    repo_path: PathBuf,
}

impl WorktreeManager {
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }

    pub fn create_worktree(&self, agent_id: &str) -> Result<PathBuf> {
        let repo = Repository::open(&self.repo_path)
            .context("Failed to open repository")?;

        let branch_name = format!("agent/{agent_id}");
        let worktree_path = self.repo_path.join(".worktrees").join(agent_id);

        let head = repo.head()?.peel_to_commit()?;
        repo.branch(&branch_name, &head, false)?;

        repo.worktree(
            agent_id,
            &worktree_path,
            Some(git2::WorktreeAddOptions::new().reference(
                Some(&repo.find_branch(&branch_name, BranchType::Local)?.into_reference()),
            )),
        )?;

        Ok(worktree_path)
    }

    pub fn remove_worktree(&self, agent_id: &str) -> Result<()> {
        let worktree_path = self.repo_path.join(".worktrees").join(agent_id);
        if worktree_path.exists() {
            std::fs::remove_dir_all(&worktree_path)?;
        }

        let repo = Repository::open(&self.repo_path)?;
        let branch_name = format!("agent/{agent_id}");
        if let Ok(mut branch) = repo.find_branch(&branch_name, BranchType::Local) {
            branch.delete()?;
        }

        Ok(())
    }

    pub fn get_diff(&self, agent_id: &str) -> Result<String> {
        let worktree_path = self.repo_path.join(".worktrees").join(agent_id);
        let repo = Repository::open(&worktree_path)?;
        let diff = repo.diff_index_to_workdir(None, None)?;

        let mut diff_text = String::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let origin = line.origin();
            if origin == '+' || origin == '-' || origin == ' ' {
                diff_text.push(origin);
            }
            diff_text.push_str(std::str::from_utf8(line.content()).unwrap_or(""));
            true
        })?;

        Ok(diff_text)
    }

    pub fn worktree_path(&self, agent_id: &str) -> PathBuf {
        self.repo_path.join(".worktrees").join(agent_id)
    }

    pub fn worktree_exists(&self, agent_id: &str) -> bool {
        self.worktree_path(agent_id).exists()
    }

    pub fn list_worktrees(&self) -> Result<Vec<String>> {
        let dir = self.repo_path.join(".worktrees");
        if !dir.exists() {
            return Ok(vec![]);
        }
        let entries: Vec<String> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        Ok(entries)
    }

    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }
}
