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
