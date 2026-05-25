//! FM-15 FR-08.2 (3): L3 LLM 冲突解决。
//!
//! 与 `merge_strategy::merge_branch_ref_only` 互补：当 `MergeStrategy::LlmResolve` 模式下
//! `merge_branch_ref_only` 报告 `FallbackTheirs` 真冲突时，调用此模块用 LLM 重新生成合并版本。
//!
//! ## 与 ref-only merge 的关系
//!
//! - `merge_branch_ref_only` 永远成功（要么 L1Auto / L2HeuristicTheirs / FallbackTheirs），
//!   先在 `target_branch` 上落了一个 "theirs" 的 merge commit。
//! - 本模块以该 merge commit 为起点：把每个冲突文件的 ours/theirs/base 内容传给 resolver，
//!   拿到 LLM 解出的合并文本后，重新写入 tree，做一个新的 "amend" merge commit 替换掉
//!   ref-only 阶段的那个 commit。
//! - 如果 LLM 任一步失败，返回错误让上层回退到 theirs（ref-only 已经落了 fallback commit）。

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use git2::{BranchType, Repository};
use std::collections::HashMap;

/// 冲突文件的三方原始内容（不含 conflict markers）。
#[derive(Debug, Clone)]
pub struct ConflictBlob {
    pub path: String,
    /// "我方"（target 分支）的版本。一侧删除文件时为 None。
    pub ours: Option<String>,
    /// "他方"（source 分支）的版本。一侧删除文件时为 None。
    pub theirs: Option<String>,
    /// 共同祖先版本。新增文件时为 None。
    pub base: Option<String>,
}

/// LLM 解冲突的 async trait。生产实现走 LLM Provider，单测可注入 mock。
#[async_trait]
pub trait LlmConflictResolver: Send + Sync {
    /// 给定一个冲突文件的三方内容，返回 merged 内容。
    /// 失败时返回 Err，调用方据此降级。
    async fn resolve(&self, conflict: &ConflictBlob) -> Result<String>;
}

#[derive(Debug, Clone)]
pub struct LlmMergeOutcome {
    pub commit_hash: String,
    /// LLM 成功解决的文件
    pub llm_resolved: Vec<String>,
    /// LLM 失败、回退到 theirs 的文件
    pub fallback_theirs: Vec<String>,
}

/// 给定 target/source 分支，读取所有冲突文件的三方内容（不修改 repo）。
///
/// 返回 `Vec<ConflictBlob>`。无冲突时返回空 vec。
pub fn collect_conflict_blobs(
    repo: &Repository,
    target_branch_name: &str,
    source_branch_name: &str,
) -> Result<Vec<ConflictBlob>> {
    let target = repo
        .find_branch(target_branch_name, BranchType::Local)
        .with_context(|| format!("target branch '{target_branch_name}' not found"))?;
    let source = repo
        .find_branch(source_branch_name, BranchType::Local)
        .with_context(|| format!("source branch '{source_branch_name}' not found"))?;
    let target_commit = target.get().peel_to_commit()?;
    let source_commit = source.get().peel_to_commit()?;

    let target_oid = target_commit.id();
    let source_oid = source_commit.id();
    if target_oid == source_oid {
        return Ok(Vec::new());
    }
    let base_oid = repo.merge_base(target_oid, source_oid)?;
    if base_oid == source_oid || base_oid == target_oid {
        return Ok(Vec::new());
    }

    let base_tree = repo.find_commit(base_oid)?.tree()?;
    let target_tree = target_commit.tree()?;
    let source_tree = source_commit.tree()?;
    let merged_index = repo.merge_trees(&base_tree, &target_tree, &source_tree, None)?;
    if !merged_index.has_conflicts() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for c in merged_index.conflicts()? {
        let c = c?;
        let path_bytes: Vec<u8> = c
            .their
            .as_ref()
            .or(c.our.as_ref())
            .or(c.ancestor.as_ref())
            .map(|e| e.path.clone())
            .unwrap_or_default();
        let path = String::from_utf8_lossy(&path_bytes).to_string();

        let ours = c
            .our
            .as_ref()
            .and_then(|e| repo.find_blob(e.id).ok())
            .and_then(|b| std::str::from_utf8(b.content()).ok().map(String::from));
        let theirs = c
            .their
            .as_ref()
            .and_then(|e| repo.find_blob(e.id).ok())
            .and_then(|b| std::str::from_utf8(b.content()).ok().map(String::from));
        let base = c
            .ancestor
            .as_ref()
            .and_then(|e| repo.find_blob(e.id).ok())
            .and_then(|b| std::str::from_utf8(b.content()).ok().map(String::from));

        out.push(ConflictBlob {
            path,
            ours,
            theirs,
            base,
        });
    }
    Ok(out)
}

