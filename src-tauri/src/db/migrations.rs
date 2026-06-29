use anyhow::Result;
use rusqlite::Connection;

const MIGRATIONS: &[(&str, &str)] = &[
    (
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
    // retryable-flow rule 1 的历史脏数据回收（一次性）。
    //
    // 背景：早期 `sign_contract` 把 `UPDATE mission_contracts SET status='signed'`
    // 放在 PlannerEngine 之前。Planner 超时 / LLM 失败时，contract 永久卡在 'signed'
    // 而 tasks 表为空，前端 ContractPanel 的 `readOnly = status === 'signed'` 把整个
    // 签约区块隐藏 → 用户进入"签约按钮消失"的死锁。
    //
    // 新版 `sign_contract` 已经把 `signed` 放进与 tasks 同一个事务（见
    // commands/preflight.rs::sign_contract 与 retryable-flow.mdc rule 1/2），未来不会
    // 再产生这种状态。但已发布版本留在用户机器上的脏数据无法自愈，本 migration
    // 用一条 UPDATE 一次性回收：
    //
    //   - 仅命中 `status='signed'` **且** mission 仍处 draft/preflight **且** 该 mission
    //     下确实没有 tasks 的 contract —— 严格表达"signed 必然伴随 tasks"不变量；
    //   - 回退到 'drafting' + 清 signed_at，**保留 contract_items**（用户写的 scope/
    //     constraints 等条目不丢）；
    //   - 成功签约的 mission（mission.status >= 'planned' 且有 tasks）永远不会被命中。
    //
    // 同时保留 idx_mission_contracts_mission 索引，让未来按 mission_id 反查 contract
    // （sign_contract 第 1 步 + 自愈逻辑）更快。
    (
        "023_retryable_flow_recover_stuck_signed_contracts",
        r#"
        CREATE INDEX IF NOT EXISTS idx_mission_contracts_mission
            ON mission_contracts(mission_id);

        UPDATE mission_contracts
        SET status = 'drafting',
            signed_at = NULL,
            updated_at = datetime('now')
        WHERE status = 'signed'
          AND mission_id IN (
              SELECT id FROM missions WHERE status IN ('draft', 'preflight')
          )
          AND NOT EXISTS (
              SELECT 1 FROM tasks WHERE tasks.mission_id = mission_contracts.mission_id
          );
        "#,
    ),
    // FM-15 v2.3：task_dependencies 增加 `kind` 列区分 producer / reference 边。
    //
    // 背景：sign_contract 把 task.consumes_artifacts 反向映射成 task_dependencies 时，
    // 每条边都被等权画到 DAG 上。但实际上有的边携带的是 **文档型 artifact**
    //（design_doc / api_spec / schema / docs / report）——上游 task 是 architect/
    // researcher/spec writer，下游消费的只是"参考资料"。这类边在长 DAG 里指数膨胀
    //（一份架构文档扇出 N 条），把视觉糊成蜘蛛网。
    //
    //   - kind='producer'：携带 code_module / test_module / config 等"实物 artifact"
    //     的边，下游真的依赖上游的代码产出。
    //   - kind='reference'：携带文档型 artifact 的边，对调度而言**当前**与 producer
    //     等价（仍要等上游 completed 才能拿到 artifact），但 UI 默认弱化/隐藏。
    //
    // 历史行存量推导：通过 task_dependencies.artifact_refs 反向解析每条 ref
    //（"upstream_task_id.local_name"）到 tasks.produces_artifacts 的 type 字段，
    // 全部命中 doc-set 则标 reference，否则 producer。无 artifact_refs 的纯拓扑
    // 边保守标 producer（不会改变默认行为）。
    //
    // 不影响调度：advance_dependencies / get_completed_parent_tasks_for / 等查询
    // 不区分 kind，新代码继续把 producer/reference 一视同仁。kind 只对 IPC 输出
    // 和前端渲染生效。
    (
        "024_task_dependencies_kind",
        r#"
        ALTER TABLE task_dependencies ADD COLUMN kind TEXT NOT NULL DEFAULT 'producer'
            CHECK (kind IN ('producer', 'reference'));

        -- 历史数据 backfill：解析 artifact_refs，每条 ref 形如 "<upstream_task_id>.<local_name>"，
        -- 到 upstream task.produces_artifacts JSON 中找 matching local_name 的 type。
        -- 所有 ref 的 type 都属于 doc-set 才算 reference。
        WITH ref_types AS (
            SELECT
                td.task_id,
                td.depends_on,
                json_extract(
                    t_up.produces_artifacts,
                    '$[' || (
                        SELECT key FROM json_each(t_up.produces_artifacts)
                        WHERE json_extract(value, '$.local_name') = substr(
                            json_each_refs.value,
                            instr(json_each_refs.value, '.') + 1
                        )
                        LIMIT 1
                    ) || '].type'
                ) AS artifact_type
            FROM task_dependencies td
            JOIN tasks t_up ON t_up.id = td.depends_on
            JOIN json_each(td.artifact_refs) AS json_each_refs
        ),
        edge_classification AS (
            SELECT
                task_id,
                depends_on,
                COUNT(*) AS total_refs,
                SUM(CASE WHEN artifact_type IN
                    ('design_doc', 'api_spec', 'schema', 'docs', 'report')
                    THEN 1 ELSE 0 END) AS doc_refs
            FROM ref_types
            GROUP BY task_id, depends_on
        )
        UPDATE task_dependencies
        SET kind = 'reference'
        WHERE (task_id, depends_on) IN (
            SELECT task_id, depends_on
            FROM edge_classification
            WHERE total_refs > 0 AND doc_refs = total_refs
        );
        "#,
    ),
    (
        // Single-Agent Uplift Phase 0.1–0.2:
        // - 扩 agent_events.kind CHECK 约束，把 engine.rs 已经在 emit 的 system_hint /
        //   guardrail_pass / guardrail_fail / guardrail_summary / note_applied 真正落库；
        //   并提前给 Phase 1/2 预留 tool_progress / tool_summary / compact / todo_update。
        //   之前这些 INSERT 全部因 CHECK 失败而被 tracing::warn! 一行吞掉，刷新就丢，
        //   是用户感知"卡住但其实在跑"的最大来源。
        // - 加 meta TEXT NULL 列：tool_use / tool_result 之类不再只能存裸字符串，
        //   后端可塞 {tool, input, output_summary, is_error, duration_ms, tool_use_id} JSON。
        // - 新增 agent_todos 表：FM-15 uplift Phase 1.2 用，让 Agent 自己维护待办清单。
        "025_single_agent_uplift_events_and_todos",
        r#"
        PRAGMA foreign_keys=OFF;

        CREATE TABLE agent_events_new (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            step INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL
                CHECK (kind IN (
                    'llm_call', 'tool_use', 'tool_result', 'checkpoint',
                    'error', 'message', 'status_change', 'review',
                    'system_hint', 'guardrail_pass', 'guardrail_fail',
                    'guardrail_summary', 'note_applied',
                    'tool_progress', 'tool_summary', 'compact', 'todo_update'
                )),
            content TEXT NOT NULL DEFAULT '',
            meta TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        INSERT INTO agent_events_new (id, agent_id, step, kind, content, created_at)
            SELECT id, agent_id, step, kind, content, created_at FROM agent_events;

        DROP TABLE agent_events;
        ALTER TABLE agent_events_new RENAME TO agent_events;

        CREATE INDEX IF NOT EXISTS idx_agent_events_agent ON agent_events(agent_id, created_at);

        PRAGMA foreign_keys=ON;

        -- agent_todos: 持久化 TodoWriteTool 写出的清单。
        -- 一个 agent 一组 todo；Agent 每次写都全量替换（语义和 Cursor / Claude Code 一致）。
        -- order_idx 决定渲染顺序；id 由 agent 端生成（uuid 或自增整数都可），同一 agent 内唯一。
        CREATE TABLE IF NOT EXISTS agent_todos (
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            id TEXT NOT NULL,
            order_idx INTEGER NOT NULL DEFAULT 0,
            content TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'in_progress', 'completed', 'cancelled')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (agent_id, id)
        );

        CREATE INDEX IF NOT EXISTS idx_agent_todos_agent ON agent_todos(agent_id, order_idx);
        "#,
    ),
    (
        // Single-Agent Uplift P0-3: Withhold-then-Recover 引入两个新 event kind。
        //
        // - `recovery_attempt`：可恢复错误（prompt_too_long / max_output_tokens / idle）
        //   触发后，agent 启动恢复流程时发的事件。meta.silent=true → 前端默认隐藏。
        // - `recovery_succeeded`：恢复路径走通后发的"什么都没发生"通知。同 silent。
        //
        // 为什么必须新增 kind 而非复用 system_hint：system_hint 默认前端可见——把
        // 静默恢复事件塞进 system_hint 会让用户看到一堆"你不需要关心"的噪音。新 kind
        // 让前端能按 kind 决策是否渲染（详见 `agent/recovery_log.rs` 文档）。
        "026_p03_recovery_event_kinds",
        r#"
        PRAGMA foreign_keys=OFF;

        CREATE TABLE agent_events_new (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            step INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL
                CHECK (kind IN (
                    'llm_call', 'tool_use', 'tool_result', 'checkpoint',
                    'error', 'message', 'status_change', 'review',
                    'system_hint', 'guardrail_pass', 'guardrail_fail',
                    'guardrail_summary', 'note_applied',
                    'tool_progress', 'tool_summary', 'compact', 'todo_update',
                    'recovery_attempt', 'recovery_succeeded'
                )),
            content TEXT NOT NULL DEFAULT '',
            meta TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        INSERT INTO agent_events_new (id, agent_id, step, kind, content, meta, created_at)
            SELECT id, agent_id, step, kind, content, meta, created_at FROM agent_events;

        DROP TABLE agent_events;
        ALTER TABLE agent_events_new RENAME TO agent_events;

        CREATE INDEX IF NOT EXISTS idx_agent_events_agent ON agent_events(agent_id, created_at);

        PRAGMA foreign_keys=ON;
        "#,
    ),
    (
        // Single-Agent Uplift P1-2 (Phase B): agents 表加 `fallback_switches_total`
        // 累计 cross-model fallback 切换次数，给 mission report 渲染 + 长期统计用。
        //
        // 为什么持久化而非只在 events 表里查：events 可能因日志保留策略被清理；
        // 这一列是 agent 的"质量指标"，应该跟随 agent 行本身永久存活。
        // 0 = 没触发过 fallback（绝大多数情况），>0 时 report 渲染 chip。
        "027_p12_fallback_switches",
        r#"
        ALTER TABLE agents ADD COLUMN fallback_switches_total INTEGER NOT NULL DEFAULT 0;
        "#,
    ),
    (
        // Single-Agent Uplift P2-1 Phase B：通用 Stop Hook 体系。
        // 加 3 个新 event kind：
        //   - `hook_executed`：hook Pass 时记录（meta 含 hook_name / phase / duration_ms）
        //   - `hook_inject`：hook InjectMessage 时（meta 含 content / severity）
        //   - `hook_prevented`：hook PreventContinuation 时（meta 含 reason / terminal）
        //
        // 为什么单独的 kind 而非塞 system_hint：hook 事件需要前端按 phase 分组渲染
        // （未来 dashboard "show me all hook activity per agent"），独立 kind 让查询
        // 简单一倍 + 不污染 system_hint 的可读语义。
        "028_p21_hook_event_kinds",
        r#"
        PRAGMA foreign_keys=OFF;

        CREATE TABLE agent_events_new (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            step INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL
                CHECK (kind IN (
                    'llm_call', 'tool_use', 'tool_result', 'checkpoint',
                    'error', 'message', 'status_change', 'review',
                    'system_hint', 'guardrail_pass', 'guardrail_fail',
                    'guardrail_summary', 'note_applied',
                    'tool_progress', 'tool_summary', 'compact', 'todo_update',
                    'recovery_attempt', 'recovery_succeeded',
                    'hook_executed', 'hook_inject', 'hook_prevented'
                )),
            content TEXT NOT NULL DEFAULT '',
            meta TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        INSERT INTO agent_events_new (id, agent_id, step, kind, content, meta, created_at)
            SELECT id, agent_id, step, kind, content, meta, created_at FROM agent_events;

        DROP TABLE agent_events;
        ALTER TABLE agent_events_new RENAME TO agent_events;

        CREATE INDEX IF NOT EXISTS idx_agent_events_agent ON agent_events(agent_id, created_at);

        PRAGMA foreign_keys=ON;
        "#,
    ),
    (
        // Explicit Merge Node v1：把"多 parent worktree 合并"从隐式基础设施步骤
        // 升级为 DAG 一等公民节点。设计文档：
        // docs/research/explicit-merge-node/proposal.md
        //
        // 数据模型只加 3 列：
        //   - tasks.kind：'work'（普通任务，默认）| 'merge'（合并节点）
        //   - tasks.merge_parents：merge 节点的 2 个 parent task id（JSON 数组）；
        //     work 节点为 NULL。**只存 2 个**——分层 reduction tree 算法把 N parents
        //     拆成 N-1 个二元 merge node，每个 merge agent 上下文小、可调试。
        //   - missions.verify_command：可选，mission 级 build/lint/test 命令。
        //     merge 节点 task_complete 时通过 Guardrail::CommandPasses 强制跑过且
        //     exit=0 才放行。未配时 scheduler 按 codebase_intel 推断的 repo type
        //     兜底（Rust→`cargo check`，Node→`npm run build`，其他→空 = 不强校验）。
        //
        // 默认所有现有 mission 不受影响：tasks.kind='work'、verify_command=NULL；
        // 是否启用显式 merge 由 AppConfig.enable_explicit_merge_node 顶层开关
        // （加在 commands/config.rs，非 schema 层）。这一 migration 只准备能力，
        // 不改变行为。
        "029_explicit_merge_node",
        r#"
        ALTER TABLE tasks ADD COLUMN kind TEXT NOT NULL DEFAULT 'work'
            CHECK (kind IN ('work', 'merge'));
        ALTER TABLE tasks ADD COLUMN merge_parents TEXT;
        ALTER TABLE missions ADD COLUMN verify_command TEXT;
        "#,
    ),
    (
        // Coding Agent Benchmark Evaluation Harness:
        // - suite/case registry for imported GA-style datasets
        // - run/result tables for reproducible benchmark execution
        // - metric snapshots and grader artifacts for task completion, token efficiency,
        //   and tool-use efficiency analysis
        "030_benchmark_evaluation",
        r#"
        CREATE TABLE IF NOT EXISTS benchmark_suites (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            source_kind TEXT NOT NULL
                CHECK (source_kind IN ('ga_tool_efficiency', 'ga_sop_bench', 'ga_lifelong_agentbench', 'ga_realfin_benchmark', 'custom')),
            source_path TEXT NOT NULL,
            source_ref TEXT,
            manifest_json TEXT NOT NULL DEFAULT '{}',
            case_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_benchmark_suites_source_kind
            ON benchmark_suites(source_kind);

        CREATE TABLE IF NOT EXISTS benchmark_cases (
            id TEXT PRIMARY KEY,
            suite_id TEXT NOT NULL REFERENCES benchmark_suites(id) ON DELETE CASCADE,
            task_id TEXT NOT NULL,
            task_type TEXT NOT NULL DEFAULT '',
            source_suite TEXT NOT NULL DEFAULT '',
            target_tool_or_capability TEXT NOT NULL DEFAULT '',
            prompt TEXT NOT NULL,
            assets_json TEXT NOT NULL DEFAULT '[]',
            expected_outputs_json TEXT NOT NULL DEFAULT '[]',
            grader_json TEXT,
            expected_output TEXT,
            raw_json TEXT NOT NULL DEFAULT '{}',
            case_hash TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(suite_id, task_id)
        );
        CREATE INDEX IF NOT EXISTS idx_benchmark_cases_suite
            ON benchmark_cases(suite_id, task_id);
        CREATE INDEX IF NOT EXISTS idx_benchmark_cases_type
            ON benchmark_cases(task_type);

        CREATE TABLE IF NOT EXISTS benchmark_runs (
            id TEXT PRIMARY KEY,
            suite_id TEXT NOT NULL REFERENCES benchmark_suites(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'created'
                CHECK (status IN ('created', 'running', 'completed', 'completed_with_failures', 'failed', 'cancelled', 'timeout')),
            agent_kind TEXT NOT NULL DEFAULT 'coding'
                CHECK (agent_kind IN ('coding', 'planner', 'evaluator', 'chat')),
            provider TEXT NOT NULL DEFAULT '',
            model TEXT NOT NULL DEFAULT '',
            base_url_hash TEXT,
            agent_config_json TEXT NOT NULL DEFAULT '{}',
            git_commit TEXT,
            git_dirty INTEGER NOT NULL DEFAULT 0,
            benchmark_source_path TEXT NOT NULL DEFAULT '',
            case_ids_json TEXT NOT NULL DEFAULT '[]',
            timeout_seconds INTEGER,
            max_steps INTEGER,
            token_budget INTEGER,
            cost_budget_usd REAL,
            workspace_root TEXT,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            started_at TEXT,
            completed_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_benchmark_runs_suite
            ON benchmark_runs(suite_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_benchmark_runs_status
            ON benchmark_runs(status);

        CREATE TABLE IF NOT EXISTS benchmark_results (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES benchmark_runs(id) ON DELETE CASCADE,
            case_id TEXT NOT NULL REFERENCES benchmark_cases(id) ON DELETE CASCADE,
            agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
            workspace_path TEXT,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled', 'timeout', 'unsupported')),
            success INTEGER,
            grading_status TEXT NOT NULL DEFAULT 'not_started'
                CHECK (grading_status IN ('not_started', 'passed', 'failed', 'ungraded')),
            final_response TEXT,
            artifact_refs_json TEXT NOT NULL DEFAULT '[]',
            error_message TEXT,
            started_at TEXT,
            completed_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(run_id, case_id)
        );
        CREATE INDEX IF NOT EXISTS idx_benchmark_results_run
            ON benchmark_results(run_id, status);
        CREATE INDEX IF NOT EXISTS idx_benchmark_results_case
            ON benchmark_results(case_id);
        CREATE INDEX IF NOT EXISTS idx_benchmark_results_agent
            ON benchmark_results(agent_id);

        CREATE TABLE IF NOT EXISTS benchmark_metric_snapshots (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES benchmark_runs(id) ON DELETE CASCADE,
            result_id TEXT REFERENCES benchmark_results(id) ON DELETE CASCADE,
            scope TEXT NOT NULL
                CHECK (scope IN ('case', 'run')),
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            cost_usd REAL NOT NULL DEFAULT 0.0,
            llm_request_count INTEGER NOT NULL DEFAULT 0,
            tool_call_count INTEGER NOT NULL DEFAULT 0,
            tool_result_count INTEGER NOT NULL DEFAULT 0,
            tool_error_count INTEGER NOT NULL DEFAULT 0,
            tool_call_count_by_name_json TEXT NOT NULL DEFAULT '{}',
            runtime_ms INTEGER,
            successful_case_count INTEGER,
            graded_case_count INTEGER,
            total_case_count INTEGER,
            all_cases_tsr REAL,
            graded_cases_tsr REAL,
            token_per_success REAL,
            tool_calls_per_success REAL,
            requests_per_success REAL,
            tool_error_rate REAL,
            guardrail_retry_count INTEGER NOT NULL DEFAULT 0,
            recovery_attempt_count INTEGER NOT NULL DEFAULT 0,
            read_only_loop_hint_count INTEGER NOT NULL DEFAULT 0,
            raw_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_benchmark_metrics_run
            ON benchmark_metric_snapshots(run_id, scope, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_benchmark_metrics_result
            ON benchmark_metric_snapshots(result_id, created_at DESC);

        CREATE TABLE IF NOT EXISTS benchmark_grader_artifacts (
            id TEXT PRIMARY KEY,
            result_id TEXT NOT NULL REFERENCES benchmark_results(id) ON DELETE CASCADE,
            grader_kind TEXT NOT NULL DEFAULT 'python',
            command_json TEXT NOT NULL DEFAULT '[]',
            exit_code INTEGER,
            stdout_json TEXT,
            stderr TEXT NOT NULL DEFAULT '',
            duration_ms INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_benchmark_grader_result
            ON benchmark_grader_artifacts(result_id, created_at DESC);
        "#,
    ),
    (
        "036_benchmark_context_policy_metrics",
        r#"
        ALTER TABLE benchmark_metric_snapshots ADD COLUMN context_saved_chars INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE benchmark_metric_snapshots ADD COLUMN tool_result_ref_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE benchmark_metric_snapshots ADD COLUMN tool_result_repeat_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE benchmark_metric_snapshots ADD COLUMN evidence_read_ref_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE benchmark_metric_snapshots ADD COLUMN shell_content_command_count INTEGER NOT NULL DEFAULT 0;
        "#,
    ),
    (
        "037_tool_result_policy_event_kind",
        r#"
        PRAGMA foreign_keys=OFF;

        CREATE TABLE agent_events_new (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            step INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL
                CHECK (kind IN (
                    'llm_call', 'tool_use', 'tool_result', 'checkpoint',
                    'error', 'message', 'status_change', 'review',
                    'system_hint', 'guardrail_pass', 'guardrail_fail',
                    'guardrail_summary', 'note_applied',
                    'tool_progress', 'tool_summary', 'compact', 'todo_update',
                    'recovery_attempt', 'recovery_succeeded',
                    'hook_executed', 'hook_inject', 'hook_prevented',
                    'tool_result_policy'
                )),
            content TEXT NOT NULL DEFAULT '',
            meta TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        INSERT INTO agent_events_new (id, agent_id, step, kind, content, meta, created_at)
            SELECT id, agent_id, step, kind, content, meta, created_at FROM agent_events;

        DROP TABLE agent_events;
        ALTER TABLE agent_events_new RENAME TO agent_events;

        CREATE INDEX IF NOT EXISTS idx_agent_events_agent ON agent_events(agent_id, created_at);

        PRAGMA foreign_keys=ON;
        "#,
    ),
    (
        "038_benchmark_tool_result_budget_metrics",
        r#"
        ALTER TABLE benchmark_metric_snapshots ADD COLUMN persisted_tool_result_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE benchmark_metric_snapshots ADD COLUMN per_message_budget_replacement_count INTEGER NOT NULL DEFAULT 0;
        "#,
    ),
    (
        "039_benchmark_contract_validation_metrics",
        r#"
        ALTER TABLE benchmark_metric_snapshots ADD COLUMN contract_validation_attempt_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE benchmark_metric_snapshots ADD COLUMN contract_violation_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE benchmark_metric_snapshots ADD COLUMN contract_repair_retry_count INTEGER NOT NULL DEFAULT 0;
        "#,
    ),
    (
        "040_contract_validation_event_kinds",
        r#"
        PRAGMA foreign_keys=OFF;

        CREATE TABLE agent_events_new (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            step INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL
                CHECK (kind IN (
                    'llm_call', 'tool_use', 'tool_result', 'checkpoint',
                    'error', 'message', 'status_change', 'review',
                    'system_hint', 'guardrail_pass', 'guardrail_fail',
                    'guardrail_summary', 'note_applied',
                    'tool_progress', 'tool_summary', 'compact', 'todo_update',
                    'recovery_attempt', 'recovery_succeeded',
                    'hook_executed', 'hook_inject', 'hook_prevented',
                    'tool_result_policy', 'contract_pass', 'contract_fail'
                )),
            content TEXT NOT NULL DEFAULT '',
            meta TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        INSERT INTO agent_events_new (id, agent_id, step, kind, content, meta, created_at)
            SELECT id, agent_id, step, kind, content, meta, created_at FROM agent_events;

        DROP TABLE agent_events;
        ALTER TABLE agent_events_new RENAME TO agent_events;

        CREATE INDEX IF NOT EXISTS idx_agent_events_agent ON agent_events(agent_id, created_at);

        PRAGMA foreign_keys=ON;
        "#,
    ),
    (
        "041_add_context_stats_assistant_text_event_kinds",
        r#"
        PRAGMA foreign_keys=OFF;

        CREATE TABLE agent_events_new (
            id TEXT PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
            step INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL
                CHECK (kind IN (
                    'llm_call', 'tool_use', 'tool_result', 'checkpoint',
                    'error', 'message', 'status_change', 'review',
                    'system_hint', 'guardrail_pass', 'guardrail_fail',
                    'guardrail_summary', 'note_applied',
                    'tool_progress', 'tool_summary', 'compact', 'todo_update',
                    'recovery_attempt', 'recovery_succeeded',
                    'hook_executed', 'hook_inject', 'hook_prevented',
                    'tool_result_policy', 'contract_pass', 'contract_fail',
                    'context_stats', 'assistant_text'
                )),
            content TEXT NOT NULL DEFAULT '',
            meta TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        INSERT INTO agent_events_new (id, agent_id, step, kind, content, meta, created_at)
            SELECT id, agent_id, step, kind, content, meta, created_at FROM agent_events;
        DROP TABLE agent_events;
        ALTER TABLE agent_events_new RENAME TO agent_events;

        CREATE INDEX IF NOT EXISTS idx_agent_events_agent ON agent_events(agent_id, created_at);

        PRAGMA foreign_keys=ON;
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
            conn.execute("INSERT INTO schema_migrations (name) VALUES (?)", [name])?;
            tracing::info!("Applied migration: {name}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod migration_023_tests {
    //! 回归测试：023 一次性回收 `signed` 但 mission 下无 tasks 的脏 contract。
    //!
    //! 见 retryable-flow.mdc rule 1。此 migration 表达的不变量：
    //!   contract.status = 'signed'  ==>  EXISTS tasks WHERE mission_id = …
    //!
    //! 测试覆盖：① 真脏数据被回退；② 成功签约的 mission 不被误伤；③ 幂等。
    use super::run;
    use rusqlite::Connection;

    /// 跑除了 023 之外的所有 migration，再插入脏数据，再跑 023。
    /// 这样能精确观察 023 单独的效果，避免被其他 migration 覆盖。
    fn setup_db_pre_023() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        // 跑全套 migration；023 此时会扫描，但表里啥都没有，等价于 no-op
        run(&conn).unwrap();
        // 删 023 的记录，让我们后面手动 setup 脏数据后能再跑一次 023
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = '023_retryable_flow_recover_stuck_signed_contracts'",
            [],
        ).unwrap();
        conn
    }

    fn read_contract_status(conn: &Connection, contract_id: &str) -> (String, Option<String>) {
        conn.query_row(
            "SELECT status, signed_at FROM mission_contracts WHERE id = ?",
            [contract_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .unwrap()
    }

    fn insert_mission(conn: &Connection, id: &str, status: &str) {
        conn.execute(
            "INSERT INTO missions (id, title, description, status) VALUES (?, ?, ?, ?)",
            rusqlite::params![id, "T", "D", status],
        )
        .unwrap();
    }

    fn insert_contract(
        conn: &Connection,
        id: &str,
        mission_id: &str,
        status: &str,
        signed_at: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO mission_contracts (id, mission_id, status, signed_at) VALUES (?, ?, ?, ?)",
            rusqlite::params![id, mission_id, status, signed_at],
        )
        .unwrap();
    }

    fn insert_task(conn: &Connection, id: &str, mission_id: &str) {
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, description, complexity, status) VALUES (?, ?, ?, ?, ?, 'pending')",
            rusqlite::params![id, mission_id, "task title", "desc", "low"],
        ).unwrap();
    }

    /// 真脏数据：mission='preflight' + contract='signed' + 0 tasks → 必须回退。
    /// 这正是用户机器上 26d64c5d 的形态。
    #[test]
    fn rolls_back_stuck_signed_contract_with_no_tasks() {
        let conn = setup_db_pre_023();
        insert_mission(&conn, "m-stuck", "preflight");
        insert_contract(
            &conn,
            "c-stuck",
            "m-stuck",
            "signed",
            Some("2025-05-12 00:00:00"),
        );
        // 也插入若干 contract_items，断言回退后不丢
        conn.execute(
            "INSERT INTO contract_items (id, contract_id, section, text) VALUES (?, ?, 'scope', ?)",
            rusqlite::params!["ci-1", "c-stuck", "build calculator"],
        )
        .unwrap();

        run(&conn).unwrap();

        let (status, signed_at) = read_contract_status(&conn, "c-stuck");
        assert_eq!(
            status, "drafting",
            "脏 contract 必须回退到 drafting，否则签约按钮永久消失"
        );
        assert!(signed_at.is_none(), "回退后 signed_at 必须清空");

        let item_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM contract_items WHERE contract_id = 'c-stuck'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            item_count, 1,
            "contract_items 必须保留 —— 用户写的 scope 不能丢"
        );
    }

    /// 成功签约的 mission（mission.status='planned' + 有 tasks）→ 必须保持 signed。
    /// 守住"不误伤"的边界。
    #[test]
    fn does_not_touch_legitimately_signed_mission() {
        let conn = setup_db_pre_023();
        insert_mission(&conn, "m-ok", "planned");
        insert_contract(&conn, "c-ok", "m-ok", "signed", Some("2025-05-12 00:00:00"));
        insert_task(&conn, "t-ok", "m-ok");

        run(&conn).unwrap();

        let (status, signed_at) = read_contract_status(&conn, "c-ok");
        assert_eq!(
            status, "signed",
            "已成功签约 + 有 tasks 的 contract 不能被回退"
        );
        assert!(signed_at.is_some(), "signed_at 必须保留");
    }

    /// 边界 case：mission='preflight' + contract='signed' + 居然有 tasks。
    /// 实际不会发生，但 migration 谨慎一些 —— 既然有 tasks，就当 sign 已完成，不动。
    #[test]
    fn does_not_touch_signed_preflight_with_tasks_present() {
        let conn = setup_db_pre_023();
        insert_mission(&conn, "m-weird", "preflight");
        insert_contract(
            &conn,
            "c-weird",
            "m-weird",
            "signed",
            Some("2025-05-12 00:00:00"),
        );
        insert_task(&conn, "t-weird", "m-weird");

        run(&conn).unwrap();

        let (status, _) = read_contract_status(&conn, "c-weird");
        assert_eq!(status, "signed", "有 tasks 时不视为脏数据");
    }

    /// 幂等：连续跑两次 migration（人为复位 schema_migrations）效果相同。
    #[test]
    fn is_idempotent() {
        let conn = setup_db_pre_023();
        insert_mission(&conn, "m-idem", "preflight");
        insert_contract(
            &conn,
            "c-idem",
            "m-idem",
            "signed",
            Some("2025-05-12 00:00:00"),
        );

        run(&conn).unwrap();
        let (status1, _) = read_contract_status(&conn, "c-idem");

        // 让 023 再跑一次
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = '023_retryable_flow_recover_stuck_signed_contracts'",
            [],
        ).unwrap();
        run(&conn).unwrap();
        let (status2, _) = read_contract_status(&conn, "c-idem");

        assert_eq!(status1, "drafting");
        assert_eq!(status2, "drafting");
    }
}

#[cfg(test)]
mod migration_024_tests {
    //! 回归测试：024 给 task_dependencies 加 kind 列 + backfill。
    //!
    //! 表达的不变量：
    //!   "边上 artifact_refs 解析到的 artifact_type 全部 ∈ doc-set"  ==>  kind='reference'
    //!   否则  ==>  kind='producer'
    //!
    //! 覆盖：① doc-only 边被标 reference；② 含 code 的边保持 producer；
    //! ③ 空 artifact_refs 的纯拓扑边保持 producer；④ ALTER 不破坏旧索引。
    use super::run;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        run(&conn).unwrap();
        conn
    }

    fn insert_mission(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO missions (id, title, description, status) VALUES (?, ?, ?, 'planned')",
            rusqlite::params![id, "T", "D"],
        )
        .unwrap();
    }

    fn insert_task_with_produces(
        conn: &Connection,
        id: &str,
        mission_id: &str,
        produces_json: &str,
    ) {
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, description, complexity, status, produces_artifacts)
             VALUES (?, ?, ?, ?, ?, 'pending', ?)",
            rusqlite::params![id, mission_id, "title", "desc", "low", produces_json],
        )
        .unwrap();
    }

    fn insert_dep(conn: &Connection, task_id: &str, depends_on: &str, refs_json: &str) {
        // 注意：迁移已经跑过，所以这里必须显式不写 kind（让 DEFAULT 生效），
        // 但 backfill 已在 migration 内完成 —— 我们要的是 backfill 完之后再插，
        // 故手动复跑 backfill 的核心 UPDATE 来验证逻辑（migration 不会重跑）。
        conn.execute(
            "INSERT INTO task_dependencies (task_id, depends_on, artifact_refs) VALUES (?, ?, ?)",
            rusqlite::params![task_id, depends_on, refs_json],
        )
        .unwrap();
    }

    /// 把 migration 024 的 backfill 子句单独再跑一次，覆盖手动 insert 的行。
    /// 与 migration 内的 UPDATE 语句保持一致，否则就是测试在演戏。
    fn rerun_backfill(conn: &Connection) {
        conn.execute_batch(
            r#"
            WITH ref_types AS (
                SELECT
                    td.task_id,
                    td.depends_on,
                    json_extract(
                        t_up.produces_artifacts,
                        '$[' || (
                            SELECT key FROM json_each(t_up.produces_artifacts)
                            WHERE json_extract(value, '$.local_name') = substr(
                                json_each_refs.value,
                                instr(json_each_refs.value, '.') + 1
                            )
                            LIMIT 1
                        ) || '].type'
                    ) AS artifact_type
                FROM task_dependencies td
                JOIN tasks t_up ON t_up.id = td.depends_on
                JOIN json_each(td.artifact_refs) AS json_each_refs
            ),
            edge_classification AS (
                SELECT
                    task_id,
                    depends_on,
                    COUNT(*) AS total_refs,
                    SUM(CASE WHEN artifact_type IN
                        ('design_doc', 'api_spec', 'schema', 'docs', 'report')
                        THEN 1 ELSE 0 END) AS doc_refs
                FROM ref_types
                GROUP BY task_id, depends_on
            )
            UPDATE task_dependencies
            SET kind = 'reference'
            WHERE (task_id, depends_on) IN (
                SELECT task_id, depends_on
                FROM edge_classification
                WHERE total_refs > 0 AND doc_refs = total_refs
            );
            "#,
        )
        .unwrap();
    }

    fn read_kind(conn: &Connection, task_id: &str, depends_on: &str) -> String {
        conn.query_row(
            "SELECT kind FROM task_dependencies WHERE task_id = ? AND depends_on = ?",
            rusqlite::params![task_id, depends_on],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// 上游产 design_doc，边只携带这一个 doc → reference。
    #[test]
    fn classifies_doc_only_edge_as_reference() {
        let conn = setup_db();
        insert_mission(&conn, "m1");
        insert_task_with_produces(
            &conn,
            "architect",
            "m1",
            r#"[{"local_name":"architecture_doc","type":"design_doc","summary":"x"}]"#,
        );
        insert_task_with_produces(&conn, "impl", "m1", "[]");
        insert_dep(
            &conn,
            "impl",
            "architect",
            r#"["architect.architecture_doc"]"#,
        );

        rerun_backfill(&conn);

        assert_eq!(read_kind(&conn, "impl", "architect"), "reference");
    }

    /// 边携带 code_module → producer（实物依赖必须保持 producer 默认）。
    #[test]
    fn classifies_code_module_edge_as_producer() {
        let conn = setup_db();
        insert_mission(&conn, "m1");
        insert_task_with_produces(
            &conn,
            "engine",
            "m1",
            r#"[{"local_name":"engine_module","type":"code_module","summary":"x"}]"#,
        );
        insert_task_with_produces(&conn, "ui", "m1", "[]");
        insert_dep(&conn, "ui", "engine", r#"["engine.engine_module"]"#);

        rerun_backfill(&conn);

        assert_eq!(read_kind(&conn, "ui", "engine"), "producer");
    }

    /// 混合边：一个 doc + 一个 code 同时被消费 → producer（保守，任一非 doc 即视为实物依赖）。
    #[test]
    fn classifies_mixed_edge_as_producer() {
        let conn = setup_db();
        insert_mission(&conn, "m1");
        insert_task_with_produces(
            &conn,
            "src",
            "m1",
            r#"[{"local_name":"doc1","type":"design_doc","summary":""},
                {"local_name":"code1","type":"code_module","summary":""}]"#,
        );
        insert_task_with_produces(&conn, "dst", "m1", "[]");
        insert_dep(&conn, "dst", "src", r#"["src.doc1","src.code1"]"#);

        rerun_backfill(&conn);

        assert_eq!(read_kind(&conn, "dst", "src"), "producer");
    }

    /// 空 artifact_refs（老/纯拓扑边）→ producer。绝对不能误标 reference 而被 UI 隐藏。
    #[test]
    fn classifies_empty_refs_edge_as_producer() {
        let conn = setup_db();
        insert_mission(&conn, "m1");
        insert_task_with_produces(&conn, "a", "m1", "[]");
        insert_task_with_produces(&conn, "b", "m1", "[]");
        insert_dep(&conn, "b", "a", "[]");

        rerun_backfill(&conn);

        assert_eq!(read_kind(&conn, "b", "a"), "producer");
    }

    /// api_spec / schema / docs / report 同样属于 doc-set。
    #[test]
    fn classifies_all_doc_like_types_as_reference() {
        let conn = setup_db();
        insert_mission(&conn, "m1");
        for (i, ty) in ["api_spec", "schema", "docs", "report"].iter().enumerate() {
            let up = format!("up{i}");
            let down = format!("dn{i}");
            insert_task_with_produces(
                &conn,
                &up,
                "m1",
                &format!(
                    r#"[{{"local_name":"a","type":"{ty}","summary":""}}]"#,
                    ty = ty
                ),
            );
            insert_task_with_produces(&conn, &down, "m1", "[]");
            insert_dep(&conn, &down, &up, &format!(r#"["{up}.a"]"#));
        }

        rerun_backfill(&conn);

        for i in 0..4 {
            assert_eq!(
                read_kind(&conn, &format!("dn{i}"), &format!("up{i}")),
                "reference"
            );
        }
    }
}

#[cfg(test)]
mod migration_041_tests {
    //! 回归测试：041 扩展 agent_events CHECK 约束，新增 context_stats 和 assistant_text。
    //!
    //! 测试覆盖：① 旧数据迁移后仍保留；② 新 kind 可插入；③ 旧 kind 仍可插入；④ 无效 kind 被拒绝。
    use super::{run, MIGRATIONS};
    use crate::db::queries::{get_events_for_agent, insert_agent, insert_event_with_meta};
    use rusqlite::Connection;

    /// 跑到 040 为止，插入旧数据，再跑 041。
    fn setup_db_pre_041() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS schema_migrations (
                 name TEXT PRIMARY KEY,
                 applied_at TEXT NOT NULL DEFAULT (datetime('now'))
             );",
        )
        .unwrap();
        for (name, sql) in MIGRATIONS {
            if *name == "041_add_context_stats_assistant_text_event_kinds" {
                break;
            }
            conn.execute_batch(sql).unwrap();
            conn.execute("INSERT INTO schema_migrations (name) VALUES (?)", [*name])
                .unwrap();
        }
        conn
    }

    fn event_created_at(index: usize) -> String {
        format!("2026-01-01 00:00:{index:02}")
    }

    /// 真实 pre-041 fixture 在跑 041 之前必须还不允许新 kind。
    #[test]
    fn pre_041_fixture_rejects_new_kinds_before_migration() {
        let conn = setup_db_pre_041();
        insert_agent(&conn, "agent-pre", "test").unwrap();

        let err =
            insert_event_with_meta(&conn, "ev-pre", "agent-pre", 1, "context_stats", "x", None)
                .unwrap_err();

        assert!(
            err.to_string().contains("CHECK constraint failed"),
            "pre-041 schema must reject context_stats via CHECK, got {err}"
        );
    }

    fn count_events(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM agent_events", [], |row| row.get(0))
            .unwrap()
    }

    /// 旧 agent_events 数据迁移后仍保留，包括 040 新增 kind 与所有复制列。
    #[test]
    fn preserves_old_data_after_migration() {
        const OLD_KINDS: &[&str] = &[
            "llm_call",
            "tool_use",
            "tool_result",
            "checkpoint",
            "error",
            "message",
            "status_change",
            "review",
            "system_hint",
            "guardrail_pass",
            "guardrail_fail",
            "guardrail_summary",
            "note_applied",
            "tool_progress",
            "tool_summary",
            "compact",
            "todo_update",
            "recovery_attempt",
            "recovery_succeeded",
            "hook_executed",
            "hook_inject",
            "hook_prevented",
            "tool_result_policy",
            "contract_pass",
            "contract_fail",
        ];

        let conn = setup_db_pre_041();
        insert_agent(&conn, "agent-1", "test").unwrap();
        for (idx, kind) in OLD_KINDS.iter().enumerate() {
            let id = format!("ev-old-{idx}");
            let content = format!("content-{kind}");
            let meta = format!(r#"{{"kind":"{kind}","idx":{idx}}}"#);
            insert_event_with_meta(
                &conn,
                &id,
                "agent-1",
                idx as i64 + 10,
                kind,
                &content,
                Some(&meta),
            )
            .unwrap();
            conn.execute(
                "UPDATE agent_events SET created_at = ? WHERE id = ?",
                rusqlite::params![event_created_at(idx), id],
            )
            .unwrap();
        }

        assert_eq!(
            count_events(&conn),
            OLD_KINDS.len() as i64,
            "events before migration"
        );

        run(&conn).unwrap();

        let events = get_events_for_agent(&conn, "agent-1").unwrap();
        assert_eq!(
            events.len(),
            OLD_KINDS.len(),
            "all events survive migration"
        );
        for (idx, kind) in OLD_KINDS.iter().enumerate() {
            let event = events
                .iter()
                .find(|event| event.id == format!("ev-old-{idx}"))
                .unwrap_or_else(|| panic!("missing ev-old-{idx}"));
            assert_eq!(event.agent_id, "agent-1");
            assert_eq!(event.step, idx as i64 + 10);
            assert_eq!(event.kind, *kind);
            assert_eq!(event.content, format!("content-{kind}"));
            assert_eq!(
                event.meta.as_deref(),
                Some(format!(r#"{{"kind":"{kind}","idx":{idx}}}"#).as_str())
            );
            assert_eq!(event.created_at, event_created_at(idx));
        }
    }

    /// 新 kind context_stats 和 assistant_text 可以通过真实 query helper 插入读取。
    #[test]
    fn allows_new_kinds_context_stats_and_assistant_text() {
        let conn = setup_db_pre_041();
        run(&conn).unwrap();
        insert_agent(&conn, "agent-2", "test").unwrap();

        insert_event_with_meta(
            &conn,
            "ev-cs",
            "agent-2",
            1,
            "context_stats",
            r#"{"tokens":100}"#,
            Some(r#"{"source":"test"}"#),
        )
        .unwrap();
        insert_event_with_meta(
            &conn,
            "ev-at",
            "agent-2",
            2,
            "assistant_text",
            "hello",
            None,
        )
        .unwrap();

        let events = get_events_for_agent(&conn, "agent-2").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "context_stats");
        assert_eq!(events[0].content, r#"{"tokens":100}"#);
        assert_eq!(events[0].meta.as_deref(), Some(r#"{"source":"test"}"#));
        assert_eq!(events[1].kind, "assistant_text");
        assert_eq!(events[1].content, "hello");
    }

    /// 040 新增 kind 迁移后仍可插入。
    #[test]
    fn allows_contract_validation_kinds_after_migration() {
        let conn = setup_db_pre_041();
        run(&conn).unwrap();
        insert_agent(&conn, "agent-3", "test").unwrap();

        insert_event_with_meta(&conn, "ev-pass", "agent-3", 1, "contract_pass", "ok", None)
            .unwrap();
        insert_event_with_meta(&conn, "ev-fail", "agent-3", 2, "contract_fail", "bad", None)
            .unwrap();

        let events = get_events_for_agent(&conn, "agent-3").unwrap();
        assert_eq!(events[0].kind, "contract_pass");
        assert_eq!(events[1].kind, "contract_fail");
    }

    /// 无效 kind 被 CHECK 约束拒绝。
    #[test]
    fn rejects_invalid_kind() {
        let conn = setup_db_pre_041();
        run(&conn).unwrap();
        insert_agent(&conn, "agent-4", "test").unwrap();

        let err = insert_event_with_meta(&conn, "ev-bad", "agent-4", 1, "invalid_kind", "x", None)
            .unwrap_err();
        assert!(
            err.to_string().contains("CHECK constraint failed"),
            "invalid kind must be rejected by CHECK constraint, got {err}"
        );
    }
}
