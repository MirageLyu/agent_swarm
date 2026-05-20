//! FM-12 Mission Report 生成器
//!
//! 设计原则：
//! - **两层结构**：确定性数据汇聚（DB 查询，永不失败）+ LLM 增强（best-effort）
//!   即使 LLM 不可用 / 超时，报告也能出，只是 executive_summary 和 decisions
//!   退化为模板拼接的版本
//! - **覆盖式生成**：mission_id UNIQUE，重复调用走 UPSERT 直接覆盖，
//!   不保留历史报告（如果未来要做"报告版本对比"再升级 schema_version）
//! - **schema_version**：当前固定为 1。前端按 version 决定如何渲染，
//!   未来字段增删通过升级版本号 + 前端兼容层处理，不破坏老报告
//!
//! 失败模式：
//! - DB 查询失败（mission 不存在 / 表损坏）：直接返回 Err，不入库
//! - LLM 失败 / 超时：fallback 到模板摘要，决策列表退化为"按 agent 列表"，
//!   报告仍正常入库
//!
//! Watchdog：
//! - LLM 调用统一 30s 超时（NFR-01: 报告生成 ≤ 30 秒）
//! - 调用方（commands/report.rs）应在 generate_mission_report 上额外加
//!   60s 总超时兜底（DB 查询 + LLM）

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::db::{queries, Database};
use crate::llm::{ContentBlock, LlmProvider, LlmRequest, Message, MessageRole};

// ──────────────────────────────────────────────────────────────────────────
// Report data model
// ──────────────────────────────────────────────────────────────────────────

