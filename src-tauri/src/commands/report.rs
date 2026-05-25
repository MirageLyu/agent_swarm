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
use crate::commands::mission::build_provider;
use crate::db::{queries, Database};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tauri::Manager;
use uuid::Uuid;

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

// ──────────────────────────────────────────────────────────────────────────
// Slice 3: vote + Markdown export
// ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct VoteDecisionRequest {
    pub report_id: String,
    pub decision_id: String,
    /// "agree" | "disagree"
    pub vote: String,
}

#[derive(Debug, Serialize)]
pub struct VoteDecisionResponse {
    pub report_id: String,
    pub decision_id: String,
    pub vote: String,
}

/// FR-04.4: 用户对 Architecture Decision 投票。
/// UNIQUE(report_id, decision_id) 约束保证同一 decision 的多次投票走 UPSERT，
/// 切换 agree↔disagree 也是幂等的。
#[tauri::command]
pub fn vote_decision(
    app: tauri::AppHandle,
    request: VoteDecisionRequest,
) -> Result<VoteDecisionResponse, String> {
    let db = app.state::<Database>();

    // 防御性校验：值在前端 + queries 两层都会做，但提前校验给前端更清晰的错误
    if request.vote != "agree" && request.vote != "disagree" {
        return Err(format!(
            "invalid vote value: '{}' (must be 'agree' or 'disagree')",
            request.vote
        ));
    }

    let vote_id = format!("vote-{}", Uuid::new_v4().simple());

    db.with_conn(|conn| {
        // 校验 report 存在
        if queries::get_mission_report_by_id(conn, &request.report_id)?.is_none() {
            anyhow::bail!("report not found: {}", request.report_id);
        }
        queries::upsert_report_vote(
            conn,
            &vote_id,
            &request.report_id,
            &request.decision_id,
            &request.vote,
        )?;
        Ok(())
    })
    .map_err(|e| e.to_string())?;

    Ok(VoteDecisionResponse {
        report_id: request.report_id,
        decision_id: request.decision_id,
        vote: request.vote,
    })
}

#[derive(Debug, Deserialize)]
pub struct ExportReportMarkdownRequest {
    pub mission_id: String,
    /// 由前端 dialog 选好的绝对路径。后端不弹 dialog，避免线程安全问题
    pub output_path: String,
}

#[derive(Debug, Serialize)]
pub struct ExportReportMarkdownResponse {
    pub bytes_written: u64,
    pub output_path: String,
}

/// FR-11: 序列化报告为 Markdown 写入用户选择的路径。
///
/// 前端流程：
/// 1. 用 `tauri-plugin-dialog` 弹出保存对话框拿到 path
/// 2. 调用本命令，后端读取 mission_reports.report_data → render_markdown → fs::write
///
/// 安全：
/// - 路径必须以 .md 结尾（防止误写覆盖其他文件）
/// - 父目录必须已存在（不自动创建，避免 typo 把文件写到错地方）
/// - 已存在的同名文件直接覆盖（dialog 已经询问过用户）
/// 校验导出路径：必须以 .md 结尾且父目录存在。抽出来便于单元测试。
fn validate_export_path(path: &std::path::Path) -> Result<(), String> {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        != Some("md".to_string())
    {
        return Err("output_path must end with .md".to_string());
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(format!(
                "parent directory does not exist: {}",
                parent.display()
            ));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn export_report_markdown(
    app: tauri::AppHandle,
    request: ExportReportMarkdownRequest,
) -> Result<ExportReportMarkdownResponse, String> {
    let path = PathBuf::from(&request.output_path);
    validate_export_path(&path)?;

    let db = app.state::<Database>();
    let report: MissionReport = db
        .with_conn(|conn| {
            let row = queries::get_mission_report_by_mission(conn, &request.mission_id)?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no report found for mission '{}'; generate it first",
                        request.mission_id
                    )
                })?;
            serde_json::from_str(&row.report_data)
                .map_err(|e| anyhow::anyhow!("corrupt report data: {}", e))
        })
        .map_err(|e| e.to_string())?;

    let markdown = report_generator::render_markdown(&report);
    let bytes = markdown.len() as u64;

    std::fs::write(&path, markdown.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", path.display(), e))?;

    Ok(ExportReportMarkdownResponse {
        bytes_written: bytes,
        output_path: request.output_path,
    })
}

#[cfg(test)]
mod tests {
    use super::validate_export_path;
    use std::path::PathBuf;

    #[test]
    fn validate_export_path_accepts_md_in_existing_dir() {
        let tmp = std::env::temp_dir();
        let p = tmp.join("miragenty_report.md");
        assert!(validate_export_path(&p).is_ok());
    }

    #[test]
    fn validate_export_path_rejects_non_md_extension() {
        let tmp = std::env::temp_dir();
        let p = tmp.join("report.txt");
        let err = validate_export_path(&p).unwrap_err();
        assert!(
            err.contains(".md"),
            "error should mention required extension: {}",
            err
        );
    }

    #[test]
    fn validate_export_path_rejects_no_extension() {
        let tmp = std::env::temp_dir();
        let p = tmp.join("report");
        assert!(validate_export_path(&p).is_err());
    }

    #[test]
    fn validate_export_path_rejects_nonexistent_parent() {
        let p = PathBuf::from("/tmp/miragenty_definitely_does_not_exist_xyz_abc/r.md");
        let err = validate_export_path(&p).unwrap_err();
        assert!(err.contains("parent directory"), "got: {}", err);
    }

    #[test]
    fn validate_export_path_accepts_uppercase_md() {
        let tmp = std::env::temp_dir();
        let p = tmp.join("report.MD");
        assert!(
            validate_export_path(&p).is_ok(),
            "case-insensitive extension check"
        );
    }
}
