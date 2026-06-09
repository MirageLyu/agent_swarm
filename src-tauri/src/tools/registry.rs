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

pub enum ResultBudgetMode {
    Inline,
    ReferencePreferred,
    ManifestPreferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFingerprintMode {
    Input,
    Path,
    Pattern,
    CommandSource,
}

/// 工具元数据。一个 tool name 对应一个 ToolSpec。
pub struct ToolSpec {
    pub name: &'static str,
    /// 是否可与其它 concurrency-safe 工具并发执行（无副作用 + 不抢 lock）。
    /// 写盘 / 跑 shell / 改 DB 一律 false。
    pub is_concurrency_safe: bool,
    pub max_result_size_chars: usize,
    pub is_read_only: bool,
    pub is_search_or_read_command: bool,
    pub result_budget_mode: ResultBudgetMode,
    pub source_fingerprint_mode: SourceFingerprintMode,
    /// 生成给 LLM 看的 schema。lazy 一点 —— 每次 hot-reload 都重建，保证修描述时不需要重启。
    pub make_definition: fn() -> ToolDefinition,
}

impl ToolSpec {
    const fn new(
        name: &'static str,
        is_concurrency_safe: bool,
        max_result_size_chars: usize,
        is_read_only: bool,
        is_search_or_read_command: bool,
        result_budget_mode: ResultBudgetMode,
        source_fingerprint_mode: SourceFingerprintMode,
        make_definition: fn() -> ToolDefinition,
    ) -> Self {
        Self {
            name,
            is_concurrency_safe,
            max_result_size_chars,
            is_read_only,
            is_search_or_read_command,
            result_budget_mode,
            source_fingerprint_mode,
            make_definition,
        }
    }

    const fn read_tool(
        name: &'static str,
        max_result_size_chars: usize,
        source_fingerprint_mode: SourceFingerprintMode,
        make_definition: fn() -> ToolDefinition,
    ) -> Self {
        Self::new(
            name,
            true,
            max_result_size_chars,
            true,
            true,
            ResultBudgetMode::ReferencePreferred,
            source_fingerprint_mode,
            make_definition,
        )
    }

    const fn write_tool(
        name: &'static str,
        max_result_size_chars: usize,
        make_definition: fn() -> ToolDefinition,
    ) -> Self {
        Self::new(
            name,
            false,
            max_result_size_chars,
            false,
            false,
            ResultBudgetMode::ManifestPreferred,
            SourceFingerprintMode::Path,
            make_definition,
        )
    }

