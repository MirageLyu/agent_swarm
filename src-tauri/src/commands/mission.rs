use serde::{Deserialize, Serialize};
use tauri::Manager;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct MissionInfo {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub total_cost_usd: f64,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateMissionRequest {
    pub title: String,
    pub description: String,
}

#[tauri::command]
pub fn create_mission(
    app: tauri::AppHandle,
    request: CreateMissionRequest,
) -> Result<MissionInfo, String> {
    let db = app.state::<crate::db::Database>();
    let id = Uuid::new_v4().to_string();

    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO missions (id, title, description) VALUES (?, ?, ?)",
            rusqlite::params![id, request.title, request.description],
        )?;

        let mission = conn.query_row(
            "SELECT id, title, description, status, total_cost_usd, created_at FROM missions WHERE id = ?",
            [&id],
            |row| {
                Ok(MissionInfo {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    description: row.get(2)?,
                    status: row.get(3)?,
                    total_cost_usd: row.get(4)?,
                    created_at: row.get(5)?,
                })
            },
        )?;

        Ok(mission)
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_missions(app: tauri::AppHandle) -> Result<Vec<MissionInfo>, String> {
    let db = app.state::<crate::db::Database>();

    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, title, description, status, total_cost_usd, created_at FROM missions ORDER BY created_at DESC",
        )?;

        let missions = stmt
            .query_map([], |row| {
                Ok(MissionInfo {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    description: row.get(2)?,
                    status: row.get(3)?,
                    total_cost_usd: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(missions)
    })
    .map_err(|e| e.to_string())
}
