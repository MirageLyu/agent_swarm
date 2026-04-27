//! 幂等的 Git 仓库初始化 helper。
//!
//! Worktree 操作要求 HEAD 已存在（不能是 unborn），因此首次 init 必须紧跟一个空的
//! Initial commit。这个 helper 把 `start_mission_execution` 里历史上分散的 init 逻辑
//! 抽出来，共享给 `create_mission`（FM-15 v2.2 FR-18 from_scratch / from_existing）。

use std::path::Path;

use anyhow::{Context, Result};
use git2::{Repository, Signature};

/// 幂等地确保 `path` 是一个有 HEAD 的 Git 仓库。
///
/// 行为矩阵：
/// - 目录不存在 → 创建；继续往下
/// - 目录存在但不是 git 仓库 → `git init` + Initial commit
/// - 已是 git 仓库 → 不动
///
/// **不会**对已有仓库的 HEAD / 工作区做任何修改，可安全反复调用。
pub fn ensure_git_repo(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("create_dir_all {}", path.display()))?;

    if Repository::open(path).is_ok() {
        return Ok(());
    }

    let repo = Repository::init(path)
        .with_context(|| format!("git init {}", path.display()))?;

    let sig = repo
        .signature()
        .or_else(|_| Signature::now("Miragenty", "miragenty@localhost"))
        .context("create git signature")?;

    let tree_id = {
        let mut idx = repo.index().context("open index")?;
        idx.write_tree().context("write empty tree")?
    };
    let tree = repo.find_tree(tree_id).context("find empty tree")?;

    repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
        .context("create initial commit")?;

    tracing::info!("Initialized empty git repo at {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ensure_creates_repo_when_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("subdir");
        ensure_git_repo(&path).unwrap();
        assert!(Repository::open(&path).is_ok());
        let repo = Repository::open(&path).unwrap();
        assert!(repo.head().is_ok(), "HEAD must exist after ensure");
    }

    #[test]
    fn ensure_inits_existing_empty_dir() {
        let dir = TempDir::new().unwrap();
        // dir exists but is not a git repo
        ensure_git_repo(dir.path()).unwrap();
        assert!(Repository::open(dir.path()).is_ok());
    }

    #[test]
    fn ensure_is_idempotent_on_existing_repo() {
        let dir = TempDir::new().unwrap();
        ensure_git_repo(dir.path()).unwrap();
        let head_before = Repository::open(dir.path())
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string();

        ensure_git_repo(dir.path()).unwrap();
        let head_after = Repository::open(dir.path())
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string();

        assert_eq!(head_before, head_after, "HEAD must not change");
    }
}
