//! Explicit Merge Node v1 —— merge agent 专用 task_desc 模板。
//!
//! ## 设计意图
//!
//! 用户原话："这个 MergeNode 要有和通用 agent 相当的能力，复用同一套 agent harness"
//! "如果失败也不要人兜底，由 LLM 给出明确的错误信息，和其它 agent 执行节点一样"
//!
//! 因此 merge 节点：
//! - 跑的是同一个 `AgentEngine`，同一套 tools (`read_file` / `write_file` /
//!   `shell_exec` / `task_complete`)、同一套 hooks / fallback / recovery
//! - 差异**只在 system 的 `## Task` 段**：本模块构造的字符串塞进
//!   `scheduler::dispatch_task` 拼好的 `task_desc`
//! - 初始 worktree 已由 `prepare_task_base` 跑过 ref-only merge + theirs 兜底，
//!   merge agent 的工作是：①读冲突文件 → ②比对 parent 意图修正 → ③跑 verify_command
//!   → ④`task_complete`
//!
//! ## 多 parent 支持（v1.1）
//!
//! v1 初稿曾硬编码 N=2（受"二叉 reduction tree"算法限制）。修正后单 merge node
//! 直接承接 X 的全部 N parents（详见 `planner_merge_inject` 文档）。本模块按
//! 1..N 编号循环渲染 parent 段落，并对单 parent description 动态分配字符 budget：
//!
//! - N=2: per-parent 600 chars  (总 ~1200)
//! - N=4: per-parent 400 chars  (总 ~1600)
//! - N=8: per-parent 300 chars  (总 ~2400，下限 300 保证可读)
//!
//! 这只是 description 截断，real diff 让 merge agent 自己用 shell 跑
//! `git diff <merge-base>..<branch>` 拉取，prompt 不预塞。
//!
//! ## 失败语义
//!
//! - 成功：guardrail（含 `CommandPasses { cmd: verify }`）通过 → task completed
//! - agent 自己放弃：自然 fail（max_steps / token budget 耗尽）→ engine 写入
//!   `last_error` + agent 的 final assistant message 显示为 mission failure reason
//! - **不进 Approval Queue**——失败就显式失败，让用户去 timeline 看 merge agent
//!   的推理过程自己判断
//!
//! 详见 `docs/research/explicit-merge-node/proposal.md`。

use anyhow::{anyhow, Result};
use std::collections::BTreeMap;

use crate::db::{queries, Database};

/// 单 parent description 字符 budget 下限（保证最少可读）。
const MIN_PER_PARENT_CHARS: usize = 300;
/// 单 parent description 字符 budget 上限（N 小时也不必无限长）。
const MAX_PER_PARENT_CHARS: usize = 600;
/// 总 budget 系数：`per_parent = clamp(BUDGET_BASE / N, MIN, MAX)`。
const BUDGET_BASE_CHARS: usize = 1200;

