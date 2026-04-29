use anyhow::Result;
use rusqlite::Connection;

const MIGRATIONS: &[(&str, &str)] = &[(
        "001_initial",
        r#"
        CREATE TABLE IF NOT EXISTS missions (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'draft'
                CHECK (status IN ('draft', 'planned', 'running', 'completed', 'failed')),
            total_cost_usd REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY,
            mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'ready', 'running', 'completed', 'failed', 'cancelled')),
            complexity TEXT NOT NULL DEFAULT 'medium'
                CHECK (complexity IN ('low', 'medium', 'high')),
            assigned_agent_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            completed_at TEXT
        );

        CREATE TABLE IF NOT EXISTS task_dependencies (
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            depends_on TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            PRIMARY KEY (task_id, depends_on)
        );

        CREATE TABLE IF NOT EXISTS agents (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            task_id TEXT REFERENCES tasks(id),
            status TEXT NOT NULL DEFAULT 'idle'
                CHECK (status IN ('idle', 'running', 'completed', 'failed', 'cancelled')),
            worktree_path TEXT,
            current_step INTEGER NOT NULL DEFAULT 0,
            total_steps INTEGER,
            tokens_used INTEGER NOT NULL DEFAULT 0,
            cost_usd REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS agent_events (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            kind TEXT NOT NULL
                CHECK (kind IN ('llm_call', 'tool_use', 'tool_result', 'checkpoint', 'error', 'message')),
            content TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_agent_events_agent ON agent_events(agent_id, created_at);

        CREATE TABLE IF NOT EXISTS cost_records (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            task_id TEXT REFERENCES tasks(id),
            model TEXT NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cost_usd REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_cost_records_agent ON cost_records(agent_id);
        "#,
    ),
    (
        "002_engine_hardening",
        r#"
        PRAGMA foreign_keys=OFF;

        CREATE TABLE agent_events_new (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            step INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL
                CHECK (kind IN ('llm_call', 'tool_use', 'tool_result', 'checkpoint', 'error', 'message', 'status_change')),
            content TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        INSERT INTO agent_events_new (id, agent_id, kind, content, created_at)
            SELECT id, agent_id, kind, content, created_at FROM agent_events;

        DROP TABLE agent_events;
        ALTER TABLE agent_events_new RENAME TO agent_events;

        CREATE INDEX IF NOT EXISTS idx_agent_events_agent ON agent_events(agent_id, created_at);

        PRAGMA foreign_keys=ON;
        "#,
    ),
    (
        "003_review_event_kind",
        r#"
        PRAGMA foreign_keys=OFF;

        CREATE TABLE agent_events_new (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            step INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL
                CHECK (kind IN ('llm_call', 'tool_use', 'tool_result', 'checkpoint', 'error', 'message', 'status_change', 'review')),
            content TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        INSERT INTO agent_events_new (id, agent_id, step, kind, content, created_at)
            SELECT id, agent_id, step, kind, content, created_at FROM agent_events;

        DROP TABLE agent_events;
        ALTER TABLE agent_events_new RENAME TO agent_events;

        CREATE INDEX IF NOT EXISTS idx_agent_events_agent ON agent_events(agent_id, created_at);

        PRAGMA foreign_keys=ON;
        "#,
    ),
    (
        "004_agent_commit_hashes",
        r#"
        ALTER TABLE agents ADD COLUMN base_commit_hash TEXT;
        ALTER TABLE agents ADD COLUMN head_commit_hash TEXT;
        "#,
    ),
    (
        "005_agent_notes",
        r#"
        CREATE TABLE IF NOT EXISTS agent_notes (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            content TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'queued'
                CHECK (status IN ('queued', 'applied', 'expired')),
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            applied_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_agent_notes_agent ON agent_notes(agent_id, status);
        "#,
    ),
    (
        "006_agent_notes_mission_scope",
        r#"
        ALTER TABLE agent_notes ADD COLUMN mission_id TEXT;
        "#,
    ),
    (
        "007_mission_directives",
        r#"
        ALTER TABLE missions ADD COLUMN directives TEXT NOT NULL DEFAULT '';
        "#,
    ),
    (
        "008_mission_repo_path",
        r#"
        ALTER TABLE missions ADD COLUMN repo_path TEXT;
        "#,
    ),
    (
        "009_preflight_contract",
        r#"
        PRAGMA foreign_keys=OFF;

        CREATE TABLE missions_new (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'draft'
                CHECK (status IN ('draft', 'preflight', 'planned', 'running', 'completed', 'failed')),
            total_cost_usd REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            directives TEXT NOT NULL DEFAULT '',
            repo_path TEXT
        );

        INSERT INTO missions_new (id, title, description, status, total_cost_usd, created_at, updated_at, directives, repo_path)
            SELECT id, title, description, status, total_cost_usd, created_at, updated_at, directives, repo_path FROM missions;

        DROP TABLE missions;
        ALTER TABLE missions_new RENAME TO missions;

        CREATE TABLE IF NOT EXISTS mission_contracts (
            id TEXT PRIMARY KEY,
            mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
            status TEXT NOT NULL DEFAULT 'drafting'
                CHECK (status IN ('drafting', 'signed')),
            budget_usd REAL,
            quality_threshold REAL,
            max_duration_hours REAL,
            signed_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS contract_items (
            id TEXT PRIMARY KEY,
            contract_id TEXT NOT NULL REFERENCES mission_contracts(id) ON DELETE CASCADE,
            section TEXT NOT NULL
                CHECK (section IN ('scope', 'constraints', 'exclusions', 'assumptions')),
            text TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'user'
                CHECK (source IN ('user', 'agent')),
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS preflight_sessions (
            id TEXT PRIMARY KEY,
            mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
            mode TEXT NOT NULL DEFAULT 'scenario_walk'
                CHECK (mode IN ('scenario_walk', 'devils_advocate', 'risk_highlighter')),
            messages TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        PRAGMA foreign_keys=ON;
        "#,
    ),
    (
        "010_belief_state",
        r#"
        ALTER TABLE preflight_sessions ADD COLUMN belief_state TEXT NOT NULL DEFAULT '{}';
        ALTER TABLE preflight_sessions ADD COLUMN convergence_score REAL NOT NULL DEFAULT 0.0;
        ALTER TABLE preflight_sessions ADD COLUMN phase TEXT NOT NULL DEFAULT 'exploring'
            CHECK (phase IN ('exploring', 'narrowing', 'confirming', 'ready_to_sign'));
        "#,
    ),
    (
        "011_decision_log",
        r#"
        CREATE TABLE IF NOT EXISTS decision_log (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES preflight_sessions(id) ON DELETE CASCADE,
            round INTEGER NOT NULL,
            decision_type TEXT NOT NULL CHECK (decision_type IN ('confirmed', 'rejected', 'inferred', 'revised', 'skipped')),
            description TEXT NOT NULL,
            rationale TEXT NOT NULL DEFAULT '',
            alternatives TEXT NOT NULL DEFAULT '[]',
            contract_item_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_decision_log_session ON decision_log(session_id);
        "#,
    ),
    (
        "012_compaction",
        r#"
        ALTER TABLE preflight_sessions ADD COLUMN compacted_at INTEGER;
        ALTER TABLE preflight_sessions ADD COLUMN compaction_summary TEXT;
        ALTER TABLE preflight_sessions ADD COLUMN last_input_tokens INTEGER;
        ALTER TABLE preflight_sessions ADD COLUMN last_output_tokens INTEGER;
        ALTER TABLE preflight_sessions ADD COLUMN cumulative_input_tokens INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE preflight_sessions ADD COLUMN cumulative_output_tokens INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE preflight_sessions ADD COLUMN compaction_failures INTEGER NOT NULL DEFAULT 0;
        "#,
    ),
    (
        "013_evaluator",
        r#"
        CREATE TABLE IF NOT EXISTS evaluator_reviews (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
            overall_score REAL NOT NULL DEFAULT 0.0,
            summary TEXT NOT NULL DEFAULT '',
            contract_compliance TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS evaluator_annotations (
            id TEXT PRIMARY KEY,
            review_id TEXT NOT NULL REFERENCES evaluator_reviews(id) ON DELETE CASCADE,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            file_path TEXT NOT NULL,
            line_number INTEGER NOT NULL,
            type TEXT NOT NULL
                CHECK (type IN ('bug', 'style', 'performance', 'security', 'suggestion')),
            severity TEXT NOT NULL DEFAULT 'info'
                CHECK (severity IN ('error', 'warning', 'info')),
            status TEXT NOT NULL DEFAULT 'open'
                CHECK (status IN ('open', 'auto_fixed', 'revision_requested', 'dismissed')),
            message TEXT NOT NULL,
            suggestion TEXT,
            auto_fixable INTEGER NOT NULL DEFAULT 0,
            original_code TEXT,
            fixed_code TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_evaluator_reviews_agent ON evaluator_reviews(agent_id);
        CREATE INDEX IF NOT EXISTS idx_evaluator_annotations_review ON evaluator_annotations(review_id);
        CREATE INDEX IF NOT EXISTS idx_evaluator_annotations_agent ON evaluator_annotations(agent_id);
        CREATE INDEX IF NOT EXISTS idx_evaluator_annotations_file ON evaluator_annotations(agent_id, file_path);
        "#,
    ),
    // FM-15 v2.2 Slice 1: tasks 新字段（S1 范围内只加 Planner Loop 必需字段；
    // artifact/file_hints/guardrail/incremental-worktree 等列在 S3/S4 通过后续迁移补齐）
    // 注：missions.repo_path 已由 008_mission_repo_path 添加，此处不重复
    (
        "014_fm15_s1_tasks_role",
        r#"
        ALTER TABLE tasks ADD COLUMN role TEXT NOT NULL DEFAULT 'implementer';
        ALTER TABLE tasks ADD COLUMN expected_output TEXT NOT NULL DEFAULT '';
        "#,
    ),
    // FM-15 v2.2 Slice 1: planner_sessions / planner_steps （Pre-flight 共用预留 kind/contract_id 列）
    (
        "015_fm15_s1_planner_loop",
        r#"
        CREATE TABLE IF NOT EXISTS planner_sessions (
            id TEXT PRIMARY KEY,
            mission_id TEXT REFERENCES missions(id) ON DELETE CASCADE,
            kind TEXT NOT NULL DEFAULT 'planner'
                CHECK (kind IN ('planner', 'preflight')),
            contract_id TEXT,
            repo_path TEXT NOT NULL,
            description TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'running'
                CHECK (status IN ('running', 'completed', 'failed', 'cancelled')),
            total_steps INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            error_message TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            completed_at TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_planner_sessions_mission ON planner_sessions(mission_id);
        CREATE INDEX IF NOT EXISTS idx_planner_sessions_kind ON planner_sessions(kind);

        CREATE TABLE IF NOT EXISTS planner_steps (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES planner_sessions(id) ON DELETE CASCADE,
            step_no INTEGER NOT NULL,
            kind TEXT NOT NULL
                CHECK (kind IN ('tool_call', 'tool_result', 'thinking', 'text')),
            tool_name TEXT,
            tool_args TEXT,
            tool_result TEXT,
            text_content TEXT,
            tokens_used INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_planner_steps_session ON planner_steps(session_id, step_no);
        "#,
    ),
    // FM-15 v2.2 Slice 2 (FR-18): Mission 创建必须声明 repo_origin（from_scratch / from_existing）。
    // 旧 mission 没有该字段 → 允许 NULL，作为「legacy」兜底；新建 mission 必填，
    // create_mission 命令负责拒绝缺失值。
    (
        "016_fm15_s2_mission_repo_origin",
        r#"
        ALTER TABLE missions ADD COLUMN repo_origin TEXT
            CHECK (repo_origin IS NULL OR repo_origin IN ('from_scratch', 'from_existing'));
        "#,
    ),
    // FM-15 v2.2 Slice 3 (S3-1):
    // - tasks 扩展 FR-04 字段（additional_skills / file_scope_hints / consumes_artifacts /
    //   produces_artifacts / guardrails / *_branch / completion_summary 等），全部 NOT NULL DEFAULT
    //   以保持向后兼容。
    // - task_dependencies 增加 artifact_refs（FR-04 边语义）。
    // - 新增 artifacts 表（FR-03）；S3 阶段只用「declare-only」即 `publish_artifact` 工具，
    //   实际 file 校验留待 Phase 3 guardrail。
    // - 新增 planner_session_fetch_grants 表（FR-05.6 single-session 临时白名单）。
    (
        "017_fm15_s3_artifacts_skills_fetch",
        r#"
        ALTER TABLE tasks ADD COLUMN additional_skills TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE tasks ADD COLUMN file_scope_hints TEXT NOT NULL DEFAULT '{"definite":[],"possible":[]}';
        ALTER TABLE tasks ADD COLUMN consumes_artifacts TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE tasks ADD COLUMN produces_artifacts TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE tasks ADD COLUMN guardrails TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE tasks ADD COLUMN guardrail_retry_budget INTEGER NOT NULL DEFAULT 3;
        ALTER TABLE tasks ADD COLUMN guardrail_retry_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE tasks ADD COLUMN actual_files_modified TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE tasks ADD COLUMN completion_summary TEXT;
        ALTER TABLE tasks ADD COLUMN merge_strategy_hint TEXT;
        ALTER TABLE tasks ADD COLUMN agent_branch TEXT;

        ALTER TABLE task_dependencies ADD COLUMN artifact_refs TEXT NOT NULL DEFAULT '[]';

        CREATE TABLE IF NOT EXISTS artifacts (
            id TEXT PRIMARY KEY,
            mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
            producer_task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            type TEXT NOT NULL
                CHECK (type IN ('design_doc','api_spec','schema','code_module','test_module','config','docs','report')),
            local_name TEXT NOT NULL,
            summary TEXT NOT NULL DEFAULT '',
            file_paths TEXT NOT NULL DEFAULT '[]',
            published INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_artifacts_mission ON artifacts(mission_id);
        CREATE INDEX IF NOT EXISTS idx_artifacts_producer ON artifacts(producer_task_id);

        CREATE TABLE IF NOT EXISTS planner_session_fetch_grants (
            session_id TEXT NOT NULL REFERENCES planner_sessions(id) ON DELETE CASCADE,
            domain TEXT NOT NULL,
            granted_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (session_id, domain)
        );
        "#,
    ),
    // FM-15 Phase 2 (FR-07 / FR-08 / FR-12 / FR-13): 增量 worktree + 三层合并 + 主分支检测。
    //
    // 一次性把 Phase 2 全部 schema 落地（避免 S1/S2/S3 各自 migration 颗粒度过细）：
    // - missions.main_branch         : 主分支名（NULL = 启动时探测后回填）— FR-12
    // - missions.use_incremental_worktree : 兼容性开关，默认 1（开启），可在创建时关闭走旧逻辑 — FR-07.6
    // - missions.merge_strategy      : 合并策略默认值，'theirs' | 'ours' | 'llm_resolve'；
    //                                  Phase 2 只实现 L1+L2，'llm_resolve' 在 Phase 3 接 LLM 解冲突
    // - task_base_conflicts          : 增量 worktree 构建 base 时的冲突记录 — FR-07.1
    // - merge_records                : 三层合并最终决策与降级原因 — FR-08.3
    (
        "018_fm15_p2_incremental_worktree",
        r#"
        ALTER TABLE missions ADD COLUMN main_branch TEXT;
        ALTER TABLE missions ADD COLUMN use_incremental_worktree INTEGER NOT NULL DEFAULT 1;
        ALTER TABLE missions ADD COLUMN merge_strategy TEXT NOT NULL DEFAULT 'theirs'
            CHECK (merge_strategy IN ('theirs','ours','llm_resolve'));

        CREATE TABLE IF NOT EXISTS task_base_conflicts (
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            parent_task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            file_path TEXT NOT NULL,
            resolution TEXT NOT NULL
                CHECK (resolution IN ('auto','heuristic_theirs','llm_resolved','llm_failed_fallback')),
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (task_id, parent_task_id, file_path)
        );
        CREATE INDEX IF NOT EXISTS idx_task_base_conflicts_task ON task_base_conflicts(task_id);

        CREATE TABLE IF NOT EXISTS merge_records (
            id TEXT PRIMARY KEY,
            mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
            source_branch TEXT NOT NULL,
            target_branch TEXT NOT NULL,
            strategy_attempted TEXT NOT NULL,
            final_strategy TEXT NOT NULL,
            conflicted_files TEXT NOT NULL DEFAULT '[]',
            llm_resolution_succeeded INTEGER,
            build_passed_after_llm INTEGER,
            fallback_reason TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_merge_records_mission ON merge_records(mission_id);
        "#,
    ),
    // FM-15 v2.2 P4-S5 — Follow-up Chat & follow-up missions：
    // - missions.parent_mission_id : 子 mission 关联（FR-15.4）
    // - mission_chats              : Chat 会话持久化（FR-15.1, FR-15.6）
    (
        "019_fm15_p4_chat_followup",
        r#"
        ALTER TABLE missions ADD COLUMN parent_mission_id TEXT
            REFERENCES missions(id) ON DELETE SET NULL;
        CREATE INDEX IF NOT EXISTS idx_missions_parent ON missions(parent_mission_id);

        CREATE TABLE IF NOT EXISTS mission_chats (
            id TEXT PRIMARY KEY,
            mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
            role TEXT NOT NULL CHECK (role IN ('user','assistant','system')),
            content TEXT NOT NULL,
            tool_calls TEXT,
            artifact_refs TEXT,
            proposed_followup_mission_id TEXT REFERENCES missions(id) ON DELETE SET NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_mission_chats_mission
            ON mission_chats(mission_id, created_at);
        "#,
    ),
    // FM-15 follow-up — 失败诊断可视化：让 task 自带最近一次失败原因 + 时间，
    // 前端 DAG / TaskDetailPanel 可直接 hover 看失败信息，避免再去翻 agent_events。
    // - tasks.last_error    : 最近一次失败原因（含分类前缀，如 "timeout: …" / "guardrail: …"）
    // - tasks.last_failed_at: 最近一次失败时间（UTC ISO8601）
    (
        "020_fm15_followup_task_last_error",
        r#"
        ALTER TABLE tasks ADD COLUMN last_error TEXT;
        ALTER TABLE tasks ADD COLUMN last_failed_at TEXT;
        "#,
    ),
    // FM-14 Approval Queue — 统一审批队列。
    //
    // 收编三类已存在的「审批语义」（fetch_url 域名确认 / followup mission 升级 /
    // chat agent commit 阈值），并新增两类（tool 拦截 / budget 超限）。所有审批请求
    // 走同一张表，前端用同一个 ApprovalQueue UI 展示。
    //
    // - agents.status 加 'waiting_approval'：SQLite 不支持 ALTER CHECK，必须重建表
    //   （仿 009_preflight_contract 模式）。
    // - approval_requests 通用化：kind 区分类别，payload 是各自结构化 JSON。
    //   - source 关联：agent_id / planner_session_id / chat_message_id 三选一非空
    //   - decision_note: 用户 reject 时填的解释，会自动 inject 到 agent 上下文
    //   - decided_by: 'user' | 'auto_threshold' | 'auto_expire' 用于审计与 FM-13 异常检测
    //   - expires_at: 写入时计算 = created_at + approval_timeout_seconds（默认 600s）
    (
        "021_fm14_approval_queue",
        r#"
        PRAGMA foreign_keys=OFF;

        CREATE TABLE agents_new (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            task_id TEXT REFERENCES tasks(id),
            status TEXT NOT NULL DEFAULT 'idle'
                CHECK (status IN ('idle', 'running', 'completed', 'failed', 'cancelled', 'waiting_approval')),
            worktree_path TEXT,
            current_step INTEGER NOT NULL DEFAULT 0,
            total_steps INTEGER,
            tokens_used INTEGER NOT NULL DEFAULT 0,
            cost_usd REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            base_commit_hash TEXT,
            head_commit_hash TEXT
        );

        INSERT INTO agents_new (
            id, name, task_id, status, worktree_path, current_step, total_steps,
            tokens_used, cost_usd, created_at, updated_at, base_commit_hash, head_commit_hash
        )
        SELECT
            id, name, task_id, status, worktree_path, current_step, total_steps,
            tokens_used, cost_usd, created_at, updated_at, base_commit_hash, head_commit_hash
        FROM agents;

        DROP TABLE agents;
        ALTER TABLE agents_new RENAME TO agents;

        PRAGMA foreign_keys=ON;

        CREATE TABLE IF NOT EXISTS approval_requests (
            id TEXT PRIMARY KEY,
            mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
            kind TEXT NOT NULL
                CHECK (kind IN ('tool', 'fetch', 'escalation', 'budget', 'chat_commit')),
            agent_id TEXT REFERENCES agents(id) ON DELETE CASCADE,
            planner_session_id TEXT REFERENCES planner_sessions(id) ON DELETE CASCADE,
            chat_message_id TEXT REFERENCES mission_chats(id) ON DELETE CASCADE,
            title TEXT NOT NULL,
            payload TEXT NOT NULL DEFAULT '{}',
            reason TEXT NOT NULL DEFAULT '',
            context_summary TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'approved', 'rejected', 'expired', 'cancelled')),
            decision_note TEXT,
            decided_by TEXT
                CHECK (decided_by IS NULL OR decided_by IN ('user', 'auto_threshold', 'auto_expire')),
            resolved_at TEXT,
            expires_at TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_approval_pending
            ON approval_requests(status, expires_at)
            WHERE status = 'pending';
        CREATE INDEX IF NOT EXISTS idx_approval_mission
            ON approval_requests(mission_id, created_at);
        CREATE INDEX IF NOT EXISTS idx_approval_agent
            ON approval_requests(agent_id)
            WHERE agent_id IS NOT NULL;
        "#,
    ),
    // FM-12 Mission Report
    //
    // 设计要点：
    // - mission_reports 一对一存储完整报告 JSON（report_data），重新生成则覆盖。
    //   - generated_at 用于乐观刷新；前端 stale 时可触发后台再生成
    //   - schema_version 预留：未来报告结构升级时按版本号迁移而非破坏性覆盖
    // - report_votes 记录用户对 Architecture Decision 的投票，UNIQUE(report_id, decision_id)
    //   保证幂等：同一 decision 的多次投票直接 UPSERT。
    //   - decision_id 来源是 report_data.decisions[].id（如 D-1），故为 TEXT 而非 FK
    //   - 切换投票（agree↔disagree）通过 ON CONFLICT DO UPDATE 实现
    (
        "022_fm12_mission_report",
        r#"
        CREATE TABLE IF NOT EXISTS mission_reports (
            id TEXT PRIMARY KEY,
            mission_id TEXT NOT NULL UNIQUE REFERENCES missions(id) ON DELETE CASCADE,
            schema_version INTEGER NOT NULL DEFAULT 1,
            report_data TEXT NOT NULL DEFAULT '{}',
            generated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_mission_reports_mission
            ON mission_reports(mission_id);

        CREATE TABLE IF NOT EXISTS report_votes (
            id TEXT PRIMARY KEY,
            report_id TEXT NOT NULL REFERENCES mission_reports(id) ON DELETE CASCADE,
            decision_id TEXT NOT NULL,
            vote TEXT NOT NULL CHECK (vote IN ('agree', 'disagree')),
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(report_id, decision_id)
        );

        CREATE INDEX IF NOT EXISTS idx_report_votes_report
            ON report_votes(report_id);
        "#,
    ),
];

pub fn run(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            name TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    for (name, sql) in MIGRATIONS {
        let applied: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM schema_migrations WHERE name = ?",
            [name],
            |row| row.get(0),
        )?;
        if !applied {
            conn.execute_batch(sql)?;
            conn.execute(
                "INSERT INTO schema_migrations (name) VALUES (?)",
                [name],
            )?;
            tracing::info!("Applied migration: {name}");
        }
    }

    Ok(())
}
