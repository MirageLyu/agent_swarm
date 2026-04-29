//! FM-15 FR-05 (S1 子集): Planner Agent Loop 的工具集与执行器。
//!
//! S1 范围（最小可用集）：
//! - **A 探索类**：`list_directory`, `read_file`（read-only，受黑名单 / 大小约束）
//! - **B 元数据类**：`list_roles`
//! - **C 构建类**：`propose_task`, `add_dependency`, `revise_task`, `drop_task`
//! - **D 校验类**：`validate_plan`
//! - **E 终止类**：`finalize_plan`
//!
//! S2/S3 增量加入：`search_code`, `detect_tech_stack`, `query_skills`,
//! `get_skill_detail`, `fetch_url`, `get_contract*`, `publish_artifact`。
//!
//! Planner 是 read-only Agent：**绝不**装载 `write_file` / `shell_exec` 等写操作工具。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use crate::agent::planner::PlannerOutput;
use crate::agent::planner_fetch::{
    self, FetchError, FetchPolicy,
};
use crate::agent::planner_state::{
    PlannerState, PlannerStateError, ProposeTaskInput, ReviseTaskInput,
};
use crate::agent::roles;
use crate::db::Database;
use crate::llm::ToolDefinition;
use crate::tools::ToolOutput;

/// 不允许 Planner 探索的目录（即使在仓库内）。
/// 与 `.gitignore` 互补——`.gitignore` 负责仓库自定义忽略，黑名单负责常见噪音目录。
const BLOCKLIST: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "dist",
    "build",
    ".worktrees",
    ".next",
    ".turbo",
    ".cache",
    ".venv",
    "venv",
    "__pycache__",
];

/// 单文件 read 上限（FR-05.3 / FR-05.4 文件 ≤ 200KB）
const MAX_READ_BYTES: u64 = 200 * 1024;
/// list_directory 默认深度
const DEFAULT_LIST_DEPTH: u32 = 1;
/// list_directory 最大允许深度（FR-05.2 ≤ 3）
const MAX_LIST_DEPTH: u32 = 3;
/// list_directory 单次返回的最大条目数（防爆炸）
const MAX_LIST_ENTRIES: usize = 500;

