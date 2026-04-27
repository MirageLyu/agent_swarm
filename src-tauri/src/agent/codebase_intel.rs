//! FM-15 FR-10: Codebase Intelligence 注入。
//!
//! 在 Coding Agent 启动时为 system prompt 追加四块上下文：
//! - `[Project Structure]`：repo 顶层 ≤3 层目录树（忽略大目录）
//! - `[Tech Stack]`：基于文件签名启发式识别语言/框架
//! - `[Upstream Context]`：上游 task 的 completion_summary + 已发布 artifacts
//! - `[Base Conflicts]`：当前 task 的 task_base_conflicts 摘要
//!
//! 优先级（FR-10.4）：超过总长度上限时按 Skills > Upstream > Project > Tech > BaseConflicts
//! 顺序保留并截断其它。本模块只产出文本块，截断/拼接由调用方负责。

use serde::Serialize;
use std::path::Path;

const TREE_MAX_DEPTH: usize = 3;
const TREE_MAX_ENTRIES_PER_DIR: usize = 30;
const PROJECT_TREE_BUDGET_CHARS: usize = 3_500;
const TECH_STACK_BUDGET_CHARS: usize = 800;
const UPSTREAM_BUDGET_CHARS: usize = 4_000;
const BASE_CONFLICTS_BUDGET_CHARS: usize = 1_500;
/// 哪些目录在生成 tree 时直接跳过（噪声 / 体积大）。
const TREE_IGNORE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".venv",
    "__pycache__",
    "vendor",
    ".idea",
    ".vscode",
    ".turbo",
];

#[derive(Debug, Clone, Default, Serialize)]
pub struct CodebaseIntel {
    pub project_structure: String,
    pub tech_stack: String,
    pub upstream_context: String,
    pub base_conflicts: String,
}

impl CodebaseIntel {
    /// 把四块拼接成 Agent system prompt 的附加段；空段自动跳过。
    pub fn render_system_block(&self) -> String {
        let mut out = String::new();
        if !self.project_structure.trim().is_empty() {
            out.push_str("\n\n[Project Structure]\n");
            out.push_str(self.project_structure.trim_end());
        }
        if !self.tech_stack.trim().is_empty() {
            out.push_str("\n\n[Tech Stack]\n");
            out.push_str(self.tech_stack.trim_end());
        }
        if !self.upstream_context.trim().is_empty() {
            out.push_str("\n\n[Upstream Context]\n");
            out.push_str(self.upstream_context.trim_end());
        }
        if !self.base_conflicts.trim().is_empty() {
            out.push_str("\n\n[Base Conflicts]\n");
            out.push_str(self.base_conflicts.trim_end());
        }
        out
    }
}

/// 一站式构建：扫描 repo + 查 DB（如果 db 提供）→ 组装 4 块。
pub fn build_intel(
    repo_root: &Path,
    task_id: Option<&str>,
    db: Option<&crate::db::Database>,
) -> CodebaseIntel {
    let project_structure = build_project_tree(repo_root);
    let tech_stack = detect_tech_stack(repo_root);

    let (upstream_context, base_conflicts) = match (task_id, db) {
        (Some(tid), Some(db)) => {
            let upstream = build_upstream_context(db, tid);
            let conflicts = build_base_conflicts(db, tid);
            (upstream, conflicts)
        }
        _ => (String::new(), String::new()),
    };

    CodebaseIntel {
        project_structure: truncate_block(&project_structure, PROJECT_TREE_BUDGET_CHARS),
        tech_stack: truncate_block(&tech_stack, TECH_STACK_BUDGET_CHARS),
        upstream_context: truncate_block(&upstream_context, UPSTREAM_BUDGET_CHARS),
        base_conflicts: truncate_block(&base_conflicts, BASE_CONFLICTS_BUDGET_CHARS),
    }
}

// ---- Project tree (FR-10.1) ----

fn build_project_tree(root: &Path) -> String {
    let mut buf = String::new();
    let label = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(".");
    buf.push_str(label);
    buf.push('\n');
    walk(root, 0, "", &mut buf);
    buf
}