/// 用 resolutions 中的内容覆盖冲突文件，产生一个新的 merge commit 在 `target_branch` 上。
///
/// `resolutions`: path -> 完整文件新内容
///
/// 仍然是 ref-only：不动 HEAD / 工作区。
///
/// 行为：
/// - 重新计算 target/source 的 merge base 和 merge_trees
/// - 对每个冲突文件，把 stage 1/2/3 清掉，把 resolutions 中的内容写为 stage 0
/// - 不在 resolutions 中的冲突文件 → 用 source 端（theirs）作为兜底
/// - 写出新 tree + 创建 merge commit + 更新 target ref
pub fn apply_resolved_merge(
    repo: &Repository,
    target_branch_name: &str,
    source_branch_name: &str,
    commit_message: &str,
    resolutions: &HashMap<String, String>,
) -> Result<String> {
    use std::path::Path;

    let target = repo.find_branch(target_branch_name, BranchType::Local)?;
    let source = repo.find_branch(source_branch_name, BranchType::Local)?;
    let target_commit = target.get().peel_to_commit()?;
    let source_commit = source.get().peel_to_commit()?;

    let target_oid = target_commit.id();
    let source_oid = source_commit.id();
    if target_oid == source_oid {
        return Ok(target_oid.to_string());
    }
    let base_oid = repo
        .merge_base(target_oid, source_oid)
        .context("no merge base")?;

    let base_tree = repo.find_commit(base_oid)?.tree()?;
    let target_tree = target_commit.tree()?;
    let source_tree = source_commit.tree()?;
    let mut merged_index = repo.merge_trees(&base_tree, &target_tree, &source_tree, None)?;

    if merged_index.has_conflicts() {
        let conflicts: Vec<git2::IndexConflict> =
            merged_index.conflicts()?.filter_map(|c| c.ok()).collect();

        for c in conflicts {
            let our = c.our;
            let their = c.their;
            let path_bytes = their
                .as_ref()
                .or(our.as_ref())
                .map(|e| e.path.clone())
                .unwrap_or_default();
            let path = String::from_utf8_lossy(&path_bytes).to_string();
            let p = Path::new(&path);

            let _ = merged_index.remove(p, 1);
            let _ = merged_index.remove(p, 2);
            let _ = merged_index.remove(p, 3);

            if let Some(content) = resolutions.get(&path) {
                let blob_id = repo.blob(content.as_bytes())?;
                let entry = git2::IndexEntry {
                    ctime: git2::IndexTime::new(0, 0),
                    mtime: git2::IndexTime::new(0, 0),
                    dev: 0,
                    ino: 0,
                    mode: their
                        .as_ref()
                        .map(|e| e.mode)
                        .or(our.as_ref().map(|e| e.mode))
                        .unwrap_or(0o100644),
                    uid: 0,
                    gid: 0,
                    file_size: content.len() as u32,
                    id: blob_id,
                    flags: 0,
                    flags_extended: 0,
                    path: path_bytes.clone(),
                };
                merged_index.add(&entry)?;
            } else if let Some(mut entry) = their {
                // 兜底：使用 theirs（与 ref-only theirs 行为一致）
                entry.flags = 0;
                merged_index.add(&entry)?;
            }
            // their=None 表示一侧删除文件 → stage 已清空即视为删除，不再 add
        }
    }

    let tree_oid = merged_index.write_tree_to(repo)?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("Miragenty", "miragenty@localhost"))?;

    let new_oid = repo.commit(
        None,
        &sig,
        &sig,
        commit_message,
        &tree,
        &[&target_commit, &source_commit],
    )?;

    repo.reference(
        &format!("refs/heads/{target_branch_name}"),
        new_oid,
        true,
        commit_message,
    )?;

    Ok(new_oid.to_string())
}

