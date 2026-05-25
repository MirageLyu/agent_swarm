use crate::llm::ToolDefinition;
use serde_json::json;

/// Coding Agent 默认工具集。FM-15 FR-03.2 的 `publish_artifact`
/// 暂未通过 ToolExecutor 路径下发——它需要 DB + repo_path 这两个
/// runtime 上下文，由 Phase 2 的 dispatch_task 在装载 ToolExecutor
/// 时通过 `with_artifact_publisher` 注入。当前函数只返回纯文件系统
/// 工具（保持单元测试不依赖 DB）。
///
/// Single-Agent Uplift Phase 1.4: 实际实现已经迁到 `tools::registry::TOOLS`，
/// 这里保留入口名是为了 backward compatibility（多处调用方在用）。
pub fn builtin_tools() -> Vec<ToolDefinition> {
    super::registry::all_definitions()
}

/// 旧实现，保留作为参照——`registry::TOOLS` 是新的事实源。改 schema 在 registry 改即可。
#[allow(dead_code)]
fn builtin_tools_legacy() -> Vec<ToolDefinition> {
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
            description: "Write content to a file, creating it if it doesn't exist. \
                          \n\n**Size guidance**: Keep `content` under ~6KB per call. LLM API responses \
                          have a per-response output token cap; generating one huge `content` string \
                          risks the response being truncated mid-string, corrupting the JSON \
                          arguments and failing the call. For large files, write a short skeleton \
                          first then use edit_file with anchor strings to insert each section."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "content": {
                        "type": "string",
                        "description": "Content to write. Keep under ~6KB per call; split large files into skeleton + edit_file appends."
                    }
                },
                "required": ["path", "content"]
            }),
            cache_control: None,
        },
        ToolDefinition {
            name: "grep".to_string(),
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
            description: "Execute a shell command (sh -c). Killed by a watchdog when it goes \
                          silent for too long or runs past the wall-clock cap. Default thresholds \
                          are 60s idle / 5min wall — set `expect_long_running: true` for known \
                          long commands like `npm install`, `pnpm install`, `cargo build`, \
                          `cargo test` (raises to 120s idle / 30min wall), or pass \
                          `timeout_seconds` / `idle_timeout_seconds` for an explicit command-level cap. \
                          The subprocess inherits the agent environment, including proxy variables like ALL_PROXY."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" },
                    "expect_long_running": {
                        "type": "boolean",
                        "description": "Set to true for installs / heavy builds / long test suites. Defaults to false."
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Optional wall-clock timeout for this command in seconds. Must be between 1 and 1800."
                    },
                    "idle_timeout_seconds": {
                        "type": "integer",
                        "description": "Optional idle timeout in seconds with no stdout/stderr. Must be between 1 and 120."
                    }
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
        // Single-Agent Uplift Phase 1.1: 精确字符串替换。比 write_file 整文件改更安全，
        // context 也更省（不需要把整个文件原样回 push 给 LLM）。
        // - old_string 必须唯一，否则结构化报错；要批量替换走 replace_all=true。
        // - 必须先 read_file 同一路径再 edit；否则报错（防 LLM 凭空臆造）。
        ToolDefinition {
            name: "edit_file".to_string(),
            description: "Replace exact text in an existing file. Safer than write_file for \
                          small changes — it preserves the rest of the file untouched. \
                          REQUIREMENTS:\n\
                          - You MUST call read_file on the same path at least once in this \
                          session before edit_file works (so the model has seen the real content).\n\
                          - `old_string` must occur EXACTLY once in the file unless \
                          `replace_all` is true; otherwise the call fails with a uniqueness error \
                          and you should add more surrounding context to old_string and retry.\n\
                          - `old_string` must match exactly (whitespace, indentation, line endings).\n\
                          - On success, returns { path, replacements, lines_added, lines_removed }."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "old_string": {
                        "type": "string",
                        "description": "Exact text to find. Must be unique unless replace_all=true."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Text to substitute. Pass empty string to delete."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace every occurrence (default false). Useful for renames."
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
            cache_control: None,
        },
        // Single-Agent Uplift Phase 1.3: 文件名通配。`grep` 内容搜索擅长，
        // 但找文件用 rg 很别扭。glob 用 globwalk 直接按 mtime 排序返回前 N 条，
        // 对应 innerCC GlobTool 的核心用例。
        ToolDefinition {
            name: "glob".to_string(),
            description: "Find files by name/path glob pattern, sorted by recency (most-recently \
                          modified first). Examples: `**/*.rs`, `src/**/test_*.ts`, \
                          `**/migrations/*.sql`. Use this when you know the filename shape; use \
                          `grep` (ripgrep) for content search."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern (e.g. `**/*.rs`, `src/components/**/*.tsx`)."
                    },
                    "path": {
                        "type": "string",
                        "description": "Root directory to search under (default: workspace root)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results (default 100, max 500)."
                    }
                },
                "required": ["pattern"]
            }),
            cache_control: None,
        },
    ]
}

