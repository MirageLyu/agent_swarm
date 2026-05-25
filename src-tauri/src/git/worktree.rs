use anyhow::{Context, Result};
use git2::{BranchType, Repository};
use serde::Serialize;
use std::path::{Path, PathBuf};

use super::merge_strategy::{merge_branch_ref_only, ConflictResolution, MergeLayer, MergeStrategy};

#[derive(Debug, Serialize, Clone)]
pub struct DiffFile {
    pub path: String,
    pub status: String,
    pub old_content: Option<String>,
    pub new_content: Option<String>,
}

pub struct WorktreeManager {
    repo_path: PathBuf,
    /// FM-15 Phase 2 (FR-12): 主分支名。None 时由 `merge_agent_branch` 等操作按需自动探测；
    /// scheduler 在启动 mission 时通常会预先调用 `detect_main_branch` 并把结果回填到 mission，
    /// 之后构造 WorktreeManager 时显式传入，避免重复探测。
    main_branch: Option<String>,
}

impl WorktreeManager {
    pub fn new(repo_path: PathBuf) -> Self {
        Self {
            repo_path,
            main_branch: None,
        }
    }

    /// 用已知的主分支名构造（推荐路径，避免每次操作都重新探测）。
    pub fn with_main_branch(repo_path: PathBuf, main_branch: impl Into<String>) -> Self {
        Self {
            repo_path,
            main_branch: Some(main_branch.into()),
        }
    }

    /// FM-15 Phase 2 (FR-12): 探测主分支名。
    /// 优先级：
    ///   1. `refs/remotes/origin/HEAD` 指向的分支（剥去 `refs/remotes/origin/` 前缀）
    ///   2. 依次检查本地是否存在 `main` / `master` / `develop`
    ///   3. fallback 到当前 HEAD 所在的本地分支
    /// 检测纯 read-only，不会修改仓库。
    pub fn detect_main_branch(&self) -> Result<String> {
        let repo = Repository::open(&self.repo_path)
            .context("Failed to open repository for main branch detection")?;

        // 1) origin/HEAD 的目标
        if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD") {
            if let Some(target) = reference.symbolic_target() {
                // target 形如 "refs/remotes/origin/main"
                if let Some(name) = target.strip_prefix("refs/remotes/origin/") {
                    if !name.is_empty() && name != "HEAD" {
                        return Ok(name.to_string());
                    }
                }
            }
        }

        // 2) 候选分支顺序探测
        for candidate in ["main", "master", "develop"] {
            if repo.find_branch(candidate, BranchType::Local).is_ok() {
                return Ok(candidate.to_string());
            }
        }

        // 3) fallback: 当前 HEAD 的 short name
        let head = repo.head().context("Failed to read HEAD")?;
        if let Some(short) = head.shorthand() {
            return Ok(short.to_string());
        }