fn walk(dir: &Path, depth: usize, prefix: &str, buf: &mut String) {
    if depth >= TREE_MAX_DEPTH {
        return;
    }
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| e.file_name());
    let visible: Vec<_> = entries
        .into_iter()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            // 隐藏文件（.git / .DS_Store …）只放行 .gitignore / .github / .cursor
            if name.starts_with('.') {
                return matches!(
                    name.as_ref(),
                    ".github" | ".cursor" | ".gitignore" | ".dockerignore"
                );
            }
            !TREE_IGNORE_DIRS.contains(&name.as_ref())
        })
        .collect();

    let total = visible.len();
    let take_n = total.min(TREE_MAX_ENTRIES_PER_DIR);

    for (idx, entry) in visible.iter().take(take_n).enumerate() {
        let is_last = idx + 1 == take_n && total <= take_n;
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = if is_last { "    " } else { "│   " };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        let is_dir = path.is_dir();
        buf.push_str(prefix);
        buf.push_str(connector);
        buf.push_str(&name);
        if is_dir {
            buf.push('/');
        }
        buf.push('\n');
        if is_dir {
            let new_prefix = format!("{prefix}{child_prefix}");
            walk(&path, depth + 1, &new_prefix, buf);
        }
    }
    if total > take_n {
        buf.push_str(prefix);
        buf.push_str(&format!("└── … ({} more entries)\n", total - take_n));
    }
}

// ---- Tech stack (FR-10.1) ----

fn detect_tech_stack(root: &Path) -> String {
    let exists = |rel: &str| root.join(rel).exists();
    let mut tags: Vec<&str> = Vec::new();

    if exists("Cargo.toml") {
        tags.push("Rust");
        if exists("src-tauri/tauri.conf.json") || exists("tauri.conf.json") {
            tags.push("Tauri 2.x");
        }
    }
    if exists("package.json") {
        tags.push("Node.js");
        if exists("pnpm-lock.yaml") {
            tags.push("pnpm");
        } else if exists("yarn.lock") {
            tags.push("yarn");
        } else if exists("package-lock.json") {
            tags.push("npm");
        }
        let pj = root.join("package.json");
        if let Ok(content) = std::fs::read_to_string(&pj) {
            if content.contains("\"react\"") {
                tags.push("React");
            }
            if content.contains("\"vite\"") {
                tags.push("Vite");
            }
            if content.contains("\"next\"") {
                tags.push("Next.js");
            }
            if content.contains("\"vue\"") {
                tags.push("Vue");
            }
            if content.contains("\"svelte\"") {
                tags.push("Svelte");
            }
            if content.contains("\"typescript\"") {
                tags.push("TypeScript");
            }
        }
    }
    if exists("pyproject.toml") || exists("requirements.txt") || exists("setup.py") {
        tags.push("Python");
    }
    if exists("go.mod") {
        tags.push("Go");
    }
    if exists("Gemfile") {
        tags.push("Ruby");
    }
    if exists("composer.json") {
        tags.push("PHP");
    }
    if exists("pom.xml") || exists("build.gradle") || exists("build.gradle.kts") {
        tags.push("JVM");
    }
    if exists("Dockerfile") {
        tags.push("Docker");
    }

    if tags.is_empty() {
        "(unable to auto-detect; inspect files manually before editing)".to_string()
    } else {
        tags.join(" + ")
    }
}

// ---- Upstream context (FR-10.2) ----

fn build_upstream_context(db: &crate::db::Database, task_id: &str) -> String {
    let upstream = match db.with_conn(|conn| {
        list_upstream_summaries(conn, task_id)
    }) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("upstream context fetch failed: {e}");
            return String::new();
        }
    };
    if upstream.is_empty() {
        return String::new();
    }
    let mut buf = String::new();
    for u in upstream {
        buf.push_str(&format!("- task {} ({}):\n", &u.id[..8.min(u.id.len())], u.title));
        if let Some(s) = &u.completion_summary {
            buf.push_str(&format!("    summary: {}\n", indent_block(s, "    ")));
        }
        for (name, ty, paths) in &u.artifacts {
            let path_brief = if paths.is_empty() {
                "(no files)".to_string()
            } else {
                paths.join(", ")
            };
            buf.push_str(&format!("    artifact {name} ({ty}) @ {path_brief}\n"));
        }
    }
    buf
}

struct UpstreamRow {
    id: String,
    title: String,
    completion_summary: Option<String>,
    artifacts: Vec<(String, String, Vec<String>)>,
}

