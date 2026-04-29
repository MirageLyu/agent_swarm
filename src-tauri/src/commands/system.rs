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

// ----------------------------------------------------------------------------
// Diagnostic export (P1 MVP polish)
//
// 一键把"够用来定位崩溃的最小信息集"打包成一个文本文件，让用户能附在 issue 里。
//
// 收集内容：
//   - app version + tauri/rust 版本（compile-time 确定）
//   - data_dir 路径
//   - schema_migrations 数（验证 DB 完整）
//   - missions 计数 + 状态分布
//   - 最近 N 行日志（默认 2000）
//
// 脱敏：
//   - API key（grep 'sk-...' / 'sk-ant-...' 等前缀，整段替换为 `<redacted-api-key>`）
//   - file:// 路径里的用户名（macOS / Linux 都把 home 下的 username 替换为 `<user>`）
//
// 设计权衡：
//   - 不收集源代码 / mission description / chat 内容（隐私）
//   - 不收集完整 SQLite 文件（太大且含敏感）
//   - 用户负责选择保存路径，后端不弹 dialog
// ----------------------------------------------------------------------------

use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct ExportDiagnosticsRequest {
    pub output_path: String,
    /// 默认 2000 行；最多 10000 防止报告过大
    pub log_tail_lines: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ExportDiagnosticsResponse {
    pub bytes_written: u64,
    pub output_path: String,
    pub log_files_included: usize,
}

const MAX_LOG_TAIL: usize = 10_000;

/// 简单脱敏：替换常见 API key 前缀和 home 路径
fn redact(line: &str) -> String {
    use std::sync::OnceLock;
    static HOME_USERNAME: OnceLock<Option<String>> = OnceLock::new();

    let mut out = line.to_string();

    // API key 模式：sk-ant-xxx / sk-xxx / 形如 32-128 长度的连续 [A-Za-z0-9_-]
    // 用简单正则替换；不依赖 regex crate（要的话再加），这里用启发式扫描
    out = redact_api_keys_simple(&out);

    // home 用户名脱敏
    let username = HOME_USERNAME.get_or_init(|| {
        std::env::var("USER").ok().or_else(|| std::env::var("USERNAME").ok())
    });
    if let Some(user) = username.as_deref() {
        if !user.is_empty() && user != "root" {
            out = out.replace(user, "<user>");
        }
    }

    out
}

fn redact_api_keys_simple(line: &str) -> String {
    // 常见前缀 → 整段（到下一个空白/引号/逗号/分号）替换。
    // 算法：每次循环找出**最早出现的任意 prefix**，而非按 PREFIXES 顺序优先匹配；
    // 否则一行内含多个不同 prefix 的 key 时，靠后但更具体的 prefix 会"吞掉"靠前的。
    const PREFIXES: &[&str] = &["sk-ant-", "sk-or-", "sk-proj-", "sk-"];
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        // 找当前位置之后最早出现的任意 prefix
        let mut earliest: Option<(usize, &str)> = None;
        for prefix in PREFIXES {
            if let Some(idx) = rest.find(prefix) {
                match earliest {
                    None => earliest = Some((idx, prefix)),
                    Some((cur_idx, _)) if idx < cur_idx => earliest = Some((idx, prefix)),
                    // 同位置时，更长的 prefix（更具体的 sk-ant- 等）优先
                    Some((cur_idx, cur_prefix)) if idx == cur_idx && prefix.len() > cur_prefix.len() => {
                        earliest = Some((idx, prefix))
                    }
                    _ => {}
                }
            }
        }

        match earliest {
            Some((idx, prefix)) => {
                out.push_str(&rest[..idx]);
                let after = &rest[idx + prefix.len()..];
                let end = after
                    .find(|c: char| c.is_whitespace() || c == '"' || c == ',' || c == ';' || c == '\'')
                    .unwrap_or(after.len());
                if end >= 12 {
                    out.push_str("<redacted-api-key>");
                    rest = &after[end..];
                } else {
                    // 长度不够，保留 prefix 原文，但跳过 prefix 避免死循环
                    out.push_str(prefix);
                    rest = after;
                }
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

#[tauri::command]
pub fn export_diagnostics(
    app: tauri::AppHandle,
    request: ExportDiagnosticsRequest,
) -> Result<ExportDiagnosticsResponse, String> {
    let tail_lines = request
        .log_tail_lines
        .unwrap_or(2000)
        .min(MAX_LOG_TAIL);

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let logs_dir = data_dir.join("logs");

    let mut buf = String::with_capacity(64 * 1024);
    buf.push_str("# Miragenty Diagnostic Bundle\n\n");
    buf.push_str(&format!("Generated: {}\n", chrono::Utc::now().to_rfc3339()));
    buf.push_str(&format!(
        "App version: {}\n",
        env!("CARGO_PKG_VERSION")
    ));
    buf.push_str(&format!("Target: {} {}\n", std::env::consts::OS, std::env::consts::ARCH));
    buf.push_str(&format!(
        "Data dir: {}\n",
        redact(&data_dir.to_string_lossy())
    ));

    // ── DB 状态
    let db = app.state::<crate::db::Database>();
    let db_summary = db
        .with_conn(|conn| {
            let migrations: i64 = conn
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
                .unwrap_or(-1);
            let missions: i64 = conn
                .query_row("SELECT COUNT(*) FROM missions", [], |r| r.get(0))
                .unwrap_or(-1);
            let by_status: Vec<(String, i64)> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT status, COUNT(*) FROM missions GROUP BY status ORDER BY status",
                    )
                    .ok();
                if let Some(s) = stmt.as_mut() {
                    s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                        .ok()
                        .map(|it| it.filter_map(|x| x.ok()).collect())
                        .unwrap_or_default()
                } else {
                    vec![]
                }
            };
            Ok::<_, anyhow::Error>(format!(
                "Migrations applied: {migrations}\nMissions: {missions}\nBy status: {by_status:?}\n"
            ))
        })
        .unwrap_or_else(|e| format!("(db query failed: {e})\n"));
    buf.push_str("\n## Database\n\n");
    buf.push_str(&db_summary);

    // ── 日志 tail
    buf.push_str("\n## Recent Logs\n\n");
    let mut log_files_included = 0usize;
    match collect_recent_log_lines(&logs_dir, tail_lines) {
        Ok((lines, files)) => {
            log_files_included = files;
            buf.push_str(&format!(
                "(showing last {} lines from {} log file(s) in {})\n\n",
                lines.len(),
                files,
                redact(&logs_dir.to_string_lossy())
            ));
            buf.push_str("```\n");
            for line in lines {
                buf.push_str(&redact(&line));
                buf.push('\n');
            }
            buf.push_str("```\n");
        }
        Err(e) => {
            buf.push_str(&format!("(failed to read logs from {}: {})\n", logs_dir.display(), e));
        }
    }

    // ── 写入用户选定路径
    let path = std::path::PathBuf::from(&request.output_path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(format!(
                "parent directory does not exist: {}",
                parent.display()
            ));
        }
    }

    std::fs::write(&path, buf.as_bytes())
        .map_err(|e| format!("failed to write diagnostic bundle to {}: {}", path.display(), e))?;

    Ok(ExportDiagnosticsResponse {
        bytes_written: buf.len() as u64,
        output_path: request.output_path,
        log_files_included,
    })
}

/// 读取 logs_dir 下最新 1-2 个滚动文件的最后 N 行。
/// 按文件名字典序倒序（rolling daily 文件名是 `miragenty.log.YYYY-MM-DD`）。
fn collect_recent_log_lines(
    logs_dir: &Path,
    tail_lines: usize,
) -> std::io::Result<(Vec<String>, usize)> {
    if !logs_dir.exists() {
        return Ok((vec![], 0));
    }
    let mut files: Vec<_> = std::fs::read_dir(logs_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("miragenty.log")
        })
        .collect();
    files.sort_by_key(|e| std::cmp::Reverse(e.file_name()));

    let mut all_lines: Vec<String> = Vec::new();
    let mut files_used = 0usize;
    // 从最新文件开始读，直到累计到 tail_lines
    for entry in files {
        let path = entry.path();
        let f = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let lines: Vec<String> = BufReader::new(f)
            .lines()
            .filter_map(|l| l.ok())
            .collect();
        // 反转后再 append，方便最后再翻回来
        let mut rev: Vec<String> = lines.into_iter().rev().collect();
        all_lines.append(&mut rev);
        files_used += 1;
        if all_lines.len() >= tail_lines {
            break;
        }
    }
    all_lines.truncate(tail_lines);
    all_lines.reverse();
    Ok((all_lines, files_used))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_api_keys_strips_anthropic_prefix() {
        let input = "Authorization: Bearer sk-ant-abc1234567890XYZ_more";
        let out = redact_api_keys_simple(input);
        assert!(out.contains("<redacted-api-key>"));
        assert!(!out.contains("abc1234567890XYZ_more"));
    }

    #[test]
    fn redact_api_keys_handles_quoted() {
        let input = r#"{"key":"sk-1234567890abcdef"}"#;
        let out = redact_api_keys_simple(input);
        assert!(out.contains("<redacted-api-key>"));
        assert!(out.starts_with("{\"key\":\""));
    }

    #[test]
    fn redact_api_keys_skips_too_short() {
        // "sk-" 后只有 3 字符，不像真 key，保留原文
        let input = "Some sk-abc thing";
        let out = redact_api_keys_simple(input);
        assert_eq!(out, input);
    }

    #[test]
    fn redact_api_keys_handles_multiple_in_one_line() {
        let input = "First sk-1234567890abcdef and second sk-ant-abcdefghijklmnop";
        let out = redact_api_keys_simple(input);
        assert_eq!(out.matches("<redacted-api-key>").count(), 2);
    }

    #[test]
    fn redact_strips_username_when_set() {
        // 不设置 USER 环境变量也不会 panic（OnceLock 在测试间共享，避免依赖测试顺序）
        let input = "/Volumes/T7/whatever/path";
        let out = redact(input);
        // 没有 username 替换的话至少不破坏内容
        assert!(out.contains("/whatever/path"));
    }

    #[test]
    fn collect_recent_log_lines_handles_missing_dir() {
        let p = Path::new("/tmp/miragenty_definitely_not_a_dir_xyz");
        let (lines, files) = collect_recent_log_lines(p, 100).unwrap();
        assert!(lines.is_empty());
        assert_eq!(files, 0);
    }

    #[test]
    fn collect_recent_log_lines_reads_tail() {
        let tmp = std::env::temp_dir().join(format!("miragenty_test_logs_{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp).unwrap();

        // 写两个文件，模拟 rolling
        std::fs::write(
            tmp.join("miragenty.log.2026-04-28"),
            "line1\nline2\nline3\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("miragenty.log.2026-04-29"),
            "todayA\ntodayB\n",
        )
        .unwrap();

        // 取最近 3 行：应该是 today 文件的全部 + yesterday 的最后一行
        let (lines, files) = collect_recent_log_lines(&tmp, 3).unwrap();
        assert_eq!(lines.len(), 3);
        // 顺序：旧 → 新（最后是 today 的最后一行）
        assert!(lines.iter().any(|l| l == "todayA"));
        assert!(lines.iter().any(|l| l == "todayB"));
        assert!(files >= 1);

        std::fs::remove_dir_all(&tmp).ok();
    }
}
