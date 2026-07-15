//! Skill registry & loader. See [`super`] for FR mapping.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

/// SKILL.md frontmatter (YAML)。仅 `name` / `description` 必需；其它可选。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    /// 激活时给 agent 的工具白名单（None = 不限制）
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// 适用 role 列表（None / 空 = 全角色）
    #[serde(default)]
    pub compatible_roles: Option<Vec<String>>,
}

/// Skill 数据来源（用于覆盖优先级排序与 UI 展示）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    /// 项目级（`<repo>/.miragenty/skills/...` 等）
    Project,
    /// 用户级（`~/.miragenty/skills/...` 等）
    User,
    /// 应用内置（`include_str!`）
    Builtin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub frontmatter: SkillFrontmatter,
    pub body: String,
    pub source: SkillSource,
    /// 来源路径（builtin 为 `builtin://<id>`）
    pub source_path: String,
}

impl Skill {
    pub fn id(&self) -> &str {
        &self.frontmatter.name
    }
}

/// 为 `list_skills` IPC 设计的轻量 DTO（不含 body，避免 8KB+ 无谓传输）。
#[derive(Debug, Clone, Serialize)]
pub struct SkillMeta {
    pub id: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatible_roles: Option<Vec<String>>,
    pub source: SkillSource,
    pub source_path: String,
}