    pub fn definition(&self) -> ToolDefinition {
        (self.make_definition)()
    }
}

/// 完整的内置工具表。
///
/// 这里 + ToolExecutor::execute 的 match 是新工具唯一要碰的两处。
/// Phase 1 增量加的：edit_file / glob / todo_write。
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec::read_tool(
        "read_file",
        10 * 1024,
        SourceFingerprintMode::Path,
        defs::read_file_def,
    ),
    ToolSpec::write_tool("write_file", 4 * 1024, defs::write_file_def),
    ToolSpec::write_tool("edit_file", 4 * 1024, defs::edit_file_def),
    ToolSpec::read_tool(
        "grep",
        8 * 1024,
        SourceFingerprintMode::Pattern,
        defs::grep_def,
    ),
    ToolSpec::read_tool(
        "glob",
        6 * 1024,
        SourceFingerprintMode::Pattern,
        defs::glob_def,
    ),
    ToolSpec::read_tool(
        "list_files",
        6 * 1024,
        SourceFingerprintMode::Path,
        defs::list_files_def,
    ),
    ToolSpec::new(
        "notebook_edit",
        false,
        8 * 1024,
        false,
        false,
        ResultBudgetMode::ManifestPreferred,
        SourceFingerprintMode::Path,
        defs::notebook_edit_def,
    ),
    ToolSpec::new(
        "shell_exec",
        false,
        6 * 1024,
        false,
        true,
        ResultBudgetMode::ReferencePreferred,
        SourceFingerprintMode::CommandSource,
        defs::shell_exec_def,
    ),
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
                 - If you read a large evidence/output file, the tool may return an evidence ref \
                   with bytes/lines/hash/excerpt instead of the full text; use grep or offset/limit \
                   for targeted retrieval.\n\
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
                 Accepts either `path` or `file_path` for the target path. Set `append: true` to \
                 append a chunk without overwriting the file.\
                 \n\n**Hard size guidance**: Keep `content` under ~4KB per call. The LLM API has a \
                 per-response output token cap (typically 16K tokens) — generating a single huge \
                 `content` string risks the response being truncated mid-string, which corrupts \
                 the JSON arguments and causes the call to fail. Never emit a full long script/report \
                 in one write_file call.\
                 \n**For large files**, prefer this pattern:\n\
                 1. write_file with `append: false` or omitted for the first short skeleton/chunk\n\
                 2. write_file with `append: true` for each additional small chunk\n\
                 3. use edit_file only for targeted corrections\n\
                 Avoid shell heredocs for long scripts unless each heredoc is short enough to finish \
                 in one tool call. For generated artifacts, create a minimal valid artifact early and iterate."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "file_path": { "type": "string", "description": "Alias for path, accepted for Claude Code-style calls" },
                    "content": {
                        "type": "string",
                        "description": "Content to write. Keep under ~4KB per call; split large files into chunks and pass append=true after the first chunk."
                    },
                    "append": {
                        "type": "boolean",
                        "description": "Append content to the existing file instead of overwriting it (default false). Use for chunked large files."
                    }
                },
                "required": ["content"],
                "anyOf": [
                    { "required": ["path"] },
                    { "required": ["file_path"] }
                ]
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
                 listing a directory use `list_files`. Use focused patterns and small context \
                 windows when searching evidence/output paths so repeated broad outputs stay out \
                 of the model context.\n\n\
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
                          `grep` (ripgrep) for content search. Default workspace glob/list views \
                          avoid evidence directories; pass an explicit evidence path only when you \
                          need to inspect preserved tool output."
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
            description: "List files and directories at a given path. Returns at most `limit` entries (default 200, max 1000) and reports when output is truncated. Default workspace listing avoids evidence directories; pass an explicit evidence path only when you need to inspect preserved tool output.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path (default: workspace root)" },
                    "limit": {
                        "type": "integer",
                        "description": "Max entries to return (default 200, max 1000). Use a more specific path when truncated."
                    }
                }
            }),
            cache_control: None,
        }
    }

    pub fn notebook_edit_def() -> ToolDefinition {
        ToolDefinition {
            name: "notebook_edit".to_string(),
            description:
                "Read or edit a Jupyter .ipynb notebook without using Python. Use this for notebook \
                 tasks instead of treating .ipynb as plain text. Operations: `read_cells`, \
                 `insert_cell`, `update_cell`, and `delete_cell`. Cell indexes are zero-based. \
                 For insertion, pass either `index` to insert at an exact position, or \
                 `after_cell_index` / `after_source_contains` to insert after an existing cell. \
                 Source may be a string or an array of lines. For notebooks that will be checked \
                 statically, include required computed values visibly in the cell source as comments, \
                 asserts, or displayed literals while preserving the derivation; do not rely only on \
                 unevaluated expressions whose values are invisible to a static grader."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Notebook path relative to workspace root" },
                    "operation": {
                        "type": "string",
                        "enum": ["read_cells", "insert_cell", "update_cell", "delete_cell"],
                        "description": "Notebook operation to perform"
                    },
                    "index": {
                        "type": "integer",
                        "description": "Zero-based cell index for read/update/delete, or exact insertion index for insert_cell"
                    },
                    "after_cell_index": {
                        "type": "integer",
                        "description": "For insert_cell: insert after this zero-based cell index"
                    },
                    "after_source_contains": {
                        "type": "string",
                        "description": "For insert_cell: insert after the first cell whose source contains this text"
                    },
                    "cell_type": {
                        "type": "string",
                        "enum": ["code", "markdown", "raw"],
                        "description": "Cell type for insert_cell/update_cell; defaults to code for inserted cells"
                    },
                    "source": {
                        "description": "Cell source as a string or array of lines. When inserting/updating derived results for statically graded notebooks, keep formulas plus visible expected values in comments/asserts/literals (for example `# mean_value = 7.25`) so the saved source is auditable without executing the notebook.",
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" } }
                        ]
                    },
                    "limit": {
                        "type": "integer",
                        "description": "For read_cells: max cells to return (default 50)"
                    }
                },
                "required": ["path", "operation"]
            }),
            cache_control: None,
        }
    }

    pub fn shell_exec_def() -> ToolDefinition {
        ToolDefinition {
            name: "shell_exec".to_string(),
            description:
                "Execute a shell command (sh -c) in the workspace. Commands run in the agent process environment, not an interactive login shell, so PATH and tool availability may differ from your terminal. Killed by a watchdog when it goes silent for too long or runs past the wall-clock cap. Default thresholds are 60s idle / 5min wall — set `expect_long_running: true` for known long commands like `npm install`, `cargo build`, `cargo test` (raises to 120s idle / 30min wall), or pass `timeout_seconds` / `idle_timeout_seconds` for an explicit command-level cap. The subprocess inherits proxy variables like ALL_PROXY, but values are not shown. Large, content-shaped, or repeated stdout/stderr may be returned as a compact evidence ref or repeat ref instead of full text; the original output remains preserved in the referenced evidence files and events. Prefer focused extraction commands, `grep`, or `read_file` with offset/limit over repeating broad `curl`, `cat`, `find`, or listing commands just to see the same full output again. When a command is missing, not executable, uses unsupported options, or times out, the tool returns structured capability feedback with exit code and stderr so you can adapt to the observed runtime instead of repeating the same command."
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
    fn all_definitions_exposes_notebook_edit() {
        let defs = all_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(
            names.contains(&"notebook_edit"),
            "notebook_edit must be exposed for .ipynb benchmark tasks; got {names:?}"
        );
        let spec = lookup("notebook_edit").expect("notebook_edit should be in TOOLS");
        assert!(!spec.is_concurrency_safe);
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

    #[test]
    fn descriptions_explain_evidence_refs_for_large_outputs() {
        let shell = lookup("shell_exec").unwrap().definition();
        let shell_desc = shell.description.to_lowercase();
        assert!(
            shell_desc.contains("evidence ref"),
            "shell_exec desc should mention evidence refs"
        );
        assert!(
            shell_desc.contains("repeat ref"),
            "shell_exec desc should mention repeat refs"
        );
        assert!(
            shell_desc.contains("grep"),
            "shell_exec desc should suggest focused grep retrieval"
        );

        let read_file = lookup("read_file").unwrap().definition();
        let read_desc = read_file.description.to_lowercase();
        assert!(
            read_desc.contains("evidence ref"),
            "read_file desc should mention evidence refs"
        );
        assert!(
            read_desc.contains("offset/limit"),
            "read_file desc should suggest paged retrieval"
        );
    }
}
