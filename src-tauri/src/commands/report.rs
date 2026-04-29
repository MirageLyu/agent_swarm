//! FM-12 Mission Report IPC commands.
//!
//! 暴露给前端的 4 个命令：
//! - `generate_mission_report`: 异步生成（含 LLM 增强），覆盖式入库
//! - `get_mission_report`: 读取已生成的报告（前端进入 ReportView 时调用）
//! - `vote_decision`: 对 Architecture Decision 投 agree/disagree（在 Slice 3 同文件追加）
//! - `export_report_markdown`: 序列化为 Markdown 写入用户选择的路径（Slice 3）
//!
//! 设计要点：
//! - generate 命令包了一层 60s 兜底超时（report_generator 内部已对 LLM 设了 30s，
//!   但加查 DB / 序列化 / upsert 的总时长仍可能拖长，外层兜底防止 IPC 卡死）。
//! - 不强制 mission 处于 completed 状态：用户也可以在 running 中预览部分报告。
//!   只是 "completed" 之外的 mission 可能 task_matrix 不全、cost 还在累加。

use crate::agent::report_generator::{self, MissionReport};
use crate::db::{queries, Database};
use crate::commands::mission::build_provider;
use serde::Serialize;
use std::time::Duration;
use tauri::Manager;

#[derive(Debug, Serialize, Clone)]
pub struct GenerateMissionReportResponse {
    pub report_id: String,
    pub generated_at: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct GetMissionReportResponse {
    pub report_id: String,
    pub mission_id: String,
    pub generated_at: String,
    pub schema_version: i64,
    pub report: MissionReport,
    /// 当前 mission 已存在的投票，按 decision_id 索引
    pub votes: Vec<DecisionVoteView>,
}

#[derive(Debug, Serialize, Clone)]
pub struct DecisionVoteView {
    pub decision_id: String,
    pub vote: String,
}

#[tauri::command]
pub async fn generate_mission_report(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<GenerateMissionReportResponse, String> {
    // 取 LLM provider；若用户没配 API key 则走降级模板（不报错）
    let llm = build_provider(&app).ok();
    let (provider, model) = match llm {
        Some((p, m)) => (Some(p), Some(m)),
        None => (None, None),
    };

    let result = tokio::time::timeout(
        Duration::from_secs(60),
        report_generator::generate_mission_report(&app, &mission_id, provider, model),
    )
    .await;

    match result {
        Ok(Ok((report_id, generated_at))) => Ok(GenerateMissionReportResponse {
            report_id,
            generated_at,
        }),
        Ok(Err(e)) => Err(format!("Failed to generate report: {}", e)),
        Err(_) => Err("Report generation timed out (>60s)".to_string()),
    }
}

#[tauri::command]
pub fn get_mission_report(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<Option<GetMissionReportResponse>, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let row = queries::get_mission_report_by_mission(conn, &mission_id)?;
        let Some(row) = row else {
            return Ok(None);
        };

        let report: MissionReport = serde_json::from_str(&row.report_data)
            .map_err(|e| anyhow::anyhow!("corrupt report data: {}", e))?;

        let votes = queries::list_report_votes(conn, &row.id)?
            .into_iter()
            .map(|v| DecisionVoteView {
                decision_id: v.decision_id,
                vote: v.vote,
            })
            .collect();

        Ok(Some(GetMissionReportResponse {
            report_id: row.id,
            mission_id: row.mission_id,
            generated_at: row.generated_at,
            schema_version: row.schema_version,
            report,
            votes,
        }))
    })
    .map_err(|e| e.to_string())
}