/// 构造 merge agent 的 task_desc 扩展段（追加在原 title/description 之后）。
///
/// 返回的字符串包含：
/// 1. "## Merge Context" 标题（说明这是 N-way merge）
/// 2. 每个 parent 的 task 信息（id / title / description 摘要 / agent 分支）
/// 3. 已记录的 `task_base_conflicts` 文件清单（按 parent 分组，如有）
/// 4. mission 级 `verify_command`（如配置）
/// 5. 明确的成功标准 & 失败处理指引
///
/// 失败情形（返回 Err）：
/// - `merge_parents_json` 为 NULL 或解析失败
/// - parents.len() < 2（merge node 没有 ≥2 个 parent 是设计错误）
/// - 任一 parent task 查不到（说明数据一致性出问题）
///
/// `merge_parents_json` 是 `tasks.merge_parents` 列的原始字符串（JSON 数组）。
pub fn build_merge_task_desc(
    mission_id: &str,
    task_id: &str,
    merge_parents_json: Option<&str>,
    db: &Database,
) -> Result<String> {
    let json = merge_parents_json
        .ok_or_else(|| anyhow!("merge task {task_id} has NULL merge_parents column"))?;
    let parents: Vec<String> =
        serde_json::from_str(json).map_err(|e| anyhow!("merge_parents JSON parse failed: {e}"))?;
    if parents.len() < 2 {
        return Err(anyhow!(
            "merge task {task_id} expects >= 2 parents, got {} (this is a planner bug)",
            parents.len()
        ));
    }

    let n = parents.len();
    let per_parent_budget =
        (BUDGET_BASE_CHARS / n).clamp(MIN_PER_PARENT_CHARS, MAX_PER_PARENT_CHARS);

    // 预先 lookup 全部 parent（任一失败立即 bail）
    let mut parent_infos: Vec<(String, String, String, Option<String>)> = Vec::with_capacity(n);
    for pid in &parents {
        let (title, desc, branch) = lookup_parent(db, pid)?;
        parent_infos.push((pid.clone(), title, desc, branch));
    }

    // 冲突清单：prepare_task_base 跑过 theirs 兜底后写入；按 parent_id 分组。
    let conflicts = db
        .with_conn(|conn| queries::get_task_base_conflicts(conn, task_id))
        .unwrap_or_default();
    let mut conflicts_by_parent: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (parent_id, file, resolution) in &conflicts {
        conflicts_by_parent
            .entry(parent_id.clone())
            .or_default()
            .push((file.clone(), resolution.clone()));
    }

    let verify_cmd = db
        .with_conn(|conn| queries::get_mission_verify_command(conn, mission_id))
        .unwrap_or(None);

    let mut out = String::new();
    out.push_str("## Merge Context\n\n");
    out.push_str(&format!(
        "You are a **merge agent**. **{n} parallel tasks** finished, and their changes have been \
         ref-only merged into your worktree (later parents' versions win on raw conflicts). \
         Your job is to ensure the merged result is **semantically coherent** and **builds clean**.\n\n"
    ));

    // 按 1..N 编号渲染每个 parent
    for (idx, (pid, title, desc, branch)) in parent_infos.iter().enumerate() {
        out.push_str(&format!("### Parent {} — `{pid}`\n", idx + 1));
        out.push_str(&format!("- **Title**: {title}\n"));
        out.push_str(&format!(
            "- **Goal**: {}\n",
            truncate(desc, per_parent_budget)
        ));
        if let Some(branch) = branch {
            out.push_str(&format!("- **Branch**: `{branch}`\n"));
            out.push_str(&format!(
                "  (Use `git diff <merge-base>..{branch}` to see exactly what this parent changed.)\n"
            ));
        }
        out.push('\n');
    }

    // 冲突文件清单（按 parent 分组）
    if !conflicts.is_empty() {
        out.push_str("### Files with raw merge conflicts (resolved via 'theirs' fallback)\n\n");
        out.push_str(
            "These files had textual conflicts during the ref-only merge. The auto-resolver \
             picked the **later parent's** version. You MUST review each one and decide whether \
             the chosen side preserves **every** parent's intent. If not, edit the file to combine \
             all intents semantically.\n\n",
        );
        for (parent_id, files) in &conflicts_by_parent {
            out.push_str(&format!("From parent `{parent_id}`:\n"));
            for (file, resolution) in files {
                out.push_str(&format!("- `{file}` — auto-resolved as `{resolution}`\n"));
            }
            out.push('\n');
        }
    } else {
        out.push_str(
            "### No raw merge conflicts recorded\n\n\
             The ref-only merge had no textual conflicts. Your job is still to verify the \
             combined changes are **semantically coherent** across all parents (e.g. one parent \
             renamed a function that another parent still calls).\n\n",
        );
    }

    out.push_str("### Success criteria\n\n");
    if let Some(cmd) = &verify_cmd {
        out.push_str(&format!(
            "1. Run `{cmd}` and ensure it exits with code 0. **This is enforced by a guardrail \
             — if you call `task_complete` without a successful verify run in this session, the \
             completion will be rejected and you'll be asked to verify.**\n"
        ));
        out.push_str(
            "2. When verify passes and you're satisfied the merge preserves every parent's \
             intent, call `task_complete` with a summary describing what conflicts you resolved \
             and how.\n\n",
        );
    } else {
        out.push_str(
            "1. No mission-level `verify_command` is configured. Use your judgment: run \
             whatever build/lint/test makes sense for this repo (e.g. `cargo check`, \
             `npm run build`, `tsc --noEmit`) via `shell_exec` to validate.\n",
        );
        out.push_str(
            "2. When you're satisfied the merge preserves every parent's intent and builds \
             clean, call `task_complete`.\n\n",
        );
    }

    out.push_str("### If you cannot reconcile\n\n");
    out.push_str(
        "If two or more parents made fundamentally incompatible changes that cannot be merged \
         without rewriting significant logic, **do not silently pick one side**. Instead:\n\
         - Document the exact incompatibility (which parents, which file, which behavior conflict)\n\
         - Explain why a clean merge is impossible from your context\n\
         - Stop iterating — let the run terminate naturally (budget/steps will exhaust)\n\n\
         Your final assistant message will be surfaced to the user as the mission failure reason. \
         Be specific and actionable so they know which parent task needs to be redesigned.\n",
    );

    Ok(out)
}