impl From<&Skill> for SkillMeta {
    fn from(s: &Skill) -> Self {
        Self {
            id: s.frontmatter.name.clone(),
            description: s.frontmatter.description.clone(),
            tools: s.frontmatter.tools.clone(),
            compatible_roles: s.frontmatter.compatible_roles.clone(),
            source: s.source,
            source_path: s.source_path.clone(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum SkillParseError {
    #[error("missing frontmatter delimiter (--- ... ---) in {0}")]
    MissingFrontmatter(String),
    #[error("invalid yaml frontmatter in {path}: {source}")]
    Yaml {
        path: String,
        #[source]
        source: serde_yml::Error,
    },
    #[error("frontmatter `name` must equal directory name (`{dir}` vs `{name}`)")]
    NameMismatch { dir: String, name: String },
    #[error("io error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// 解析单个 SKILL.md 内容（不读盘，便于测试 + builtin include_str! 复用）。
///
/// `expected_dir_name`: 上层目录名，用于校验 frontmatter 的 name 与目录一致（FR-02.7
/// 约定 skill_id == 目录名）。
pub fn parse_skill_md(
    content: &str,
    expected_dir_name: &str,
    source: SkillSource,
    source_path: String,
) -> Result<Skill, SkillParseError> {
    // 形式：
    //   ---\n
    //   <yaml>\n
    //   ---\n
    //   <body>
    let stripped = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"));
    let after_open = match stripped {
        Some(s) => s,
        None => return Err(SkillParseError::MissingFrontmatter(source_path)),
    };
    // 找到第二个 "---" 行
    let close_idx = after_open
        .find("\n---\n")
        .or_else(|| after_open.find("\n---\r\n"))
        .or_else(|| after_open.find("\n---"));
    let close_idx = match close_idx {
        Some(i) => i,
        None => return Err(SkillParseError::MissingFrontmatter(source_path)),
    };
    let yaml_text = &after_open[..close_idx];
    // body 在 close 之后；跳过 "\n---\n" 或 "\n---\r\n" 或 "\n---" + 后续换行
    let body_offset = close_idx + 1; // skip leading \n
    let rest = &after_open[body_offset..];
    let body = if let Some(b) = rest.strip_prefix("---\r\n") {
        b
    } else if let Some(b) = rest.strip_prefix("---\n") {
        b
    } else if let Some(b) = rest.strip_prefix("---") {
        b.trim_start_matches('\n')
    } else {
        rest
    };

    let frontmatter: SkillFrontmatter =
        serde_yml::from_str(yaml_text).map_err(|e| SkillParseError::Yaml {
            path: source_path.clone(),
            source: e,
        })?;

    if frontmatter.name != expected_dir_name {
        return Err(SkillParseError::NameMismatch {
            dir: expected_dir_name.to_string(),
            name: frontmatter.name,
        });
    }

    Ok(Skill {
        frontmatter,
        body: body.trim().to_string(),
        source,
        source_path,
    })
}

/// 内置 6 skill。每个 SKILL.md 通过 `include_str!` 嵌入二进制。
fn builtin_skills() -> Vec<Skill> {
    let entries: &[(&str, &str)] = &[
        (
            "system-design",
            include_str!("builtin/system-design/SKILL.md"),
        ),
        (
            "code-implementation",
            include_str!("builtin/code-implementation/SKILL.md"),
        ),
        (
            "refactoring-patterns",
            include_str!("builtin/refactoring-patterns/SKILL.md"),
        ),
        (
            "test-authoring",
            include_str!("builtin/test-authoring/SKILL.md"),
        ),
        (
            "integration-glue",
            include_str!("builtin/integration-glue/SKILL.md"),
        ),
        ("research", include_str!("builtin/research/SKILL.md")),
    ];

    entries
        .iter()
        .filter_map(|(id, content)| {
            match parse_skill_md(content, id, SkillSource::Builtin, format!("builtin://{id}")) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::error!(
                        "[skills] builtin skill `{id}` failed to parse: {e}. \
                        This is a bundled-asset bug, please file an issue."
                    );
                    None
                }
            }
        })
        .collect()
}

/// 进程级 Skill registry。线程安全：所有公共方法均不可变借用。
#[derive(Debug)]
pub struct SkillRegistry {
    by_id: HashMap<String, Skill>,
    /// 保留稳定顺序（按加载顺序，便于 prompt / UI 列表稳定）。
    ordered_ids: Vec<String>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            by_id: HashMap::new(),
            ordered_ids: Vec::new(),
        }
    }

    /// 插入一个 skill；如果同 id 已存在，按调用顺序「**先到为主**」。
    /// `init_with_levels` 内部按「project → user → builtin」顺序插入，
    /// 因此项目级会覆盖用户级，用户级会覆盖 builtin（FR-02.3）。
    pub fn upsert(&mut self, skill: Skill) {
        let id = skill.id().to_string();
        if !self.by_id.contains_key(&id) {
            self.ordered_ids.push(id.clone());
        }
        // by_id.entry: 已存在则不覆盖
        self.by_id.entry(id).or_insert(skill);
    }

    pub fn get(&self, id: &str) -> Option<&Skill> {
        self.by_id.get(id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    pub fn all(&self) -> Vec<&Skill> {
        self.ordered_ids
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .collect()
    }

    pub fn metas(&self) -> Vec<SkillMeta> {
        self.all().into_iter().map(SkillMeta::from).collect()
    }

    /// 返回逗号分隔的 id 列表，便于 Planner prompt 列举。
    pub fn ids_csv(&self) -> String {
        self.ordered_ids.join(", ")
    }

    /// 校验 skill_id + role_id 兼容；用于 PlannerTask FR-04.1。
    pub fn validate_skill_role(&self, skill_id: &str, role_id: &str) -> Result<(), String> {
        let skill = self
            .get(skill_id)
            .ok_or_else(|| format!("unknown skill `{skill_id}`"))?;
        if let Some(roles) = &skill.frontmatter.compatible_roles {
            if !roles.is_empty() && !roles.iter().any(|r| r == role_id) {
                return Err(format!(
                    "skill `{skill_id}` is not compatible with role `{role_id}` \
                     (allowed: {})",
                    roles.join(", ")
                ));
            }
        }
        Ok(())
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 用户家目录下扫描的根目录名（依 FR-02 顺序）。
const USER_LEVEL_DIRS: &[&str] = &[
    ".miragenty/skills",
    ".cursor/skills",
    ".claude/skills",
    // .codex / .agents 暂不在用户级扫，按 FR-02.1 仅作项目级兼容
];

/// 项目根（repo）下扫描的根目录名。
const PROJECT_LEVEL_DIRS: &[&str] = &[
    ".miragenty/skills",
    ".cursor/skills",
    ".claude/skills",
    ".codex/skills",
    ".agents/skills",
];

/// 扫描某个根目录下所有 `<id>/SKILL.md`（不递归 SKILL 内部子目录），
/// 解析失败的 skill 跳过并 warn。
fn scan_dir(root: &Path, source: SkillSource, sink: &mut Vec<Skill>) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return, // 目录不存在 / 无权限 → 静默跳过
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("[skills] failed to read {}: {e}", skill_md.display());
                continue;
            }
        };
        match parse_skill_md(
            &content,
            &dir_name,
            source,
            skill_md.to_string_lossy().into_owned(),
        ) {
            Ok(skill) => sink.push(skill),
            Err(e) => tracing::warn!("[skills] skipping {}: {e}", skill_md.display()),
        }
    }
}

/// 按 FR-02.1/FR-02.3 顺序扫描：项目级 → 用户级 → builtin。
/// 同名以**先到**为准 = 项目级覆盖用户级覆盖 builtin。
pub fn build_registry(repo_root: Option<&Path>) -> SkillRegistry {
    let mut collected: Vec<Skill> = Vec::new();

    if let Some(root) = repo_root {
        for sub in PROJECT_LEVEL_DIRS {
            scan_dir(&root.join(sub), SkillSource::Project, &mut collected);
        }
    }

    if let Some(home) = dirs::home_dir() {
        for sub in USER_LEVEL_DIRS {
            scan_dir(&home.join(sub), SkillSource::User, &mut collected);
        }
    }

    collected.extend(builtin_skills());

    let mut reg = SkillRegistry::new();
    for s in collected {
        reg.upsert(s);
    }
    reg
}

/// 全局单例：app 启动时通过 `init_global()` 加载（无 repo_root，仅 builtin + user）。
/// 与具体 repo 相关的项目级 skill 应当在 `plan_mission` / `dispatch_task` 时
/// 用 `build_registry(Some(repo_root))` 临时构建（避免不同 mission 串扰）。
static GLOBAL_REGISTRY: OnceLock<SkillRegistry> = OnceLock::new();

pub fn init_global() -> &'static SkillRegistry {
    GLOBAL_REGISTRY.get_or_init(|| build_registry(None))
}