/// 一站式 LLM 解冲突：
/// 1. 收集冲突文件的三方内容
/// 2. 对每个冲突调用 resolver；失败的退回到 theirs
/// 3. apply_resolved_merge 应用 LLM 给出的合并版本
///
/// 无冲突时 → 直接 fast-forward / no-op 走 sync 路径，调用方应该先用 ref-only merge
/// 判定是否有真冲突再调用本函数。
pub async fn merge_with_llm(
    repo: &Repository,
    target_branch_name: &str,
    source_branch_name: &str,
    commit_message: &str,
    resolver: &dyn LlmConflictResolver,
) -> Result<LlmMergeOutcome> {
    let blobs = collect_conflict_blobs(repo, target_branch_name, source_branch_name)?;
    if blobs.is_empty() {
        return Err(anyhow!(
            "merge_with_llm called but no conflicts found between '{target_branch_name}' and '{source_branch_name}'"
        ));
    }

    let mut resolutions: HashMap<String, String> = HashMap::new();
    let mut llm_ok: Vec<String> = Vec::new();
    let mut fallback: Vec<String> = Vec::new();

    for blob in &blobs {
        match resolver.resolve(blob).await {
            Ok(content) => {
                resolutions.insert(blob.path.clone(), content);
                llm_ok.push(blob.path.clone());
            }
            Err(e) => {
                tracing::warn!(
                    "LLM failed to resolve `{}`: {}; falling back to theirs",
                    blob.path,
                    e
                );
                fallback.push(blob.path.clone());
                // 不写入 resolutions → apply_resolved_merge 走 theirs 兜底
            }
        }
    }

    let commit_hash = apply_resolved_merge(
        repo,
        target_branch_name,
        source_branch_name,
        commit_message,
        &resolutions,
    )?;

    Ok(LlmMergeOutcome {
        commit_hash,
        llm_resolved: llm_ok,
        fallback_theirs: fallback,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Repository;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn init_repo(dir: &PathBuf) -> Repository {
        let repo = Repository::init(dir).unwrap();
        let sig = git2::Signature::now("Test", "test@test.local").unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };
        {
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        repo.set_head("refs/heads/main").unwrap();
        repo
    }

    fn commit_on_branch(repo: &Repository, branch: &str, file: &str, content: &str) {
        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        if repo.find_branch(branch, BranchType::Local).is_err() {
            repo.branch(branch, &main_tip, false).unwrap();
        }
        let parent = repo
            .find_branch(branch, BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
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
        repo.commit(Some("HEAD"), &sig, &sig, "commit", &tree, &[&parent])
            .unwrap();
    }

    struct MockResolver {
        produce: String,
        fail: bool,
    }

    #[async_trait]
    impl LlmConflictResolver for MockResolver {
        async fn resolve(&self, _blob: &ConflictBlob) -> Result<String> {
            if self.fail {
                Err(anyhow!("mock failure"))
            } else {
                Ok(self.produce.clone())
            }
        }
    }

    #[test]
    fn collect_blobs_returns_three_sides() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(&tmp.path().to_path_buf());
        commit_on_branch(&repo, "main", "shared.txt", "BASE\n");

        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        repo.branch("task-base/T1", &main_tip, false).unwrap();
        commit_on_branch(&repo, "task-base/T1", "shared.txt", "OURS\n");
        commit_on_branch(&repo, "agent/A", "shared.txt", "THEIRS\n");

        let blobs = collect_conflict_blobs(&repo, "task-base/T1", "agent/A").unwrap();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].path, "shared.txt");
        assert_eq!(blobs[0].ours.as_deref(), Some("OURS\n"));
        assert_eq!(blobs[0].theirs.as_deref(), Some("THEIRS\n"));
        assert_eq!(blobs[0].base.as_deref(), Some("BASE\n"));
    }

    #[tokio::test]
    async fn merge_with_llm_writes_resolved_content_to_target() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(&tmp.path().to_path_buf());
        commit_on_branch(&repo, "main", "shared.txt", "BASE\n");
        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        repo.branch("task-base/T1", &main_tip, false).unwrap();
        commit_on_branch(&repo, "task-base/T1", "shared.txt", "OURS\n");
        commit_on_branch(&repo, "agent/A", "shared.txt", "THEIRS\n");
        repo.set_head("refs/heads/main").unwrap();

        let resolver = MockResolver {
            produce: "MERGED BY LLM\n".into(),
            fail: false,
        };
        let outcome = merge_with_llm(&repo, "task-base/T1", "agent/A", "merge llm", &resolver)
            .await
            .unwrap();

        assert_eq!(outcome.llm_resolved, vec!["shared.txt"]);
        assert!(outcome.fallback_theirs.is_empty());

        let t1 = repo.find_branch("task-base/T1", BranchType::Local).unwrap();
        let tree = t1.get().peel_to_commit().unwrap().tree().unwrap();
        let blob = repo
            .find_blob(tree.get_path(Path::new("shared.txt")).unwrap().id())
            .unwrap();
        assert_eq!(
            std::str::from_utf8(blob.content()).unwrap(),
            "MERGED BY LLM\n"
        );
    }

    #[tokio::test]
    async fn merge_with_llm_falls_back_to_theirs_on_resolver_error() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(&tmp.path().to_path_buf());
        commit_on_branch(&repo, "main", "shared.txt", "BASE\n");
        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        repo.branch("task-base/T1", &main_tip, false).unwrap();
        commit_on_branch(&repo, "task-base/T1", "shared.txt", "OURS\n");
        commit_on_branch(&repo, "agent/A", "shared.txt", "THEIRS\n");
        repo.set_head("refs/heads/main").unwrap();

        let resolver = MockResolver {
            produce: String::new(),
            fail: true,
        };
        let outcome = merge_with_llm(&repo, "task-base/T1", "agent/A", "merge llm", &resolver)
            .await
            .unwrap();
        assert!(outcome.llm_resolved.is_empty());
        assert_eq!(outcome.fallback_theirs, vec!["shared.txt"]);

        let t1 = repo.find_branch("task-base/T1", BranchType::Local).unwrap();
        let tree = t1.get().peel_to_commit().unwrap().tree().unwrap();
        let blob = repo
            .find_blob(tree.get_path(Path::new("shared.txt")).unwrap().id())
            .unwrap();
        assert_eq!(std::str::from_utf8(blob.content()).unwrap(), "THEIRS\n");
    }

    #[tokio::test]
    async fn merge_with_llm_errors_when_no_conflict() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(&tmp.path().to_path_buf());
        commit_on_branch(&repo, "main", "shared.txt", "x\n");
        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap();
        repo.branch("task-base/T1", &main_tip, false).unwrap();
        // agent only adds a NEW file, no conflict with shared.txt
        commit_on_branch(&repo, "agent/A", "new.txt", "AAA\n");
        repo.set_head("refs/heads/main").unwrap();

        let resolver = MockResolver {
            produce: "x".into(),
            fail: false,
        };
        let res = merge_with_llm(&repo, "task-base/T1", "agent/A", "merge", &resolver).await;
        assert!(res.is_err());
    }
}