pub const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionReport {
    pub schema_version: u32,
    pub mission: ReportMission,
    pub summary: ReportSummary,
    pub decisions: Vec<ReportDecision>,
    pub evaluator_review: ReportEvaluatorReview,
    pub task_matrix: Vec<ReportTaskRow>,
    pub cost_breakdown: ReportCostBreakdown,
    pub limitations: Vec<String>,
    pub contract: Option<ReportContract>,
    pub artifacts: Vec<ReportArtifact>,
    pub learning_flywheel: ReportLearningFlywheel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportMission {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_seconds: i64,
    pub total_cost_usd: f64,
    pub main_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSummary {
    pub executive: String,
    pub metrics: ReportMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportMetrics {
    pub tasks_total: i64,
    pub tasks_completed: i64,
    pub tasks_failed: i64,
    pub duration_seconds: i64,
    pub total_cost_usd: f64,
    pub avg_quality_score: Option<f64>,
    pub auto_fixes: i64,
    pub review_reduction_rate: Option<f64>,
    /// Single-Agent Uplift P1-2: 本 mission 全部 agent 累计 cross-model fallback
    /// 切换次数。0 = 主模型全程稳定（绝大多数 mission）。>0 表明上游 overload /
    /// rate-limit 期间 agent 通过备用模型保住了进度——是 fallback 机制兜底成功
    /// 的硬证据。
    ///
    /// 给前端用：渲染为 chip "fallback × N" 提示用户"过程中有切换，可能影响成本"。
    #[serde(default)]
    pub fallback_switches_total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportDecision {
    pub id: String,
    pub title: String,
    pub rationale: String,
    pub trade_off: String,
    pub risk: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportEvaluatorReview {
    pub rounds: Vec<ReportEvaluatorRound>,
    pub total_issues: i64,
    pub auto_fixed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportEvaluatorRound {
    pub agent_id: String,
    pub agent_name: String,
    pub task_title: String,
    pub score: f64,
    pub issues: i64,
    pub auto_fixed: i64,
    pub summary: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportTaskRow {
    pub task_id: String,
    pub title: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub score: Option<f64>,
    pub cost_usd: f64,
    pub duration_seconds: Option<i64>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCostBreakdown {
    pub total_usd: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub by_model: Vec<ReportCostModel>,
    pub by_task: Vec<ReportCostTask>,
    pub by_agent: Vec<ReportCostAgent>,
    pub budget_usd: Option<f64>,
    pub budget_used_ratio: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCostModel {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCostTask {
    pub task_id: String,
    pub title: String,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCostAgent {
    pub agent_id: String,
    pub agent_name: String,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportContract {
    pub status: String,
    pub items: Vec<ReportContractItem>,
    pub budget_usd: Option<f64>,
    pub quality_threshold: Option<f64>,
    pub max_duration_hours: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportContractItem {
    pub section: String,
    pub text: String,
    /// 是否达成。当前实现是"启发式 + 用户可手动覆盖（待做）"：
    /// - exclusions/assumptions：默认 true（无需主动验证）
    /// - scope/constraints：mission 整体 completed → true，failed → false
    /// 未来 FM-11 evaluator 可注入条目级证据
    pub achieved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportArtifact {
    pub artifact_type: String,
    pub local_name: String,
    pub summary: String,
    pub file_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportLearningFlywheel {
    /// 历史 mission 中用户高频投反对票的决策模式（占位，需要 FM-13 数据积累）
    pub past_decision_patterns: Vec<String>,
    pub insight: String,
}

// ──────────────────────────────────────────────────────────────────────────
// Public entrypoint
// ──────────────────────────────────────────────────────────────────────────

/// 生成报告并落库，返回 (report_id, generated_at)。
///
/// 步骤：
/// 1. 同步从 DB 汇聚所有确定性数据（mission/tasks/agents/cost/evaluator/contract/artifacts）
/// 2. 异步调用 LLM 生成 executive_summary 和 decisions（带 30s 超时 + 降级）
/// 3. 同步 upsert 到 mission_reports 表
///
/// # Arguments
/// - `provider`: LLM 提供方，可为 None（强制走降级模板）
/// - `model`: LLM 模型名
pub async fn generate_mission_report(
    app: &AppHandle,
    mission_id: &str,
    provider: Option<Arc<dyn LlmProvider>>,
    model: Option<String>,
) -> Result<(String, String)> {
    let db = app
        .try_state::<Database>()
        .ok_or_else(|| anyhow::anyhow!("Database state not registered"))?;

    // Step 1: 同步汇聚确定性数据（DB 查询）
    let aggregate = db
        .with_conn(|conn| aggregate_data(conn, mission_id))
        .map_err(|e| anyhow::anyhow!("aggregate_data failed: {}", e))?;

    // Step 2: 异步 LLM 增强（best-effort，30s 超时降级）
    let (executive, decisions) = match (provider, model) {
        (Some(p), Some(m)) => {
            match tokio::time::timeout(
                Duration::from_secs(30),
                enhance_with_llm(p.clone(), &m, &aggregate),
            )
            .await
            {
                Ok(Ok(enhanced)) => enhanced,
                Ok(Err(err)) => {
                    tracing::warn!(
                        mission_id,
                        error = %err,
                        "LLM enhancement failed, using fallback"
                    );
                    fallback_executive_and_decisions(&aggregate)
                }
                Err(_) => {
                    tracing::warn!(mission_id, "LLM enhancement timed out (30s), using fallback");
                    fallback_executive_and_decisions(&aggregate)
                }
            }
        }
        _ => {
            tracing::info!(mission_id, "no LLM provider, using fallback summary");
            fallback_executive_and_decisions(&aggregate)
        }
    };

    // Step 3: 组装最终报告
    let mut report = aggregate.into_report();
    report.summary.executive = executive;
    report.decisions = decisions;

    // Step 4: 落库
    let report_id = format!("rep-{}", Uuid::new_v4().simple());
    let report_data = serde_json::to_string(&report)
        .with_context(|| "failed to serialize MissionReport to JSON")?;

    let result_id = db
        .with_conn(|conn| {
            queries::upsert_mission_report(conn, &report_id, mission_id, &report_data)
        })
        .map_err(|e| anyhow::anyhow!("upsert_mission_report failed: {}", e))?;

    let generated_at = db
        .with_conn(|conn| {
            queries::get_mission_report_by_id(conn, &result_id)
                .map(|opt| opt.map(|r| r.generated_at).unwrap_or_default())
        })
        .map_err(|e| anyhow::anyhow!("read back report failed: {}", e))?;

    Ok((result_id, generated_at))
}

// ──────────────────────────────────────────────────────────────────────────
// Step 1: 数据汇聚（同步，DB-only）
// ──────────────────────────────────────────────────────────────────────────

/// 中间结构：包含所有 DB 数据 + 派生指标，但 executive 和 decisions 留空。
struct AggregateData {
    mission: ReportMission,
    metrics: ReportMetrics,
    evaluator_review: ReportEvaluatorReview,
    task_matrix: Vec<ReportTaskRow>,
    cost_breakdown: ReportCostBreakdown,
    contract: Option<ReportContract>,
    artifacts: Vec<ReportArtifact>,
    /// 给 LLM 用的提示数据（agent 描述、score 列表、关键 tool_use 概要）
    llm_hint: LlmHintData,
}

struct LlmHintData {
    mission_title: String,
    mission_description: String,
    status: String,
    agent_summaries: Vec<String>,
    failure_reasons: Vec<String>,
}

impl AggregateData {
    fn into_report(self) -> MissionReport {
        let limitations = derive_limitations(&self);
        let learning_flywheel = ReportLearningFlywheel {
            past_decision_patterns: vec![],
            insight: if self.mission.status == "completed" {
                "Mission 已完成。后续可在多次 mission 完成后查看反馈聚合趋势。".to_string()
            } else {
                "Mission 未完整成功，本次结果不计入历史 pattern。".to_string()
            },
        };

        MissionReport {
            schema_version: REPORT_SCHEMA_VERSION,
            mission: self.mission,
            summary: ReportSummary {
                executive: String::new(), // 由 LLM/fallback 填充
                metrics: self.metrics,
            },
            decisions: vec![], // 由 LLM/fallback 填充
            evaluator_review: self.evaluator_review,
            task_matrix: self.task_matrix,
            cost_breakdown: self.cost_breakdown,
            limitations,
            contract: self.contract,
            artifacts: self.artifacts,
            learning_flywheel,
        }
    }
}

fn aggregate_data(conn: &Connection, mission_id: &str) -> Result<AggregateData> {
    // ── mission 主行
    let (title, description, status, total_cost_usd, created_at, updated_at, main_branch): (
        String,
        String,
        String,
        f64,
        String,
        String,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT title, description, status, total_cost_usd, created_at, updated_at, main_branch
             FROM missions WHERE id = ?1",
            [mission_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .with_context(|| format!("mission not found: {}", mission_id))?;

    let duration_seconds = compute_duration_seconds(conn, mission_id, &created_at, &updated_at);

    let mission = ReportMission {
        id: mission_id.to_string(),
        title: title.clone(),
        description: description.clone(),
        status: status.clone(),
        started_at: created_at,
        completed_at: if status == "completed" || status == "failed" {
            Some(updated_at)
        } else {
            None
        },
        duration_seconds,
        total_cost_usd,
        main_branch,
    };

    // ── tasks + agents + scores（task matrix）
    let task_matrix = collect_task_matrix(conn, mission_id)?;

    let tasks_total = task_matrix.len() as i64;
    let tasks_completed = task_matrix.iter().filter(|t| t.status == "completed").count() as i64;
    let tasks_failed = task_matrix
        .iter()
        .filter(|t| t.status == "failed" || t.status == "cancelled")
        .count() as i64;

    // ── evaluator reviews
    let evaluator_review = collect_evaluator_review(conn, mission_id)?;

    let avg_quality_score = if evaluator_review.rounds.is_empty() {
        None
    } else {
        let sum: f64 = evaluator_review.rounds.iter().map(|r| r.score).sum();
        Some(sum / evaluator_review.rounds.len() as f64)
    };

    let auto_fixes = evaluator_review.auto_fixed;

    // Review Reduction Rate: auto_fixed / total_issues（无数据则 None）
    let review_reduction_rate = if evaluator_review.total_issues > 0 {
        Some(evaluator_review.auto_fixed as f64 / evaluator_review.total_issues as f64)
    } else {
        None
    };

    // P1-2 Phase B：聚合本 mission 内所有 agent 的 cross-model fallback 计数。
    // 缺列时（旧 DB / 单测里 migration 027 没跑）回退 0；SUM(NULL) 默认 NULL。
    let fallback_switches_total: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(a.fallback_switches_total), 0) \
             FROM agents a JOIN tasks t ON a.task_id = t.id \
             WHERE t.mission_id = ?1",
            [mission_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0);

    let metrics = ReportMetrics {
        tasks_total,
        tasks_completed,
        tasks_failed,
        duration_seconds,
        total_cost_usd,
        avg_quality_score,
        auto_fixes,
        review_reduction_rate,
        fallback_switches_total,
    };

    // ── cost breakdown
    let cost_breakdown = collect_cost_breakdown(conn, mission_id, total_cost_usd)?;

    // ── contract
    let contract = collect_contract(conn, mission_id, &status)?;

    // ── artifacts
    let artifacts = collect_artifacts(conn, mission_id)?;

    // ── LLM hint
    let llm_hint = LlmHintData {
        mission_title: title,
        mission_description: description,
        status,
        agent_summaries: task_matrix
            .iter()
            .filter_map(|t| {
                let agent = t.agent_name.as_deref().unwrap_or("(unassigned)");
                Some(format!(
                    "- {} (agent: {}, status: {}, score: {})",
                    t.title,
                    agent,
                    t.status,
                    t.score.map(|s| format!("{:.1}", s)).unwrap_or_else(|| "-".into())
                ))
            })
            .collect(),
        failure_reasons: collect_failure_reasons(conn, mission_id)?,
    };

    Ok(AggregateData {
        mission,
        metrics,
        evaluator_review,
        task_matrix,
        cost_breakdown,
        contract,
        artifacts,
        llm_hint,
    })
}

fn compute_duration_seconds(
    conn: &Connection,
    mission_id: &str,
    created_at: &str,
    updated_at: &str,
) -> i64 {
    // 优先用 SQLite 的 julianday 差，避免 ISO 字符串自己解析
    let result: Option<f64> = conn
        .query_row(
            "SELECT (julianday(?1) - julianday(?2)) * 86400.0",
            [updated_at, created_at],
            |r| r.get::<_, Option<f64>>(0),
        )
        .ok()
        .flatten();
    let _ = mission_id;
    // julianday 是双精度浮点，900s 可能算出 899.9999...；用 round 而不是 trunc 保留语义。
    result.map(|s| s.max(0.0).round() as i64).unwrap_or(0)
}

fn collect_task_matrix(conn: &Connection, mission_id: &str) -> Result<Vec<ReportTaskRow>> {
    // 一次 LEFT JOIN 把 task / agent / cost 拼齐
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.status, t.created_at, t.completed_at,
                a.id, a.name,
                COALESCE((SELECT SUM(cr.cost_usd) FROM cost_records cr WHERE cr.agent_id = a.id), 0.0) AS agent_cost
         FROM tasks t
         LEFT JOIN agents a ON a.task_id = t.id
         WHERE t.mission_id = ?1
         ORDER BY t.created_at ASC",
    )?;

    let rows: Vec<(String, String, String, String, Option<String>, Option<String>, Option<String>, f64)> = stmt
        .query_map([mission_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut out = Vec::with_capacity(rows.len());
    for (task_id, title, status, created_at, completed_at, agent_id, agent_name, cost_usd) in rows {
        let score = if let Some(ref aid) = agent_id {
            queries::get_evaluator_review_for_agent(conn, aid)
                .ok()
                .flatten()
                .map(|rev| rev.overall_score)
        } else {
            None
        };

        let duration_seconds = if let Some(end) = completed_at.as_deref() {
            conn.query_row(
                "SELECT (julianday(?1) - julianday(?2)) * 86400.0",
                [end, &created_at],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten()
            .map(|s| s.max(0.0).round() as i64)
        } else {
            None
        };

        out.push(ReportTaskRow {
            task_id,
            title,
            agent_id,
            agent_name,
            score,
            cost_usd,
            duration_seconds,
            status,
        });
    }
    Ok(out)
}

fn collect_evaluator_review(
    conn: &Connection,
    mission_id: &str,
) -> Result<ReportEvaluatorReview> {
    // 所有 evaluator_reviews + 关联的 task title + annotation 计数
    let mut stmt = conn.prepare(
        "SELECT er.id, er.agent_id, er.overall_score, er.summary, er.created_at,
                a.name, t.title
         FROM evaluator_reviews er
         JOIN agents a ON a.id = er.agent_id
         LEFT JOIN tasks t ON t.id = a.task_id
         WHERE er.mission_id = ?1
         ORDER BY er.created_at ASC",
    )?;

    let rows: Vec<(String, String, f64, String, String, String, Option<String>)> = stmt
        .query_map([mission_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut total_issues = 0i64;
    let mut auto_fixed_total = 0i64;
    let mut rounds = Vec::with_capacity(rows.len());

    for (review_id, agent_id, score, summary, created_at, agent_name, task_title) in rows {
        // 单次 review 的 issues / auto_fixed 计数
        let (issues, auto_fixed): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), SUM(CASE WHEN status = 'auto_fixed' THEN 1 ELSE 0 END)
                 FROM evaluator_annotations WHERE review_id = ?1",
                [&review_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
            )
            .unwrap_or((0, 0));

        total_issues += issues;
        auto_fixed_total += auto_fixed;

        rounds.push(ReportEvaluatorRound {
            agent_id,
            agent_name,
            task_title: task_title.unwrap_or_else(|| "(detached)".into()),
            score,
            issues,
            auto_fixed,
            summary,
            created_at,
        });
    }

    Ok(ReportEvaluatorReview {
        rounds,
        total_issues,
        auto_fixed: auto_fixed_total,
    })
}

fn collect_cost_breakdown(
    conn: &Connection,
    mission_id: &str,
    total_cost_usd: f64,
) -> Result<ReportCostBreakdown> {
    let summary = queries::get_mission_cost_summary(conn, mission_id)?;

    // by model
    let mut stmt = conn.prepare(
        "SELECT cr.model,
                SUM(cr.input_tokens), SUM(cr.output_tokens), SUM(cr.cost_usd)
         FROM cost_records cr
         JOIN agents a ON a.id = cr.agent_id
         JOIN tasks t ON t.id = a.task_id
         WHERE t.mission_id = ?1
         GROUP BY cr.model
         ORDER BY SUM(cr.cost_usd) DESC",
    )?;
    let by_model: Vec<ReportCostModel> = stmt
        .query_map([mission_id], |r| {
            Ok(ReportCostModel {
                model: r.get(0)?,
                input_tokens: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                output_tokens: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                cost_usd: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    // by task
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, COALESCE(SUM(cr.cost_usd), 0.0)
         FROM tasks t
         LEFT JOIN agents a ON a.task_id = t.id
         LEFT JOIN cost_records cr ON cr.agent_id = a.id
         WHERE t.mission_id = ?1
         GROUP BY t.id, t.title
         ORDER BY SUM(cr.cost_usd) DESC NULLS LAST",
    )?;
    let by_task: Vec<ReportCostTask> = stmt
        .query_map([mission_id], |r| {
            Ok(ReportCostTask {
                task_id: r.get(0)?,
                title: r.get(1)?,
                cost_usd: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    // by agent
    let mut stmt = conn.prepare(
        "SELECT a.id, a.name, COALESCE(SUM(cr.cost_usd), 0.0)
         FROM agents a
         LEFT JOIN cost_records cr ON cr.agent_id = a.id
         JOIN tasks t ON t.id = a.task_id
         WHERE t.mission_id = ?1
         GROUP BY a.id, a.name
         ORDER BY SUM(cr.cost_usd) DESC NULLS LAST",
    )?;
    let by_agent: Vec<ReportCostAgent> = stmt
        .query_map([mission_id], |r| {
            Ok(ReportCostAgent {
                agent_id: r.get(0)?,
                agent_name: r.get(1)?,
                cost_usd: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    // budget（来自 contract）
    let budget_usd: Option<f64> = conn
        .query_row(
            "SELECT budget_usd FROM mission_contracts WHERE mission_id = ?1",
            [mission_id],
            |r| r.get::<_, Option<f64>>(0),
        )
        .ok()
        .flatten();

    let budget_used_ratio = budget_usd
        .filter(|b| *b > 0.0)
        .map(|b| total_cost_usd / b);

    Ok(ReportCostBreakdown {
        total_usd: summary.total_cost,
        total_input_tokens: summary.total_input_tokens,
        total_output_tokens: summary.total_output_tokens,
        by_model,
        by_task,
        by_agent,
        budget_usd,
        budget_used_ratio,
    })
}

fn collect_contract(
    conn: &Connection,
    mission_id: &str,
    mission_status: &str,
) -> Result<Option<ReportContract>> {
    let contract: Option<(String, String, Option<f64>, Option<f64>, Option<f64>)> = conn
        .query_row(
            "SELECT id, status, budget_usd, quality_threshold, max_duration_hours
             FROM mission_contracts WHERE mission_id = ?1",
            [mission_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<f64>>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                    r.get::<_, Option<f64>>(4)?,
                ))
            },
        )
        .ok();

    let Some((contract_id, status, budget_usd, quality_threshold, max_duration_hours)) = contract
    else {
        return Ok(None);
    };

    let mut stmt = conn.prepare(
        "SELECT section, text FROM contract_items
         WHERE contract_id = ?1
         ORDER BY section ASC, created_at ASC",
    )?;
    let items: Vec<ReportContractItem> = stmt
        .query_map([&contract_id], |r| {
            let section: String = r.get(0)?;
            let text: String = r.get(1)?;
            Ok((section, text))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(section, text)| {
            // 简单启发式：mission completed → scope/constraints 视作达成
            // exclusions/assumptions 视作 N/A（默认 true，UI 可不画 ✗）
            let achieved = match section.as_str() {
                "scope" | "constraints" => mission_status == "completed",
                _ => true,
            };
            ReportContractItem {
                section,
                text,
                achieved,
            }
        })
        .collect();

    Ok(Some(ReportContract {
        status,
        items,
        budget_usd,
        quality_threshold,
        max_duration_hours,
    }))
}

fn collect_artifacts(conn: &Connection, mission_id: &str) -> Result<Vec<ReportArtifact>> {
    let rows = queries::list_artifacts_for_mission(conn, mission_id)?;
    Ok(rows
        .into_iter()
        .map(|a| ReportArtifact {
            artifact_type: a.artifact_type,
            local_name: a.local_name,
            summary: a.summary,
            file_paths: serde_json::from_str(&a.file_paths).unwrap_or_default(),
        })
        .collect())
}

fn collect_failure_reasons(conn: &Connection, mission_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT t.title, t.last_error
         FROM tasks t
         WHERE t.mission_id = ?1 AND t.status IN ('failed', 'cancelled') AND t.last_error IS NOT NULL
         ORDER BY t.last_failed_at DESC NULLS LAST
         LIMIT 5",
    )?;
    let rows: Vec<String> = stmt
        .query_map([mission_id], |r| {
            let title: String = r.get(0)?;
            let err: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
            Ok(format!("- {}: {}", title, err))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn derive_limitations(data: &AggregateData) -> Vec<String> {
    let mut limitations = Vec::new();

    if data.metrics.tasks_failed > 0 {
        limitations.push(format!(
            "{} 个任务未完成。建议手动审查 last_error 后决定是否重试。",
            data.metrics.tasks_failed
        ));
    }

    if let Some(ratio) = data.cost_breakdown.budget_used_ratio {
        if ratio >= 0.9 {
            limitations.push(format!(
                "成本接近预算上限（{:.0}%）。后续 mission 建议提高预算或缩小 scope。",
                ratio * 100.0
            ));
        }
    }

    if let Some(avg) = data.metrics.avg_quality_score {
        if let Some(threshold) = data.contract.as_ref().and_then(|c| c.quality_threshold) {
            if avg < threshold {
                limitations.push(format!(
                    "平均质量评分 {:.1} 低于 Contract 阈值 {:.1}。",
                    avg, threshold
                ));
            }
        }
    }

    if data.evaluator_review.rounds.is_empty() && data.metrics.tasks_completed > 0 {
        limitations
            .push("Evaluator 未对任一 agent 产生评审记录，质量数据缺失。".to_string());
    }

    limitations
}

// ──────────────────────────────────────────────────────────────────────────
// Step 2: LLM 增强 + 降级
// ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct LlmEnhancement {
    executive_summary: String,
    decisions: Vec<LlmDecision>,
}

#[derive(Debug, Clone, Deserialize)]
struct LlmDecision {
    title: String,
    rationale: String,
    #[serde(default)]
    trade_off: String,
    #[serde(default)]
    risk: String,
}

async fn enhance_with_llm(
    provider: Arc<dyn LlmProvider>,
    model: &str,
    aggregate: &AggregateData,
) -> Result<(String, Vec<ReportDecision>)> {
    let prompt = build_llm_prompt(aggregate);
    let request = LlmRequest {
        model: model.to_string(),
        system: Some(LLM_SYSTEM_PROMPT.to_string()),
        messages: vec![Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: prompt }],
            cache_control: None,
        }],
        tools: vec![],
        max_tokens: 2048,
        provider_extras: None,
    };

    let response = provider
        .chat(&request)
        .await
        .with_context(|| "LLM chat call failed")?;

    let text = extract_text(&response.content);
    let parsed = parse_llm_enhancement(&text)
        .with_context(|| format!("failed to parse LLM JSON: {}", text))?;

    let decisions = parsed
        .decisions
        .into_iter()
        .enumerate()
        .map(|(i, d)| ReportDecision {
            id: format!("D-{}", i + 1),
            title: d.title,
            rationale: d.rationale,
            trade_off: d.trade_off,
            risk: d.risk,
        })
        .collect();

    Ok((parsed.executive_summary, decisions))
}

const LLM_SYSTEM_PROMPT: &str = r#"You are a senior engineering writer summarizing a multi-agent code generation mission.
Output STRICT JSON with the following schema and nothing else (no Markdown fences, no commentary):

{
  "executive_summary": "<1-2 paragraphs in Simplified Chinese explaining what was built, what worked, and what didn't>",
  "decisions": [
    {
      "title": "<short noun phrase>",
      "rationale": "<why this decision was made, in Simplified Chinese>",
      "trade_off": "<what was given up>",
      "risk": "<remaining risk>"
    }
  ]
}

Rules:
- Extract 2-5 decisions. Skip trivial choices.
- If you have no information, return decisions: [] rather than fabricating.
- Always write executive_summary even if data is sparse.
- Output ONLY the JSON object, no extra text.
"#;

fn build_llm_prompt(data: &AggregateData) -> String {
    let mut prompt = String::with_capacity(2048);
    prompt.push_str("Mission summary data (extract decisions and write executive summary):\n\n");
    prompt.push_str(&format!("Title: {}\n", data.llm_hint.mission_title));
    prompt.push_str(&format!("Description: {}\n", data.llm_hint.mission_description));
    prompt.push_str(&format!("Status: {}\n", data.llm_hint.status));
    prompt.push_str(&format!(
        "Tasks: {} total, {} completed, {} failed\n",
        data.metrics.tasks_total, data.metrics.tasks_completed, data.metrics.tasks_failed
    ));
    prompt.push_str(&format!(
        "Cost: ${:.4} total ({} input + {} output tokens)\n",
        data.metrics.total_cost_usd,
        data.cost_breakdown.total_input_tokens,
        data.cost_breakdown.total_output_tokens
    ));
    if let Some(avg) = data.metrics.avg_quality_score {
        prompt.push_str(&format!("Average quality score: {:.2}\n", avg));
    }
    prompt.push_str(&format!(
        "Auto-fixes: {} (out of {} total issues)\n",
        data.metrics.auto_fixes, data.evaluator_review.total_issues
    ));

    prompt.push_str("\nTasks:\n");
    if data.llm_hint.agent_summaries.is_empty() {
        prompt.push_str("(no tasks)\n");
    } else {
        for s in &data.llm_hint.agent_summaries {
            prompt.push_str(s);
            prompt.push('\n');
        }
    }

    if !data.llm_hint.failure_reasons.is_empty() {
        prompt.push_str("\nFailure reasons:\n");
        for r in &data.llm_hint.failure_reasons {
            prompt.push_str(r);
            prompt.push('\n');
        }
    }

    if let Some(c) = &data.contract {
        prompt.push_str("\nContract scope/constraints:\n");
        for item in c.items.iter().take(10) {
            if item.section == "scope" || item.section == "constraints" {
                prompt.push_str(&format!("- [{}] {}\n", item.section, item.text));
            }
        }
    }

    prompt.push_str("\nReturn the JSON object now.");
    prompt
}

fn extract_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|b| {
            if let ContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// 健壮 JSON 解析：剥离可能的 ```json``` fence + 截取首个 `{...}`。
fn parse_llm_enhancement(text: &str) -> Result<LlmEnhancement> {
    let cleaned = text.trim();
    let cleaned = cleaned
        .strip_prefix("```json")
        .or_else(|| cleaned.strip_prefix("```"))
        .unwrap_or(cleaned)
        .trim_end_matches("```")
        .trim();

    // 截取第一个 { 到匹配的 }
    let start = cleaned.find('{').context("no JSON object in LLM response")?;
    let json_str = &cleaned[start..];
    // 简单平衡花括号；嵌套深度 >100 直接放弃
    let mut depth = 0i32;
    let mut end = None;
    for (i, ch) in json_str.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.context("unbalanced braces in LLM JSON")?;
    let parsed: LlmEnhancement = serde_json::from_str(&json_str[..end])
        .with_context(|| format!("LLM JSON parse failed; payload was: {}", &json_str[..end]))?;
    Ok(parsed)
}

fn fallback_executive_and_decisions(
    data: &AggregateData,
) -> (String, Vec<ReportDecision>) {
    let m = &data.metrics;
    let executive = format!(
        "Mission「{}」当前状态为 {}。共 {} 个任务，{} 个完成、{} 个失败/取消，\
         总耗时约 {} 秒，总成本 ${:.4}。{}{}",
        data.mission.title,
        data.mission.status,
        m.tasks_total,
        m.tasks_completed,
        m.tasks_failed,
        m.duration_seconds,
        m.total_cost_usd,
        m.avg_quality_score
            .map(|s| format!("Evaluator 平均评分 {:.2}。", s))
            .unwrap_or_default(),
        if m.auto_fixes > 0 {
            format!("其中 {} 个问题已被 auto-fix。", m.auto_fixes)
        } else {
            String::new()
        }
    );

    // 降级 decisions：每个完成的 agent 算一个"任务交付"决策
    let decisions = data
        .task_matrix
        .iter()
        .filter(|t| t.status == "completed")
        .take(5)
        .enumerate()
        .map(|(i, t)| ReportDecision {
            id: format!("D-{}", i + 1),
            title: format!("交付任务：{}", t.title),
            rationale: format!(
                "由 {} 完成，{}",
                t.agent_name.clone().unwrap_or_else(|| "(未分配)".into()),
                t.score
                    .map(|s| format!("Evaluator 评分 {:.1}", s))
                    .unwrap_or_else(|| "暂无 Evaluator 评分".into())
            ),
            trade_off: "(LLM 摘要不可用，未能提取详细 trade-off)".to_string(),
            risk: "(LLM 摘要不可用，未能识别遗留风险)".to_string(),
        })
        .collect();

    (executive, decisions)
}

// ──────────────────────────────────────────────────────────────────────────
// Markdown export
// ──────────────────────────────────────────────────────────────────────────

/// 序列化报告为 Markdown 文本。Slice 3 的 `export_report_markdown` 命令调用此函数。
pub fn render_markdown(report: &MissionReport) -> String {
    let mut md = String::with_capacity(4096);

    md.push_str(&format!("# Mission Report — {}\n\n", escape_md(&report.mission.title)));
    md.push_str(&format!(
        "> Status: **{}** · Duration: {}s · Cost: ${:.4}\n\n",
        report.mission.status,
        report.mission.duration_seconds,
        report.mission.total_cost_usd
    ));

    if !report.mission.description.is_empty() {
        md.push_str(&format!("{}\n\n", escape_md(&report.mission.description)));
    }

    md.push_str("## Executive Summary\n\n");
    md.push_str(&format!("{}\n\n", report.summary.executive));

    md.push_str("### Metrics\n\n");
    md.push_str("| Metric | Value |\n|---|---|\n");
    md.push_str(&format!(
        "| Tasks | {} total / {} completed / {} failed |\n",
        report.summary.metrics.tasks_total,
        report.summary.metrics.tasks_completed,
        report.summary.metrics.tasks_failed
    ));
    md.push_str(&format!(
        "| Duration | {}s |\n",
        report.summary.metrics.duration_seconds
    ));
    md.push_str(&format!(
        "| Cost | ${:.4} |\n",
        report.summary.metrics.total_cost_usd
    ));
    if let Some(s) = report.summary.metrics.avg_quality_score {
        md.push_str(&format!("| Avg Quality Score | {:.2} |\n", s));
    }
    md.push_str(&format!("| Auto-fixes | {} |\n", report.summary.metrics.auto_fixes));
    // P1-2 Phase B：仅在发生过 fallback 时渲染行，避免 0 值给绝大多数 mission 添噪音
    if report.summary.metrics.fallback_switches_total > 0 {
        md.push_str(&format!(
            "| Model fallbacks | {} (upstream overload/rate-limit triggered cross-model switch) |\n",
            report.summary.metrics.fallback_switches_total
        ));
    }
    md.push('\n');

    if !report.decisions.is_empty() {
        md.push_str("## Architecture Decisions\n\n");
        for d in &report.decisions {
            md.push_str(&format!("### {} — {}\n\n", d.id, escape_md(&d.title)));
            md.push_str(&format!("**Rationale**: {}\n\n", escape_md(&d.rationale)));
            if !d.trade_off.is_empty() {
                md.push_str(&format!("**Trade-off**: {}\n\n", escape_md(&d.trade_off)));
            }
            if !d.risk.is_empty() {
                md.push_str(&format!("**Risk**: {}\n\n", escape_md(&d.risk)));
            }
        }
    }

    if !report.evaluator_review.rounds.is_empty() {
        md.push_str("## Evaluator Review\n\n");
        for r in &report.evaluator_review.rounds {
            md.push_str(&format!(
                "- **{}** (agent {}): score {:.1}, {} issues, {} auto-fixed\n",
                escape_md(&r.task_title),
                escape_md(&r.agent_name),
                r.score,
                r.issues,
                r.auto_fixed
            ));
            if !r.summary.is_empty() {
                md.push_str(&format!("  > {}\n", escape_md(&r.summary)));
            }
        }
        md.push('\n');
    }

    if !report.task_matrix.is_empty() {
        md.push_str("## Task Completion Matrix\n\n");
        md.push_str("| Task | Agent | Score | Cost | Status |\n|---|---|---|---|---|\n");
        for t in &report.task_matrix {
            md.push_str(&format!(
                "| {} | {} | {} | ${:.4} | {} |\n",
                escape_md_cell(&t.title),
                escape_md_cell(t.agent_name.as_deref().unwrap_or("-")),
                t.score.map(|s| format!("{:.1}", s)).unwrap_or_else(|| "-".into()),
                t.cost_usd,
                t.status
            ));
        }
        md.push('\n');
    }

    md.push_str("## Cost Breakdown\n\n");
    md.push_str(&format!(
        "Total: ${:.4} ({} input + {} output tokens)\n\n",
        report.cost_breakdown.total_usd,
        report.cost_breakdown.total_input_tokens,
        report.cost_breakdown.total_output_tokens
    ));
    if !report.cost_breakdown.by_model.is_empty() {
        md.push_str("### By Model\n\n");
        md.push_str("| Model | Input | Output | Cost |\n|---|---|---|---|\n");
        for m in &report.cost_breakdown.by_model {
            md.push_str(&format!(
                "| {} | {} | {} | ${:.4} |\n",
                escape_md_cell(&m.model),
                m.input_tokens,
                m.output_tokens,
                m.cost_usd
            ));
        }
        md.push('\n');
    }

    if !report.limitations.is_empty() {
        md.push_str("## Known Limitations\n\n");
        for l in &report.limitations {
            md.push_str(&format!("- {}\n", l));
        }
        md.push('\n');
    }

    if let Some(c) = &report.contract {
        md.push_str("## Contract Compliance\n\n");
        md.push_str(&format!(
            "Contract status: **{}**{}{}\n\n",
            c.status,
            c.budget_usd
                .map(|b| format!(" · Budget ${:.2}", b))
                .unwrap_or_default(),
            c.quality_threshold
                .map(|q| format!(" · Quality threshold {:.1}", q))
                .unwrap_or_default()
        ));
        for item in &c.items {
            let mark = if item.achieved { "✓" } else { "✗" };
            md.push_str(&format!(
                "- {} `[{}]` {}\n",
                mark,
                item.section,
                escape_md(&item.text)
            ));
        }
        md.push('\n');
    }

    if !report.artifacts.is_empty() {
        md.push_str("## Artifacts\n\n");
        for a in &report.artifacts {
            md.push_str(&format!(
                "- **{}** (`{}`): {}\n",
                escape_md(&a.local_name),
                a.artifact_type,
                escape_md(&a.summary)
            ));
            for fp in &a.file_paths {
                md.push_str(&format!("  - `{}`\n", fp));
            }
        }
        md.push('\n');
    }

    md.push_str("---\n");
    md.push_str(&format!(
        "_Generated by Miragenty · schema v{}_\n",
        report.schema_version
    ));

    md
}

fn escape_md(s: &str) -> String {
    // 不破坏正文换行，但转义 backtick 和反斜杠以防代码段意外被打开
    s.replace('\\', "\\\\").replace('`', "\\`")
}

fn escape_md_cell(s: &str) -> String {
    // 表格单元格不能含 |，要替换为转义；并把换行折成空格
    s.replace('|', "\\|").replace('\n', " ")
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) fn aggregate_data_for_test(
    conn: &Connection,
    mission_id: &str,
) -> Result<MissionReport> {
    let agg = aggregate_data(conn, mission_id)?;
    let mut report = agg.into_report();
    let (exec, decisions) = fallback_executive_and_decisions(&aggregate_data(conn, mission_id)?);
    report.summary.executive = exec;
    report.decisions = decisions;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_llm_enhancement_strict_json() {
        let text = r#"{
            "executive_summary": "Did the work.",
            "decisions": [
                {"title": "Use SQLite", "rationale": "Simple", "trade_off": "no concurrent writers", "risk": "low"}
            ]
        }"#;
        let parsed = parse_llm_enhancement(text).unwrap();
        assert_eq!(parsed.executive_summary, "Did the work.");
        assert_eq!(parsed.decisions.len(), 1);
        assert_eq!(parsed.decisions[0].title, "Use SQLite");
    }

    #[test]
    fn parse_llm_enhancement_with_markdown_fence() {
        let text = "```json\n{\"executive_summary\":\"x\",\"decisions\":[]}\n```";
        let parsed = parse_llm_enhancement(text).unwrap();
        assert_eq!(parsed.executive_summary, "x");
        assert!(parsed.decisions.is_empty());
    }

    #[test]
    fn parse_llm_enhancement_with_leading_garbage() {
        let text = "Sure, here it is:\n{\"executive_summary\":\"x\",\"decisions\":[]}\nDone.";
        let parsed = parse_llm_enhancement(text).unwrap();
        assert_eq!(parsed.executive_summary, "x");
    }

    #[test]
    fn parse_llm_enhancement_with_nested_braces() {
        let text = r#"{"executive_summary":"a {nested} brace","decisions":[{"title":"x","rationale":"{y}"}]}"#;
        let parsed = parse_llm_enhancement(text).unwrap();
        assert_eq!(parsed.executive_summary, "a {nested} brace");
        assert_eq!(parsed.decisions.len(), 1);
    }

    #[test]
    fn parse_llm_enhancement_rejects_unbalanced() {
        let text = "{\"executive_summary\":\"x\",\"decisions\":[]";
        assert!(parse_llm_enhancement(text).is_err());
    }

    #[test]
    fn fallback_summary_includes_metrics() {
        let data = make_test_aggregate();
        let (exec, decisions) = fallback_executive_and_decisions(&data);
        assert!(exec.contains("3 个任务"));
        assert!(exec.contains("$0.0500"));
        assert_eq!(decisions.len(), 1, "only 1 completed task → 1 decision");
        assert_eq!(decisions[0].id, "D-1");
        assert!(decisions[0].title.contains("Build login"));
    }

    #[test]
    fn render_markdown_handles_pipes_in_titles() {
        let mut report = make_test_report();
        report.task_matrix[0].title = "Edge | case".to_string();
        let md = render_markdown(&report);
        assert!(md.contains("Edge \\| case"), "pipe in cell must be escaped");
    }

    #[test]
    fn render_markdown_handles_backticks_in_summary() {
        let mut report = make_test_report();
        report.summary.executive = "Fixed `bug` in code".to_string();
        let md = render_markdown(&report);
        // executive 段不经过 escape_md（保留原样以容纳 LLM 富文本），但 description/title 等会
        assert!(md.contains("Executive Summary"));
        assert!(md.contains("Fixed `bug`"));
    }

    #[test]
    fn render_markdown_omits_empty_sections() {
        let mut report = make_test_report();
        report.decisions.clear();
        report.evaluator_review.rounds.clear();
        report.limitations.clear();
        report.contract = None;
        report.artifacts.clear();
        let md = render_markdown(&report);
        assert!(!md.contains("## Architecture Decisions"));
        assert!(!md.contains("## Evaluator Review"));
        assert!(!md.contains("## Known Limitations"));
        assert!(!md.contains("## Contract Compliance"));
        assert!(!md.contains("## Artifacts"));
        assert!(md.contains("## Cost Breakdown"));
    }

    #[test]
    fn derive_limitations_flags_failed_tasks() {
        let mut data = make_test_aggregate();
        data.metrics.tasks_failed = 2;
        let lims = derive_limitations(&data);
        assert!(lims.iter().any(|l| l.contains("2 个任务未完成")));
    }

    #[test]
    fn derive_limitations_flags_budget_overrun() {
        let mut data = make_test_aggregate();
        data.cost_breakdown.budget_used_ratio = Some(0.95);
        let lims = derive_limitations(&data);
        assert!(lims.iter().any(|l| l.contains("成本接近预算上限")));
    }

    #[test]
    fn derive_limitations_flags_quality_below_threshold() {
        let mut data = make_test_aggregate();
        data.metrics.avg_quality_score = Some(5.0);
        data.contract = Some(ReportContract {
            status: "signed".into(),
            items: vec![],
            budget_usd: None,
            quality_threshold: Some(7.0),
            max_duration_hours: None,
        });
        let lims = derive_limitations(&data);
        assert!(lims.iter().any(|l| l.contains("低于 Contract 阈值")));
    }

    fn make_test_aggregate() -> AggregateData {
        AggregateData {
            mission: ReportMission {
                id: "m1".into(),
                title: "Test mission".into(),
                description: "desc".into(),
                status: "completed".into(),
                started_at: "2026-04-29 10:00:00".into(),
                completed_at: Some("2026-04-29 10:30:00".into()),
                duration_seconds: 1800,
                total_cost_usd: 0.05,
                main_branch: Some("main".into()),
            },
            metrics: ReportMetrics {
                tasks_total: 3,
                tasks_completed: 1,
                tasks_failed: 0,
                duration_seconds: 1800,
                total_cost_usd: 0.05,
                avg_quality_score: Some(8.5),
                auto_fixes: 1,
                review_reduction_rate: Some(0.5),
                fallback_switches_total: 0,
            },
            evaluator_review: ReportEvaluatorReview {
                rounds: vec![],
                total_issues: 2,
                auto_fixed: 1,
            },
            task_matrix: vec![ReportTaskRow {
                task_id: "t1".into(),
                title: "Build login".into(),
                agent_id: Some("a1".into()),
                agent_name: Some("Agent-1".into()),
                score: Some(8.5),
                cost_usd: 0.03,
                duration_seconds: Some(900),
                status: "completed".into(),
            }],
            cost_breakdown: ReportCostBreakdown {
                total_usd: 0.05,
                total_input_tokens: 1000,
                total_output_tokens: 500,
                by_model: vec![],
                by_task: vec![],
                by_agent: vec![],
                budget_usd: None,
                budget_used_ratio: None,
            },
            contract: None,
            artifacts: vec![],
            llm_hint: LlmHintData {
                mission_title: "Test mission".into(),
                mission_description: "desc".into(),
                status: "completed".into(),
                agent_summaries: vec![],
                failure_reasons: vec![],
            },
        }
    }

    fn make_test_report() -> MissionReport {
        let agg = make_test_aggregate();
        let mut report = agg.into_report();
        report.summary.executive = "Mission worked.".to_string();
        report.decisions = vec![ReportDecision {
            id: "D-1".into(),
            title: "Use approach X".into(),
            rationale: "It was simpler.".into(),
            trade_off: "Less flexibility.".into(),
            risk: "May not scale.".into(),
        }];
        report
    }

    // ──────────────────────────────────────────────────────────────────
    // Integration tests: aggregate_data on a real in-memory DB
    // ──────────────────────────────────────────────────────────────────

    use crate::db::migrations_run_on;
    use rusqlite::{params, Connection};

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations_run_on(&conn).unwrap();
        conn
    }

    fn seed_completed_mission_with_one_task(conn: &Connection) {
        conn.execute(
            "INSERT INTO missions (id, title, description, status, total_cost_usd, created_at, updated_at)
             VALUES ('m1', 'Build login flow', 'Add OAuth2 login', 'completed', 0.05,
                     '2026-04-29 10:00:00', '2026-04-29 10:30:00')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, description, status, complexity, created_at, completed_at)
             VALUES ('t1', 'm1', 'Implement OAuth2 callback', '', 'completed', 'medium',
                     '2026-04-29 10:05:00', '2026-04-29 10:20:00')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO agents (id, name, task_id, status, worktree_path, created_at, updated_at)
             VALUES ('ag1', 'OAuth Agent', 't1', 'completed', '/tmp/wt-1',
                     '2026-04-29 10:05:00', '2026-04-29 10:20:00')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO cost_records (id, agent_id, task_id, model, input_tokens, output_tokens, cost_usd)
             VALUES ('cr1', 'ag1', 't1', 'claude-sonnet-4-5', 1000, 500, 0.05)",
            [],
        ).unwrap();
    }

    #[test]
    fn aggregate_data_basic_completed_mission() {
        let conn = fresh_db();
        seed_completed_mission_with_one_task(&conn);

        let report = aggregate_data_for_test(&conn, "m1").unwrap();

        assert_eq!(report.mission.id, "m1");
        assert_eq!(report.mission.title, "Build login flow");
        assert_eq!(report.mission.status, "completed");
        assert_eq!(report.mission.duration_seconds, 1800, "10:00 → 10:30 == 1800s");
        assert!((report.mission.total_cost_usd - 0.05).abs() < 1e-9);

        assert_eq!(report.summary.metrics.tasks_total, 1);
        assert_eq!(report.summary.metrics.tasks_completed, 1);
        assert_eq!(report.summary.metrics.tasks_failed, 0);

        assert_eq!(report.task_matrix.len(), 1);
        let row = &report.task_matrix[0];
        assert_eq!(row.task_id, "t1");
        assert_eq!(row.agent_name.as_deref(), Some("OAuth Agent"));
        assert!((row.cost_usd - 0.05).abs() < 1e-9);
        assert_eq!(row.duration_seconds, Some(900), "10:05 → 10:20 == 900s");

        assert_eq!(report.cost_breakdown.by_model.len(), 1);
        assert_eq!(report.cost_breakdown.by_model[0].model, "claude-sonnet-4-5");
        assert_eq!(report.cost_breakdown.total_input_tokens, 1000);
        assert_eq!(report.cost_breakdown.total_output_tokens, 500);

        assert!(report.contract.is_none(), "no contract seeded");
        assert!(report.artifacts.is_empty());

        // 降级 executive 必含关键指标
        assert!(report.summary.executive.contains("1 个任务"));
        assert!(report.summary.executive.contains("$0.0500"));

        // Markdown 渲染端到端跑通
        let md = render_markdown(&report);
        assert!(md.contains("# Mission Report — Build login flow"));
        assert!(md.contains("OAuth Agent"));
        assert!(md.contains("claude-sonnet-4-5"));
        assert!(md.contains("$0.0500"));
    }

    #[test]
    fn aggregate_data_with_evaluator_review() {
        let conn = fresh_db();
        seed_completed_mission_with_one_task(&conn);

        // evaluator_review + 2 个 annotations，1 个 auto_fixed
        conn.execute(
            "INSERT INTO evaluator_reviews (id, agent_id, mission_id, overall_score, summary, created_at)
             VALUES ('er1', 'ag1', 'm1', 8.2, 'Looks good with minor concerns.', '2026-04-29 10:25:00')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO evaluator_annotations
             (id, review_id, agent_id, file_path, line_number, type, severity, status, message, auto_fixable)
             VALUES
             ('a1', 'er1', 'ag1', 'src/login.rs', 10, 'bug', 'warning', 'open', 'check this', 1),
             ('a2', 'er1', 'ag1', 'src/login.rs', 20, 'style', 'info', 'auto_fixed', 'formatted', 1)",
            [],
        ).unwrap();

        let report = aggregate_data_for_test(&conn, "m1").unwrap();

        assert_eq!(report.evaluator_review.rounds.len(), 1);
        assert!((report.evaluator_review.rounds[0].score - 8.2).abs() < 1e-9);
        assert_eq!(report.evaluator_review.total_issues, 2);
        assert_eq!(report.evaluator_review.auto_fixed, 1);
        assert_eq!(
            report.summary.metrics.review_reduction_rate,
            Some(0.5),
            "1 / 2 = 0.5"
        );
        assert_eq!(report.summary.metrics.avg_quality_score, Some(8.2));
        assert_eq!(report.summary.metrics.auto_fixes, 1);

        assert_eq!(report.task_matrix[0].score, Some(8.2));
    }

    #[test]
    fn aggregate_data_with_signed_contract() {
        let conn = fresh_db();
        seed_completed_mission_with_one_task(&conn);
        conn.execute(
            "INSERT INTO mission_contracts (id, mission_id, status, budget_usd, quality_threshold)
             VALUES ('c1', 'm1', 'signed', 1.0, 7.5)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO contract_items (id, contract_id, section, text, source) VALUES
             ('ci1', 'c1', 'scope', 'OAuth2 login flow', 'user'),
             ('ci2', 'c1', 'constraints', 'No third-party SDK', 'user'),
             ('ci3', 'c1', 'exclusions', 'No password reset', 'user')",
            [],
        ).unwrap();

        let report = aggregate_data_for_test(&conn, "m1").unwrap();

        let c = report.contract.expect("contract should be present");
        assert_eq!(c.status, "signed");
        assert_eq!(c.budget_usd, Some(1.0));
        assert_eq!(c.quality_threshold, Some(7.5));
        assert_eq!(c.items.len(), 3);

        // mission completed → scope/constraints achieved=true
        assert!(c.items.iter().find(|i| i.section == "scope").unwrap().achieved);
        assert!(c.items.iter().find(|i| i.section == "constraints").unwrap().achieved);
        // exclusions 默认 true（无需主动验证）
        assert!(c.items.iter().find(|i| i.section == "exclusions").unwrap().achieved);

        assert_eq!(report.cost_breakdown.budget_usd, Some(1.0));
        // budget_used_ratio = 0.05 / 1.0 = 0.05
        let ratio = report.cost_breakdown.budget_used_ratio.unwrap();
        assert!((ratio - 0.05).abs() < 1e-9);
    }

    #[test]
    fn aggregate_data_failed_mission_marks_scope_unachieved() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO missions (id, title, description, status, total_cost_usd, created_at, updated_at)
             VALUES ('m2', 'Failed mission', '', 'failed', 0.01,
                     '2026-04-29 11:00:00', '2026-04-29 11:05:00')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO mission_contracts (id, mission_id, status) VALUES ('c2', 'm2', 'signed')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO contract_items (id, contract_id, section, text, source)
             VALUES ('ci-x', 'c2', 'scope', 'Build the thing', 'user')",
            [],
        ).unwrap();

        let report = aggregate_data_for_test(&conn, "m2").unwrap();
        let scope = report
            .contract
            .as_ref()
            .unwrap()
            .items
            .iter()
            .find(|i| i.section == "scope")
            .unwrap();
        assert!(!scope.achieved, "failed mission → scope unachieved");
    }

    #[test]
    fn aggregate_data_handles_failed_task_in_metrics() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO missions (id, title, status, created_at, updated_at)
             VALUES ('m3', 'Mixed', 'completed', '2026-04-29 09:00:00', '2026-04-29 09:30:00')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, status, created_at) VALUES
             ('t1', 'm3', 'A', 'completed', '2026-04-29 09:05:00'),
             ('t2', 'm3', 'B', 'failed', '2026-04-29 09:10:00'),
             ('t3', 'm3', 'C', 'cancelled', '2026-04-29 09:15:00')",
            [],
        ).unwrap();

        let report = aggregate_data_for_test(&conn, "m3").unwrap();
        assert_eq!(report.summary.metrics.tasks_total, 3);
        assert_eq!(report.summary.metrics.tasks_completed, 1);
        assert_eq!(report.summary.metrics.tasks_failed, 2);
        assert!(report.limitations.iter().any(|l| l.contains("2 个任务未完成")));
    }

    #[test]
    fn aggregate_data_returns_err_for_unknown_mission() {
        let conn = fresh_db();
        let res = aggregate_data_for_test(&conn, "does-not-exist");
        assert!(res.is_err());
        let _ = params![]; // 引用避免 unused-import 警告
    }
}
