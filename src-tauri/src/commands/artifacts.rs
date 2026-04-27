//! FM-15 FR-03: Artifact 列表 IPC。
//!
//! 当前阶段（Phase 1 / S3-3）只暴露读取能力——前端 DAG 边/节点
//! 渲染 artifact badge 需要这些数据。`publish_artifact` 工具的写入
//! 路径由 Coding Agent 在 runtime 调用，走 `agent/artifacts.rs` 内部 API，
//! 不需要 IPC。

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::queries;
use crate::db::Database;

#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactDto {
    pub id: String,
    pub mission_id: String,
    pub producer_task_id: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub local_name: String,
    pub summary: String,
    pub file_paths: Vec<String>,
    pub published: bool,
    pub created_at: String,
}

impl From<queries::ArtifactRow> for ArtifactDto {
    fn from(row: queries::ArtifactRow) -> Self {
        let file_paths: Vec<String> = serde_json::from_str(&row.file_paths).unwrap_or_default();
        Self {
            id: row.id,
            mission_id: row.mission_id,
            producer_task_id: row.producer_task_id,
            artifact_type: row.artifact_type,
            local_name: row.local_name,
            summary: row.summary,
            file_paths,
            published: row.published,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListArtifactsResponse {
    pub artifacts: Vec<ArtifactDto>,
}

#[tauri::command]
pub fn list_mission_artifacts(
    db: State<'_, Database>,
    mission_id: String,
) -> Result<ListArtifactsResponse, String> {
    let rows = db
        .with_conn(|conn| queries::list_artifacts_for_mission(conn, &mission_id))
        .map_err(|e| e.to_string())?;
    Ok(ListArtifactsResponse {
        artifacts: rows.into_iter().map(ArtifactDto::from).collect(),
    })
}

#[tauri::command]
pub fn list_task_artifacts(
    db: State<'_, Database>,
    task_id: String,
) -> Result<ListArtifactsResponse, String> {
    let rows = db
        .with_conn(|conn| queries::list_artifacts_for_task(conn, &task_id))
        .map_err(|e| e.to_string())?;
    Ok(ListArtifactsResponse {
        artifacts: rows.into_iter().map(ArtifactDto::from).collect(),
    })
}