fn lookup_parent(db: &Database, task_id: &str) -> Result<(String, String, Option<String>)> {
    let title_desc = db
        .with_conn(|conn| queries::get_task_title_and_description(conn, task_id))?
        .ok_or_else(|| anyhow!("parent task {task_id} not found"))?;
    let agent_id = db
        .with_conn(|conn| queries::get_agent_id_for_task(conn, task_id))
        .unwrap_or(None);
    let branch = agent_id.map(|aid| format!("agent/{aid}"));
    Ok((title_desc.0, title_desc.1, branch))
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let taken: String = s.chars().take(max_chars).collect();
    format!("{taken}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_marked() {
        let out = truncate("abcdefghijklmnop", 5);
        assert_eq!(out, "abcde…");
    }

    #[test]
    fn missing_merge_parents_json_errors() {
        let db = Database::open_in_memory().unwrap();
        let err = build_merge_task_desc("m1", "t1", None, &db).unwrap_err();
        assert!(err.to_string().contains("NULL merge_parents"));
    }

    #[test]
    fn fewer_than_two_parents_errors() {
        let db = Database::open_in_memory().unwrap();
        let err = build_merge_task_desc("m1", "t1", Some(r#"["only-one"]"#), &db).unwrap_err();
        assert!(err.to_string().contains(">= 2 parents"), "got: {}", err);
    }

    /// 端到端：两个 parent 都存在 → 拼出完整 merge prompt。
    #[test]
    fn end_to_end_two_parents_yields_complete_prompt() {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO missions (id, title, repo_path) VALUES ('m1', 'Mission One', '/tmp')",
                [],
            )?;
            c.execute(
                "INSERT INTO tasks (id, mission_id, title, description, complexity) \
                 VALUES ('p1', 'm1', 'Auth refactor', 'Switch from session to JWT', 'medium'),\
                        ('p2', 'm1', 'DB pool', 'Add connection pool to Postgres', 'medium'),\
                        ('mg', 'm1', 'Merge: Auth + DB', 'reconcile', 'low')",
                [],
            )?;
            // 给一个 task_base_conflict 让 conflicts 段落渲染（resolution 必须命中
            // CHECK 约束的合法值集合）
            c.execute(
                "INSERT INTO task_base_conflicts (task_id, parent_task_id, file_path, resolution) \
                 VALUES ('mg', 'p2', 'src/db.rs', 'heuristic_theirs')",
                [],
            )?;
            // verify_command
            c.execute(
                "UPDATE missions SET verify_command = 'cargo check' WHERE id = 'm1'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let out = build_merge_task_desc("m1", "mg", Some(r#"["p1","p2"]"#), &db)
            .expect("should build prompt");

        assert!(out.contains("Merge Context"));
        assert!(out.contains("**2 parallel tasks**"));
        assert!(out.contains("Parent 1"));
        assert!(out.contains("Auth refactor"));
        assert!(out.contains("Switch from session to JWT"));
        assert!(out.contains("Parent 2"));
        assert!(out.contains("DB pool"));
        assert!(out.contains("src/db.rs"));
        assert!(out.contains("heuristic_theirs"));
        assert!(out.contains("cargo check"));
        assert!(out.contains("guardrail"));
        assert!(out.contains("If you cannot reconcile"));
    }

    #[test]
    fn no_conflicts_no_verify_still_renders() {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO missions (id, title, repo_path) VALUES ('m1', 'Mission One', '/tmp')",
                [],
            )?;
            c.execute(
                "INSERT INTO tasks (id, mission_id, title, description, complexity) \
                 VALUES ('p1', 'm1', 'A', 'desc A', 'low'),\
                        ('p2', 'm1', 'B', 'desc B', 'low'),\
                        ('mg', 'm1', 'M', 'm', 'low')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let out = build_merge_task_desc("m1", "mg", Some(r#"["p1","p2"]"#), &db).unwrap();
        assert!(out.contains("No raw merge conflicts recorded"));
        assert!(out.contains("No mission-level `verify_command`"));
    }

    /// v1.1 关键：N=4 parents 单 merge node 也能完整渲染（不再硬编码 N=2）。
    #[test]
    fn end_to_end_four_parents_renders_all_sections() {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO missions (id, title, repo_path) VALUES ('m1', 'Mission One', '/tmp')",
                [],
            )?;
            c.execute(
                "INSERT INTO tasks (id, mission_id, title, description, complexity) \
                 VALUES ('p1', 'm1', 'Auth', 'JWT switch', 'medium'),\
                        ('p2', 'm1', 'DB', 'pgpool', 'medium'),\
                        ('p3', 'm1', 'Cache', 'redis layer', 'medium'),\
                        ('p4', 'm1', 'Logs', 'json logger', 'low'),\
                        ('mg', 'm1', 'Merge 4 ways', 'reconcile', 'low')",
                [],
            )?;
            c.execute(
                "INSERT INTO task_base_conflicts (task_id, parent_task_id, file_path, resolution) \
                 VALUES ('mg', 'p2', 'src/db.rs', 'heuristic_theirs'),\
                        ('mg', 'p3', 'src/cache.rs', 'heuristic_theirs')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let out = build_merge_task_desc("m1", "mg", Some(r#"["p1","p2","p3","p4"]"#), &db)
            .expect("should build N=4 prompt");

        // 关键：4 个 parent 段落都要渲染
        assert!(
            out.contains("**4 parallel tasks**"),
            "header should mention N=4"
        );
        assert!(out.contains("Parent 1 — `p1`"));
        assert!(out.contains("Parent 2 — `p2`"));
        assert!(out.contains("Parent 3 — `p3`"));
        assert!(out.contains("Parent 4 — `p4`"));
        assert!(out.contains("JWT switch"));
        assert!(out.contains("pgpool"));
        assert!(out.contains("redis layer"));
        assert!(out.contains("json logger"));
        // 冲突按 parent 分组
        assert!(out.contains("From parent `p2`"));
        assert!(out.contains("From parent `p3`"));
        assert!(out.contains("src/db.rs"));
        assert!(out.contains("src/cache.rs"));
    }

    #[test]
    fn per_parent_budget_shrinks_for_many_parents() {
        // N=8 时 per-parent budget 应被压到下限 300，但仍保证 description
        // 不被压成空字符串
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO missions (id, title, repo_path) VALUES ('m1', 'Mission One', '/tmp')",
                [],
            )?;
            let mut stmt =
                "INSERT INTO tasks (id, mission_id, title, description, complexity) VALUES "
                    .to_string();
            let mut rows: Vec<String> = (0..8)
                .map(|i| {
                    format!("('p{i}', 'm1', 'T{i}', 'description for parent number {i}', 'low')")
                })
                .collect();
            rows.push("('mg', 'm1', 'M', 'm', 'low')".to_string());
            stmt.push_str(&rows.join(","));
            c.execute(&stmt, [])?;
            Ok(())
        })
        .unwrap();

        let parents_json =
            serde_json::to_string(&(0..8).map(|i| format!("p{i}")).collect::<Vec<_>>()).unwrap();

        let out = build_merge_task_desc("m1", "mg", Some(&parents_json), &db).unwrap();
        assert!(out.contains("**8 parallel tasks**"));
        for i in 0..8 {
            assert!(out.contains(&format!("Parent {} — `p{i}`", i + 1)));
        }
    }
}