fn list_upstream_summaries(
    conn: &rusqlite::Connection,
    task_id: &str,
) -> anyhow::Result<Vec<UpstreamRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.completion_summary
         FROM tasks t
         JOIN task_dependencies td ON td.depends_on = t.id
         WHERE td.task_id = ?1 AND t.status = 'completed'
         ORDER BY t.completed_at ASC NULLS LAST, t.id ASC",
    )?;
    let mut rows: Vec<UpstreamRow> = stmt
        .query_map([task_id], |row| {
            Ok(UpstreamRow {
                id: row.get(0)?,
                title: row.get(1)?,
                completion_summary: row.get(2)?,
                artifacts: Vec::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // 拉每个上游 task 的已发布 artifacts
    for row in rows.iter_mut() {
        let mut a_stmt = conn.prepare(
            "SELECT local_name, type, file_paths FROM artifacts
             WHERE producer_task_id = ?1 AND published = 1",
        )?;
        let arts: Vec<(String, String, String)> = a_stmt
            .query_map([&row.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        row.artifacts = arts
            .into_iter()
            .map(|(name, ty, fp)| {
                let paths: Vec<String> = serde_json::from_str(&fp).unwrap_or_default();
                (name, ty, paths)
            })
            .collect();
    }
    Ok(rows)
}

// ---- Base conflicts (FR-10.2) ----

fn build_base_conflicts(db: &crate::db::Database, task_id: &str) -> String {
    let rows = match db.with_conn(|conn| crate::db::queries::get_task_base_conflicts(conn, task_id)) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("base conflicts fetch failed: {e}");
            return String::new();
        }
    };
    if rows.is_empty() {
        return String::new();
    }
    let mut buf = String::new();
    buf.push_str(
        "Your base branch was built by merging upstream task outputs. The following file(s) had conflicts during base preparation:\n",
    );
    for (parent_task_id, file_path, resolution) in rows {
        let parent_short = if parent_task_id.len() >= 8 {
            &parent_task_id[..8]
        } else {
            &parent_task_id
        };
        buf.push_str(&format!(
            "- `{file_path}` (from upstream {parent_short}, resolved by {resolution})\n"
        ));
    }
    buf.push_str("Inspect these files carefully before editing — your base may not reflect the original intent of all upstream tasks.\n");
    buf
}

// ---- helpers ----

fn truncate_block(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut head: String = s.chars().take(max_chars).collect();
    head.push_str("\n…(truncated)");
    head
}

fn indent_block(s: &str, indent: &str) -> String {
    s.lines()
        .enumerate()
        .map(|(i, line)| if i == 0 { line.to_string() } else { format!("{indent}{line}") })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn project_tree_contains_root_label_and_files() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src").join("lib.rs"), "// x").unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        let tree = build_project_tree(dir.path());
        assert!(tree.contains("Cargo.toml"));
        assert!(tree.contains("src/"));
        assert!(tree.contains("lib.rs"));
    }

    #[test]
    fn project_tree_skips_ignored_dirs() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("node_modules")).unwrap();
        fs::write(dir.path().join("node_modules").join("a.js"), "x").unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        let tree = build_project_tree(dir.path());
        assert!(!tree.contains("node_modules"), "ignored dir leaked: {tree}");
    }

    #[test]
    fn detect_tech_stack_rust_tauri_react() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        fs::create_dir(dir.path().join("src-tauri")).unwrap();
        fs::write(dir.path().join("src-tauri").join("tauri.conf.json"), "{}").unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"react":"^19","vite":"^5","typescript":"^5"}}"#,
        )
        .unwrap();
        let stack = detect_tech_stack(dir.path());
        assert!(stack.contains("Rust"), "got: {stack}");
        assert!(stack.contains("Tauri"), "got: {stack}");
        assert!(stack.contains("React"), "got: {stack}");
        assert!(stack.contains("Vite"), "got: {stack}");
        assert!(stack.contains("TypeScript"), "got: {stack}");
    }

    #[test]
    fn detect_tech_stack_unknown_returns_hint() {
        let dir = TempDir::new().unwrap();
        let stack = detect_tech_stack(dir.path());
        assert!(stack.contains("auto-detect"));
    }

    #[test]
    fn render_system_block_includes_only_non_empty_sections() {
        let intel = CodebaseIntel {
            project_structure: "root\n├── src/".into(),
            tech_stack: "Rust".into(),
            upstream_context: String::new(),
            base_conflicts: String::new(),
        };
        let s = intel.render_system_block();
        assert!(s.contains("[Project Structure]"));
        assert!(s.contains("[Tech Stack]"));
        assert!(!s.contains("[Upstream Context]"));
        assert!(!s.contains("[Base Conflicts]"));
    }

    #[test]
    fn truncate_block_keeps_under_budget() {
        let long = "x".repeat(5000);
        let truncated = truncate_block(&long, 100);
        assert!(truncated.len() < 200);
        assert!(truncated.contains("truncated"));
    }
}
