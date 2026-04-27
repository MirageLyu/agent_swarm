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

// ----------------------------------------------------------------------------
// FM-15 v2.2 P4-S4: 让前端 MissionDeliveryPanel 一键打开 mission 工作区。
//
// 三个命令：
//   - open_in_editor: 调用系统默认编辑器或 VS Code
//   - open_in_terminal: 在 mission 工作区打开新的终端窗口
//   - open_in_finder: 在 Finder/Explorer 中显示 mission 工作区目录
//
// 平台支持：
//   - macOS: open / open -a Terminal / open（Finder）
//   - Linux: xdg-open
//   - Windows: start / explorer
// ----------------------------------------------------------------------------

use std::path::PathBuf;
use std::process::Command;

fn validate_path(path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    if !p.exists() {
        return Err(format!("Path does not exist: {path}"));
    }
    if !p.is_dir() {
        return Err(format!("Path is not a directory: {path}"));
    }
    Ok(p)
}

#[tauri::command]
pub fn open_in_editor(path: String, editor: Option<String>) -> Result<(), String> {
    let p = validate_path(&path)?;
    let p_str = p.to_string_lossy().to_string();

    // 优先用调用方指定的编辑器（前端 setting 可填 "code", "subl" 等）
    if let Some(editor_cmd) = editor.filter(|s| !s.is_empty()) {
        return Command::new(&editor_cmd)
            .arg(&p_str)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to spawn editor `{editor_cmd}`: {e}"));
    }

    // 尝试 VS Code (`code` 在 PATH 中) → 失败则用系统默认 (open / xdg-open / start)
    #[cfg(target_os = "macos")]
    {
        if Command::new("code").arg(&p_str).spawn().is_ok() {
            return Ok(());
        }
        Command::new("open")
            .arg(&p_str)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to spawn `open`: {e}"))
    }
    #[cfg(target_os = "linux")]
    {
        if Command::new("code").arg(&p_str).spawn().is_ok() {
            return Ok(());
        }
        Command::new("xdg-open")
            .arg(&p_str)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to spawn `xdg-open`: {e}"))
    }
    #[cfg(target_os = "windows")]
    {
        if Command::new("code").arg(&p_str).spawn().is_ok() {
            return Ok(());
        }
        Command::new("cmd")
            .args(["/C", "start", "", &p_str])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to spawn editor: {e}"))
    }
}

#[tauri::command]
pub fn open_in_terminal(path: String) -> Result<(), String> {
    let p = validate_path(&path)?;
    let p_str = p.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .args(["-a", "Terminal"])
            .arg(&p_str)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to open Terminal: {e}"))
    }
    #[cfg(target_os = "linux")]
    {
        // 尝试常见终端
        for term in ["x-terminal-emulator", "gnome-terminal", "konsole", "xterm"] {
            if let Ok(_) = Command::new(term)
                .args(["--working-directory", &p_str])
                .spawn()
            {
                return Ok(());
            }
        }
        Err("No supported terminal emulator found".into())
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "cmd", "/K", &format!("cd /d {}", p_str)])
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to open terminal: {e}"))
    }
}

#[tauri::command]
pub fn open_in_finder(path: String) -> Result<(), String> {
    let p = validate_path(&path)?;
    let p_str = p.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&p_str)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to open Finder: {e}"))
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(&p_str)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to open file manager: {e}"))
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(&p_str)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Failed to open Explorer: {e}"))
    }
}
