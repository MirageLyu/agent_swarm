//! FM-15 FR-03: Artifact 数据模型 + `publish_artifact` 工具的校验与持久化。
//!
//! Phase 1 (S3-3) 范围：
//! - 类型 + 校验工具（snake_case、合法 type、文件存在）
//! - DB 持久化（声明 declare-only / 完成 publish）
//! - `ToolDefinition` 暴露给 Coding Agent（FR-03.2）
//!
//! Coding Agent 真正调用 `publish_artifact` 的运行时编织放在 Phase 2/3，
//! 但**校验函数 + DB 写入**这一层在 Phase 1 就具备完整可测试能力，
//! 等 Phase 2 的 `dispatch_task` 把 ToolExecutor 接 DB 时直接用即可。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

/// FM-15 C-04: Artifact 类型枚举。变体名直接落盘，不要随便改字符串。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    DesignDoc,
    ApiSpec,
    Schema,
    CodeModule,
    TestModule,
    Config,
    Docs,
    Report,
}

impl ArtifactType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DesignDoc => "design_doc",
            Self::ApiSpec => "api_spec",
            Self::Schema => "schema",
            Self::CodeModule => "code_module",
            Self::TestModule => "test_module",
            Self::Config => "config",
            Self::Docs => "docs",
            Self::Report => "report",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "design_doc" => Some(Self::DesignDoc),
            "api_spec" => Some(Self::ApiSpec),
            "schema" => Some(Self::Schema),
            "code_module" => Some(Self::CodeModule),
            "test_module" => Some(Self::TestModule),
            "config" => Some(Self::Config),
            "docs" => Some(Self::Docs),
            "report" => Some(Self::Report),
            _ => None,
        }
    }

    pub fn all() -> &'static [&'static str] {
        &[
            "design_doc",
            "api_spec",
            "schema",
            "code_module",
            "test_module",
            "config",
            "docs",
            "report",
        ]
    }
}

/// Planner-time artifact 声明（嵌入 PlannerTask.produces_artifacts）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactDecl {
    /// snake_case，task 内唯一
    pub local_name: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    #[serde(default)]
    pub summary: String,
}

/// `publish_artifact` 工具的输入 schema（runtime Coding Agent 使用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishArtifactInput {
    pub local_name: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub file_paths: Vec<String>,
}

/// 完整的 artifact 行，对应 `artifacts` 表。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub mission_id: String,
    pub producer_task_id: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub local_name: String,
    pub summary: String,
    pub file_paths: Vec<String>,
    pub published: bool,
    pub created_at: String,
}

