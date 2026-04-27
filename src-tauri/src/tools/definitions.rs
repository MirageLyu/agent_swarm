use crate::llm::ToolDefinition;
use serde_json::json;

/// Coding Agent 默认工具集。FM-15 FR-03.2 的 `publish_artifact`
/// 暂未通过 ToolExecutor 路径下发——它需要 DB + repo_path 这两个
/// runtime 上下文，由 Phase 2 的 dispatch_task 在装载 ToolExecutor
/// 时通过 `with_artifact_publisher` 注入。当前函数只返回纯文件系统
/// 工具（保持单元测试不依赖 DB）。
pub fn builtin_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read_file".to_string(),
            description: "Read the contents of a file".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" }
                },
                "required": ["path"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "write_file".to_string(),
            description: "Write content to a file, creating it if it doesn't exist".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["path", "content"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "search_files".to_string(),
            description: "Search for a pattern in files using ripgrep".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Search pattern (regex)" },
                    "path": { "type": "string", "description": "Directory to search in (default: workspace root)" }
                },
                "required": ["pattern"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "shell_exec".to_string(),
            description: "Execute a shell command".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" }
                },
                "required": ["command"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "list_files".to_string(),
            description: "List files and directories at a given path".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path (default: workspace root)" }
                }
            }),
            cache_control: None,
        },
    ]
}

/// 完成检测的"标记"工具名（FM-15 FR-09.3）。Coding Agent 调用此工具表示任务完成；
/// AgentEngine 拦截后跳出主循环并执行 guardrails。
pub const TASK_COMPLETE_TOOL: &str = "task_complete";

/// FM-15 FR-09.3: 完成检测工具定义。Agent 调用 `task_complete(summary)` 显式声明完成；
/// AgentEngine 拦截 tool_use → 跑 guardrails → 决定是否真正进入 Completed 状态。
pub fn task_complete_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TASK_COMPLETE_TOOL.to_string(),
        description:
            "Signal that the task is complete. Provide a concise summary (1-3 sentences) of \
             what was done. The system will then run automated guardrail checks; if any check \
             fails, you will receive feedback and must continue working. Call this exactly once, \
             AFTER all required artifacts have been published via publish_artifact and after all \
             changes are saved to disk."
                .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Concise summary of what this task accomplished."
                }
            },
            "required": ["summary"]
        }),
        cache_control: None,
    }
}

/// FM-15 FR-03.2 + FR-09.3: 在 Coding Agent 任务真正执行时，把 `publish_artifact` 与
/// `task_complete` 追加到默认工具集；scheduler 在派发 task 时使用此函数。
pub fn coding_agent_tools_with_artifact_support() -> Vec<ToolDefinition> {
    let mut tools = builtin_tools();
    tools.push(crate::agent::artifacts::publish_artifact_tool_definition());
    tools.push(task_complete_tool_definition());
    tools
}

// ---- FM-15 v2.2 P4-S5: Chat Agent 工具集 ----

/// FR-15.4: Follow-up Chat 用于"超规模"场景升级到 plan 流程的工具。
pub const PROPOSE_FOLLOWUP_TOOL: &str = "propose_followup_mission";

pub fn propose_followup_mission_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: PROPOSE_FOLLOWUP_TOOL.to_string(),
        description:
            "Propose escalating the user's request to a brand-new follow-up mission \
             (which will go through Planner → DAG → Scheduler). Call this ONLY when the \
             user request clearly exceeds the small-edit threshold: more than 3 files, \
             more than ~30 lines of code change, or introduces new modules/dependencies. \
             After calling this you will pause and wait for the user's decision."
                .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short title for the proposed follow-up mission."
                },
                "rationale": {
                    "type": "string",
                    "description": "Why this should be a planned mission rather than a chat edit."
                },
                "estimated_tasks": {
                    "type": "integer",
                    "description": "Rough estimate of how many tasks the follow-up will need."
                },
                "request_summary": {
                    "type": "string",
                    "description": "One-paragraph summary of the user's request, suitable for use \
                                    as the new mission's description."
                }
            },
            "required": ["title", "rationale", "estimated_tasks", "request_summary"]
        }),
        cache_control: None,
    }
}

/// Chat Agent 工具集：与 Coding Agent 共享文件/搜索/shell 工具，但替换完成语义。
/// 不开 worktree，commit 直接落到 main（由 chat.rs 在 task_complete 后处理）。
pub fn chat_agent_tools() -> Vec<ToolDefinition> {
    let mut tools = builtin_tools();
    tools.push(propose_followup_mission_tool_definition());
    tools.push(task_complete_tool_definition());
    tools
}
