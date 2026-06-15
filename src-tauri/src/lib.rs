pub mod agent;
pub mod benchmark;
pub mod commands;
pub mod db;
pub mod error_code;
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
use std::path::Path;
use tauri::Manager;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// 日志子系统初始化。
///
/// - stdout layer：开发期 `cargo tauri dev` 直接看输出
/// - file layer：生产期落盘到 `<data_dir>/logs/miragenty.log.<rotation-suffix>`
///   按天滚动，旧文件由用户/OS 自行清理（macOS Finder/Windows 资源管理器都能定位）
/// - WorkerGuard 必须 `mem::forget` 到全局，否则它 drop 时关闭文件句柄，
///   后续 `tracing::*` 调用会静默失败。这里牺牲一个 guard 的生命周期换日志可靠性
///
/// 容错：
/// - 创建日志目录失败（权限、磁盘满）：fallback 到 stdout-only，不阻塞 app 启动
/// - tracing 全局 subscriber 已被设置（极端情况，如重复 init）：忽略错误
fn init_logging(data_dir: &Path) {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("miragenty=debug,info"));

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_writer(std::io::stdout);

    let logs_dir = data_dir.join("logs");
    let mut file_layer_opt = None;

    match std::fs::create_dir_all(&logs_dir) {
        Ok(()) => {
            // daily rotation；日志文件名 = miragenty.log.YYYY-MM-DD
            let appender = tracing_appender::rolling::daily(&logs_dir, "miragenty.log");
            // non_blocking 防止文件 IO 阻塞 agent 主循环
            let (non_blocking, guard) = tracing_appender::non_blocking(appender);
            // guard 必须活到进程退出；这里 leak 是可接受的（一次性，~bytes 大小）
            std::mem::forget(guard);
            let file_layer = tracing_subscriber::fmt::layer()
                .with_ansi(false) // 文件里不要 ANSI 转义码
                .with_target(true)
                .with_writer(non_blocking);
            file_layer_opt = Some(file_layer);
        }
        Err(e) => {
            // 还没初始化 tracing 时不能用 tracing::warn!；用 eprintln 兜底
            eprintln!(
                "[init_logging] failed to create logs dir {}: {} (continuing with stdout only)",
                logs_dir.display(),
                e
            );
        }
    }

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer);

    let result = match file_layer_opt {
        Some(file_layer) => registry.with(file_layer).try_init(),
        None => registry.try_init(),
    };

    if let Err(e) = result {
        eprintln!(
            "[init_logging] global subscriber already set, skipping: {}",
            e
        );
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to resolve app data dir");

            // 日志在 data_dir 已知后才初始化。设计要点：
            // - stdout layer：开发期 cargo tauri dev / pnpm tauri dev 看得见
            // - file layer (rolling daily)：生产期用户机器上落盘到 logs/miragenty.log.YYYY-MM-DD
            //   保留 7 天。后续 export_diagnostics 命令读这个文件
            // - guard 必须 leak 到全局，否则 drop 时会 flush 然后关闭文件，再写就静默失败
            init_logging(&data_dir);

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
                    let expired_ids =
                        match db.with_conn(|conn| queries::expire_overdue_approvals(conn)) {
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
            commands::test_llm_connection,
            commands::test_tool_summarizer_connection,
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
            commands::list_agent_todos,
            commands::submit_user_question_answer,
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
            commands::get_mission_delivery,
            commands::generate_mission_delivery,
            commands::confirm_planner_fetch,
            commands::start_preflight,
            commands::send_preflight_message,
            commands::retry_preflight_message,
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
            commands::open_log_directory,
            commands::list_chat_messages,
            commands::send_chat_message,
            commands::confirm_followup_proposal,
            commands::reject_followup_proposal,
            // FM-14: Approval Queue
            commands::list_pending_approvals,
            commands::get_approval,
            commands::resolve_approval,
            commands::resolve_all_approvals,
            commands::get_approval_policy,
            commands::update_approval_policy,
            // FM-12: Mission Report
            commands::generate_mission_report,
            commands::get_mission_report,
            commands::vote_decision,
            commands::export_report_markdown,
            // MVP polish: diagnostic export
            commands::export_diagnostics,
            // FM-13 lite: insights / anomaly detection
            commands::get_cost_trend,
            commands::get_anomalies,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
