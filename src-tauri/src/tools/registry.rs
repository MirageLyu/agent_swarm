//! Single-Agent Uplift Phase 1.4: Tool registry。
//!
//! 之前加新工具要改三处（definitions.rs schema、definitions.rs builtin_tools()
//! 列表、executor.rs match 分支），任何一处漏了都不会编译报错而是运行时静默 fallback。
//! 这里把"工具有哪些"和"工具长啥样"集中到一个 registry，加新内置工具只需：
//!   ① 在本文件加 ToolSpec 一项
//!   ② 在 ToolExecutor::execute 加 match 分支调对应的 async fn
//!
//! 没用 `inventory` crate 是因为 Tauri 静态链接 + macOS dyld 上经常吃 linker section
//! 剥离，导致 release build 工具缺失而 dev 正常。这种"幻象 bug"非常贵。
//! 显式 const 数组虽然 verbose，但保证编译期检查 + 单一事实源。
//!
//! Phase 2.1 StreamingToolExecutor 直接从 spec.is_concurrency_safe 决策能不能并发跑。

use crate::llm::ToolDefinition;

/// 工具元数据。一个 tool name 对应一个 ToolSpec。
pub struct ToolSpec {
    pub name: &'static str,
    /// 是否可与其它 concurrency-safe 工具并发执行（无副作用 + 不抢 lock）。
    /// 写盘 / 跑 shell / 改 DB 一律 false。
    pub is_concurrency_safe: bool,
    /// 生成给 LLM 看的 schema。lazy 一点 —— 每次 hot-reload 都重建，保证修描述时不需要重启。
    pub make_definition: fn() -> ToolDefinition,
}

impl ToolSpec {
    pub fn definition(&self) -> ToolDefinition {
        (self.make_definition)()
    }
}

/// 完整的内置工具表。
///
/// 这里 + ToolExecutor::execute 的 match 是新工具唯一要碰的两处。
/// Phase 1 增量加的：edit_file / glob / todo_write。
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "read_file",
        is_concurrency_safe: true,
        make_definition: defs::read_file_def,
    },
    ToolSpec {
        name: "write_file",
        is_concurrency_safe: false,
        make_definition: defs::write_file_def,
    },
    ToolSpec {
        name: "edit_file",
        is_concurrency_safe: false,
        make_definition: defs::edit_file_def,
    },
    ToolSpec {
        name: "search_files",
        is_concurrency_safe: true,
        make_definition: defs::search_files_def,
    },
    ToolSpec {
        name: "glob",
        is_concurrency_safe: true,
        make_definition: defs::glob_def,
    },
    ToolSpec {
        name: "list_files",
        is_concurrency_safe: true,
        make_definition: defs::list_files_def,
    },
    ToolSpec {
        name: "shell_exec",
        is_concurrency_safe: false,
        make_definition: defs::shell_exec_def,
    },
];

/// O(N) lookup —— TOOLS 长度 ~10 内不需要 HashMap，cache friendliness 反而更好。
pub fn lookup(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.name == name)
}

/// 给 LLM 暴露的所有内置工具定义。
pub fn all_definitions() -> Vec<ToolDefinition> {
    TOOLS.iter().map(|t| t.definition()).collect()
}

mod defs {
    //! 每个 def 函数返回一个 ToolDefinition。从 super::definitions 模块的旧函数迁过来的；
    //! 新工具加在这里 + 在 TOOLS 数组里挂一行就完事。
    use super::*;
    use serde_json::json;

    pub fn read_file_def() -> ToolDefinition {
        ToolDefinition {
            name: "read_file".to_string(),
            description: "Read the contents of a file at the given path".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" }
                },
                "required": ["path"]
            }),
            cache_control: None,
        }
    }

    pub fn write_file_def() -> ToolDefinition {
        ToolDefinition {
            name: "write_file".to_string(),
            description: "Write content to a file. Creates parent directories if needed."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["path", "content"]
            }),
            cache_control: None,
        }
    }

    pub fn edit_file_def() -> ToolDefinition {
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
        }
    }

    pub fn search_files_def() -> ToolDefinition {
        ToolDefinition {
            name: "search_files".to_string(),
            description: "Search for a pattern in files (uses ripgrep-like semantics)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Search pattern (regex)" },
                    "path": { "type": "string", "description": "Directory to search in" }
                },
                "required": ["pattern"]
            }),
            cache_control: None,
        }
    }

    pub fn glob_def() -> ToolDefinition {
        ToolDefinition {
            name: "glob".to_string(),
            description: "Find files by name/path glob pattern, sorted by recency (most-recently \
                          modified first). Examples: `**/*.rs`, `src/**/test_*.ts`, \
                          `**/migrations/*.sql`. Use this when you know the filename shape; use \
                          `search_files` (ripgrep) for content search."
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
        }
    }

    pub fn list_files_def() -> ToolDefinition {
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
        }
    }

    pub fn shell_exec_def() -> ToolDefinition {
        ToolDefinition {
            name: "shell_exec".to_string(),
            description: "Execute a shell command (use sparingly)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" }
                },
                "required": ["command"]
            }),
            cache_control: None,
        }
    }
}
