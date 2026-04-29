pub mod agent;
pub mod commands;
pub mod db;
pub mod git;
pub mod llm;
pub mod mission_template;
pub mod skills;
pub mod tools;

use agent::approval::ApprovalCoordinator;
use agent::planner_fetch::PlannerFetchCoordinator;
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

            // FM-15 v2.2 (S3-4): Planner fetch_url 用户确认协调器
            app.manage(PlannerFetchCoordinator::new());

            // FM-14: 统一审批协调器 + 后台过期清理任务（每 30s 扫一次）。
            //   后续 slice 会把 PlannerFetchCoordinator / chat propose 转调到这里，
            //   现在先把基础设施立起来。
            let approval_coord = ApprovalCoordinator::new();
            app.manage(approval_coord.clone());
            let approval_app_handle = app.handle().clone();
            let approval_coord_for_task = approval_coord.clone();
            tauri::async_runtime::spawn(async move {
                use crate::db::queries;
                use tauri::Emitter;
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    let db = approval_app_handle.state::<Database>();
                    let expired_ids = match db.with_conn(|conn| queries::expire_overdue_approvals(conn)) {
                        Ok(ids) => ids,
                        Err(e) => {
                            tracing::warn!("[approval] expire sweep failed: {e}");
                            continue;
                        }
                    };
                    for id in expired_ids {
                        approval_coord_for_task.forget(&id).await;
                        let _ = approval_app_handle.emit(
                            "approval-resolved",
                            serde_json::json!({
                                "request_id": id,
                                "status": "expired",
                            }),
                        );
                    }
                }
            });

            // FM-15 v2.2 (S3-2): 启动时初始化 skill 全局注册表（builtin + 用户级 SKILL.md）。
            // 项目级 skill 在 plan/dispatch 时基于 mission.repo_path 临时叠加，避免串扰。
            let skill_count = crate::skills::registry::init_global().all().len();
            tracing::info!("[skills] global registry loaded {skill_count} skills");

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
            commands::set_task_dependencies,
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
            commands::export_mission_template,
            commands::import_mission_template,
            commands::get_planner_session,
            commands::list_planner_steps,
            commands::list_skills,
            commands::list_mission_artifacts,
            commands::list_task_artifacts,
            commands::confirm_planner_fetch,
            commands::start_preflight,
            commands::send_preflight_message,
            commands::add_contract_item,
            commands::remove_contract_item,
            commands::update_contract_config,
            commands::get_contract,
            commands::get_preflight_session,
            commands::get_decision_log,
            commands::sign_contract,
            commands::trigger_evaluation,
            commands::get_evaluation_result,
            commands::get_annotations,
            commands::update_annotation_status,
            commands::list_merge_records,
            commands::list_task_base_conflicts,
            commands::open_in_editor,
            commands::open_in_terminal,
            commands::open_in_finder,
            commands::list_chat_messages,
            commands::send_chat_message,
            commands::confirm_followup_proposal,
            commands::reject_followup_proposal,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