        anyhow::bail!("Cannot detect main branch: HEAD is unresolved")
    }

    /// 解析当前 manager 持有的或运行时探测的主分支名。
    fn resolve_main_branch(&self) -> Result<String> {
        if let Some(name) = &self.main_branch {
            return Ok(name.clone());
        }
        self.detect_main_branch()
    }

    pub fn create_worktree(&self, agent_id: &str) -> Result<PathBuf> {
        // 默认从主分支派生（保留旧行为：不开启增量 worktree 时使用）。
        let main_branch = self.resolve_main_branch()?;
        self.create_worktree_from_branch(agent_id, &main_branch)
    }

    /// FM-15 Phase 2 (FR-07): 从指定分支（如 `task-base/<task_id>` 或 `main`）派生 agent 分支
    /// 并创建对应 worktree。
    ///
    /// 调用方负责保证 `base_branch` 存在（`prepare_task_base` 完成后会建好 task-base 分支）。
    pub fn create_worktree_from_branch(
        &self,
        agent_id: &str,
        base_branch: &str,
    ) -> Result<PathBuf> {
        let repo = Repository::open(&self.repo_path).context("Failed to open repository")?;

        let branch_name = format!("agent/{agent_id}");
        let worktree_path = self.repo_path.join(".worktrees").join(agent_id);
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let base = repo
            .find_branch(base_branch, BranchType::Local)
            .with_context(|| format!("base branch '{base_branch}' not found"))?;
        let base_commit = base
            .get()
            .peel_to_commit()
            .context("base branch has no commit")?;

        repo.branch(&branch_name, &base_commit, false)
            .with_context(|| format!("failed to create branch '{branch_name}'"))?;

        repo.worktree(
            agent_id,
            &worktree_path,
            Some(
                git2::WorktreeAddOptions::new().reference(Some(
                    &repo
                        .find_branch(&branch_name, BranchType::Local)?
                        .into_reference(),
                )),
            ),
        )?;

        Ok(worktree_path)
    }

    /// FM-15 Phase 2 (FR-07.1): 为某 task 准备增量 base 分支。
    ///
    /// 流程：
    /// 1. 删除旧的 `task-base/<task_id>` 分支（重试场景）。
    /// 2. 在 `main_branch` HEAD 之上创建空白 `task-base/<task_id>`。
    /// 3. 按 `parent_agent_branches`（拓扑后序）依次 ref-only 合并；冲突按 `strategy` 决策。
    /// 4. 返回累计冲突列表 + 每个父任务的合并 outcome 摘要。
    ///
    /// **不切换 HEAD**：所有操作走 ref-only merge，不触碰主仓库工作区，
    /// 与已有 agent worktree 完全并发安全。
    pub fn prepare_task_base(
        &self,
        task_id: &str,
        parents_topo: &[(String, String)],
        strategy: MergeStrategy,
    ) -> Result<TaskBaseOutcome> {
        let repo = Repository::open(&self.repo_path)
            .context("Failed to open repository for task base preparation")?;

        let main_branch = self.resolve_main_branch()?;
        let base_branch = format!("task-base/{task_id}");

        // (1) 清理旧分支（仅删 ref；与之关联的 worktree 不该存在，因为 task-base 不挂 worktree）
        if let Ok(mut existing) = repo.find_branch(&base_branch, BranchType::Local) {
            existing
                .delete()
                .with_context(|| format!("failed to delete stale branch '{base_branch}'"))?;
        }

        // (2) 从 main_branch tip 起步
        let main_tip = repo
            .find_branch(&main_branch, BranchType::Local)
            .with_context(|| format!("main branch '{main_branch}' not found"))?
            .get()
            .peel_to_commit()
            .context("main branch has no commit")?;
        repo.branch(&base_branch, &main_tip, false)
            .with_context(|| format!("failed to create '{base_branch}' from {main_branch}"))?;

        let mut all_conflicts: Vec<TaskBaseConflict> = Vec::new();
        let mut parent_summaries: Vec<TaskBaseParentSummary> = Vec::new();
        let mut overall_layer = MergeLayer::L1Auto;

        // (3) 按拓扑顺序逐个父分支合入
        for (parent_task_id, parent_agent_id) in parents_topo {
            let source_branch = format!("agent/{parent_agent_id}");
            let outcome = merge_branch_ref_only(
                &repo,
                &base_branch,
                &source_branch,
                &format!("merge {source_branch} into {base_branch}"),
                strategy,
            )
            .with_context(|| format!("failed merging '{source_branch}' into '{base_branch}'"))?;

            for conflict in outcome.conflicts.iter() {
                all_conflicts.push(TaskBaseConflict {
                    parent_task_id: parent_task_id.clone(),
                    file_path: conflict.path.clone(),
                    layer: conflict.layer,
                });
            }

            // 整体层级取最严重
            overall_layer = match (overall_layer, outcome.layer_summary) {
                (MergeLayer::FallbackTheirs, _) | (_, MergeLayer::FallbackTheirs) => {
                    MergeLayer::FallbackTheirs
                }
                (MergeLayer::L2HeuristicTheirs, _) | (_, MergeLayer::L2HeuristicTheirs) => {
                    MergeLayer::L2HeuristicTheirs
                }
                _ => MergeLayer::L1Auto,
            };

            parent_summaries.push(TaskBaseParentSummary {
                parent_task_id: parent_task_id.clone(),
                parent_agent_branch: source_branch,
                commit_hash: outcome.commit_hash,
                created_new_commit: outcome.created_new_commit,
                layer_summary: outcome.layer_summary,
                conflicts: outcome.conflicts,
            });
        }

        Ok(TaskBaseOutcome {
            base_branch,
            parent_summaries,
            conflicts: all_conflicts,
            layer_summary: overall_layer,
        })
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
        let repo =
            Repository::open(&worktree_path).context("Failed to open worktree repository")?;

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

        let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head_commit])?;

        Ok(Some(oid.to_string()))
    }

    /// FM-15 v2.2 P4-S5 / FR-15.5: Chat Agent 直接修改 main 工作区后用本函数提交。
    ///
    /// - 切到主分支（如果不在）
    /// - `git add -A`（含未跟踪文件）
    /// - 计算改动行数并返回，便于上层做 30 行硬阈值校验
    /// - 仅在有 staged 变化时才创建 commit；否则返回 `Ok(None)`
    pub fn commit_main_workdir(&self, commit_message: &str) -> Result<Option<MainCommitOutcome>> {
        let repo = Repository::open(&self.repo_path).context("Failed to open repository")?;
        let main_branch = self.resolve_main_branch()?;
        let main_ref = format!("refs/heads/{main_branch}");

        // 确保 HEAD 在主分支
        let need_checkout = match repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(String::from))
        {
            Some(name) if name == main_branch => false,
            _ => true,
        };
        if need_checkout {
            repo.set_head(&main_ref)
                .with_context(|| format!("Failed to set HEAD to '{main_ref}'"))?;
            repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
                .with_context(|| format!("Failed to checkout '{main_branch}'"))?;
        }

        let mut index = repo.index().context("Failed to load index")?;
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .context("Failed to git-add changes")?;
        index.write().context("Failed to write index")?;

        // 检测是否真的有变化
        let head_tree = repo.head()?.peel_to_commit()?.tree()?;
        let staged_tree_id = index.write_tree().context("Failed to write staged tree")?;
        let staged_tree = repo.find_tree(staged_tree_id)?;

        let mut opts = git2::DiffOptions::new();
        let diff = repo
            .diff_tree_to_tree(Some(&head_tree), Some(&staged_tree), Some(&mut opts))
            .context("Failed to compute diff between HEAD and index")?;

        let stats = diff.stats().context("Failed to compute diff stats")?;
        let files_changed = stats.files_changed();
        let lines_changed = stats.insertions() + stats.deletions();

        if files_changed == 0 && lines_changed == 0 {
            return Ok(None);
        }

        // 收集改动的文件路径
        let mut changed_paths: Vec<String> = Vec::new();
        diff.foreach(
            &mut |delta, _| {
                if let Some(p) = delta.new_file().path().or_else(|| delta.old_file().path()) {
                    changed_paths.push(p.to_string_lossy().into_owned());
                }
                true
            },
            None,
            None,
            None,
        )
        .ok();

        let parent = repo.head()?.peel_to_commit()?;
        let sig = repo
            .signature()
            .or_else(|_| git2::Signature::now("Miragenty Chat", "miragenty-chat@localhost"))
            .context("Failed to create signature")?;
        let oid = repo
            .commit(
                Some(&main_ref),
                &sig,
                &sig,
                commit_message,
                &staged_tree,
                &[&parent],
            )
            .context("Failed to create chat commit")?;

        // 让工作区与新 HEAD 同步
        repo.set_head(&main_ref)?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;

        Ok(Some(MainCommitOutcome {
            commit_hash: oid.to_string(),
            files_changed,
            lines_changed,
            changed_paths,
        }))
    }

    /// FM-15 FR-08.2 (3): 在主分支当前 HEAD 之上追加一个 commit，把 LLM 解出的合并版本写入对应文件。
    ///
    /// `files`: 路径 → 新内容（完整文件正文，覆盖原文件）。
    /// 调用方应在 `merge_agent_branch_with_strategy` 落了 ref-only fallback commit 之后再调用本函数，
    /// 这样形成 "merge → LLM fix" 的 commit 序列，历史可追溯。
    pub fn apply_llm_resolutions(
        &self,
        files: &std::collections::HashMap<String, String>,
        commit_message: &str,
    ) -> Result<String> {
        use std::fs;
        if files.is_empty() {
            anyhow::bail!("apply_llm_resolutions called with empty files map");
        }

        let repo = Repository::open(&self.repo_path).context("Failed to open repository")?;
        let main_branch = self.resolve_main_branch()?;
        let main_ref = format!("refs/heads/{main_branch}");

        // 切到主分支 + 强制 checkout，让工作区匹配 ref
        repo.set_head(&main_ref)
            .with_context(|| format!("Failed to set HEAD to '{main_ref}'"))?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .with_context(|| format!("Failed to checkout '{main_branch}'"))?;

        let workdir = repo
            .workdir()
            .context("Repository has no worktree")?
            .to_path_buf();
        let mut index = repo.index().context("Failed to load index")?;

        for (rel_path, content) in files {
            let abs = workdir.join(rel_path);
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create dir for '{rel_path}'"))?;
            }
            fs::write(&abs, content).with_context(|| format!("Failed to write '{rel_path}'"))?;
            index
                .add_path(Path::new(rel_path))
                .with_context(|| format!("Failed to git-add '{rel_path}'"))?;
        }
        index.write().context("Failed to write index")?;

        let tree_id = index.write_tree().context("Failed to write tree")?;
        let tree = repo.find_tree(tree_id)?;
        let parent = repo.head()?.peel_to_commit()?;

        let sig = repo
            .signature()
            .or_else(|_| git2::Signature::now("Miragenty", "miragenty@localhost"))
            .context("Failed to create signature")?;

        let oid = repo
            .commit(
                Some(&main_ref),
                &sig,
                &sig,
                commit_message,
                &tree,
                &[&parent],
            )
            .context("Failed to create LLM-resolution commit")?;

        // 让工作区跟上新 commit
        repo.set_head(&main_ref)?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;

        Ok(oid.to_string())
    }

    /// FM-15 Phase 2 (FR-08): 用分层策略把 agent 分支合入主分支，并把工作区同步到合并后的 ref。
    ///
    /// 内部走 ref-only merge → 更新主分支 ref → checkout 主分支让工作区落地。
    /// 与 `merge_agent_branch` 的差别：
    /// - 返回 `LayeredMergeOutcome`，含每个冲突的层级标签（L1/L2/Fallback）
    /// - 接受 strategy 参数控制 L2 启发式与冲突归属
    pub fn merge_agent_branch_with_strategy(
        &self,
        agent_id: &str,
        strategy: MergeStrategy,
    ) -> Result<crate::git::LayeredMergeOutcome> {
        let repo = Repository::open(&self.repo_path).context("Failed to open repository")?;

        let main_branch = self.resolve_main_branch()?;
        let source_branch = format!("agent/{agent_id}");

        let outcome = merge_branch_ref_only(
            &repo,
            &main_branch,
            &source_branch,
            &format!("Merge branch '{source_branch}' into {main_branch}"),
            strategy,
        )?;

        // ref 已更新，现在把工作区切到主分支并强制 checkout，让磁盘内容跟上 ref。
        let main_ref = format!("refs/heads/{main_branch}");
        repo.set_head(&main_ref)
            .with_context(|| format!("Failed to set HEAD to '{main_ref}'"))?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .with_context(|| format!("Failed to checkout HEAD to '{main_branch}'"))?;
        repo.cleanup_state().ok();

        Ok(outcome)
    }

    /// Merge an agent branch into the main branch.
    /// Conflicts are auto-resolved by accepting the agent branch version (theirs),
    /// because agents are merged in DAG topo order and their output is the deliverable.
    ///
    /// FM-15 Phase 2 (FR-12): 不再硬编码 `main`；目标分支按 manager 持有的 `main_branch` 或
    /// 运行时探测得到。
    pub fn merge_agent_branch(&self, agent_id: &str) -> Result<MergeOutcome> {
        let repo = Repository::open(&self.repo_path).context("Failed to open main repository")?;

        let main_branch = self.resolve_main_branch()?;
        let main_ref = format!("refs/heads/{main_branch}");

        // 切到主分支再合并，避免在其它分支 HEAD 上误更新
        if let Ok(head) = repo.head() {
            if head.shorthand() != Some(main_branch.as_str()) {
                repo.set_head(&main_ref)
                    .with_context(|| format!("Failed to checkout {main_branch} before merge"))?;
                repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;
            }
        }

        let branch_name = format!("agent/{agent_id}");
        let branch = repo
            .find_branch(&branch_name, BranchType::Local)
            .context(format!("Branch {branch_name} not found"))?;
        let their_commit = branch
            .get()
            .peel_to_commit()
            .context("Failed to resolve branch to commit")?;

        let head = repo.head()?.peel_to_commit()?;

        let merge_base = repo
            .merge_base(head.id(), their_commit.id())
            .context("No merge base found")?;

        // Fast-forward: main hasn't diverged since this agent branched
        if merge_base == head.id() {
            repo.reference(
                &main_ref,
                their_commit.id(),
                true,
                &format!("merge agent/{agent_id}: fast-forward"),
            )?;
            repo.set_head(&main_ref)?;
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
                let path = conflict
                    .their
                    .as_ref()
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
            format!(
                "\n\nAuto-resolved conflicts (accepted theirs): {}",
                auto_resolved.join(", ")
            )
        };
        let msg = format!("Merge branch '{branch_name}' into {main_branch}{resolved_note}");

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
        let repo = Repository::open(&self.repo_path).context("Failed to open main repository")?;

        let branch_name = format!("agent/{agent_id}");
        let agent_branch = repo
            .find_branch(&branch_name, BranchType::Local)
            .context(format!(
                "Agent branch '{branch_name}' not found — worktree may have been removed"
            ))?;
        let agent_commit = agent_branch
            .get()
            .peel_to_commit()
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
        let repo = Repository::open(&self.repo_path).context("Failed to open main repository")?;

        let base_oid = git2::Oid::from_str(base_hash).context("Invalid base commit hash")?;
        let head_oid = git2::Oid::from_str(head_hash).context("Invalid head commit hash")?;

        let base_tree = repo
            .find_commit(base_oid)
            .context("Base commit not found in repository")?
            .tree()?;
        let head_tree = repo
            .find_commit(head_oid)
            .context("Head commit not found in repository")?
            .tree()?;

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

/// FM-15 v2.2 P4-S5 / FR-15.5: Chat Agent 直接 commit 到 main 后的摘要。
#[derive(Debug, Clone, Serialize)]
pub struct MainCommitOutcome {
    pub commit_hash: String,
    pub files_changed: usize,
    pub lines_changed: usize,
    pub changed_paths: Vec<String>,
}

/// FM-15 Phase 2 (FR-07.1): 单个父任务合入 task-base 时的摘要。
#[derive(Debug, Clone)]
pub struct TaskBaseParentSummary {
    pub parent_task_id: String,
    pub parent_agent_branch: String,
    pub commit_hash: String,
    pub created_new_commit: bool,
    pub layer_summary: MergeLayer,
    pub conflicts: Vec<ConflictResolution>,
}

/// FM-15 Phase 2 (FR-07.1): `prepare_task_base` 的总输出。
#[derive(Debug, Clone)]
pub struct TaskBaseOutcome {
    pub base_branch: String,
    pub parent_summaries: Vec<TaskBaseParentSummary>,
    pub conflicts: Vec<TaskBaseConflict>,
    pub layer_summary: MergeLayer,
}

/// FM-15 Phase 2 (FR-07.1): 一条 task-base 冲突记录，配合 `db::queries::record_task_base_conflict`
/// 落库到 `task_base_conflicts` 表。
#[derive(Debug, Clone)]
pub struct TaskBaseConflict {
    pub parent_task_id: String,
    pub file_path: String,
    pub layer: MergeLayer,
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

    fn commit_file_in_repo(
        repo: &Repository,
        dir: &Path,
        filename: &str,
        content: &str,
        message: &str,
    ) {
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
        wt.commit_worktree("agent-ff", "agent work")
            .expect("commit worktree");

        let outcome = wt.merge_agent_branch("agent-ff").expect("merge");
        match outcome {
            MergeOutcome::Merged { auto_resolved, .. } => {
                assert!(
                    auto_resolved.is_empty(),
                    "ff merge should have no auto-resolved files"
                );
            }
        }

        let merged_file = repo_path.join("new_file.txt");
        assert!(
            merged_file.exists(),
            "new_file.txt should exist after ff merge"
        );
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
        commit_file_in_repo(
            &repo,
            &repo_path,
            "main_file.txt",
            "from main",
            "main commit",
        );

        // Agent adds yet another file (no overlap)
        fs::write(wt_path.join("agent_file.txt"), "from agent").expect("write in worktree");
        wt.commit_worktree("agent-nc", "agent work")
            .expect("commit worktree");

        let outcome = wt.merge_agent_branch("agent-nc").expect("merge");
        match outcome {
            MergeOutcome::Merged { auto_resolved, .. } => {
                assert!(
                    auto_resolved.is_empty(),
                    "non-conflicting merge should have no auto-resolved"
                );
            }
        }

        assert_eq!(
            fs::read_to_string(repo_path.join("main_file.txt")).unwrap(),
            "from main"
        );
        assert_eq!(
            fs::read_to_string(repo_path.join("agent_file.txt")).unwrap(),
            "from agent"
        );
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
        commit_file_in_repo(
            &repo,
            &repo_path,
            "shared.txt",
            "main's version",
            "main edit",
        );

        // Agent modifies the same file
        fs::write(wt_path.join("shared.txt"), "agent's version").expect("write in worktree");
        wt.commit_worktree("agent-cr", "agent edit")
            .expect("commit worktree");

        let outcome = wt.merge_agent_branch("agent-cr").expect("merge");
        match outcome {
            MergeOutcome::Merged { auto_resolved, .. } => {
                assert_eq!(
                    auto_resolved,
                    vec!["shared.txt"],
                    "shared.txt should be auto-resolved"
                );
            }
        }

        let content = fs::read_to_string(repo_path.join("shared.txt")).unwrap();
        assert_eq!(
            content, "agent's version",
            "conflict should be resolved to agent's version"
        );
        assert!(
            !content.contains("<<<<"),
            "must not contain conflict markers"
        );
        assert!(
            !content.contains(">>>>"),
            "must not contain conflict markers"
        );
        assert!(
            !content.contains("======="),
            "must not contain conflict markers"
        );
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
        wt.commit_worktree("agent-mf", "agent edits")
            .expect("commit");

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

        let result = wt
            .commit_worktree("agent-noop", "empty commit")
            .expect("commit");
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
        wt.commit_worktree("agent-sd1", "modify file")
            .expect("commit");

        let files = wt
            .get_structured_diff("agent-sd1")
            .expect("structured diff");
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
        wt.commit_worktree("agent-sd2", "multi-file changes")
            .expect("commit");

        let files = wt
            .get_structured_diff("agent-sd2")
            .expect("structured diff");
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

        let files = wt
            .get_structured_diff("agent-sd3")
            .expect("structured diff");
        assert!(files.is_empty(), "should return empty list when no changes");
    }

    #[test]
    fn ut01_4_diff_by_hashes_after_branch_deleted() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "hello.txt", "original");

        let repo = Repository::open(&repo_path).unwrap();
        let base_hash = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        drop(repo);

        let wt = WorktreeManager::new(repo_path.clone());
        let wt_path = wt.create_worktree("agent-hash").expect("create worktree");

        fs::write(wt_path.join("hello.txt"), "changed").expect("write");
        let head_hash = wt
            .commit_worktree("agent-hash", "edit")
            .expect("commit")
            .unwrap();

        // Merge and delete the branch (simulating post-mission cleanup)
        wt.merge_agent_branch("agent-hash").expect("merge");
        wt.remove_worktree("agent-hash").expect("remove");

        // Branch-based diff should now fail
        assert!(wt.get_structured_diff("agent-hash").is_err());

        // Hash-based diff should still work
        let files = wt
            .get_structured_diff_by_hashes(&base_hash, &head_hash)
            .expect("hash-based diff should work after branch deletion");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "hello.txt");
        assert_eq!(files[0].status, "modified");
        assert_eq!(files[0].old_content.as_deref(), Some("original"));
        assert_eq!(files[0].new_content.as_deref(), Some("changed"));
    }

    // ---- FM-15 Phase 2 (FR-12): detect_main_branch ----

    /// 仓库主分支为 `main`（init_repo_with_file 默认创建）：直接命中候选列表第一项。
    #[test]
    fn detect_main_branch_returns_main_when_present() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        let wt = WorktreeManager::new(repo_path);
        let detected = wt.detect_main_branch().expect("detect");
        assert_eq!(detected, "main");
    }

    /// 仓库主分支为 `master`（无 main / develop）：候选列表回退到第二项。
    #[test]
    fn detect_main_branch_returns_master_when_main_missing() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        // 把默认创建的 main 重命名为 master
        let repo = Repository::open(&repo_path).unwrap();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("master", &head_commit, false).unwrap();
        repo.set_head("refs/heads/master").unwrap();
        // 删除 main 分支需要先 detach；这里直接通过 reference 删除
        let mut main_branch = repo.find_branch("main", BranchType::Local).unwrap();
        main_branch.delete().unwrap();

        let wt = WorktreeManager::new(repo_path);
        let detected = wt.detect_main_branch().expect("detect");
        assert_eq!(detected, "master");
    }

    /// 仓库当前 HEAD 不在 main/master/develop（如 trunk）：fallback 到当前 HEAD shorthand。
    #[test]
    fn detect_main_branch_falls_back_to_head_shorthand() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        let repo = Repository::open(&repo_path).unwrap();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("trunk", &head_commit, false).unwrap();
        repo.set_head("refs/heads/trunk").unwrap();
        let mut main_branch = repo.find_branch("main", BranchType::Local).unwrap();
        main_branch.delete().unwrap();

        let wt = WorktreeManager::new(repo_path);
        let detected = wt.detect_main_branch().expect("detect");
        assert_eq!(detected, "trunk");
    }

    /// 显式 with_main_branch 注入应直接生效，不再触发探测。
    #[test]
    fn with_main_branch_overrides_detection() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        let wt = WorktreeManager::with_main_branch(repo_path, "release");
        // resolve_main_branch 私有，但 main_branch 已注入 → merge 流程会用它（这里只断言字段读取）
        assert_eq!(wt.main_branch.as_deref(), Some("release"));
    }

    /// 在非 main 分支（master）上完整跑一次 merge_agent_branch：fast-forward 后应回到 master HEAD。
    #[test]
    fn merge_agent_branch_uses_dynamic_main_master() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        // 改名 main → master（用 scope 限定 repo 借用，在构造 manager 前释放）
        {
            let repo = Repository::open(&repo_path).unwrap();
            let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("master", &head_commit, false).unwrap();
            repo.set_head("refs/heads/master").unwrap();
            repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
                .unwrap();
            drop(head_commit);
            let mut main_branch = repo.find_branch("main", BranchType::Local).unwrap();
            main_branch.delete().unwrap();
        }

        let wt = WorktreeManager::with_main_branch(repo_path.clone(), "master");
        let wt_path = wt.create_worktree("agent-master").expect("create worktree");
        fs::write(wt_path.join("note.txt"), "agent output").expect("write");
        wt.commit_worktree("agent-master", "agent work")
            .expect("commit");

        let outcome = wt.merge_agent_branch("agent-master").expect("merge");
        match outcome {
            MergeOutcome::Merged { auto_resolved, .. } => {
                assert!(
                    auto_resolved.is_empty(),
                    "ff merge should have no auto-resolved"
                );
            }
        }

        // master HEAD 现在应指向 agent 分支的 commit
        let repo = Repository::open(&repo_path).unwrap();
        let master_branch = repo
            .find_branch("master", BranchType::Local)
            .expect("master exists");
        assert!(
            master_branch.get().peel_to_commit().is_ok(),
            "master should have a commit"
        );
        assert!(
            repo_path.join("note.txt").exists(),
            "merged file should be present in worktree"
        );
    }

    // ---- FM-15 Phase 2 (FR-07.1): prepare_task_base 菱形 DAG ----

    /// 模拟菱形 DAG：A → B / C → D。
    /// A 的产物 a.txt；B 在 A 之上加 b.txt；C 在 A 之上加 c.txt；
    /// 调用 prepare_task_base(D, parents=[B, C])，期望 task-base/D 同时含 a.txt+b.txt+c.txt。
    #[test]
    fn prepare_task_base_diamond_merges_b_and_c() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        let wt = WorktreeManager::with_main_branch(repo_path.clone(), "main");

        // ---- 模拟 agent A 完成（在 main 上加 a.txt 并合并）----
        let wt_a = wt
            .create_worktree_from_branch("agent-A", "main")
            .expect("wt A");
        fs::write(wt_a.join("a.txt"), "from A").expect("write a");
        wt.commit_worktree("agent-A", "A: add a").expect("commit A");
        // 立即把 agent/A 合回 main，让 B/C 都能从 main 派生（即都包含 A 的产物）
        wt.merge_agent_branch("agent-A").expect("merge A");

        // ---- agent B 在 main（已含 A）上加 b.txt ----
        let wt_b = wt
            .create_worktree_from_branch("agent-B", "main")
            .expect("wt B");
        fs::write(wt_b.join("b.txt"), "from B").expect("write b");
        wt.commit_worktree("agent-B", "B: add b").expect("commit B");
        // B 不立即合回 main——仍保留在 agent/B 分支上

        // ---- agent C 在 main（已含 A）上加 c.txt ----
        let wt_c = wt
            .create_worktree_from_branch("agent-C", "main")
            .expect("wt C");
        fs::write(wt_c.join("c.txt"), "from C").expect("write c");
        wt.commit_worktree("agent-C", "C: add c").expect("commit C");

        // ---- prepare_task_base for D, parents = [B, C]（拓扑后序）----
        let parents = vec![
            ("T-B".to_string(), "agent-B".to_string()),
            ("T-C".to_string(), "agent-C".to_string()),
        ];
        let outcome = wt
            .prepare_task_base("T-D", &parents, MergeStrategy::LlmResolve)
            .expect("prepare base");

        assert_eq!(outcome.base_branch, "task-base/T-D");
        assert_eq!(outcome.parent_summaries.len(), 2);
        assert!(outcome.conflicts.is_empty(), "no conflicts expected");
        assert_eq!(outcome.layer_summary, MergeLayer::L1Auto);

        // task-base/T-D tree 必须同时含 a.txt + b.txt + c.txt
        let repo = Repository::open(&repo_path).unwrap();
        let tree = repo
            .find_branch("task-base/T-D", BranchType::Local)
            .expect("task-base exists")
            .get()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap();
        assert!(
            tree.get_path(Path::new("a.txt")).is_ok(),
            "a.txt must be in task-base"
        );
        assert!(
            tree.get_path(Path::new("b.txt")).is_ok(),
            "b.txt must be in task-base"
        );
        assert!(
            tree.get_path(Path::new("c.txt")).is_ok(),
            "c.txt must be in task-base"
        );

        // 主仓库 HEAD 仍在 main——不应被 ref-only 操作触动
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "main");
    }

    /// FM-15 Phase 2 (FR-08): frontier merge 端到端 —— 模拟菱形 DAG，
    /// 用 prepare_task_base + create_worktree_from_branch + merge_agent_branch_with_strategy
    /// 串起完整流程，验证：
    /// - frontier task (D) 合并后 main 包含全部叶子产物
    /// - 不需要重复合并 A/B/C（它们的产物通过 D 的 commit 已经传到 main）
    #[test]
    fn frontier_merge_diamond_end_to_end() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        let wt = WorktreeManager::with_main_branch(repo_path.clone(), "main");

        // ---- Task A (root) ----
        // dispatch: 没有父 → 直接从 main 派生
        let wt_a = wt
            .create_worktree_from_branch("ag-A", "main")
            .expect("wt A");
        fs::write(wt_a.join("a.txt"), "from A").expect("write a");
        wt.commit_worktree("ag-A", "A: add a").expect("commit A");

        // ---- Task B (depends on A) ----
        // dispatch: prepare_task_base(B, parents=[A]) → task-base/B 含 A 产物
        let base_b = wt
            .prepare_task_base(
                "T-B",
                &[("T-A".into(), "ag-A".into())],
                MergeStrategy::LlmResolve,
            )
            .expect("prepare base B");
        assert!(base_b.conflicts.is_empty());
        let wt_b = wt
            .create_worktree_from_branch("ag-B", &base_b.base_branch)
            .expect("wt B");
        assert!(wt_b.join("a.txt").exists(), "B should inherit A's a.txt");
        fs::write(wt_b.join("b.txt"), "from B").expect("write b");
        wt.commit_worktree("ag-B", "B: add b").expect("commit B");

        // ---- Task C (depends on A) ----
        let base_c = wt
            .prepare_task_base(
                "T-C",
                &[("T-A".into(), "ag-A".into())],
                MergeStrategy::LlmResolve,
            )
            .expect("prepare base C");
        let wt_c = wt
            .create_worktree_from_branch("ag-C", &base_c.base_branch)
            .expect("wt C");
        assert!(wt_c.join("a.txt").exists(), "C should inherit A's a.txt");
        fs::write(wt_c.join("c.txt"), "from C").expect("write c");
        wt.commit_worktree("ag-C", "C: add c").expect("commit C");

        // ---- Task D (depends on B & C) ----
        // prepare_task_base(D, parents=[B,C]) 应自动 merge B+C，得到 a/b/c 三文件
        let base_d = wt
            .prepare_task_base(
                "T-D",
                &[("T-B".into(), "ag-B".into()), ("T-C".into(), "ag-C".into())],
                MergeStrategy::LlmResolve,
            )
            .expect("prepare base D");
        assert!(base_d.conflicts.is_empty(), "B and C touch disjoint files");
        let wt_d = wt
            .create_worktree_from_branch("ag-D", &base_d.base_branch)
            .expect("wt D");
        assert!(wt_d.join("a.txt").exists());
        assert!(wt_d.join("b.txt").exists());
        assert!(wt_d.join("c.txt").exists());
        fs::write(wt_d.join("d.txt"), "from D").expect("write d");
        wt.commit_worktree("ag-D", "D: add d").expect("commit D");

        // ---- frontier merge: 只 merge D（叶子），main 应同时含 a/b/c/d ----
        let merge_outcome = wt
            .merge_agent_branch_with_strategy("ag-D", MergeStrategy::LlmResolve)
            .expect("merge D into main");
        assert_eq!(merge_outcome.layer_summary, MergeLayer::L1Auto);
        assert!(merge_outcome.conflicts.is_empty());

        // main HEAD 工作区应同时含 a/b/c/d
        assert!(repo_path.join("a.txt").exists(), "a.txt must be on main");
        assert!(repo_path.join("b.txt").exists(), "b.txt must be on main");
        assert!(repo_path.join("c.txt").exists(), "c.txt must be on main");
        assert!(repo_path.join("d.txt").exists(), "d.txt must be on main");

        // main HEAD 仍指向 main 分支
        let repo = Repository::open(&repo_path).unwrap();
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "main");
    }

    /// `create_worktree_from_branch` 应让 agent worktree 起始内容包含 base_branch 的所有文件。
    #[test]
    fn create_worktree_from_branch_inherits_base_content() {
        let tmp = TempDir::new().expect("tmpdir");
        let repo_path = tmp.path().to_path_buf();
        init_repo_with_file(&repo_path, "readme.txt", "hello");

        let wt = WorktreeManager::with_main_branch(repo_path.clone(), "main");

        // 准备一个简单的 task-base 分支（直接基于 main，再加一个文件）
        {
            let repo = Repository::open(&repo_path).unwrap();
            let main_tip = repo
                .find_branch("main", BranchType::Local)
                .unwrap()
                .get()
                .peel_to_commit()
                .unwrap();
            repo.branch("task-base/T1", &main_tip, false).unwrap();
        }

        // 在 task-base/T1 临时 worktree 上加文件，模拟 prepare_task_base 后状态
        let stage = wt
            .create_worktree_from_branch("stage-prep", "task-base/T1")
            .expect("stage wt");
        fs::write(stage.join("preamble.txt"), "from base").expect("write preamble");
        wt.commit_worktree("stage-prep", "base prep")
            .expect("commit prep");
        // 把这次 commit 推回 task-base/T1（merge_agent_branch 默认合到 main，不合适；这里手工 ref 操作）
        {
            let repo = Repository::open(&repo_path).unwrap();
            let stage_tip = repo
                .find_branch("agent/stage-prep", BranchType::Local)
                .unwrap()
                .get()
                .peel_to_commit()
                .unwrap();
            repo.reference("refs/heads/task-base/T1", stage_tip.id(), true, "stage")
                .unwrap();
        }

        // 真正模拟 dispatch：从 task-base/T1 派生 agent worktree
        let agent_wt = wt
            .create_worktree_from_branch("agent-X", "task-base/T1")
            .expect("agent wt");

        // agent worktree 起步即应含 preamble.txt
        assert!(
            agent_wt.join("preamble.txt").exists(),
            "agent worktree should inherit base branch content"
        );
    }
}
