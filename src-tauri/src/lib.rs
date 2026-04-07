pub mod agent;
pub mod commands;
pub mod db;
pub mod git;
pub mod llm;
pub mod tools;

use agent::{AgentRegistry, Scheduler};
use commands::ConfigManager;
use db::Database;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "miragenty=debug,info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to resolve app data dir");

            let database = Database::open(&data_dir)?;
            app.manage(database);

            let config_manager = ConfigManager::load(&data_dir);
            app.manage(config_manager);

            app.manage(AgentRegistry::new());
            app.manage(Scheduler::new());

            tracing::info!("Miragenty initialized, data_dir: {}", data_dir.display());

            #[cfg(debug_assertions)]
            if let Some(window) = app.get_webview_window("main") {
                window.open_devtools();
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_info,
            commands::get_db_status,
            commands::create_mission,
            commands::list_missions,
            commands::plan_mission,
            commands::get_mission_detail,
            commands::update_task,
            commands::delete_task,
            commands::add_task,
            commands::confirm_mission,
            commands::delete_mission,
            commands::get_config,
            commands::set_api_key,
            commands::update_config,
            commands::run_agent,
            commands::stop_agent,
            commands::get_agent_events,
            commands::get_agent_detail,
            commands::list_agents,
            commands::start_mission_execution,
            commands::get_scheduler_status,
            commands::list_agents_by_mission,
            commands::get_default_workspace_path,
            commands::list_agent_events,
            commands::get_mission_cost_summary,
            commands::get_agent_diff,
            commands::submit_review_action,
            commands::inject_agent_note,
            commands::list_agent_notes,
            commands::inject_mission_note,
            commands::list_mission_notes,
            commands::stop_mission_execution,
            commands::restart_mission,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