pub fn global() -> &'static SkillRegistry {
    init_global()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_frontmatter() {
        let md = "---\nname: foo\ndescription: bar\n---\n# body\nhello\n";
        let s = parse_skill_md(md, "foo", SkillSource::Builtin, "builtin://foo".into()).unwrap();
        assert_eq!(s.frontmatter.name, "foo");
        assert_eq!(s.frontmatter.description, "bar");
        assert!(s.body.contains("# body"));
        assert!(s.body.contains("hello"));
        assert!(s.frontmatter.tools.is_none());
        assert!(s.frontmatter.compatible_roles.is_none());
    }

    #[test]
    fn parses_full_frontmatter() {
        let md = "---\n\
            name: rust-async\n\
            description: tokio patterns\n\
            tools: [read_file, write_file]\n\
            compatible_roles: [implementer]\n\
            ---\n\
            body here";
        let s = parse_skill_md(md, "rust-async", SkillSource::User, "/tmp/x".into()).unwrap();
        assert_eq!(
            s.frontmatter.tools.unwrap(),
            vec!["read_file", "write_file"]
        );
        assert_eq!(s.frontmatter.compatible_roles.unwrap(), vec!["implementer"]);
    }

    #[test]
    fn rejects_missing_delimiter() {
        let md = "name: foo\ndescription: bar\n";
        assert!(parse_skill_md(md, "foo", SkillSource::Builtin, "x".into()).is_err());
    }

    #[test]
    fn rejects_name_dir_mismatch() {
        let md = "---\nname: foo\ndescription: bar\n---\n";
        let err = parse_skill_md(md, "bar", SkillSource::Builtin, "x".into()).unwrap_err();
        assert!(matches!(err, SkillParseError::NameMismatch { .. }));
    }

    #[test]
    fn builtin_six_skills_loaded() {
        let skills = builtin_skills();
        let ids: Vec<&str> = skills.iter().map(|s| s.id()).collect();
        for expected in [
            "system-design",
            "code-implementation",
            "refactoring-patterns",
            "test-authoring",
            "integration-glue",
            "research",
        ] {
            assert!(
                ids.contains(&expected),
                "missing builtin skill: {}",
                expected
            );
        }
    }

    #[test]
    fn project_overrides_user_and_builtin() {
        // 模拟：先插入项目级 foo，再插入 builtin 同名 foo —— 项目级先到，应保留。
        let mut reg = SkillRegistry::new();
        let project = Skill {
            frontmatter: SkillFrontmatter {
                name: "x".into(),
                description: "PROJECT".into(),
                tools: None,
                compatible_roles: None,
            },
            body: "p".into(),
            source: SkillSource::Project,
            source_path: "p".into(),
        };
        let builtin = Skill {
            frontmatter: SkillFrontmatter {
                name: "x".into(),
                description: "BUILTIN".into(),
                tools: None,
                compatible_roles: None,
            },
            body: "b".into(),
            source: SkillSource::Builtin,
            source_path: "b".into(),
        };
        reg.upsert(project);
        reg.upsert(builtin);
        assert_eq!(reg.get("x").unwrap().frontmatter.description, "PROJECT");
    }

    #[test]
    fn validate_skill_role_compatibility() {
        let reg = build_registry(None);
        // refactoring-patterns 兼容 refactorer / implementer
        assert!(reg
            .validate_skill_role("refactoring-patterns", "refactorer")
            .is_ok());
        assert!(reg
            .validate_skill_role("refactoring-patterns", "implementer")
            .is_ok());
        assert!(reg
            .validate_skill_role("refactoring-patterns", "tester")
            .is_err());
        // unknown skill
        assert!(reg
            .validate_skill_role("nonexistent", "implementer")
            .is_err());
    }

    #[test]
    fn scan_dir_picks_up_user_skill_and_skips_invalid() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // valid skill
        std::fs::create_dir_all(root.join("foo")).unwrap();
        let mut f = std::fs::File::create(root.join("foo/SKILL.md")).unwrap();
        writeln!(f, "---\nname: foo\ndescription: ok\n---\nbody").unwrap();

        // invalid: name mismatch
        std::fs::create_dir_all(root.join("bar")).unwrap();
        let mut f = std::fs::File::create(root.join("bar/SKILL.md")).unwrap();
        writeln!(f, "---\nname: WRONG\ndescription: ok\n---\nbody").unwrap();

        // dir without SKILL.md
        std::fs::create_dir_all(root.join("baz")).unwrap();

        let mut sink = Vec::new();
        scan_dir(root, SkillSource::User, &mut sink);
        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].id(), "foo");
    }

    #[test]
    fn project_registry_discovers_handoff_skills() {
        let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri has repo parent")
            .to_path_buf();
        let reg = build_registry(Some(&repo_root));
        for expected in [
            "handoff",
            "handoff-project",
            "transfer-context",
            "handoffplan",
        ] {
            let skill = reg
                .get(expected)
                .unwrap_or_else(|| panic!("missing project handoff skill: {expected}"));
            assert_eq!(skill.source, SkillSource::Project);
            assert!(
                skill.source_path.contains(".claude/skills"),
                "unexpected source path for {expected}: {}",
                skill.source_path
            );
        }
    }

    #[test]
    fn metas_omits_body() {
        let reg = build_registry(None);
        let metas = reg.metas();
        assert!(metas.iter().any(|m| m.id == "code-implementation"));
        // Serialize → string should not contain large body markers
        let json = serde_json::to_string(&metas).unwrap();
        assert!(!json.contains("Implementation playbook"));
    }
}