/// 完成检测的"标记"工具名（FM-15 FR-09.3）。Coding Agent 调用此工具表示任务完成；
/// AgentEngine 拦截后跳出主循环并执行 guardrails。
pub const TASK_COMPLETE_TOOL: &str = "task_complete";

/// Single-Agent Uplift Phase 1.2: TodoWriteTool 工具名常量。
pub const TODO_WRITE_TOOL: &str = "todo_write";

/// Single-Agent Uplift Phase 2.4: EnterPlanMode 工具名常量。
pub const ENTER_PLAN_MODE_TOOL: &str = "enter_plan_mode";

/// Single-Agent Uplift B1: AskUserQuestion 工具名常量。
pub const ASK_USER_QUESTION_TOOL: &str = "ask_user_question";

/// Single-Agent Uplift B1: AskUserQuestion 工具定义。
///
/// 适用场景：当且仅当下列条件都满足，才该调用这个工具：
///   - 多个等价/合理的实现路径，**没有客观标准**让 agent 自己挑（典型：UI 风格、API 命名风格）
///   - 选错的代价**不可逆**或**昂贵**（写到主分支 / 推到生产 / 大量级联改动）
///
/// 反例（应当 agent 自己定）：
///   - 编译错误怎么修——技术决定，自己查 / 试
///   - 用 `Vec` 还是 `LinkedList`——查一下基准
///   - 命名 i 还是 idx——别浪费用户时间
///
/// 行为：调用后 agent 暂停当前 LLM 步骤等用户答复（默认 30 分钟超时；
/// 用户取消该 agent 也会唤醒）。返回值是 JSON `{ session_id, answers: { qid: [option_id, ...] } }`，
/// 用户没回复时返回 `{ timed_out: true }`，agent 应据此自行决断或 task_complete 报告无法继续。
pub fn ask_user_question_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: ASK_USER_QUESTION_TOOL.to_string(),
        description:
            "Ask the user one or more multiple-choice questions and pause until they answer. \
             Use this VERY sparingly — only when (a) there are multiple equally-valid \
             approaches with no objective tiebreaker, AND (b) picking wrong is expensive or \
             irreversible. Do NOT use it for things you can decide yourself (build errors, \
             naming choices, library benchmarks). \
             Behavior: each question has labeled options; the user picks one (or several if \
             allow_multiple=true) per question. The call blocks for up to 30 minutes; if no \
             answer arrives, you'll get `{ timed_out: true }` and should pick a sensible \
             default or call task_complete to report you cannot continue."
                .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "One or more questions to present together.",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Stable id used to key the answer back."
                            },
                            "prompt": {
                                "type": "string",
                                "description": "The question text shown to the user."
                            },
                            "options": {
                                "type": "array",
                                "description": "At least 2 options.",
                                "minItems": 2,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "label": { "type": "string" }
                                    },
                                    "required": ["id", "label"]
                                }
                            },
                            "allow_multiple": {
                                "type": "boolean",
                                "description": "If true, user can select multiple options. Default false."
                            }
                        },
                        "required": ["id", "prompt", "options"]
                    }
                }
            },
            "required": ["questions"]
        }),
        cache_control: None,
    }
}

