use anyhow::{Context, Result};
use git2::{Repository, BranchType};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Clone)]
pub struct DiffFile {
    pub path: String,
    pub status: String,
    pub old_content: Option<String>,
    pub new_content: Option<String>,
}

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
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

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

    /// Stage all changes and commit in the agent's worktree.
    /// Returns the commit hash, or None if there was nothing to commit.
    pub fn commit_worktree(&self, agent_id: &str, message: &str) -> Result<Option<String>> {
        let worktree_path = self.worktree_path(agent_id);
        let repo = Repository::open(&worktree_path)
            .context("Failed to open worktree repository")?;

        let mut index = repo.index()?;
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;
        let tree_id = index.write_tree()?;

        // Check if there are actual changes compared to parent
        let head_commit = repo.head()?.peel_to_commit()?;
        if head_commit.tree()?.id() == tree_id {
            return Ok(None);
        }

        let tree = repo.find_tree(tree_id)?;
        let sig = repo
            .signature()
            .or_else(|_| git2::Signature::now("Miragenty Agent", "agent@miragenty.local"))
            .context("Failed to create signature")?;

        let oid = repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            message,
            &tree,
            &[&head_commit],
        )?;

        Ok(Some(oid.to_string()))
    }

    /// Merge an agent branch into the main branch.
    /// Conflicts are auto-resolved by accepting the agent branch version (theirs),
    /// because agents are merged in DAG topo order and their output is the deliverable.
    pub fn merge_agent_branch(&self, agent_id: &str) -> Result<MergeOutcome> {
        let repo = Repository::open(&self.repo_path)
            .context("Failed to open main repository")?;

        let branch_name = format!("agent/{agent_id}");
        let branch = repo.find_branch(&branch_name, BranchType::Local)
            .context(format!("Branch {branch_name} not found"))?;
        let their_commit = branch.get().peel_to_commit()
            .context("Failed to resolve branch to commit")?;

        let head = repo.head()?.peel_to_commit()?;

        let merge_base = repo.merge_base(head.id(), their_commit.id())
            .context("No merge base found")?;

        // Fast-forward: main hasn't diverged since this agent branched
        if merge_base == head.id() {
            repo.reference(
                "refs/heads/main",
                their_commit.id(),
                true,
                &format!("merge agent/{agent_id}: fast-forward"),
            )?;
            repo.set_head("refs/heads/main")?;
            repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;
            return Ok(MergeOutcome::Merged {
                commit_hash: their_commit.id().to_string(),
                auto_resolved: Vec::new(),
            });
        }

        // Real merge
        let their_annotated = repo.find_annotated_commit(their_commit.id())?;
        repo.merge(&[&their_annotated], None, None)?;

        let mut index = repo.index()?;
        let mut auto_resolved: Vec<String> = Vec::new();

        if index.has_conflicts() {
            let conflicts: Vec<git2::IndexConflict> =
                index.conflicts()?.filter_map(|c| c.ok()).collect();

            for conflict in conflicts {
                let path = conflict.their.as_ref()
                    .or(conflict.our.as_ref())
                    .and_then(|entry| std::str::from_utf8(&entry.path).ok())
                    .unwrap_or("<unknown>")
                    .to_string();

                // Remove conflict stages: 1=ancestor, 2=ours, 3=theirs
                let p = Path::new(&path);
                let _ = index.remove(p, 1);
                let _ = index.remove(p, 2);
                let _ = index.remove(p, 3);

                if let Some(mut their_entry) = conflict.their {
                    their_entry.flags = 0; // stage 0 = resolved
                    index.add(&their_entry)?;
                }
                // If their_entry is None (agent deleted the file), removal above is sufficient
                auto_resolved.push(path);
            }

            index.write()?;
        }

        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        let sig = repo
            .signature()
            .or_else(|_| git2::Signature::now("Miragenty", "miragenty@localhost"))
            .context("Failed to create signature")?;

        let resolved_note = if auto_resolved.is_empty() {
            String::new()
        } else {
            format!("\n\nAuto-resolved conflicts (accepted theirs): {}", auto_resolved.join(", "))
        };
        let msg = format!("Merge branch '{branch_name}' into main{resolved_note}");

        let oid = repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &msg,
            &tree,
            &[&head, &their_commit],
        )?;
        repo.cleanup_state()?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;

        Ok(MergeOutcome::Merged {
            commit_hash: oid.to_string(),
            auto_resolved,
        })
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

    /// Return structured per-file diff between the merge-base and the agent branch HEAD.
    /// Requires the agent branch to still exist.
    pub fn get_structured_diff(&self, agent_id: &str) -> Result<Vec<DiffFile>> {
        let repo = Repository::open(&self.repo_path)
            .context("Failed to open main repository")?;

        let branch_name = format!("agent/{agent_id}");
        let agent_branch = repo
            .find_branch(&branch_name, BranchType::Local)
            .context(format!("Agent branch '{branch_name}' not found — worktree may have been removed"))?;
        let agent_commit = agent_branch.get().peel_to_commit()
            .context("Failed to resolve agent branch to commit")?;

        let main_head = repo.head()?.peel_to_commit()?;
        let merge_base_oid = repo
            .merge_base(main_head.id(), agent_commit.id())
            .context("No merge base found between main and agent branch")?;
        let base_tree = repo.find_commit(merge_base_oid)?.tree()?;
        let agent_tree = agent_commit.tree()?;

        Self::diff_trees(&repo, &base_tree, &agent_tree)
    }

    /// Return structured per-file diff using stored commit hashes.
    /// Works even after the agent branch has been deleted, as long as
    /// the commit objects are still reachable in the repository.
    pub fn get_structured_diff_by_hashes(
        &self,
        base_hash: &str,
        head_hash: &str,
    ) -> Result<Vec<DiffFile>> {
        let repo = Repository::open(&self.repo_path)
            .context("Failed to open main repository")?;

        let base_oid = git2::Oid::from_str(base_hash)
            .context("Invalid base commit hash")?;
        let head_oid = git2::Oid::from_str(head_hash)
            .context("Invalid head commit hash")?;

        let base_tree = repo.find_commit(base_oid)
            .context("Base commit not found in repository")?.tree()?;
        let head_tree = repo.find_commit(head_oid)
            .context("Head commit not found in repository")?.tree()?;

        Self::diff_trees(&repo, &base_tree, &head_tree)
    }

    fn diff_trees(
        repo: &Repository,
        base_tree: &git2::Tree,
        head_tree: &git2::Tree,
    ) -> Result<Vec<DiffFile>> {
        let diff = repo.diff_tree_to_tree(Some(base_tree), Some(head_tree), None)?;

        let mut files = Vec::new();
        for delta in diff.deltas() {
            let status = match delta.status() {
                git2::Delta::Added => "added",
                git2::Delta::Deleted => "deleted",
                _ => "modified",
            };

            let path = delta
                .new_file()
                .path()
                .or(delta.old_file().path())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            let old_content = if status != "added" {
                Self::read_blob_text(repo, base_tree, &path)
            } else {
                None
            };

            let new_content = if status != "deleted" {
                Self::read_blob_text(repo, head_tree, &path)
            } else {
                None
            };

            files.push(DiffFile {
                path,
                status: status.to_string(),
                old_content,
                new_content,
            });
        }

        Ok(files)
    }

    fn read_blob_text(repo: &Repository, tree: &git2::Tree, path: &str) -> Option<String> {
        tree.get_path(Path::new(path))
            .ok()
            .and_then(|entry| repo.find_blob(entry.id()).ok())
            .and_then(|blob| {
                if blob.is_binary() {
                    None
                } else {
                    std::str::from_utf8(blob.content()).ok().map(String::from)
                }
            })
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

#[derive(Debug)]
pub enum MergeOutcome {
    Merged {
        commit_hash: String,
        /// Files where conflicts were auto-resolved by accepting the agent's version.
        auto_resolved: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn init_repo_with_file(dir: &Path, filename: &str, content: &str) {
        let repo = Repository::init(dir).expect("init repo");
        fs::write(dir.join(filename), content).expect("write file");

        let mut index = repo.index().expect("index");
        index.add_path(Path::new(filename)).expect("add path");
        index.write().expect("write index");
        let tree_id = index.write_tree().expect("write tree");

        {
            let tree = repo.find_tree(tree_id).expect("find tree");
            let sig = git2::Signature::now("Test", "test@test.local").expect("sig");
            repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
                .expect("initial commit");
        }

        repo.set_head("refs/heads/main").expect("set head");
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .expect("checkout head");
    }

    fn commit_file_in_repo(repo: &Repository, dir: &Path, filename: &str, content: &str, message: &str) {
        let file_path = dir.join(filename);
        fs::write(&file_path, content).expect("write file");
        let mut index = repo.index().expect("index");
        index.add_path(Path::new(filename)).expect("add path");
        index.write().expect("write index");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = git2::Signature::now("Test", "test@test.local").expect("sig");
        let parent = repo.head().expect("head").peel_to_commit().expect("commit");
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
            .expect("commit");
    }

    #[test]
    fn test_merge_fast_forward() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        let wt = WorktreeManager::new(repo_path.clone());
        let wt_path = wt.create_worktree("agent-ff").expect("create worktree");

        fs::write(wt_path.join("new_file.txt"), "agent output").expect("write in worktree");
        wt.commit_worktree("agent-ff", "agent work").expect("commit worktree");

        let outcome = wt.merge_agent_branch("agent-ff").expect("merge");
        match outcome {
            MergeOutcome::Merged { auto_resolved, .. } => {
                assert!(auto_resolved.is_empty(), "ff merge should have no auto-resolved files");
            }
        }

        let merged_file = repo_path.join("new_file.txt");
        assert!(merged_file.exists(), "new_file.txt should exist after ff merge");
        assert_eq!(fs::read_to_string(&merged_file).unwrap(), "agent output");
    }

    #[test]
    fn test_merge_no_conflict() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");
        let repo = Repository::open(&repo_path).unwrap();

        let wt = WorktreeManager::new(repo_path.clone());
        let wt_path = wt.create_worktree("agent-nc").expect("create worktree");

        // Diverge main: add a different file
        commit_file_in_repo(&repo, &repo_path, "main_file.txt", "from main", "main commit");

        // Agent adds yet another file (no overlap)
        fs::write(wt_path.join("agent_file.txt"), "from agent").expect("write in worktree");
        wt.commit_worktree("agent-nc", "agent work").expect("commit worktree");

        let outcome = wt.merge_agent_branch("agent-nc").expect("merge");
        match outcome {
            MergeOutcome::Merged { auto_resolved, .. } => {
                assert!(auto_resolved.is_empty(), "non-conflicting merge should have no auto-resolved");
            }
        }

        assert_eq!(fs::read_to_string(repo_path.join("main_file.txt")).unwrap(), "from main");
        assert_eq!(fs::read_to_string(repo_path.join("agent_file.txt")).unwrap(), "from agent");
    }

    #[test]
    fn test_merge_conflict_auto_resolve_theirs() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "shared.txt", "original content");
        let repo = Repository::open(&repo_path).unwrap();

        let wt = WorktreeManager::new(repo_path.clone());
        let wt_path = wt.create_worktree("agent-cr").expect("create worktree");

        // Diverge main: modify the same file differently
        commit_file_in_repo(&repo, &repo_path, "shared.txt", "main's version", "main edit");

        // Agent modifies the same file
        fs::write(wt_path.join("shared.txt"), "agent's version").expect("write in worktree");
        wt.commit_worktree("agent-cr", "agent edit").expect("commit worktree");

        let outcome = wt.merge_agent_branch("agent-cr").expect("merge");
        match outcome {
            MergeOutcome::Merged { auto_resolved, .. } => {
                assert_eq!(auto_resolved, vec!["shared.txt"], "shared.txt should be auto-resolved");
            }
        }

        let content = fs::read_to_string(repo_path.join("shared.txt")).unwrap();
        assert_eq!(content, "agent's version", "conflict should be resolved to agent's version");
        assert!(!content.contains("<<<<"), "must not contain conflict markers");
        assert!(!content.contains(">>>>"), "must not contain conflict markers");
        assert!(!content.contains("======="), "must not contain conflict markers");
    }

    #[test]
    fn test_merge_conflict_multiple_files() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "file_a.txt", "a-original");
        {
            let repo = Repository::open(&repo_path).unwrap();
            commit_file_in_repo(&repo, &repo_path, "file_b.txt", "b-original", "add file_b");
        }
        let repo = Repository::open(&repo_path).unwrap();

        let wt = WorktreeManager::new(repo_path.clone());
        let wt_path = wt.create_worktree("agent-mf").expect("create worktree");

        // Diverge main on both files
        commit_file_in_repo(&repo, &repo_path, "file_a.txt", "a-main", "main edit a");
        commit_file_in_repo(&repo, &repo_path, "file_b.txt", "b-main", "main edit b");

        // Agent edits both files differently
        fs::write(wt_path.join("file_a.txt"), "a-agent").expect("write");
        fs::write(wt_path.join("file_b.txt"), "b-agent").expect("write");
        wt.commit_worktree("agent-mf", "agent edits").expect("commit");

        let outcome = wt.merge_agent_branch("agent-mf").expect("merge");
        match outcome {
            MergeOutcome::Merged { auto_resolved, .. } => {
                assert_eq!(auto_resolved.len(), 2, "both files should be auto-resolved");
                assert!(auto_resolved.contains(&"file_a.txt".to_string()));
                assert!(auto_resolved.contains(&"file_b.txt".to_string()));
            }
        }

        let a = fs::read_to_string(repo_path.join("file_a.txt")).unwrap();
        let b = fs::read_to_string(repo_path.join("file_b.txt")).unwrap();
        assert_eq!(a, "a-agent");
        assert_eq!(b, "b-agent");
        for content in [&a, &b] {
            assert!(!content.contains("<<<<"), "no conflict markers");
            assert!(!content.contains(">>>>"), "no conflict markers");
        }
    }

    #[test]
    fn test_commit_worktree_no_changes() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        let wt = WorktreeManager::new(repo_path.clone());
        wt.create_worktree("agent-noop").expect("create worktree");

        let result = wt.commit_worktree("agent-noop", "empty commit").expect("commit");
        assert!(result.is_none(), "should return None when no changes");
    }

    // ---- FM-05: Structured diff tests (UT-01) ----

    #[test]
    fn ut01_1_single_file_modified() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "hello.txt", "original content");

        let wt = WorktreeManager::new(repo_path.clone());
        let wt_path = wt.create_worktree("agent-sd1").expect("create worktree");

        fs::write(wt_path.join("hello.txt"), "modified content").expect("write");
        wt.commit_worktree("agent-sd1", "modify file").expect("commit");

        let files = wt.get_structured_diff("agent-sd1").expect("structured diff");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "hello.txt");
        assert_eq!(files[0].status, "modified");
        assert_eq!(files[0].old_content.as_deref(), Some("original content"));
        assert_eq!(files[0].new_content.as_deref(), Some("modified content"));
    }

    #[test]
    fn ut01_2_multiple_files() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "a.txt", "aaa");

        let wt = WorktreeManager::new(repo_path.clone());
        let wt_path = wt.create_worktree("agent-sd2").expect("create worktree");

        fs::write(wt_path.join("a.txt"), "aaa-modified").expect("write");
        fs::write(wt_path.join("b.txt"), "new file b").expect("write");
        wt.commit_worktree("agent-sd2", "multi-file changes").expect("commit");

        let files = wt.get_structured_diff("agent-sd2").expect("structured diff");
        assert_eq!(files.len(), 2, "should have 2 changed files");

        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"a.txt"), "should contain a.txt");
        assert!(paths.contains(&"b.txt"), "should contain b.txt");

        let added = files.iter().find(|f| f.path == "b.txt").unwrap();
        assert_eq!(added.status, "added");
        assert!(added.old_content.is_none());
        assert_eq!(added.new_content.as_deref(), Some("new file b"));
    }

    #[test]
    fn ut01_3_no_changes() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        let wt = WorktreeManager::new(repo_path.clone());
        wt.create_worktree("agent-sd3").expect("create worktree");

        let files = wt.get_structured_diff("agent-sd3").expect("structured diff");
        assert!(files.is_empty(), "should return empty list when no changes");
    }

    #[test]
    fn ut01_4_diff_by_hashes_after_branch_deleted() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "hello.txt", "original");

        let repo = Repository::open(&repo_path).unwrap();
        let base_hash = repo.head().unwrap().peel_to_commit().unwrap().id().to_string();
        drop(repo);

        let wt = WorktreeManager::new(repo_path.clone());
        let wt_path = wt.create_worktree("agent-hash").expect("create worktree");

        fs::write(wt_path.join("hello.txt"), "changed").expect("write");
        let head_hash = wt.commit_worktree("agent-hash", "edit").expect("commit").unwrap();

        // Merge and delete the branch (simulating post-mission cleanup)
        wt.merge_agent_branch("agent-hash").expect("merge");
        wt.remove_worktree("agent-hash").expect("remove");

        // Branch-based diff should now fail
        assert!(wt.get_structured_diff("agent-hash").is_err());

        // Hash-based diff should still work
        let files = wt.get_structured_diff_by_hashes(&base_hash, &head_hash)
            .expect("hash-based diff should work after branch deletion");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "hello.txt");
        assert_eq!(files[0].status, "modified");
        assert_eq!(files[0].old_content.as_deref(), Some("original"));
        assert_eq!(files[0].new_content.as_deref(), Some("changed"));
    }
}
