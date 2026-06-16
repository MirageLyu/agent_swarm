use crate::agent::delivery::{generate_degraded_delivery_snapshot, MissionDeliverySnapshot};
use crate::db::{queries, Database};
use serde::Serialize;
use tauri::Manager;

#[derive(Debug, Serialize, Clone)]
pub struct MissionDeliveryView {
    pub mission_id: String,
    pub version: i64,
    pub generation_status: String,
    pub curator_model: Option<String>,
    pub source_task_ids: String,
    pub source_event_ids: String,
    pub stale: bool,
    pub created_at: String,
    pub updated_at: String,
    pub snapshot: MissionDeliverySnapshot,
}

#[derive(Debug, Serialize, Clone)]
pub struct GenerateMissionDeliveryResponse {
    pub mission_id: String,
    pub generation_status: String,
    pub delivery: MissionDeliveryView,
}

#[tauri::command]
pub fn get_mission_delivery(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<Option<MissionDeliveryView>, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| {
        let Some(row) = queries::get_mission_delivery(conn, &mission_id)? else {
            return Ok(None);
        };
        mission_delivery_view_from_row(row).map(Some)
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn generate_mission_delivery(
    app: tauri::AppHandle,
    mission_id: String,
) -> Result<GenerateMissionDeliveryResponse, String> {
    let db = app.state::<Database>();
    let generation = db
        .with_conn(|conn| generate_degraded_delivery_snapshot(conn, &mission_id))
        .map_err(|e| e.to_string())?;

    let (snapshot, generation_status, curator_model) =
        match crate::commands::mission::build_provider(&app) {
            Ok((provider, model)) => {
                let curated = crate::agent::delivery::curate_delivery_with_llm(
                    provider,
                    &model,
                    generation.snapshot.clone(),
                )
                .await;
                (curated, "generated", Some(model))
            }
            Err(err) => {
                tracing::warn!(
                    mission_id = %mission_id,
                    error = %err,
                    "delivery curator provider unavailable; using degraded snapshot"
                );
                (
                    generation.snapshot,
                    "degraded",
                    Some("deterministic".to_string()),
                )
            }
        };

    db.with_conn(|conn| {
        crate::agent::delivery::persist_delivery_snapshot(
            conn,
            &snapshot,
            generation_status,
            curator_model.as_deref(),
            &generation.source_task_ids,
        )
    })
    .map_err(|e| e.to_string())?;

    db.with_conn(|conn| {
        let row = queries::get_mission_delivery(conn, &mission_id)?.ok_or_else(|| {
            anyhow::anyhow!("delivery snapshot was not persisted for mission: {mission_id}")
        })?;
        let generation_status = row.generation_status.clone();
        let delivery = mission_delivery_view_from_row(row)?;
        Ok(GenerateMissionDeliveryResponse {
            mission_id,
            generation_status,
            delivery,
        })
    })
    .map_err(|e| e.to_string())
}

fn mission_delivery_view_from_row(
    row: queries::MissionDeliveryRow,
) -> anyhow::Result<MissionDeliveryView> {
    let snapshot: MissionDeliverySnapshot = serde_json::from_str(&row.snapshot_json)
        .map_err(|e| anyhow::anyhow!("corrupt mission delivery snapshot: {e}"))?;
    Ok(MissionDeliveryView {
        mission_id: row.mission_id,
        version: row.version,
        generation_status: row.generation_status,
        curator_model: row.curator_model,
        source_task_ids: row.source_task_ids,
        source_event_ids: row.source_event_ids,
        stale: row.stale,
        created_at: row.created_at,
        updated_at: row.updated_at,
        snapshot,
    })
}