/// LLM-facing 工具 schema 列表。仅在 Planner Loop 中装载。
pub fn planner_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "list_directory".into(),
            description:
                "List files and subdirectories under a path inside the repository (read-only). \
                Respects a builtin blocklist (node_modules, target, .git, dist, build, ...). \
                Use this to scout repository structure before proposing tasks."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path relative to repo root. Use '.' for the root."
                    },
                    "max_depth": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LIST_DEPTH,
                        "description": "Recursion depth (1-3). Default 1."
                    }
                },
                "required": ["path"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "read_file".into(),
            description:
                "Read the contents of a file inside the repository (read-only). \
                File must be ≤ 200KB. Use to inspect existing code / config / docs."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to repo root." }
                },
                "required": ["path"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "list_roles".into(),
            description:
                "List all available task roles (architect / implementer / refactorer / tester / \
                integrator / researcher) with their description and expected artifact types. \
                Call this once before proposing tasks so you pick a role from this closed set."
                    .into(),
            input_schema: json!({ "type": "object", "properties": {} }),
            cache_control: None,
        },
        ToolDefinition {
            name: "list_skills".into(),
            description:
                "List all available pluggable skills with id, description and the role ids each \
                skill is compatible with. Call this once before deciding `additional_skills` on \
                tasks. A skill attached to a role it isn't compatible with is rejected by \
                propose_task / revise_task."
                    .into(),
            input_schema: json!({ "type": "object", "properties": {} }),
            cache_control: None,
        },
        ToolDefinition {
            name: "propose_task".into(),
            description:
                "Add a new task to the in-progress plan. Each call appends one task; the call is \
                rejected immediately on validation errors (unknown role, duplicate id, missing \
                expected_output, unknown dependency, would-create cycle). On error, fix and retry."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id":              { "type": "string", "description": "Stable task id, e.g. 'T1'. Must be unique within the plan." },
                    "title":           { "type": "string", "description": "Short imperative title." },
                    "description":     { "type": "string", "description": "Self-contained task description for the coding agent." },
                    "complexity":      { "type": "string", "enum": ["low", "medium", "high"], "description": "Default 'medium'." },
                    "expected_output": { "type": "string", "description": "Acceptance contract: what concrete artifact / behavior must be produced." },
                    "role":            { "type": "string", "description": "Role id (must come from list_roles)." },
                    "depends_on":      { "type": "array", "items": { "type": "string" }, "description": "Optional list of upstream task ids." },
                    "additional_skills": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional skill ids (from list_skills) to attach on top of the role's defaults. Each must be compatible with the chosen role."
                    },
                    "produces_artifacts": {
                        "type": "array",
                        "description": "Artifacts this task is expected to publish via publish_artifact during execution.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "local_name": { "type": "string", "description": "snake_case identifier, unique within this task." },
                                "type":       { "type": "string", "enum": ["design_doc","api_spec","schema","code_module","test_module","config","docs","report"] },
                                "summary":    { "type": "string", "description": "1-2 sentence purpose statement." }
                            },
                            "required": ["local_name", "type"]
                        }
                    },
                    "consumes_artifacts": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Artifact ids in `<task_id>.<local_name>` form that this task reads. The producing task must already be declared and must be in this task's transitive depends_on closure."
                    },
                    "file_scope_hints": {
                        "type": "object",
                        "description": "Best-effort hint of repo-relative paths this task will touch. Used for conflict prediction; not a hard constraint.",
                        "properties": {
                            "definite": { "type": "array", "items": { "type": "string" } },
                            "possible": { "type": "array", "items": { "type": "string" } }
                        }
                    }
                },
                "required": ["id", "title", "description", "expected_output", "role"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "add_dependency".into(),
            description:
                "Declare that `task_id` depends on `depends_on`. Both ids must already exist in \
                the plan. Cycles are rejected. Idempotent: re-adding an existing edge succeeds."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id":    { "type": "string" },
                    "depends_on": { "type": "string" }
                },
                "required": ["task_id", "depends_on"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "revise_task".into(),
            description:
                "Modify fields of an existing task. Only fields you pass are updated; omitted \
                fields stay unchanged. Useful for fixing a bad title / description / role / \
                expected_output without dropping and re-proposing."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id":         { "type": "string" },
                    "title":           { "type": "string" },
                    "description":     { "type": "string" },
                    "complexity":      { "type": "string", "enum": ["low", "medium", "high"] },
                    "expected_output": { "type": "string" },
                    "role":            { "type": "string" },
                    "additional_skills":  {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Whole-list replacement of additional_skills. Omit to keep current value."
                    },
                    "produces_artifacts": {
                        "type": "array",
                        "description": "Whole-list replacement of declared artifacts. Omit to keep current value.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "local_name": { "type": "string" },
                                "type":       { "type": "string", "enum": ["design_doc","api_spec","schema","code_module","test_module","config","docs","report"] },
                                "summary":    { "type": "string" }
                            },
                            "required": ["local_name", "type"]
                        }
                    },
                    "consumes_artifacts": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Whole-list replacement of consumed artifact ids."
                    },
                    "file_scope_hints": {
                        "type": "object",
                        "description": "Whole-replacement of file scope hints.",
                        "properties": {
                            "definite": { "type": "array", "items": { "type": "string" } },
                            "possible": { "type": "array", "items": { "type": "string" } }
                        }
                    }
                },
                "required": ["task_id"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "drop_task".into(),
            description:
                "Remove a task from the plan. Any other task that depended on it loses that edge. \
                Use sparingly—prefer revise_task when possible."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" }
                },
                "required": ["task_id"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "validate_plan".into(),
            description:
                "Run a holistic check on the current plan: cycles, dangling dependencies, empty \
                plan, etc. Returns a list of issues; an empty list means the plan is ready for \
                finalize_plan. Always call this before finalize_plan."
                    .into(),
            input_schema: json!({ "type": "object", "properties": {} }),
            cache_control: None,
        },
        ToolDefinition {
            name: "fetch_url".into(),
            description:
                "Fetch a public web page or JSON document. EVERY call requires user confirmation \
                unless the host is on the user's allowlist or was already approved earlier in \
                this planner session. Use sparingly: prefer reading the repo to grounding from \
                external docs. Local / private hosts are always rejected."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Full http(s) URL. Body is capped at ~256KB; only text/* and JSON/XML/YAML are accepted."
                    },
                    "reason": {
                        "type": "string",
                        "description": "1-line reason shown to the user in the confirmation dialog."
                    }
                },
                "required": ["url", "reason"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "finalize_plan".into(),
            description:
                "Mark the plan as complete. Requires validate_plan to return zero issues. \
                Provide a concise mission_title (5-10 words). After this call, no further plan \
                edits are accepted."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mission_title": { "type": "string", "description": "Mission title (5-10 words)." }
                },
                "required": ["mission_title"]
            }),
            cache_control: None,
        },
    ]
}

/// Planner 工具调用结果。`finalize_plan` 成功时附带 `PlannerOutput`，
/// 上层引擎据此判断 Loop 是否终止。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerToolResult {
    pub output: ToolOutput,
    /// 仅 finalize_plan 成功时为 Some
    pub finalized: Option<PlannerOutput>,
}

impl PlannerToolResult {
    fn ok(content: String) -> Self {
        Self {
            output: ToolOutput {
                content,
                is_error: false,
            },
            finalized: None,
        }
    }

    fn err(kind: &str, message: &str) -> Self {
        Self {
            output: ToolOutput {
                content: json!({ "error": kind, "message": message }).to_string(),
                is_error: true,
            },
            finalized: None,
        }
    }

    fn finalized(output: PlannerOutput, content: String) -> Self {
        Self {
            output: ToolOutput {
                content,
                is_error: false,
            },
            finalized: Some(output),
        }
    }
}

/// 仅读 sandbox 内的 `list_directory` / `read_file`，被 PlannerToolExecutor 与
/// Pre-flight Agent 共享（FM-15 v2.2 / FR-PF-01: from_existing 模式下 Pre-flight 也可探索仓库）。
#[derive(Clone)]
pub struct ReadOnlyExplorer {
    repo_root: PathBuf,
}

impl ReadOnlyExplorer {
    pub fn new(repo_root: PathBuf) -> Self {
        let repo_root = repo_root.canonicalize().unwrap_or(repo_root);
        Self { repo_root }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// 仅返回探索类两个 tool 的 schema，可拼到任意 Agent 的 tool list 里。
    pub fn tool_definitions() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "list_directory".into(),
                description:
                    "List files and subdirectories under a path inside the repository (read-only). \
                    Respects a builtin blocklist (node_modules, target, .git, dist, build, ...). \
                    Use this to scout repository structure."
                        .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Directory path relative to repo root. Use '.' for the root."
                        },
                        "max_depth": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": MAX_LIST_DEPTH,
                            "description": "Recursion depth (1-3). Default 1."
                        }
                    },
                    "required": ["path"]
                }),
                cache_control: None,
            },
            ToolDefinition {
                name: "read_file".into(),
                description:
                    "Read the contents of a file inside the repository (read-only). \
                    File must be ≤ 200KB. Use to inspect existing code / config / docs."
                        .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path relative to repo root." }
                    },
                    "required": ["path"]
                }),
                cache_control: None,
            },
        ]
    }

    /// 工具名匹配则执行并返回 `Some(ToolOutput)`；否则返回 `None`，表示「不是我管的工具」。
    pub async fn execute(&self, name: &str, input: &serde_json::Value) -> Option<ToolOutput> {
        match name {
            "list_directory" => Some(self.list_directory(input).await),
            "read_file" => Some(self.read_file(input).await),
            _ => None,
        }
    }

    async fn list_directory(&self, input: &serde_json::Value) -> ToolOutput {
        let path = match input["path"].as_str() {
            Some(p) => p,
            None => return tool_err("parameter_error", "Missing 'path'"),
        };
        let max_depth = input["max_depth"]
            .as_u64()
            .map(|d| (d as u32).min(MAX_LIST_DEPTH).max(1))
            .unwrap_or(DEFAULT_LIST_DEPTH);

        let full_path = match self.resolve_path(path) {
            Ok(p) => p,
            Err(msg) => return tool_err("sandbox_violation", &msg),
        };
        if !full_path.exists() {
            return tool_err("file_not_found", &format!("{path} does not exist"));
        }
        if !full_path.is_dir() {
            return tool_err(
                "not_a_directory",
                &format!("{path} is not a directory; use read_file for files"),
            );
        }

        let mut out = Vec::<String>::new();
        let truncated = walk_dir(&full_path, &full_path, 0, max_depth, &mut out);
        if truncated {
            out.push(format!(
                "... (truncated at {MAX_LIST_ENTRIES} entries; refine path or reduce max_depth)"
            ));
        }
        tool_ok(out.join("\n"))
    }

    async fn read_file(&self, input: &serde_json::Value) -> ToolOutput {
        let path = match input["path"].as_str() {
            Some(p) => p,
            None => return tool_err("parameter_error", "Missing 'path'"),
        };
        let full_path = match self.resolve_path(path) {
            Ok(p) => p,
            Err(msg) => return tool_err("sandbox_violation", &msg),
        };
        if path_in_blocklist(&full_path, &self.repo_root) {
            return tool_err(
                "blocked_path",
                &format!("Path '{path}' is inside a blocked directory"),
            );
        }
        let metadata = match tokio::fs::metadata(&full_path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return tool_err("file_not_found", &format!("{path} does not exist"));
            }
            Err(e) => return tool_err("io_error", &e.to_string()),
        };
        if metadata.is_dir() {
            return tool_err(
                "not_a_file",
                &format!("{path} is a directory; use list_directory"),
            );
        }
        if metadata.len() > MAX_READ_BYTES {
            return tool_err(
                "file_too_large",
                &format!(
                    "{path} is {} bytes (limit {MAX_READ_BYTES}); read a smaller file or use \
                     list_directory to explore",
                    metadata.len()
                ),
            );
        }
        match tokio::fs::read_to_string(&full_path).await {
            Ok(content) => tool_ok(content),
            Err(e) => tool_err("io_error", &e.to_string()),
        }
    }

    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf, String> {
        let candidate = if rel_path == "." || rel_path.is_empty() {
            self.repo_root.clone()
        } else {
            self.repo_root.join(rel_path)
        };
        let normalized = normalize_lexical(&candidate);
        if !normalized.starts_with(&self.repo_root) {
            return Err(format!(
                "Path '{rel_path}' escapes repository root {}",
                self.repo_root.display()
            ));
        }
        Ok(normalized)
    }
}

fn tool_ok(content: String) -> ToolOutput {
    ToolOutput {
        content,
        is_error: false,
    }
}

fn tool_err(kind: &str, message: &str) -> ToolOutput {
    ToolOutput {
        content: json!({ "error": kind, "message": message }).to_string(),
        is_error: true,
    }
}

/// fetch_url runtime 上下文：包含 session_id（用于 grant 计数 + 事件 payload）、
/// AppHandle（emit `planner-fetch-confirmation` + 通过 try_state 取 Database +
/// ApprovalCoordinator）、Policy（allowlist + 配额）。
///
/// FM-14 改造：用户确认走统一的 ApprovalCoordinator（不再用旧 PlannerFetchCoordinator）。
/// fetch 类型在 ApprovalCard 里给三个按钮：Allow Once / Allow Session / Deny；
/// 后端通过 `outcome.note` 区分前两者（Some("session") 时写 per-session grant）。
///
/// 旧 `PlannerFetchCoordinator` 仍然在 manage 列表（兼容老前端 IPC `confirm_planner_fetch`），
/// 但不再被这里使用，详见 `commands/planner.rs::confirm_planner_fetch` 的桥接说明。
///
/// 单元测试不传这个 → fetch_url 直接报 `fetch_unavailable`。
pub struct PlannerFetchRuntime {
    pub session_id: String,
    pub app_handle: tauri::AppHandle,
    pub policy: FetchPolicy,
}

/// Read-only filesystem 范围 + DAG state machine 执行器。
pub struct PlannerToolExecutor {
    explorer: ReadOnlyExplorer,
    state: Arc<Mutex<PlannerState>>,
    fetch: Option<PlannerFetchRuntime>,
}

impl PlannerToolExecutor {
    pub fn new(repo_root: PathBuf, state: Arc<Mutex<PlannerState>>) -> Self {
        Self {
            explorer: ReadOnlyExplorer::new(repo_root),
            state,
            fetch: None,
        }
    }

    pub fn with_fetch_runtime(mut self, runtime: PlannerFetchRuntime) -> Self {
        self.fetch = Some(runtime);
        self
    }

    pub fn repo_root_display(&self) -> String {
        self.explorer.repo_root().display().to_string()
    }

    pub async fn execute(&self, name: &str, input: &serde_json::Value) -> PlannerToolResult {
        // 优先委托只读探索工具
        if let Some(output) = self.explorer.execute(name, input).await {
            return PlannerToolResult {
                output,
                finalized: None,
            };
        }
        match name {
            "list_roles" => self.list_roles().await,
            "list_skills" => self.list_skills().await,
            "propose_task" => self.propose_task(input).await,
            "add_dependency" => self.add_dependency(input).await,
            "revise_task" => self.revise_task(input).await,
            "drop_task" => self.drop_task(input).await,
            "validate_plan" => self.validate_plan().await,
            "finalize_plan" => self.finalize_plan(input).await,
            "fetch_url" => self.fetch_url(input).await,
            other => PlannerToolResult::err(
                "unknown_tool",
                &format!(
                    "Unknown planner tool '{other}'. Allowed tools: list_directory, read_file, \
                     list_roles, list_skills, propose_task, add_dependency, revise_task, \
                     drop_task, validate_plan, finalize_plan, fetch_url."
                ),
            ),
        }
    }

    // ---------- B: 元数据类 ----------

    async fn list_roles(&self) -> PlannerToolResult {
        let reg = roles::registry();
        let payload: Vec<serde_json::Value> = reg
            .all()
            .iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "display_name": r.display_name,
                    "description": r.description,
                    "expected_artifact_types": r.expected_artifact_types,
                })
            })
            .collect();
        PlannerToolResult::ok(serde_json::to_string_pretty(&payload).unwrap_or_default())
    }

    async fn list_skills(&self) -> PlannerToolResult {
        let reg = crate::skills::registry::global();
        let payload: Vec<serde_json::Value> = reg
            .all()
            .iter()
            .map(|s| {
                json!({
                    "id": s.frontmatter.name,
                    "description": s.frontmatter.description,
                    "compatible_roles": s.frontmatter.compatible_roles,
                    "source": s.source,
                })
            })
            .collect();
        PlannerToolResult::ok(serde_json::to_string_pretty(&payload).unwrap_or_default())
    }

    // ---------- C: 构建类 ----------

    async fn propose_task(&self, input: &serde_json::Value) -> PlannerToolResult {
        let typed: ProposeTaskInput = match serde_json::from_value(input.clone()) {
            Ok(v) => v,
            Err(e) => {
                return PlannerToolResult::err(
                    "parameter_error",
                    &format!("Invalid propose_task input: {e}"),
                )
            }
        };
        let mut state = self.state.lock().await;
        match state.propose_task(typed) {
            Ok(id) => PlannerToolResult::ok(format!(
                "Task '{id}' added. Plan now has {} task(s).",
                state.task_count()
            )),
            Err(e) => state_error(&e),
        }
    }

    async fn add_dependency(&self, input: &serde_json::Value) -> PlannerToolResult {
        let task_id = match input["task_id"].as_str() {
            Some(s) => s.to_string(),
            None => return PlannerToolResult::err("parameter_error", "Missing 'task_id'"),
        };
        let depends_on = match input["depends_on"].as_str() {
            Some(s) => s.to_string(),
            None => return PlannerToolResult::err("parameter_error", "Missing 'depends_on'"),
        };
        let mut state = self.state.lock().await;
        match state.add_dependency(&task_id, &depends_on) {
            Ok(()) => PlannerToolResult::ok(format!("Edge '{task_id} <- {depends_on}' recorded")),
            Err(e) => state_error(&e),
        }
    }

    async fn revise_task(&self, input: &serde_json::Value) -> PlannerToolResult {
        let typed: ReviseTaskInput = match serde_json::from_value(input.clone()) {
            Ok(v) => v,
            Err(e) => {
                return PlannerToolResult::err(
                    "parameter_error",
                    &format!("Invalid revise_task input: {e}"),
                )
            }
        };
        let task_id = typed.task_id.clone();
        let mut state = self.state.lock().await;
        match state.revise_task(typed) {
            Ok(()) => PlannerToolResult::ok(format!("Task '{task_id}' revised")),
            Err(e) => state_error(&e),
        }
    }

    async fn drop_task(&self, input: &serde_json::Value) -> PlannerToolResult {
        let task_id = match input["task_id"].as_str() {
            Some(s) => s.to_string(),
            None => return PlannerToolResult::err("parameter_error", "Missing 'task_id'"),
        };
        let mut state = self.state.lock().await;
        match state.drop_task(&task_id) {
            Ok(()) => PlannerToolResult::ok(format!("Task '{task_id}' dropped")),
            Err(e) => state_error(&e),
        }
    }

    // ---------- D: 校验类 ----------

    async fn validate_plan(&self) -> PlannerToolResult {
        let state = self.state.lock().await;
        let issues = state.validate_plan();
        PlannerToolResult::ok(
            serde_json::to_string_pretty(&json!({
                "task_count": state.task_count(),
                "issues": issues,
            }))
            .unwrap_or_default(),
        )
    }

    // ---------- F: 远程获取 (FR-05.x) ----------

    async fn fetch_url(&self, input: &serde_json::Value) -> PlannerToolResult {
        let Some(rt) = self.fetch.as_ref() else {
            return PlannerToolResult::err(
                "fetch_unavailable",
                "fetch_url runtime is not attached (no AppHandle / DB). Tool is disabled in this context.",
            );
        };
        // Database 通过 AppHandle 取，避免 PlannerToolExecutor 强依赖 Arc<Database>。
        use tauri::Manager;
        let db = match rt.app_handle.try_state::<Database>() {
            Some(d) => d,
            None => {
                return PlannerToolResult::err(
                    "internal",
                    "Database state not registered with Tauri",
                )
            }
        };

        let url_raw = match input["url"].as_str() {
            Some(s) => s.trim().to_string(),
            None => return PlannerToolResult::err("parameter_error", "Missing 'url'"),
        };
        let reason = input["reason"].as_str().unwrap_or("(no reason provided)").to_string();

        // 1) URL parse + blocklist
        let (host, normalized_url) = match planner_fetch::parse_and_check_host(&url_raw) {
            Ok(v) => v,
            Err(e) => return planner_fetch_error(&e),
        };

        // 2) 配额：先看 session 内已经用了多少次 fetch_url
        let used = match db.with_conn(|conn| {
            crate::db::queries::count_planner_fetch_calls(conn, &rt.session_id)
        }) {
            Ok(c) => c as u32,
            Err(e) => {
                return PlannerToolResult::err("internal", &format!("count_planner_fetch_calls: {e}"))
            }
        };
        // 注意：PlannerEngine 已经把本次 tool_call 行写进 planner_steps，
        // 所以 `used` 包含本次。只有当历史 + 本次 > cap 时才算超额。
        let cap = rt.policy.max_per_session;
        if cap > 0 && used > cap {
            return planner_fetch_error(&FetchError::BudgetExceeded { used, cap });
        }

        // 3) allowlist
        let mut allowed_without_prompt = rt.policy.is_allowlisted(&host);

        // 4) per-session grant
        if !allowed_without_prompt {
            allowed_without_prompt = match db.with_conn(|conn| {
                crate::db::queries::is_planner_fetch_granted(conn, &rt.session_id, &host)
            }) {
                Ok(b) => b,
                Err(e) => {
                    return PlannerToolResult::err(
                        "internal",
                        &format!("is_planner_fetch_granted: {e}"),
                    )
                }
            };
        }

        // 5) 否则需要用户确认 —— 走 FM-14 统一 ApprovalCoordinator
        if !allowed_without_prompt {
            use crate::agent::approval::{
                self, ApprovalCoordinator, ApprovalDecision, ApprovalKind, ApprovalRequestSpec,
            };

            // 反查 mission_id（planner_session.mission_id 在 link 后才有）；缺失就跳过审批
            // 并直接 Deny，避免越权。
            let mission_id_opt: Option<String> = db
                .with_conn(|conn| {
                    crate::db::queries::get_planner_session(conn, &rt.session_id)
                        .map(|opt| opt.and_then(|s| s.mission_id))
                })
                .unwrap_or(None);
            let Some(mission_id) = mission_id_opt else {
                tracing::warn!(
                    "[planner.fetch_url] session {} has no mission_id; denying fetch",
                    rt.session_id
                );
                return planner_fetch_error(&FetchError::UserDenied(host));
            };

            let coord = match rt.app_handle.try_state::<Arc<ApprovalCoordinator>>() {
                Some(c) => c.inner().clone(),
                None => {
                    return PlannerToolResult::err(
                        "internal",
                        "ApprovalCoordinator not registered; fetch_url cannot await user",
                    )
                }
            };

            // payload：前端按 kind="fetch" 渲染三按钮（Allow Once / Session / Deny）。
            let payload_json = serde_json::json!({
                "url": normalized_url,
                "host": host,
                "session_id": rt.session_id,
                "reason": reason,
            })
            .to_string();
            let cfg_timeout = rt
                .app_handle
                .try_state::<crate::commands::ConfigManager>()
                .map(|c| c.get_config_snapshot().approval_policy.timeout_seconds as i64)
                .unwrap_or(approval::DEFAULT_APPROVAL_TIMEOUT_SECS);

            let mut spec = ApprovalRequestSpec::new(
                mission_id,
                ApprovalKind::Fetch,
                format!("Fetch URL: {host}"),
            );
            spec.planner_session_id = Some(rt.session_id.clone());
            spec.payload = payload_json.clone();
            spec.reason = reason.clone();
            spec.context_summary = format!("Planner wants to GET {normalized_url}");
            spec.timeout_seconds = Some(cfg_timeout);

            // 兼容旧 UI：让 PlannerFetchConfirmDialog 仍能弹出（接收的 request_id 就是
            // approval_request id；前端 confirm_planner_fetch 调用会被桥接到 ApprovalCoordinator）。
            // 同时 emit 新事件让 ApprovalQueue 高亮一下。
            //
            // request_id 由 submit_and_wait 内部生成；我们先生成一个相同 id 走的策略不可行，
            // 所以这里改成"先 submit 拿 id 再 emit"。但 submit 是阻塞 await——所以
            // 拆成 submit + 在 submit 之前不 emit；submit 内部已写 DB，前端通过订阅
            // approval-requested + 旧 planner-fetch-confirmation 都能感知。
            let cancel = tokio_util::sync::CancellationToken::new();
            let db_state = rt.app_handle.state::<Database>();

            let (request_id, outcome) = match approval::submit_and_wait(
                &coord,
                db_state.inner(),
                &spec,
                &cancel,
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    return PlannerToolResult::err(
                        "internal",
                        &format!("submit_and_wait(fetch): {e}"),
                    )
                }
            };

            // approval-requested + planner-fetch-confirmation 都补发（订阅方可能已经在 list 上看到，
            // 但事件能让旧弹窗组件继续工作）。
            let _ = tauri::Emitter::emit(
                &rt.app_handle,
                "approval-requested",
                serde_json::json!({
                    "request_id": request_id,
                    "mission_id": spec.mission_id,
                    "kind": "fetch",
                    "title": spec.title,
                }),
            );
            let _ = tauri::Emitter::emit(
                &rt.app_handle,
                "planner-fetch-confirmation",
                serde_json::json!({
                    "request_id": request_id,
                    "session_id": rt.session_id,
                    "url": normalized_url,
                    "host": host,
                    "reason": reason,
                }),
            );

            match outcome.decision {
                ApprovalDecision::Approved => {
                    // note=="session" → 写 grant；其他（"once" 或 None）→ 不写
                    if outcome.note.as_deref() == Some("session") {
                        if let Err(e) = db.with_conn(|conn| {
                            crate::db::queries::record_planner_fetch_grant(
                                conn,
                                &rt.session_id,
                                &host,
                            )
                        }) {
                            tracing::warn!("record_planner_fetch_grant failed: {e}");
                        }
                    }
                }
                ApprovalDecision::Rejected => {
                    return planner_fetch_error(&FetchError::UserDenied(host));
                }
                ApprovalDecision::Expired => {
                    return planner_fetch_error(&FetchError::ConfirmationTimeout(host));
                }
                ApprovalDecision::Cancelled => {
                    return planner_fetch_error(&FetchError::UserDenied(host));
                }
            }
        }

        // 6) 真正去 fetch
        match planner_fetch::http_fetch(&normalized_url).await {
            Ok(body) => {
                let preview = if body.len() > 4096 {
                    format!(
                        "{} ... (truncated for display, full {} bytes returned to LLM context)",
                        &body[..4096],
                        body.len()
                    )
                } else {
                    body.clone()
                };
                let envelope = json!({
                    "url": normalized_url,
                    "host": host,
                    "bytes": body.len(),
                    "body": body,
                });
                tracing::info!(
                    "[planner.fetch_url] session={} host={} bytes={} preview_len={}",
                    rt.session_id,
                    host,
                    body.len(),
                    preview.len()
                );
                PlannerToolResult::ok(envelope.to_string())
            }
            Err(e) => planner_fetch_error(&e),
        }
    }

    // ---------- E: 终止类 ----------

    async fn finalize_plan(&self, input: &serde_json::Value) -> PlannerToolResult {
        let title = match input["mission_title"].as_str() {
            Some(s) => s.trim().to_string(),
            None => return PlannerToolResult::err("parameter_error", "Missing 'mission_title'"),
        };
        if title.is_empty() {
            return PlannerToolResult::err(
                "parameter_error",
                "mission_title must not be empty",
            );
        }
        let mut state = self.state.lock().await;
        match state.finalize(title) {
            Ok(out) => {
                let summary = format!(
                    "Plan finalized: {} task(s). Mission title: {}",
                    out.tasks.len(),
                    out.mission_title
                );
                PlannerToolResult::finalized(out, summary)
            }
            Err(e) => state_error(&e),
        }
    }

}

fn planner_fetch_error(e: &FetchError) -> PlannerToolResult {
    let kind = match e {
        FetchError::InvalidUrl(_) => "invalid_url",
        FetchError::UnsupportedScheme(_) => "unsupported_scheme",
        FetchError::HostBlocked(_) => "host_blocked",
        FetchError::UserDenied(_) => "user_denied",
        FetchError::ConfirmationTimeout(_) => "confirmation_timeout",
        FetchError::BudgetExceeded { .. } => "budget_exceeded",
        FetchError::Http(_) => "http_error",
        FetchError::ResponseTooLarge => "response_too_large",
        FetchError::UnsupportedContentType(_) => "unsupported_content_type",
        FetchError::BadStatus { .. } => "bad_status",
        FetchError::Internal(_) => "internal",
    };
    PlannerToolResult::err(kind, &e.to_string())
}

fn state_error(e: &PlannerStateError) -> PlannerToolResult {
    let kind = match e {
        PlannerStateError::DuplicateTaskId(_) => "duplicate_task_id",
        PlannerStateError::TaskNotFound(_) => "task_not_found",
        PlannerStateError::InvalidTaskId(_) => "invalid_task_id",
        PlannerStateError::EmptyTitle(_) => "empty_title",
        PlannerStateError::EmptyDescription(_) => "empty_description",
        PlannerStateError::EmptyExpectedOutput(_) => "empty_expected_output",
        PlannerStateError::InvalidComplexity { .. } => "invalid_complexity",
        PlannerStateError::InvalidRole { .. } => "invalid_role",
        PlannerStateError::SelfDependency(_) => "self_dependency",
        PlannerStateError::UnknownDependency { .. } => "unknown_dependency",
        PlannerStateError::CyclicDependency { .. } => "cyclic_dependency",
        PlannerStateError::EmptyPlan => "empty_plan",
        PlannerStateError::InvalidSkill { .. } => "invalid_skill",
        PlannerStateError::InvalidProducedArtifact { .. } => "invalid_produced_artifact",
        PlannerStateError::DuplicateProducedArtifact { .. } => "duplicate_produced_artifact",
        PlannerStateError::UnknownConsumedArtifact { .. } => "unknown_consumed_artifact",
        PlannerStateError::ConsumedArtifactWithoutDependency { .. } => {
            "consumed_artifact_without_dependency"
        }
        PlannerStateError::InvalidFileScopePath { .. } => "invalid_file_scope_path",
    };
    PlannerToolResult::err(kind, &e.to_string())
}

/// 递归列目录，过滤黑名单。返回 true 表示因 entry 数量上限被截断。
fn walk_dir(
    root: &Path,
    current: &Path,
    depth: u32,
    max_depth: u32,
    out: &mut Vec<String>,
) -> bool {
    let read = match std::fs::read_dir(current) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let mut entries: Vec<_> = read.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        if out.len() >= MAX_LIST_ENTRIES {
            return true;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if BLOCKLIST.iter().any(|b| *b == name) {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
        let is_dir = path.is_dir();
        let suffix = if is_dir { "/" } else { "" };
        out.push(format!("{rel}{suffix}"));
        if is_dir && depth + 1 < max_depth {
            if walk_dir(root, &path, depth + 1, max_depth, out) {
                return true;
            }
        }
    }
    false
}

fn path_in_blocklist(path: &Path, repo_root: &Path) -> bool {
    let rel = match path.strip_prefix(repo_root) {
        Ok(r) => r,
        Err(_) => return false,
    };
    rel.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        BLOCKLIST.iter().any(|b| *b == s.as_ref())
    })
}

fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !parts.is_empty() {
                    parts.pop();
                }
            }
            Component::CurDir => {}
            c => parts.push(c),
        }
    }
    parts.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PlannerToolExecutor, Arc<Mutex<PlannerState>>) {
        let dir = TempDir::new().unwrap();
        // 必须 canonicalize repo root，否则 macOS 上 /var → /private/var 会让 strip_prefix 失败
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("src/lib")).unwrap();
        fs::write(root.join("README.md"), "# demo").unwrap();
        fs::write(root.join("src/lib/main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(root.join("node_modules/foo")).unwrap();
        fs::write(root.join("node_modules/foo/index.js"), "// noisy").unwrap();
        let state = Arc::new(Mutex::new(PlannerState::new()));
        let exec = PlannerToolExecutor::new(root, state.clone());
        (dir, exec, state)
    }

    #[tokio::test]
    async fn tool_definitions_include_minimum_set() {
        let defs = planner_tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        for expected in [
            "list_directory",
            "read_file",
            "list_roles",
            "propose_task",
            "add_dependency",
            "revise_task",
            "drop_task",
            "validate_plan",
            "finalize_plan",
            "fetch_url",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
        // 写工具不允许出现
        assert!(!names.contains(&"write_file"));
        assert!(!names.contains(&"shell_exec"));
    }

    #[tokio::test]
    async fn fetch_url_without_runtime_returns_unavailable() {
        let (_dir, exec, _) = setup();
        let r = exec
            .execute(
                "fetch_url",
                &json!({ "url": "https://example.com", "reason": "test" }),
            )
            .await;
        assert!(r.output.is_error);
        let v: serde_json::Value = serde_json::from_str(&r.output.content).unwrap();
        assert_eq!(v["error"], "fetch_unavailable");
    }

    #[tokio::test]
    async fn list_directory_excludes_blocklist() {
        let (_dir, exec, _) = setup();
        let result = exec
            .execute("list_directory", &json!({ "path": ".", "max_depth": 2 }))
            .await;
        assert!(!result.output.is_error, "{}", result.output.content);
        assert!(result.output.content.contains("README.md"));
        assert!(result.output.content.contains("src/"));
        assert!(!result.output.content.contains("node_modules"));
    }

    #[tokio::test]
    async fn list_directory_rejects_escape() {
        let (_dir, exec, _) = setup();
        let result = exec
            .execute("list_directory", &json!({ "path": "../../etc" }))
            .await;
        assert!(result.output.is_error);
        let v: serde_json::Value = serde_json::from_str(&result.output.content).unwrap();
        assert_eq!(v["error"], "sandbox_violation");
    }

    #[tokio::test]
    async fn read_file_reads_existing_file() {
        let (_dir, exec, _) = setup();
        let result = exec
            .execute("read_file", &json!({ "path": "README.md" }))
            .await;
        assert!(!result.output.is_error);
        assert!(result.output.content.contains("# demo"));
    }

    #[tokio::test]
    async fn read_file_rejects_blocked_path() {
        let (_dir, exec, _) = setup();
        let result = exec
            .execute("read_file", &json!({ "path": "node_modules/foo/index.js" }))
            .await;
        assert!(result.output.is_error);
        let v: serde_json::Value = serde_json::from_str(&result.output.content).unwrap();
        assert_eq!(v["error"], "blocked_path");
    }

    #[tokio::test]
    async fn read_file_rejects_oversize() {
        let (dir, exec, _) = setup();
        let big = vec![b'a'; (MAX_READ_BYTES + 1) as usize];
        fs::write(dir.path().join("huge.txt"), &big).unwrap();
        let result = exec
            .execute("read_file", &json!({ "path": "huge.txt" }))
            .await;
        assert!(result.output.is_error);
        let v: serde_json::Value = serde_json::from_str(&result.output.content).unwrap();
        assert_eq!(v["error"], "file_too_large");
    }

    #[tokio::test]
    async fn list_roles_returns_six_builtins() {
        let (_dir, exec, _) = setup();
        let result = exec.execute("list_roles", &json!({})).await;
        assert!(!result.output.is_error);
        let arr: Vec<serde_json::Value> = serde_json::from_str(&result.output.content).unwrap();
        assert_eq!(arr.len(), 6);
        assert_eq!(arr[0]["id"], "architect");
    }

    #[tokio::test]
    async fn end_to_end_propose_validate_finalize() {
        let (_dir, exec, _state) = setup();

        // T1
        let r = exec
            .execute(
                "propose_task",
                &json!({
                    "id": "T1",
                    "title": "Design auth API",
                    "description": "Design authentication endpoints",
                    "expected_output": "OpenAPI spec at docs/api/auth.yaml",
                    "role": "architect"
                }),
            )
            .await;
        assert!(!r.output.is_error, "{}", r.output.content);
        // T2 with dep
        let r = exec
            .execute(
                "propose_task",
                &json!({
                    "id": "T2",
                    "title": "Implement auth handlers",
                    "description": "Wire up handlers per spec",
                    "expected_output": "Handlers with passing build",
                    "role": "implementer",
                    "depends_on": ["T1"]
                }),
            )
            .await;
        assert!(!r.output.is_error, "{}", r.output.content);

        let r = exec.execute("validate_plan", &json!({})).await;
        assert!(!r.output.is_error);
        let v: serde_json::Value = serde_json::from_str(&r.output.content).unwrap();
        assert_eq!(v["task_count"], 2);
        assert_eq!(v["issues"].as_array().unwrap().len(), 0);

        let r = exec
            .execute("finalize_plan", &json!({ "mission_title": "Auth feature" }))
            .await;
        assert!(!r.output.is_error, "{}", r.output.content);
        let out = r.finalized.expect("finalize_plan should yield PlannerOutput");
        assert_eq!(out.tasks.len(), 2);
        assert_eq!(out.mission_title, "Auth feature");
    }

    #[tokio::test]
    async fn propose_task_invalid_role_returns_structured_error() {
        let (_dir, exec, _) = setup();
        let r = exec
            .execute(
                "propose_task",
                &json!({
                    "id": "T1",
                    "title": "x",
                    "description": "y",
                    "expected_output": "z",
                    "role": "ceo"
                }),
            )
            .await;
        assert!(r.output.is_error);
        let v: serde_json::Value = serde_json::from_str(&r.output.content).unwrap();
        assert_eq!(v["error"], "invalid_role");
        assert!(v["message"].as_str().unwrap().contains("architect"));
    }

    #[tokio::test]
    async fn finalize_with_empty_plan_errors() {
        let (_dir, exec, _) = setup();
        let r = exec
            .execute("finalize_plan", &json!({ "mission_title": "x" }))
            .await;
        assert!(r.output.is_error);
        let v: serde_json::Value = serde_json::from_str(&r.output.content).unwrap();
        assert_eq!(v["error"], "empty_plan");
    }

    #[tokio::test]
    async fn unknown_tool_returns_structured_error() {
        let (_dir, exec, _) = setup();
        let r = exec.execute("write_file", &json!({})).await;
        assert!(r.output.is_error);
        let v: serde_json::Value = serde_json::from_str(&r.output.content).unwrap();
        assert_eq!(v["error"], "unknown_tool");
    }
}