/// Single-Agent Uplift Phase 2.4: EnterPlanMode 工具定义。
///
/// 为什么和 TodoWrite 同时提供：TodoWrite 是一个迭代的 in-flight 状态机
/// （pending → in_progress → completed），适合执行过程中追踪进度；
/// EnterPlanMode 是一次性的"我打算这样做"声明，发生在动手前，输出整段 markdown
/// 计划方便用户/guardrail 一眼审阅。两者职能不同——
///   - `enter_plan_mode`：在第一次写盘之前调用，把整体方案讲清楚
///   - `todo_write`：跟踪具体步骤进度
///
/// 落地：LLM 调它后，后端 emit `tool_use` 事件携带 plan 文本（meta.plan），
/// 工具返回值是简短确认。前端 ToolUseLine 已经能解析 meta 渲染出来——故意复用
/// 现有 renderer 而不是新造一个 plan UI，以最小化 Phase 2 的改动面。
pub fn enter_plan_mode_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: ENTER_PLAN_MODE_TOOL.to_string(),
        description: "Declare an end-to-end plan BEFORE making any code changes. Use this for \
                      tasks with non-trivial scope (3+ files, architectural choices, or risky \
                      changes). Writing the plan down lets the user/guardrail catch \
                      mis-direction early instead of waiting for 30 wasted steps. \
                      Skip this for trivial single-file edits — overhead would slow the \
                      feedback loop. After calling this, proceed with the implementation; \
                      use `todo_write` to track step-by-step progress."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan": {
                    "type": "string",
                    "description": "Concise markdown plan: goal, approach, files to touch, risks."
                }
            },
            "required": ["plan"]
        }),
        cache_control: None,
    }
}

/// Single-Agent Uplift Phase 1.2: TodoWrite 工具定义。
///
/// 让 Agent 把"待办清单"作为外显状态——相比把 todo 混进 message 流，前端能独立
/// 渲染 panel，用户一眼看到进度。语义对齐 Cursor / Claude Code：
///   - 每次调用都是"我现在的清单是这样"——后端**全量替换**。
///   - 同一时间最多一个 in_progress，pending → in_progress → completed。
///   - 所有 pending/in_progress 完成 → completed。
pub fn todo_write_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TODO_WRITE_TOOL.to_string(),
        description:
            "Maintain a structured todo list for the current task. Each call replaces the entire \
             list — pass the FULL current state, not just the new item. Mark exactly one item as \
             in_progress when actively working on it; mark items completed as soon as they finish. \
             Use this for tasks with 3+ steps so the user can see progress in real time. Skip it \
             for trivial single-step requests."
                .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Full ordered list of todos for this task.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Stable id for this item (any unique string within the list)."
                            },
                            "content": {
                                "type": "string",
                                "description": "Short, action-oriented description (imperative)."
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"],
                                "description": "Current state. At most ONE item should be in_progress."
                            }
                        },
                        "required": ["id", "content", "status"]
                    }
                }
            },
            "required": ["todos"]
        }),
        cache_control: None,
    }
}

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
    tools.push(todo_write_tool_definition());
    tools.push(enter_plan_mode_tool_definition());
    tools.push(ask_user_question_tool_definition());
    tools.push(task_complete_tool_definition());
    tools
}

// ---- FM-15 v2.2 P4-S5: Chat Agent 工具集 ----

/// FR-15.4: Follow-up Chat 用于"超规模"场景升级到 plan 流程的工具。
pub const PROPOSE_FOLLOWUP_TOOL: &str = "propose_followup_mission";

pub fn propose_followup_mission_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: PROPOSE_FOLLOWUP_TOOL.to_string(),
        description: "Propose escalating the user's request to a brand-new follow-up mission \
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
