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
        // 历史名 `search_files`；2026-05 重命名为 `grep` 对齐业界（Claude Code / Cursor 都叫 Grep），
        // 让 LLM tool-routing 更准。`search_files` 作为 alias 仍被 `lookup` / `executor` 接受，
        // 保留旧 hook config / 旧 session replay 的兼容性（详见 `canonicalize`）。
        name: "grep",
        is_concurrency_safe: true,
        make_definition: defs::grep_def,
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

/// Tool alias 表：`(old_name, canonical_name)`。
///
/// 重命名工具时把旧名加进这里即可让 `lookup` / `canonicalize` / hook matcher 透明兼容。
/// 见 `canonicalize` 文档与 `tools::executor::ToolExecutor::execute` 的 alias 路由。
const ALIASES: &[(&str, &str)] = &[
    // 2026-05: search_files → grep（对齐 Claude Code / Cursor 命名）
    ("search_files", "grep"),
];

/// 把任意工具名（含 alias）规范化为 canonical 名。
///
/// 调用方：
/// - `lookup`：alias 反查到 spec
/// - `executor::execute`：alias 路由到主实现
/// - `hooks::command::matches`：双向 canonicalize 确保旧 hook config（`matcher: "search_files"`）
///   在新 `grep` 事件上仍能匹配，反之亦然
///
/// 行为：
/// - 已是主名 → 原样返回
/// - 是 alias → 返回主名
/// - 未知名 → 原样返回（不做任何猜测；保持调用方现有 unknown-tool 错处理逻辑）
pub fn canonicalize(name: &str) -> &str {
    for (alias, canonical) in ALIASES {
        if *alias == name {
            return canonical;
        }
    }
    name
}

