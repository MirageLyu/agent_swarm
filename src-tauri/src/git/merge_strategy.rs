//! FM-15 Phase 2 (FR-08): 三层冲突合并的纯函数实施。
//!
//! 本模块提供 ref-only 的 git merge 操作——不切换 HEAD、不触碰任何 worktree，
//! 只通过更新分支 ref 完成 merge。这样 scheduler 在 dispatch 阶段构造 task-base
//! 分支时，可以在主仓库上无副作用地操作，与已有 agent worktree 完全并发。
//!
//! ## 三层降级
//!
//! - **L1 / `L1Auto`**：fast-forward 或 git 自动合并（无冲突）。
//! - **L2 / `L2HeuristicTheirs`**：保守启发式——双方内容去除所有空白字符后等价
//!   （仅缩进 / 换行 / 末尾空行差异），接受 theirs。**不做语言相关判断**（如 Python
//!   import 重排），避免语义误判。
//! - **FallbackTheirs**：真实冲突。Phase 2 一律接受 theirs（与历史行为一致）；
//!   Phase 3 接入 LLM 解冲突时，会把这一层替换为 `L3LlmResolved` /
//!   `L3LlmFailedFallback`。
//!
//! ## 配置语义
//!
//! `MergeStrategy::Theirs` 跳过 L2 启发式，所有冲突直接落 theirs（与现有行为一致）。
//! `MergeStrategy::LlmResolve` 在 Phase 2 等价于 `Theirs + L2`（L3 在 Phase 3 接入）。
//! `MergeStrategy::Ours` 反向——所有冲突保留 ours。

use anyhow::{Context, Result};
use git2::{BranchType, Repository};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeLayer {
    /// 无冲突或 fast-forward 完成。
    L1Auto,
    /// 纯空白/换行差异，启发式接受 theirs。
    L2HeuristicTheirs,
    /// 真实冲突，强制接受 theirs（Phase 3 起被 LLM 层替换）。
    FallbackTheirs,
}

