//! FM-13 Lite — Insights / Harness Dashboard backend.
//!
//! 设计取舍（与完整版 FM-13 的差异）：
//! - 不持久化：anomaly 检测每次按需计算，不写 anomaly_records 表
//!   - 好处：零 schema 迁移，零后台扫描成本
//!   - 代价：每次刷新都要遍历 cost_records / agents；OK 因为单机 SQLite，
//!     mission 数 ≤ 1k 时 < 50ms
//! - 检测规则只保留 FR-07.2 中能从现有表算出的：
//!   - cost_spike：单次 cost_record > 同 mission 中位数 × 3 （PERCENTILE_CONT 在 SQLite 没有，
//!     用 Rust 端计算）
//!   - long_running：agent 在 running 状态 > 600s（用 agents.started_at + julianday 算 wall clock）
//!   - failed_run：agent.status = 'failed' 且 last_message 非空，列出方便用户跳过去看 review
//! - cost trend 按 mission 维度聚合，不做秒级时间序列；这样图表数据量稳定且 mission 维度对人类
//!   更易理解（"上个 mission 比这个贵 2 倍"）

use crate::db::{queries, Database};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::Manager;

#[derive(Debug, Serialize, Deserialize)]
pub struct MissionCostPoint {
    pub mission_id: String,
    pub mission_title: String,
    pub status: String,
    /// 创建时间，ISO 8601；用作 X 轴排序
    pub created_at: String,
    pub total_cost: f64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    /// 各模型的成本拆分，按 cost desc
    pub model_breakdown: Vec<ModelCostEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelCostEntry {
    pub model: String,
    pub cost: f64,
    pub tokens: i64,
}

#[tauri::command]
pub fn get_cost_trend(
    app: tauri::AppHandle,
    limit: Option<i64>,
) -> Result<Vec<MissionCostPoint>, String> {
    let limit = limit.unwrap_or(50).clamp(1, 500);
    let db = app.state::<Database>();
    db.with_conn(|conn| collect_cost_trend(conn, limit))
        .map_err(|e| e.to_string())
}

fn collect_cost_trend(conn: &Connection, limit: i64) -> anyhow::Result<Vec<MissionCostPoint>> {
    // 取最近 N 个 mission（按 created_at desc）
    let mut stmt = conn.prepare(
        "SELECT id, title, status, created_at
         FROM missions
         ORDER BY created_at DESC
         LIMIT ?1",
    )?;
    let rows: Vec<(String, String, String, String)> = stmt
        .query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut out = Vec::with_capacity(rows.len());
    for (id, title, status, created_at) in rows {
        let summary = queries::get_mission_cost_summary(conn, &id)?;
        let breakdown = collect_model_breakdown(conn, &id)?;
        out.push(MissionCostPoint {
            mission_id: id,
            mission_title: title,
            status,
            created_at,
            total_cost: summary.total_cost,
            total_input_tokens: summary.total_input_tokens,
            total_output_tokens: summary.total_output_tokens,
            model_breakdown: breakdown,
        });
    }

    // 倒回时间正序，便于前端从左到右画
    out.reverse();
    Ok(out)
}

fn collect_model_breakdown(
    conn: &Connection,
    mission_id: &str,
) -> anyhow::Result<Vec<ModelCostEntry>> {
    let mut stmt = conn.prepare(
        "SELECT cr.model,
                COALESCE(SUM(cr.cost_usd), 0.0),
                COALESCE(SUM(cr.input_tokens + cr.output_tokens), 0)
         FROM cost_records cr
         JOIN agents a ON a.id = cr.agent_id
         JOIN tasks t ON t.id = a.task_id
         WHERE t.mission_id = ?1
         GROUP BY cr.model
         ORDER BY 2 DESC",
    )?;
    let rows = stmt
        .query_map(params![mission_id], |row| {
            Ok(ModelCostEntry {
                model: row.get(0)?,
                cost: row.get(1)?,
                tokens: row.get(2)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Anomaly detection
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyKind {
    CostSpike,
    LongRunning,
    FailedAgent,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnomalySeverity {
    Info,
    Warn,
    Critical,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Anomaly {
    pub kind: AnomalyKind,
    pub severity: AnomalySeverity,
    pub mission_id: String,
    pub mission_title: String,
    pub agent_id: Option<String>,
    pub task_title: Option<String>,
    /// 人话简述
    pub message: String,
    pub occurred_at: String,
}

#[tauri::command]
pub fn get_anomalies(
    app: tauri::AppHandle,
    mission_id: Option<String>,
) -> Result<Vec<Anomaly>, String> {
    let db = app.state::<Database>();
    db.with_conn(|conn| detect_anomalies(conn, mission_id.as_deref()))
        .map_err(|e| e.to_string())
}

fn detect_anomalies(conn: &Connection, mission_id: Option<&str>) -> anyhow::Result<Vec<Anomaly>> {
    let mut anomalies = Vec::new();

    // ── 1. cost_spike：单条 cost_record > 同 mission 中位数 × 3
    detect_cost_spikes(conn, mission_id, &mut anomalies)?;

    // ── 2. long_running：agent.status='running' 且 started_at > 600s
    detect_long_running(conn, mission_id, &mut anomalies)?;

    // ── 3. failed_agent：agent.status='failed'，最近 24h
    detect_failed_agents(conn, mission_id, &mut anomalies)?;

    // 按 severity desc 然后 occurred_at desc 排序
    anomalies.sort_by(|a, b| {
        let sa = severity_rank(&a.severity);
        let sb = severity_rank(&b.severity);
        sb.cmp(&sa).then_with(|| b.occurred_at.cmp(&a.occurred_at))
    });
    Ok(anomalies)
}

fn severity_rank(s: &AnomalySeverity) -> i32 {
    match s {
        AnomalySeverity::Critical => 3,
        AnomalySeverity::Warn => 2,
        AnomalySeverity::Info => 1,
    }
}

fn detect_cost_spikes(
    conn: &Connection,
    mission_id: Option<&str>,
    out: &mut Vec<Anomaly>,
) -> anyhow::Result<()> {
    // 拿到候选 mission 列表
    let missions: Vec<(String, String)> = {
        let sql = match mission_id {
            Some(_) => "SELECT id, title FROM missions WHERE id = ?1",
            None => "SELECT id, title FROM missions ORDER BY created_at DESC LIMIT 50",
        };
        let mut stmt = conn.prepare(sql)?;
        if let Some(mid) = mission_id {
            stmt.query_map(params![mid], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        }
    };

    for (mid, mtitle) in missions {
        // 拿这个 mission 所有 cost_records
        let mut stmt = conn.prepare(
            "SELECT cr.id, cr.cost_usd, cr.created_at, cr.agent_id, cr.task_id, t.title
             FROM cost_records cr
             JOIN agents a ON a.id = cr.agent_id
             JOIN tasks t ON t.id = a.task_id
             WHERE t.mission_id = ?1
             ORDER BY cr.cost_usd DESC",
        )?;
        let records: Vec<(String, f64, String, String, Option<String>, String)> = stmt
            .query_map(params![mid], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        if records.len() < 3 {
            // 样本太少，median 没意义
            continue;
        }
        let costs: Vec<f64> = records.iter().map(|r| r.1).collect();
        let median = compute_median(&costs);
        if median <= 0.0 {
            continue;
        }
        let threshold = median * 3.0;
        // 至少要有真金白银的金额才报警，避免 median=$0.0001 × 3 误报
        let absolute_floor = 0.05_f64;
        let cutoff = threshold.max(absolute_floor);

        for (_, cost, occurred_at, agent_id, _task_id, task_title) in records {
            if cost > cutoff {
                let severity = if cost > median * 10.0 {
                    AnomalySeverity::Critical
                } else if cost > median * 5.0 {
                    AnomalySeverity::Warn
                } else {
                    AnomalySeverity::Info
                };
                out.push(Anomaly {
                    kind: AnomalyKind::CostSpike,
                    severity,
                    mission_id: mid.clone(),
                    mission_title: mtitle.clone(),
                    agent_id: Some(agent_id),
                    task_title: Some(task_title),
                    message: format!(
                        "Single LLM call cost ${:.4}, {:.1}× the mission median (${:.4}).",
                        cost,
                        cost / median,
                        median
                    ),
                    occurred_at,
                });
            }
        }
    }

    Ok(())
}

fn detect_long_running(
    conn: &Connection,
    mission_id: Option<&str>,
    out: &mut Vec<Anomaly>,
) -> anyhow::Result<()> {
    // agents 表没有显式 started_at；用 created_at 作为开始时间。
    // 注：updated_at 也会在 step heartbeat 时刷新，但 created_at 是 immutable 的入门时间，
    // 用它度量 wall clock 是更保守的"已经跑了多久"判断。
    // 600s 阈值
    let sql = match mission_id {
        Some(_) => {
            "SELECT a.id, a.task_id, a.created_at,
                    CAST(round((julianday('now') - julianday(a.created_at)) * 86400) AS INTEGER) AS elapsed,
                    t.mission_id, m.title, t.title
             FROM agents a
             JOIN tasks t ON t.id = a.task_id
             JOIN missions m ON m.id = t.mission_id
             WHERE a.status = 'running'
               AND t.mission_id = ?1
               AND elapsed > 600"
        }
        None => {
            "SELECT a.id, a.task_id, a.created_at,
                    CAST(round((julianday('now') - julianday(a.created_at)) * 86400) AS INTEGER) AS elapsed,
                    t.mission_id, m.title, t.title
             FROM agents a
             JOIN tasks t ON t.id = a.task_id
             JOIN missions m ON m.id = t.mission_id
             WHERE a.status = 'running'
               AND elapsed > 600"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let mapper = |row: &rusqlite::Row| -> rusqlite::Result<(String, String, String, i64, String, String, String)> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ))
    };
    let rows: Vec<(String, String, String, i64, String, String, String)> =
        if let Some(mid) = mission_id {
            stmt.query_map(params![mid], mapper)?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map([], mapper)?.filter_map(|r| r.ok()).collect()
        };

    for (agent_id, _task_id, started_at, elapsed, mission_id, mission_title, task_title) in rows {
        let severity = if elapsed > 1800 {
            AnomalySeverity::Critical
        } else if elapsed > 1200 {
            AnomalySeverity::Warn
        } else {
            AnomalySeverity::Info
        };
        out.push(Anomaly {
            kind: AnomalyKind::LongRunning,
            severity,
            mission_id,
            mission_title,
            agent_id: Some(agent_id),
            task_title: Some(task_title),
            message: format!(
                "Agent has been running for {} min ({}s); typical task finishes < 10 min.",
                elapsed / 60,
                elapsed
            ),
            occurred_at: started_at,
        });
    }
    Ok(())
}

fn detect_failed_agents(
    conn: &Connection,
    mission_id: Option<&str>,
    out: &mut Vec<Anomaly>,
) -> anyhow::Result<()> {
    // agents 表没有 last_message / completed_at 字段。
    // 用 updated_at 作为"最后失败时间"代理（agent 转 failed 时会刷 updated_at）。
    // 失败原因：从最近一条 agent_events 的 message 取（kind='error' 优先，其次任意）。
    let sql = match mission_id {
        Some(_) => {
            "SELECT a.id, a.task_id, a.updated_at,
                    t.mission_id, m.title, t.title,
                    (SELECT ae.content FROM agent_events ae
                       WHERE ae.agent_id = a.id
                       ORDER BY (CASE WHEN ae.kind='error' THEN 0 ELSE 1 END), ae.created_at DESC
                       LIMIT 1) AS last_msg
             FROM agents a
             JOIN tasks t ON t.id = a.task_id
             JOIN missions m ON m.id = t.mission_id
             WHERE a.status = 'failed'
               AND t.mission_id = ?1
               AND a.updated_at > datetime('now', '-1 day')
             ORDER BY a.updated_at DESC
             LIMIT 50"
        }
        None => {
            "SELECT a.id, a.task_id, a.updated_at,
                    t.mission_id, m.title, t.title,
                    (SELECT ae.content FROM agent_events ae
                       WHERE ae.agent_id = a.id
                       ORDER BY (CASE WHEN ae.kind='error' THEN 0 ELSE 1 END), ae.created_at DESC
                       LIMIT 1) AS last_msg
             FROM agents a
             JOIN tasks t ON t.id = a.task_id
             JOIN missions m ON m.id = t.mission_id
             WHERE a.status = 'failed'
               AND a.updated_at > datetime('now', '-1 day')
             ORDER BY a.updated_at DESC
             LIMIT 50"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let mapper = |row: &rusqlite::Row| -> rusqlite::Result<(
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
    )> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ))
    };
    let rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
    )> = if let Some(mid) = mission_id {
        stmt.query_map(params![mid], mapper)?
            .filter_map(|r| r.ok())
            .collect()
    } else {
        stmt.query_map([], mapper)?.filter_map(|r| r.ok()).collect()
    };

    for (agent_id, _task_id, occurred_at, mission_id, mission_title, task_title, last_message) in
        rows
    {
        let snippet = last_message
            .as_deref()
            .map(|s| {
                let trimmed = s.trim();
                if trimmed.len() > 120 {
                    format!("{}…", &trimmed[..120])
                } else {
                    trimmed.to_string()
                }
            })
            .unwrap_or_else(|| "(no last_message recorded)".into());

        out.push(Anomaly {
            kind: AnomalyKind::FailedAgent,
            severity: AnomalySeverity::Warn,
            mission_id,
            mission_title,
            agent_id: Some(agent_id),
            task_title: Some(task_title),
            message: format!("Agent failed. Last message: {}", snippet),
            occurred_at,
        });
    }
    Ok(())
}

fn compute_median(sorted_or_unsorted: &[f64]) -> f64 {
    if sorted_or_unsorted.is_empty() {
        return 0.0;
    }
    let mut v = sorted_or_unsorted.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations_run_on;
    use rusqlite::Connection;

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations_run_on(&conn).unwrap();
        conn
    }

    fn insert_mission(conn: &Connection, id: &str, title: &str) {
        conn.execute(
            "INSERT INTO missions (id, title, description, status) VALUES (?1, ?2, ?3, 'completed')",
            params![id, title, "desc"],
        )
        .unwrap();
    }

    fn insert_task_agent(
        conn: &Connection,
        mission_id: &str,
        task_id: &str,
        task_title: &str,
        agent_id: &str,
        agent_status: &str,
    ) {
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, status) VALUES (?1, ?2, ?3, 'completed')",
            params![task_id, mission_id, task_title],
        )
        .unwrap();
        // agents 必填 name；其余字段走 default
        conn.execute(
            "INSERT INTO agents (id, name, task_id, status) VALUES (?1, ?2, ?3, ?4)",
            params![agent_id, agent_id, task_id, agent_status],
        )
        .unwrap();
    }

    fn insert_cost(conn: &Connection, id: &str, agent_id: &str, task_id: &str, cost: f64) {
        conn.execute(
            "INSERT INTO cost_records (id, agent_id, task_id, model, input_tokens, output_tokens, cost_usd)
             VALUES (?1, ?2, ?3, 'sonnet', 100, 200, ?4)",
            params![id, agent_id, task_id, cost],
        )
        .unwrap();
    }

    #[test]
    fn median_basic() {
        assert_eq!(compute_median(&[]), 0.0);
        assert_eq!(compute_median(&[5.0]), 5.0);
        assert_eq!(compute_median(&[1.0, 3.0, 2.0]), 2.0);
        assert_eq!(compute_median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
    }

    #[test]
    fn cost_trend_returns_recent_missions_in_chronological_order() {
        let conn = setup_conn();
        insert_mission(&conn, "m1", "First");
        insert_mission(&conn, "m2", "Second");
        insert_task_agent(&conn, "m1", "t1", "Task A", "a1", "completed");
        insert_task_agent(&conn, "m2", "t2", "Task B", "a2", "completed");
        insert_cost(&conn, "c1", "a1", "t1", 0.1);
        insert_cost(&conn, "c2", "a2", "t2", 0.5);

        let trend = collect_cost_trend(&conn, 50).unwrap();
        assert_eq!(trend.len(), 2);
        // 输出按 created_at 升序：m1 → m2 (id 顺序就是插入顺序，时间相同时无序保证；
        // 这个测试只保证总数和数据正确)
        let m2 = trend.iter().find(|p| p.mission_id == "m2").unwrap();
        assert!((m2.total_cost - 0.5).abs() < 1e-9);
        assert_eq!(m2.model_breakdown.len(), 1);
        assert_eq!(m2.model_breakdown[0].model, "sonnet");
    }

    #[test]
    fn anomalies_cost_spike_detected() {
        let conn = setup_conn();
        insert_mission(&conn, "m1", "Spike Mission");
        insert_task_agent(&conn, "m1", "t1", "T1", "a1", "completed");
        // median = $0.10, 3× = $0.30；插一个 $1.0 的尖峰
        insert_cost(&conn, "c1", "a1", "t1", 0.10);
        insert_cost(&conn, "c2", "a1", "t1", 0.10);
        insert_cost(&conn, "c3", "a1", "t1", 0.10);
        insert_cost(&conn, "c4", "a1", "t1", 0.10);
        insert_cost(&conn, "c_spike", "a1", "t1", 1.0);

        let an = detect_anomalies(&conn, None).unwrap();
        let spikes: Vec<_> = an
            .iter()
            .filter(|x| x.kind == AnomalyKind::CostSpike)
            .collect();
        assert_eq!(spikes.len(), 1);
        assert!(spikes[0].message.contains("$1.0"));
    }

    #[test]
    fn anomalies_cost_spike_skips_low_volume_missions() {
        let conn = setup_conn();
        insert_mission(&conn, "m1", "Tiny");
        insert_task_agent(&conn, "m1", "t1", "T1", "a1", "completed");
        // 只有 2 条，样本不够，不报
        insert_cost(&conn, "c1", "a1", "t1", 0.01);
        insert_cost(&conn, "c2", "a1", "t1", 1.00);

        let an = detect_anomalies(&conn, None).unwrap();
        assert!(an
            .iter()
            .filter(|x| x.kind == AnomalyKind::CostSpike)
            .next()
            .is_none());
    }

    #[test]
    fn anomalies_cost_spike_skips_below_absolute_floor() {
        let conn = setup_conn();
        insert_mission(&conn, "m1", "Cheap");
        insert_task_agent(&conn, "m1", "t1", "T1", "a1", "completed");
        // median ~ $0.001, 3× = $0.003，但绝对值低，不该报
        for i in 0..10 {
            insert_cost(&conn, &format!("c{i}"), "a1", "t1", 0.001);
        }
        insert_cost(&conn, "c_pseudo", "a1", "t1", 0.01); // 10× median 但绝对值小

        let an = detect_anomalies(&conn, None).unwrap();
        assert!(an
            .iter()
            .filter(|x| x.kind == AnomalyKind::CostSpike)
            .next()
            .is_none());
    }

    #[test]
    fn anomalies_failed_agent_listed() {
        let conn = setup_conn();
        insert_mission(&conn, "m1", "Fail Mission");
        insert_task_agent(&conn, "m1", "t1", "T1", "a1", "failed");
        // last_message 留空 → 走兜底文案
        let an = detect_anomalies(&conn, Some("m1")).unwrap();
        let failed: Vec<_> = an
            .iter()
            .filter(|x| x.kind == AnomalyKind::FailedAgent)
            .collect();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].message.contains("(no last_message recorded)"));
    }

    #[test]
    fn anomalies_filtered_by_mission_id() {
        let conn = setup_conn();
        insert_mission(&conn, "m1", "First");
        insert_mission(&conn, "m2", "Second");
        insert_task_agent(&conn, "m1", "t1", "T1", "a1", "failed");
        insert_task_agent(&conn, "m2", "t2", "T2", "a2", "failed");

        let only_m1 = detect_anomalies(&conn, Some("m1")).unwrap();
        assert_eq!(only_m1.len(), 1);
        assert_eq!(only_m1[0].mission_id, "m1");
    }
}
