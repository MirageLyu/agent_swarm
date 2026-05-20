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

use crate::db::{queries, Database};

/// Merge prompt 中"parent diff snippet 总字节"软上限。
/// 单 parent 已 truncate 到 8KB；两 parent 合计 16KB 远低于 P0-2 budget。
const PARENT_DIFF_TOTAL_BUDGET_BYTES: usize = 16_384;

/// 单 parent diff 截断长度。**先 truncate 后 join**：保护 prompt 不被大改动塞爆。
const SINGLE_PARENT_DIFF_BYTES: usize = 8_192;

/// 构造 merge agent 的 task_desc 扩展段（追加在原 title/description 之后）。
///
/// 返回的字符串包含：
/// 1. "## Merge Context" 标题
/// 2. 两个 parent 的 task 信息（id / title / description 摘要 / agent 分支）
/// 3. 已记录的 `task_base_conflicts` 文件清单（如有）
/// 4. mission 级 `verify_command`（如配置）
/// 5. 明确的成功标准 & 失败处理指引
///
/// 失败情形（返回 Err）：
/// - `merge_parents_json` 解析失败或不是 2 个元素
/// - parent task 查不到（说明数据一致性出问题）
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
    let parents: Vec<String> = serde_json::from_str(json)
        .map_err(|e| anyhow!("merge_parents JSON parse failed: {e}"))?;
    if parents.len() != 2 {
        return Err(anyhow!(
            "merge task expects exactly 2 parents (v1 binary reduction tree), got {}",
            parents.len()
        ));
    }

    let p1_id = &parents[0];
    let p2_id = &parents[1];

    let (p1_title, p1_desc, p1_agent_branch) = lookup_parent(db, p1_id)?;
    let (p2_title, p2_desc, p2_agent_branch) = lookup_parent(db, p2_id)?;

    // 冲突清单：prepare_task_base 跑过 theirs 兜底后写入；可能为空。
    let conflicts = db
        .with_conn(|conn| queries::get_task_base_conflicts(conn, task_id))
        .unwrap_or_default();

    let verify_cmd = db
        .with_conn(|conn| queries::get_mission_verify_command(conn, mission_id))
        .unwrap_or(None);

    let mut out = String::new();
    out.push_str("## Merge Context\n\n");
    out.push_str(
        "You are a **merge agent**. Two parallel tasks finished, and their changes have been \
         ref-only merged into your worktree (later parent's version wins on raw conflicts). \
         Your job is to ensure the merged result is **semantically coherent** and **builds clean**.\n\n",
    );

    // Parent 1
    out.push_str(&format!("### Parent 1 — `{p1_id}`\n"));
    out.push_str(&format!("- **Title**: {p1_title}\n"));
    out.push_str(&format!("- **Goal**: {}\n", truncate(&p1_desc, 600)));
    if let Some(branch) = &p1_agent_branch {
        out.push_str(&format!("- **Branch**: `{branch}`\n"));
        out.push_str(&format!(
            "  (Use `git diff <merge-base>..{branch}` to see exactly what this parent changed.)\n"
        ));
    }
    out.push('\n');

    // Parent 2
    out.push_str(&format!("### Parent 2 — `{p2_id}`\n"));
    out.push_str(&format!("- **Title**: {p2_title}\n"));
    out.push_str(&format!("- **Goal**: {}\n", truncate(&p2_desc, 600)));
    if let Some(branch) = &p2_agent_branch {
        out.push_str(&format!("- **Branch**: `{branch}`\n"));
        out.push_str(&format!(
            "  (Use `git diff <merge-base>..{branch}` to see exactly what this parent changed.)\n"
        ));
    }
    out.push('\n');

    // 总字节软上限提示（让 LLM 知道我们故意截了）
    let _budget = PARENT_DIFF_TOTAL_BUDGET_BYTES;
    let _single = SINGLE_PARENT_DIFF_BYTES;

    // 冲突文件清单
    if !conflicts.is_empty() {
        out.push_str("### Files with raw merge conflicts (resolved via 'theirs' fallback)\n\n");
        out.push_str(
            "These files had textual conflicts during the ref-only merge. The auto-resolver \
             picked the **later parent's** version. You MUST review each one and decide whether \
             the chosen side preserves both parents' intent. If not, edit the file to combine \
             both intents semantically.\n\n",
        );
        for (parent_id, file, resolution) in &conflicts {
            out.push_str(&format!(
                "- `{file}` — auto-resolved as `{resolution}` (from parent `{parent_id}`)\n"
            ));
        }
        out.push('\n');
    } else {
        out.push_str(
            "### No raw merge conflicts recorded\n\n\
             The ref-only merge had no textual conflicts. Your job is still to verify the \
             combined changes are **semantically coherent** (e.g. one parent renamed a function \
             that the other parent still calls).\n\n",
        );
    }

    // Verify command
    out.push_str("### Success criteria\n\n");
    if let Some(cmd) = &verify_cmd {
        out.push_str(&format!(
            "1. Run `{cmd}` and ensure it exits with code 0. **This is enforced by a guardrail \
             — if you call `task_complete` without a successful verify run in this session, the \
             completion will be rejected and you'll be asked to verify.**\n"
        ));
        out.push_str(
            "2. When verify passes and you're satisfied the merge preserves both parents' \
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
            "2. When you're satisfied the merge preserves both parents' intent and builds \
             clean, call `task_complete`.\n\n",
        );
    }

    // 失败语义（重要：不进 approval queue）
    out.push_str("### If you cannot reconcile\n\n");
    out.push_str(
        "If the two parents made fundamentally incompatible changes that cannot be merged \
         without rewriting significant logic, **do not silently pick one side**. Instead:\n\
         - Document the exact incompatibility (which file, which behavior conflict)\n\
         - Explain why a clean merge is impossible from your context\n\
         - Stop iterating — let the run terminate naturally (budget/steps will exhaust)\n\n\
         Your final assistant message will be surfaced to the user as the mission failure reason. \
         Be specific and actionable so they know which parent's task needs to be redesigned.\n",
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
    fn invalid_parent_count_errors() {
        let db = Database::open_in_memory().unwrap();
        let err =
            build_merge_task_desc("m1", "t1", Some(r#"["only-one"]"#), &db).unwrap_err();
        assert!(err.to_string().contains("exactly 2 parents"));
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

        let out = build_merge_task_desc(
            "m1",
            "mg",
            Some(r#"["p1","p2"]"#),
            &db,
        )
        .expect("should build prompt");

        assert!(out.contains("Merge Context"));
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
}