#[derive(thiserror::Error, Debug)]
pub enum ArtifactError {
    #[error("local_name `{0}` is not snake_case (a–z, 0–9 and `_`; cannot start with a digit)")]
    LocalNameNotSnake(String),
    #[error("invalid artifact type `{got}` (allowed: {allowed})")]
    InvalidType { got: String, allowed: String },
    #[error(
        "file_paths is empty for artifact `{0}` — `publish_artifact` requires at least one path"
    )]
    EmptyFilePaths(String),
    #[error("file `{0}` does not exist under repo root")]
    FileMissing(String),
    #[error("file `{0}` is empty; publish_artifact requires non-empty files")]
    FileEmpty(String),
    #[error("file `{0}` escapes repo root (sandbox violation)")]
    FileEscapesRepo(String),
    #[error("artifact `{0}` is already published in this task")]
    AlreadyPublished(String),
    #[error("invalid input: {0}")]
    BadInput(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// snake_case：仅 [a-z0-9_]，长度 ≥ 1，首字符必须是小写字母。
pub fn validate_local_name(name: &str) -> Result<(), ArtifactError> {
    if name.is_empty() {
        return Err(ArtifactError::LocalNameNotSnake(name.to_string()));
    }
    let bytes = name.as_bytes();
    if !(bytes[0].is_ascii_lowercase()) {
        return Err(ArtifactError::LocalNameNotSnake(name.to_string()));
    }
    for &b in bytes {
        let ok = matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_');
        if !ok {
            return Err(ArtifactError::LocalNameNotSnake(name.to_string()));
        }
    }
    Ok(())
}

/// `<task_id>.<local_name>` —— 全局唯一性来自 task_id 前缀。
pub fn artifact_id(task_id: &str, local_name: &str) -> String {
    format!("{task_id}.{local_name}")
}

/// 仅校验输入合法性，不读盘、不写库；用于 PlannerTask FR-04.1 校验
/// 与 publish 时的快速预检。
pub fn validate_decl(local_name: &str, type_str: &str) -> Result<(), ArtifactError> {
    validate_local_name(local_name)?;
    if ArtifactType::parse(type_str).is_none() {
        return Err(ArtifactError::InvalidType {
            got: type_str.to_string(),
            allowed: ArtifactType::all().join(","),
        });
    }
    Ok(())
}

/// 校验 file_path 在 `repo_root` 内且文件存在。
fn validate_file_path(repo_root: &Path, rel: &str) -> Result<PathBuf, ArtifactError> {
    // 先 canonicalize repo_root，避免 macOS 上 `/var` → `/private/var` 这类
    // 符号链接导致 starts_with 假阳性。
    let repo_canonical = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let candidate = repo_canonical.join(rel);
    match candidate.canonicalize() {
        Ok(canonical) => {
            if !canonical.starts_with(&repo_canonical) {
                Err(ArtifactError::FileEscapesRepo(rel.to_string()))
            } else {
                Ok(candidate)
            }
        }
        Err(_) => {
            // canonicalize 失败 ⇒ 路径不可达。区分"逃逸"与"缺失"：
            // 词法上若已经 ../ 出 repo，就标 escape；否则就是 missing。
            let lexical = normalize_lexical(&candidate);
            if !lexical.starts_with(&repo_canonical) {
                Err(ArtifactError::FileEscapesRepo(rel.to_string()))
            } else {
                Err(ArtifactError::FileMissing(rel.to_string()))
            }
        }
    }
}

fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut parts: Vec<Component<'_>> = Vec::new();
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

/// 把 publish_artifact 的输入校验、与可选的 task.produces_artifacts 声明对账，
/// 然后写入 / 更新 artifacts 表（已存在的 declare row → 标记 published=1）。
///
/// `produces_decls`: 若提供，则要求 `input.local_name` + `input.artifact_type` 必须匹配
/// 该 task 的某条 declaration（FR-03.6 的核心），不匹配则返回 BadInput。
pub fn record_publish(
    conn: &Connection,
    repo_root: &Path,
    mission_id: &str,
    task_id: &str,
    input: &PublishArtifactInput,
    produces_decls: Option<&[ArtifactDecl]>,
) -> Result<Artifact, ArtifactError> {
    validate_decl(&input.local_name, &input.artifact_type)?;

    if input.file_paths.is_empty() {
        return Err(ArtifactError::EmptyFilePaths(input.local_name.clone()));
    }

    // declare 对账（如果提供 produces_decls，则必须命中）
    if let Some(decls) = produces_decls {
        let matched = decls
            .iter()
            .any(|d| d.local_name == input.local_name && d.artifact_type == input.artifact_type);
        if !matched {
            return Err(ArtifactError::BadInput(format!(
                "task `{task_id}` did not declare artifact `{}` (type=`{}`); declared: [{}]",
                input.local_name,
                input.artifact_type,
                decls
                    .iter()
                    .map(|d| format!("{}:{}", d.local_name, d.artifact_type))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    // 校验所有 file_paths 真实存在
    for fp in &input.file_paths {
        let full = validate_file_path(repo_root, fp)?;
        let metadata =
            std::fs::metadata(&full).map_err(|_| ArtifactError::FileMissing(fp.clone()))?;
        if metadata.is_file() && metadata.len() == 0 {
            return Err(ArtifactError::FileEmpty(fp.clone()));
        }
    }

    let id = artifact_id(task_id, &input.local_name);
    let file_paths_json = serde_json::to_string(&input.file_paths)
        .map_err(|e| ArtifactError::BadInput(format!("file_paths serialize failed: {e}")))?;

    // 已有同 id row（typically declared by Planner 时 published=0）→ 升级为 published；
    // 否则 INSERT 新 row。
    let existing = conn
        .query_row(
            "SELECT published FROM artifacts WHERE id = ?1",
            params![id],
            |row| row.get::<_, i64>(0),
        )
        .ok();

    match existing {
        Some(1) => return Err(ArtifactError::AlreadyPublished(id)),
        Some(0) => {
            conn.execute(
                "UPDATE artifacts
                 SET summary = ?1, file_paths = ?2, published = 1
                 WHERE id = ?3",
                params![input.summary, file_paths_json, id],
            )?;
        }
        Some(_) => {
            // CHECK 约束之外的 published 值不应存在；保险起见当作未声明处理
            tracing::warn!("[artifacts] unexpected published value for {id}");
        }
        None => {
            conn.execute(
                "INSERT INTO artifacts (id, mission_id, producer_task_id, type, local_name, summary, file_paths, published)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
                params![
                    id,
                    mission_id,
                    task_id,
                    input.artifact_type,
                    input.local_name,
                    input.summary,
                    file_paths_json,
                ],
            )?;
        }
    }

    Ok(Artifact {
        id: id.clone(),
        mission_id: mission_id.to_string(),
        producer_task_id: task_id.to_string(),
        artifact_type: input.artifact_type.clone(),
        local_name: input.local_name.clone(),
        summary: input.summary.clone(),
        file_paths: input.file_paths.clone(),
        published: true,
        created_at: String::new(), // 调用方若需要可重新 SELECT
    })
}

/// Planner 阶段把 task.produces_artifacts 中每个 decl 写成一条 published=0 的 row。
/// 同 task 多次 plan 可能重写，所以使用 INSERT OR REPLACE。
pub fn record_declaration(
    conn: &Connection,
    mission_id: &str,
    task_id: &str,
    decl: &ArtifactDecl,
) -> Result<String, ArtifactError> {
    validate_decl(&decl.local_name, &decl.artifact_type)?;
    let id = artifact_id(task_id, &decl.local_name);
    conn.execute(
        "INSERT OR REPLACE INTO artifacts
         (id, mission_id, producer_task_id, type, local_name, summary, file_paths, published)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, '[]',
                 COALESCE((SELECT published FROM artifacts WHERE id = ?1), 0))",
        params![
            id,
            mission_id,
            task_id,
            decl.artifact_type,
            decl.local_name,
            decl.summary,
        ],
    )?;
    Ok(id)
}

/// 提供给 Coding Agent 的工具定义（Phase 2 由 dispatch_task 装载）。
pub fn publish_artifact_tool_definition() -> crate::llm::ToolDefinition {
    use serde_json::json;
    crate::llm::ToolDefinition {
        name: "publish_artifact".to_string(),
        description: "Declare that a concrete artifact has been produced by this task. \
            Each artifact your task is supposed to produce (per the plan) MUST be published \
            via this tool exactly once, after the underlying files exist on disk."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "local_name": {
                    "type": "string",
                    "description": "snake_case identifier, must match one of the task's planned produces_artifacts entries"
                },
                "type": {
                    "type": "string",
                    "enum": ArtifactType::all(),
                    "description": "Artifact category"
                },
                "summary": {
                    "type": "string",
                    "description": "1-2 sentence description of what was produced (downstream agents will read this)"
                },
                "file_paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Repo-relative paths of files comprising this artifact (must exist)"
                }
            },
            "required": ["local_name", "type", "summary", "file_paths"]
        }),
        cache_control: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_in_memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations_run_on(&conn).unwrap();
        // 给一个 mission + task 让外键约束能过
        conn.execute(
            "INSERT INTO missions (id, title, description) VALUES ('M1', 't', 'd')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title) VALUES ('T1', 'M1', 'task one')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn snake_case_valid() {
        for ok in ["foo", "foo_bar", "x", "user_model_spec", "a1_b2"] {
            assert!(validate_local_name(ok).is_ok(), "should accept `{ok}`");
        }
    }

    #[test]
    fn snake_case_invalid() {
        for bad in ["", "Foo", "foo-bar", "1foo", "foo bar", "fooBar", "FOO"] {
            assert!(validate_local_name(bad).is_err(), "should reject `{bad}`");
        }
    }

    #[test]
    fn artifact_type_parse_roundtrip() {
        for s in ArtifactType::all() {
            let parsed = ArtifactType::parse(s).unwrap();
            assert_eq!(parsed.as_str(), *s);
        }
        assert!(ArtifactType::parse("nonexistent").is_none());
    }

    #[test]
    fn id_format_matches_spec() {
        assert_eq!(artifact_id("T3", "auth_handler"), "T3.auth_handler");
    }

    #[test]
    fn record_declaration_writes_unpublished_row() {
        let conn = open_in_memory();
        let id = record_declaration(
            &conn,
            "M1",
            "T1",
            &ArtifactDecl {
                local_name: "user_model_spec".into(),
                artifact_type: "design_doc".into(),
                summary: "User model".into(),
            },
        )
        .unwrap();
        assert_eq!(id, "T1.user_model_spec");

        let (published, ty): (i64, String) = conn
            .query_row(
                "SELECT published, type FROM artifacts WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(published, 0);
        assert_eq!(ty, "design_doc");
    }

    #[test]
    fn record_publish_validates_files_and_upgrades_declaration() {
        use std::io::Write;
        let conn = open_in_memory();
        let tmp = tempfile::tempdir().unwrap();

        // 文件存在于 repo_root
        let mut f = std::fs::File::create(tmp.path().join("a.md")).unwrap();
        writeln!(f, "x").unwrap();

        // 先声明
        record_declaration(
            &conn,
            "M1",
            "T1",
            &ArtifactDecl {
                local_name: "spec".into(),
                artifact_type: "design_doc".into(),
                summary: "sum".into(),
            },
        )
        .unwrap();

        let decls = vec![ArtifactDecl {
            local_name: "spec".into(),
            artifact_type: "design_doc".into(),
            summary: "sum".into(),
        }];
        let res = record_publish(
            &conn,
            tmp.path(),
            "M1",
            "T1",
            &PublishArtifactInput {
                local_name: "spec".into(),
                artifact_type: "design_doc".into(),
                summary: "final".into(),
                file_paths: vec!["a.md".into()],
            },
            Some(&decls),
        )
        .unwrap();
        assert!(res.published);

        let (published, summary): (i64, String) = conn
            .query_row(
                "SELECT published, summary FROM artifacts WHERE id = 'T1.spec'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(published, 1);
        assert_eq!(summary, "final");
    }

    #[test]
    fn record_publish_rejects_undeclared_artifact() {
        let conn = open_in_memory();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("x.md"), "y").unwrap();

        let decls = vec![ArtifactDecl {
            local_name: "alpha".into(),
            artifact_type: "design_doc".into(),
            summary: "".into(),
        }];
        let err = record_publish(
            &conn,
            tmp.path(),
            "M1",
            "T1",
            &PublishArtifactInput {
                local_name: "beta".into(),
                artifact_type: "design_doc".into(),
                summary: "".into(),
                file_paths: vec!["x.md".into()],
            },
            Some(&decls),
        )
        .unwrap_err();
        assert!(matches!(err, ArtifactError::BadInput(_)));
    }

    #[test]
    fn record_publish_rejects_missing_file() {
        let conn = open_in_memory();
        let tmp = tempfile::tempdir().unwrap();
        let err = record_publish(
            &conn,
            tmp.path(),
            "M1",
            "T1",
            &PublishArtifactInput {
                local_name: "spec".into(),
                artifact_type: "design_doc".into(),
                summary: "".into(),
                file_paths: vec!["nonexistent.md".into()],
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ArtifactError::FileMissing(_)));
    }

    #[test]
    fn record_publish_rejects_empty_file() {
        let conn = open_in_memory();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("empty.md"), "").unwrap();
        let err = record_publish(
            &conn,
            tmp.path(),
            "M1",
            "T1",
            &PublishArtifactInput {
                local_name: "empty_report".into(),
                artifact_type: "report".into(),
                summary: "s".into(),
                file_paths: vec!["empty.md".into()],
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ArtifactError::FileEmpty(_)));
    }

    #[test]
    fn record_publish_rejects_path_escape() {
        let conn = open_in_memory();
        let tmp = tempfile::tempdir().unwrap();
        let err = record_publish(
            &conn,
            tmp.path(),
            "M1",
            "T1",
            &PublishArtifactInput {
                local_name: "spec".into(),
                artifact_type: "design_doc".into(),
                summary: "".into(),
                file_paths: vec!["../etc/passwd".into()],
            },
            None,
        )
        .unwrap_err();
        // canonicalize 失败时也会变成 file_missing；只要拒绝即可
        assert!(matches!(
            err,
            ArtifactError::FileEscapesRepo(_) | ArtifactError::FileMissing(_)
        ));
    }

    #[test]
    fn record_publish_double_publish_rejected() {
        let conn = open_in_memory();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "x").unwrap();
        let input = PublishArtifactInput {
            local_name: "spec".into(),
            artifact_type: "design_doc".into(),
            summary: "".into(),
            file_paths: vec!["a.md".into()],
        };
        record_publish(&conn, tmp.path(), "M1", "T1", &input, None).unwrap();
        let err = record_publish(&conn, tmp.path(), "M1", "T1", &input, None).unwrap_err();
        assert!(matches!(err, ArtifactError::AlreadyPublished(_)));
    }

    #[test]
    fn tool_definition_includes_all_types() {
        let def = publish_artifact_tool_definition();
        assert_eq!(def.name, "publish_artifact");
        let schema = def.input_schema.to_string();
        for t in ArtifactType::all() {
            assert!(schema.contains(t), "schema missing type `{t}`: {schema}");
        }
    }
}