/// O(N) lookup —— TOOLS 长度 ~10 内不需要 HashMap，cache friendliness 反而更好。
///
/// 同时识别 alias：`lookup("search_files")` 等价于 `lookup("grep")`，返回主 spec。
pub fn lookup(name: &str) -> Option<&'static ToolSpec> {
    let canonical = canonicalize(name);
    TOOLS.iter().find(|t| t.name == canonical)
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
            description:
                "Read a text file. Returns each line prefixed with `LINE_NUMBER|` (right-padded to \
                 6 columns) so you can reference exact lines when calling edit_file. \
                 Behavior:\n\
                 - Without offset/limit: reads up to the first 2000 lines; if truncated, the \
                   output ends with a hint telling you the next offset to pass.\n\
                 - With offset (1-indexed) and/or limit: returns that explicit window. Use this \
                   to page through large files (e.g. offset=2001 limit=2000).\n\
                 - If you re-read a file whose mtime has not changed since your last read or \
                   write, you'll get an `unchanged_since_last_read` stub instead of the full \
                   content — reuse the previous output rather than re-reading.\n\
                 - Binary files (NUL bytes) and files larger than 4 MiB are rejected; use \
                   grep / shell_exec for those."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "offset": {
                        "type": "integer",
                        "description": "1-indexed starting line. Pair with `limit` to page through large files."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max number of lines to return (hard cap 5000)."
                    }
                },
                "required": ["path"]
            }),
            cache_control: None,
        }
    }

    pub fn write_file_def() -> ToolDefinition {
        ToolDefinition {
            name: "write_file".to_string(),
            description:
                "Write content to a file (creates parent directories if needed). \
                 \n\n**Size limit guidance**: Keep `content` under ~6KB per call. The LLM API has a \
                 per-response output token cap (typically 16K tokens) — generating a single huge \
                 `content` string risks the response being truncated mid-string, which corrupts \
                 the JSON arguments and causes the call to fail. \
                 \n**For large files**, prefer this pattern:\n\
                 1. write_file with a short skeleton (headers, outline, top-level structure)\n\
                 2. edit_file (with `edits[]`) to insert each section using anchor strings\n\
                 Or use shell_exec with `cat <<EOF >> path` to append in chunks (each chunk \
                 must finish in one call)."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "content": {
                        "type": "string",
                        "description": "Content to write. Keep under ~6KB per call; split large files into a skeleton + edit_file appends."
                    }
                },
                "required": ["path", "content"]
            }),
            cache_control: None,
        }
    }

    pub fn edit_file_def() -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".to_string(),
            description:
                "Replace exact text in an existing file. Safer than write_file for small changes \
                 — it preserves the rest of the file untouched.\n\
                 REQUIREMENTS:\n\
                 - You MUST call read_file on the same path at least once in this session before \
                   edit_file works (so the model has seen the real content). write_file on the \
                   same path also satisfies this.\n\
                 - For each edit, `old_string` must occur EXACTLY once in the file unless \
                   `replace_all` is true.\n\
                 - `old_string` must match exactly (whitespace, indentation, line endings). \
                   Helpful fallbacks are applied automatically: line-number prefixes from \
                   read_file are stripped, and curly quotes / NBSP / em-dashes are normalized to \
                   ASCII before matching.\n\n\
                 USAGE: pass either the single-edit shape (`old_string` + `new_string` [+ \
                 `replace_all`]) or the multi-edit shape (`edits: [{ old_string, new_string, \
                 replace_all? }, ...]`) to apply N changes in one call. Multi-edit is atomic — \
                 if any individual edit fails, nothing is written.\n\n\
                 On success returns: { path, replacements, edits_applied, lines_added, \
                 lines_removed }."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "old_string": {
                        "type": "string",
                        "description": "[single-edit] Exact text to find. Must be unique unless replace_all=true."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "[single-edit] Text to substitute. Pass empty string to delete."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "[single-edit] Replace every occurrence (default false). Useful for renames."
                    },
                    "edits": {
                        "type": "array",
                        "description": "[multi-edit] Ordered array of edits applied atomically.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string" },
                                "new_string": { "type": "string" },
                                "replace_all": { "type": "boolean" }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path"]
            }),
            cache_control: None,
        }
    }

    pub fn grep_def() -> ToolDefinition {
        ToolDefinition {
            name: "grep".to_string(),
            description:
                "A powerful search tool built on ripgrep. Use this to search file contents \
                 (regex over text). For finding files by name/path pattern use `glob`; for \
                 listing a directory use `list_files`.\n\n\
                 Output modes:\n\
                 - `content` (default): matching lines with `path:lineno:text`. Optional context \
                   via `context_before` / `context_after` / `context`.\n\
                 - `files_with_matches`: just the file paths that contain a match.\n\
                 - `count`: per-file match counts.\n\
                 Filter scope with `glob` (e.g. `\"*.rs\"`, `\"!**/target/**\"`) or `type` (e.g. \
                 `\"rust\"`, `\"py\"`). Pass `case_insensitive: true` for `-i` semantics, or \
                 `multiline: true` so `.` matches across line breaks. Use `head_limit` to cap \
                 the size of the returned output (default 200 lines)."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern (ripgrep syntax)" },
                    "path": {
                        "type": "string",
                        "description": "Directory or single file to search under (default: workspace root)."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Path filter glob, e.g. `*.rs` or `!**/target/**`."
                    },
                    "type": {
                        "type": "string",
                        "description": "Filetype filter, e.g. `rust`, `py`, `ts`."
                    },
                    "case_insensitive": {
                        "type": "boolean",
                        "description": "Case-insensitive search (rg -i)."
                    },
                    "multiline": {
                        "type": "boolean",
                        "description": "Multiline + dotall mode so `.` matches newlines."
                    },
                    "output_mode": {
                        "type": "string",
                        "enum": ["content", "files_with_matches", "count"],
                        "description": "What to return; defaults to `content`."
                    },
                    "context_before": {
                        "type": "integer",
                        "description": "Lines of context before each match (rg -B). Content mode only."
                    },
                    "context_after": {
                        "type": "integer",
                        "description": "Lines of context after each match (rg -A). Content mode only."
                    },
                    "context": {
                        "type": "integer",
                        "description": "Symmetric context (rg -C). Content mode only."
                    },
                    "head_limit": {
                        "type": "integer",
                        "description": "Truncate output to first N lines. Default 200."
                    }
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
            description:
                "Execute a shell command (sh -c). Killed by a watchdog when it goes silent for \
                 too long or runs past the wall-clock cap. Default thresholds are 60s idle / \
                 5min wall — set `expect_long_running: true` for known long commands like \
                 `npm install`, `cargo build`, `cargo test` (raises to 120s idle / 30min wall), \
                 or pass `timeout_seconds` / `idle_timeout_seconds` for an explicit command-level cap. \
                 The subprocess inherits the agent environment, including proxy variables like ALL_PROXY."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" },
                    "expect_long_running": {
                        "type": "boolean",
                        "description": "Set to true for installs / heavy builds / long test suites."
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_passes_through_unknown() {
        assert_eq!(canonicalize("read_file"), "read_file");
        assert_eq!(canonicalize("nonexistent_tool"), "nonexistent_tool");
        assert_eq!(canonicalize(""), "");
    }

    #[test]
    fn canonicalize_resolves_search_files_alias() {
        assert_eq!(canonicalize("search_files"), "grep");
    }

    #[test]
    fn canonicalize_idempotent_on_canonical_name() {
        // grep 是主名，canonicalize 不应再改它
        assert_eq!(canonicalize("grep"), "grep");
        assert_eq!(canonicalize(canonicalize("search_files")), "grep");
    }

    #[test]
    fn lookup_finds_canonical_name() {
        let spec = lookup("grep").expect("grep should be in TOOLS");
        assert_eq!(spec.name, "grep");
        assert!(spec.is_concurrency_safe);
    }

    #[test]
    fn lookup_finds_alias_via_canonicalize() {
        // search_files 是 alias，应能反查到 grep 的 spec
        let spec = lookup("search_files").expect("search_files alias should resolve");
        assert_eq!(spec.name, "grep");
        assert!(
            spec.is_concurrency_safe,
            "concurrency-safe bit must survive alias lookup; otherwise old session replays \
             would lose parallel-read capability"
        );
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        assert!(lookup("totally_made_up").is_none());
    }

    #[test]
    fn all_definitions_exposes_grep_as_primary_name() {
        // 给 LLM 看的 tool 列表里必须是 grep，不能漏；search_files 不应作为独立条目重复出现。
        let defs = all_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(
            names.contains(&"grep"),
            "grep must be exposed; got {names:?}"
        );
        assert!(
            !names.contains(&"search_files"),
            "search_files should be alias-only (hidden from LLM), got {names:?}"
        );
    }

    #[test]
    fn grep_definition_description_mentions_ripgrep_and_grep() {
        // LLM 命名识别基础：description 第一段应明示 ripgrep + 区分 glob 用途。
        let def = lookup("grep").unwrap().definition();
        let desc = def.description.to_lowercase();
        assert!(
            desc.contains("ripgrep"),
            "desc must mention ripgrep: {}",
            def.description
        );
        assert!(
            desc.contains("glob"),
            "desc should hint when to use glob instead"
        );
    }
}