impl MergeLayer {
    /// 把枚举映射成 `task_base_conflicts.resolution` / `merge_records.final_strategy`
    /// 对应的字符串值。命名与 v2.2 数据需求一致。
    pub fn as_resolution_str(self) -> &'static str {
        match self {
            MergeLayer::L1Auto => "auto",
            MergeLayer::L2HeuristicTheirs => "heuristic_theirs",
            MergeLayer::FallbackTheirs => "llm_failed_fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// L1 → L2 → Fallback theirs（Phase 2 默认；Phase 3 把 Fallback 替换为 LLM）。
    LlmResolve,
    /// L1 → 跳过 L2，所有冲突直接 theirs（保持历史行为）。
    Theirs,
    /// L1 → 所有冲突保留 ours（很少使用，主要用于 follow-up chat 直接 commit 场景）。
    Ours,
}

impl MergeStrategy {
    pub fn from_str(s: &str) -> Self {
        match s {
            "theirs" => MergeStrategy::Theirs,
            "ours" => MergeStrategy::Ours,
            _ => MergeStrategy::LlmResolve,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConflictResolution {
    pub path: String,
    pub layer: MergeLayer,
}

#[derive(Debug, Clone)]
pub struct LayeredMergeOutcome {
    /// 新生成的 merge commit hash。fast-forward / 已 up-to-date 时为对应分支当前 tip。
    pub commit_hash: String,
    /// 是否本次执行实际产生了新 commit（fast-forward / up-to-date 时为 false）。
    pub created_new_commit: bool,
    /// 冲突文件列表（无冲突时为空）。
    pub conflicts: Vec<ConflictResolution>,
    /// 整体最高层级。无冲突 = L1Auto；任一文件 FallbackTheirs = FallbackTheirs。
    pub layer_summary: MergeLayer,
}

/// 把 `source_branch_name` ref-only 合并到 `target_branch_name`。
/// 所有冲突按 `strategy` 决策，最终 target 分支 ref 被更新到新 merge commit。
///
/// **不会修改 HEAD、不会写入任何 worktree**。
pub fn merge_branch_ref_only(
    repo: &Repository,
    target_branch_name: &str,
    source_branch_name: &str,
    commit_message: &str,
    strategy: MergeStrategy,
) -> Result<LayeredMergeOutcome> {
    let target = repo
        .find_branch(target_branch_name, BranchType::Local)
        .with_context(|| format!("target branch '{target_branch_name}' not found"))?;
    let source = repo
        .find_branch(source_branch_name, BranchType::Local)
        .with_context(|| format!("source branch '{source_branch_name}' not found"))?;

    let target_commit = target
        .get()
        .peel_to_commit()
        .context("target branch has no commit")?;
    let source_commit = source
        .get()
        .peel_to_commit()
        .context("source branch has no commit")?;

    // 已 up-to-date：source 是 target 祖先（或同 tip）
    let target_oid = target_commit.id();
    let source_oid = source_commit.id();
    if target_oid == source_oid {
        return Ok(LayeredMergeOutcome {
            commit_hash: target_oid.to_string(),
            created_new_commit: false,
            conflicts: Vec::new(),
            layer_summary: MergeLayer::L1Auto,
        });
    }

    let base_oid = repo.merge_base(target_oid, source_oid).with_context(|| {
        format!("no merge base between '{target_branch_name}' and '{source_branch_name}'")
    })?;

    if base_oid == source_oid {
        // source 已包含于 target，无需合并
        return Ok(LayeredMergeOutcome {
            commit_hash: target_oid.to_string(),
            created_new_commit: false,
            conflicts: Vec::new(),
            layer_summary: MergeLayer::L1Auto,
        });
    }

    // Fast-forward: target 是 source 祖先 → 直接把 target ref 指向 source。
    if base_oid == target_oid {
        repo.reference(
            &format!("refs/heads/{target_branch_name}"),
            source_oid,
            true,
            &format!("ff: {commit_message}"),
        )?;
        return Ok(LayeredMergeOutcome {
            commit_hash: source_oid.to_string(),
            created_new_commit: false,
            conflicts: Vec::new(),
            layer_summary: MergeLayer::L1Auto,
        });
    }

    // 3-way tree merge：纯内存操作，不动 HEAD/worktree
    let base_tree = repo.find_commit(base_oid)?.tree()?;
    let target_tree = target_commit.tree()?;
    let source_tree = source_commit.tree()?;

    let mut merged_index = repo
        .merge_trees(&base_tree, &target_tree, &source_tree, None)
        .context("merge_trees failed")?;

    let mut conflicts: Vec<ConflictResolution> = Vec::new();
    let mut overall_layer = MergeLayer::L1Auto;

    if merged_index.has_conflicts() {
        let conflict_entries: Vec<git2::IndexConflict> =
            merged_index.conflicts()?.filter_map(|c| c.ok()).collect();

        for conflict in conflict_entries {
            // git2 的 IndexEntry 不实现 Clone，统一 take 出来后用引用读、按需 move 写。
            let our = conflict.our;
            let their = conflict.their;
            let ancestor = conflict.ancestor;

            let path_bytes: Vec<u8> = their
                .as_ref()
                .or(our.as_ref())
                .or(ancestor.as_ref())
                .map(|e| e.path.clone())
                .unwrap_or_default();
            let path = String::from_utf8_lossy(&path_bytes).to_string();

            let ours_text = our
                .as_ref()
                .and_then(|e| repo.find_blob(e.id).ok())
                .and_then(|b| std::str::from_utf8(b.content()).ok().map(String::from));
            let theirs_text = their
                .as_ref()
                .and_then(|e| repo.find_blob(e.id).ok())
                .and_then(|b| std::str::from_utf8(b.content()).ok().map(String::from));

            let layer =
                classify_conflict_layer(strategy, ours_text.as_deref(), theirs_text.as_deref());

            // 清掉 ancestor / ours / theirs 三个 stage
            let p = Path::new(&path);
            let _ = merged_index.remove(p, 1);
            let _ = merged_index.remove(p, 2);
            let _ = merged_index.remove(p, 3);

            // 按 layer 决定写入 ours 还是 theirs（move，不 clone）
            let entry_to_keep = match strategy {
                MergeStrategy::Ours => our,
                _ => their,
            };
            if let Some(mut entry) = entry_to_keep {
                entry.flags = 0; // stage 0 = resolved
                merged_index.add(&entry)?;
            }
            // 若 entry 是 None（一侧删除文件），删除三个 stage 已经达到了"按一方意图删除"的效果

            conflicts.push(ConflictResolution { path, layer });

            // 更新 overall layer：FallbackTheirs > L2HeuristicTheirs > L1Auto
            overall_layer = match (overall_layer, layer) {
                (MergeLayer::FallbackTheirs, _) | (_, MergeLayer::FallbackTheirs) => {
                    MergeLayer::FallbackTheirs
                }
                (MergeLayer::L2HeuristicTheirs, _) | (_, MergeLayer::L2HeuristicTheirs) => {
                    MergeLayer::L2HeuristicTheirs
                }
                _ => MergeLayer::L1Auto,
            };
        }
    }

    let tree_oid = merged_index
        .write_tree_to(repo)
        .context("write merged tree failed")?;
    let tree = repo.find_tree(tree_oid)?;

    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("Miragenty", "miragenty@localhost"))
        .context("Failed to create signature")?;

    // 关键：update_ref = None，不动 HEAD；写完 commit 后手工更新 target ref。
    let new_oid = repo
        .commit(
            None,
            &sig,
            &sig,
            commit_message,
            &tree,
            &[&target_commit, &source_commit],
        )
        .context("create merge commit failed")?;

    repo.reference(
        &format!("refs/heads/{target_branch_name}"),
        new_oid,
        true,
        commit_message,
    )?;

    Ok(LayeredMergeOutcome {
        commit_hash: new_oid.to_string(),
        created_new_commit: true,
        conflicts,
        layer_summary: overall_layer,
    })
}

/// 决定单个冲突文件落在哪一层。
///
/// - `LlmResolve`：尝试 L2 启发式；不通过则 FallbackTheirs（Phase 3 起会先尝试 L3）。
/// - `Theirs` / `Ours`：跳过 L2，所有真冲突一律 FallbackTheirs（layer_summary 仅用于
///   观测；具体接受 ours 还是 theirs 在主循环中按 strategy 决定）。
fn classify_conflict_layer(
    strategy: MergeStrategy,
    ours: Option<&str>,
    theirs: Option<&str>,
) -> MergeLayer {
    if !matches!(strategy, MergeStrategy::LlmResolve) {
        return MergeLayer::FallbackTheirs;
    }

    match (ours, theirs) {
        (Some(o), Some(t)) if normalize_whitespace(o) == normalize_whitespace(t) => {
            MergeLayer::L2HeuristicTheirs
        }
        _ => MergeLayer::FallbackTheirs,
    }
}

/// 保守启发式：剥离所有空白字符后比较。
/// 涵盖缩进 / 换行 / 末尾换行 / tab 与空格混用等纯格式差异。
/// 故意**不做** import 顺序、注释顺序等语言相关判断，避免语义误判。
fn normalize_whitespace(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Repository;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn init_repo(dir: &PathBuf) -> Repository {
        let repo = Repository::init(dir).expect("init");
        let sig = git2::Signature::now("Test", "test@test.local").unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        drop(tree);
        repo
    }

    fn commit_on_branch(
        repo: &Repository,
        branch: &str,
        file: &str,
        content: &str,
        msg: &str,
    ) -> git2::Oid {
        // checkout / fast-forward 到目标分支
        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();

        // 确保 branch 存在
        if repo.find_branch(branch, BranchType::Local).is_err() {
            repo.branch(branch, &main_tip, false).unwrap();
        }
        let parent_commit = repo
            .find_branch(branch, BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();

        // 切到该分支并写文件
        repo.set_head(&format!("refs/heads/{branch}")).unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();

        let work = repo.workdir().unwrap();
        fs::write(work.join(file), content).unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(Path::new(file)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Test", "test@test.local").unwrap();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&parent_commit])
            .unwrap();
        oid
    }

    #[test]
    fn ref_only_merge_fast_forward_no_conflict() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let repo = init_repo(&path);

        commit_on_branch(&repo, "main", "a.txt", "hello", "main first");

        // 创建 task-base/T1 指向 main tip
        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        repo.branch("task-base/T1", &main_tip, false).unwrap();

        // 在 agent/A 上加文件
        commit_on_branch(&repo, "agent/A", "b.txt", "from A", "agent A");

        // 切回 main 不影响 ref-only 合并
        repo.set_head("refs/heads/main").unwrap();

        let outcome = merge_branch_ref_only(
            &repo,
            "task-base/T1",
            "agent/A",
            "merge agent/A",
            MergeStrategy::LlmResolve,
        )
        .expect("merge");

        assert_eq!(outcome.layer_summary, MergeLayer::L1Auto);
        assert!(outcome.conflicts.is_empty());

        // task-base/T1 ref 现在应指向 agent/A 的 commit（fast-forward）
        let t1 = repo.find_branch("task-base/T1", BranchType::Local).unwrap();
        let agent_a = repo.find_branch("agent/A", BranchType::Local).unwrap();
        assert_eq!(
            t1.get().peel_to_commit().unwrap().id(),
            agent_a.get().peel_to_commit().unwrap().id(),
        );

        // HEAD 仍在 main
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "main");
    }

    #[test]
    fn ref_only_merge_no_conflict_real_merge() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let repo = init_repo(&path);
        commit_on_branch(&repo, "main", "shared.txt", "base", "main base");

        // task-base 在 main 之上加文件 b.txt
        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        repo.branch("task-base/T1", &main_tip, false).unwrap();
        commit_on_branch(
            &repo,
            "task-base/T1",
            "b.txt",
            "from base merge",
            "T1 add b",
        );

        // agent/A 也 fork 自 main 并加文件 c.txt
        commit_on_branch(&repo, "agent/A", "c.txt", "from A", "A add c");

        // 切回 main 验证 ref-only
        repo.set_head("refs/heads/main").unwrap();

        let outcome = merge_branch_ref_only(
            &repo,
            "task-base/T1",
            "agent/A",
            "merge A",
            MergeStrategy::LlmResolve,
        )
        .unwrap();

        assert_eq!(outcome.layer_summary, MergeLayer::L1Auto);
        assert!(outcome.created_new_commit);
        assert!(outcome.conflicts.is_empty());

        // 三个文件都应该在合并后的 task-base/T1 tree 里
        let t1_tree = repo
            .find_branch("task-base/T1", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap();
        assert!(t1_tree.get_path(Path::new("shared.txt")).is_ok());
        assert!(t1_tree.get_path(Path::new("b.txt")).is_ok());
        assert!(t1_tree.get_path(Path::new("c.txt")).is_ok());
    }

    #[test]
    fn ref_only_merge_l2_whitespace_only_conflict() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let repo = init_repo(&path);
        commit_on_branch(&repo, "main", "shared.txt", "line1\nline2\n", "main");

        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        repo.branch("task-base/T1", &main_tip, false).unwrap();
        // base 改 shared.txt 加缩进
        commit_on_branch(
            &repo,
            "task-base/T1",
            "shared.txt",
            "    line1\n    line2\n",
            "base indents",
        );

        // agent 改 shared.txt 末尾加换行 + 中间无空白差异（语义等价）
        commit_on_branch(
            &repo,
            "agent/A",
            "shared.txt",
            "line1\nline2\n\n",
            "agent extra newline",
        );

        repo.set_head("refs/heads/main").unwrap();

        let outcome = merge_branch_ref_only(
            &repo,
            "task-base/T1",
            "agent/A",
            "merge A whitespace",
            MergeStrategy::LlmResolve,
        )
        .unwrap();

        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(outcome.conflicts[0].path, "shared.txt");
        assert_eq!(outcome.conflicts[0].layer, MergeLayer::L2HeuristicTheirs);
        assert_eq!(outcome.layer_summary, MergeLayer::L2HeuristicTheirs);

        // 接受了 theirs：内容应是 agent 的版本
        let t1 = repo.find_branch("task-base/T1", BranchType::Local).unwrap();
        let tree = t1.get().peel_to_commit().unwrap().tree().unwrap();
        let blob_id = tree.get_path(Path::new("shared.txt")).unwrap().id();
        let blob = repo.find_blob(blob_id).unwrap();
        let content = std::str::from_utf8(blob.content()).unwrap();
        assert_eq!(content, "line1\nline2\n\n");
    }

    #[test]
    fn ref_only_merge_real_conflict_fallback_theirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let repo = init_repo(&path);
        commit_on_branch(&repo, "main", "shared.txt", "base content\n", "main");

        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        repo.branch("task-base/T1", &main_tip, false).unwrap();
        commit_on_branch(
            &repo,
            "task-base/T1",
            "shared.txt",
            "BASE EDIT VERSION\n",
            "base edit",
        );

        commit_on_branch(
            &repo,
            "agent/A",
            "shared.txt",
            "AGENT EDIT VERSION\n",
            "agent edit",
        );

        repo.set_head("refs/heads/main").unwrap();

        let outcome = merge_branch_ref_only(
            &repo,
            "task-base/T1",
            "agent/A",
            "merge A real conflict",
            MergeStrategy::LlmResolve,
        )
        .unwrap();

        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(outcome.conflicts[0].layer, MergeLayer::FallbackTheirs);
        assert_eq!(outcome.layer_summary, MergeLayer::FallbackTheirs);

        let tree = repo
            .find_branch("task-base/T1", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap();
        let blob = repo
            .find_blob(tree.get_path(Path::new("shared.txt")).unwrap().id())
            .unwrap();
        assert_eq!(
            std::str::from_utf8(blob.content()).unwrap(),
            "AGENT EDIT VERSION\n"
        );
    }

    #[test]
    fn ref_only_merge_strategy_theirs_skips_l2() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let repo = init_repo(&path);
        commit_on_branch(&repo, "main", "shared.txt", "x\ny\n", "main");

        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        repo.branch("task-base/T1", &main_tip, false).unwrap();
        commit_on_branch(
            &repo,
            "task-base/T1",
            "shared.txt",
            "  x\n  y\n",
            "base indents",
        );
        commit_on_branch(
            &repo,
            "agent/A",
            "shared.txt",
            "x\ny\n\n",
            "agent extra newline",
        );
        repo.set_head("refs/heads/main").unwrap();

        let outcome = merge_branch_ref_only(
            &repo,
            "task-base/T1",
            "agent/A",
            "merge A theirs strategy",
            MergeStrategy::Theirs,
        )
        .unwrap();

        // Theirs 策略下，所有冲突应被划为 FallbackTheirs（跳过 L2 启发式分类）
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(outcome.conflicts[0].layer, MergeLayer::FallbackTheirs);
    }

    #[test]
    fn ref_only_merge_idempotent_when_already_up_to_date() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let repo = init_repo(&path);
        commit_on_branch(&repo, "main", "a.txt", "x", "main");
        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        repo.branch("task-base/T1", &main_tip, false).unwrap();
        // agent/A == main_tip 同点
        repo.branch("agent/A", &main_tip, false).unwrap();
        repo.set_head("refs/heads/main").unwrap();

        let outcome = merge_branch_ref_only(
            &repo,
            "task-base/T1",
            "agent/A",
            "merge identity",
            MergeStrategy::LlmResolve,
        )
        .unwrap();

        assert!(!outcome.created_new_commit);
        assert_eq!(outcome.layer_summary, MergeLayer::L1Auto);
        assert!(outcome.conflicts.is_empty());
    }
}
