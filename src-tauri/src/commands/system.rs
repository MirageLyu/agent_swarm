use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub version: String,
    pub data_dir: String,
}

#[tauri::command]
pub fn get_app_info(app: tauri::AppHandle) -> Result<AppInfo, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;

    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        data_dir: data_dir.to_string_lossy().to_string(),
    })
}

use tauri::Manager;

#[tauri::command]
pub fn get_db_status(app: tauri::AppHandle) -> Result<String, String> {
    let db = app.state::<crate::db::Database>();
    db.with_conn(|conn| {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| row.get(0))
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(format!("{count} migrations applied"))
    })
    .map_err(|e| e.to_string())
}
