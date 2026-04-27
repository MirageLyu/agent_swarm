//! FM-15 FR-01: Role Template 系统
//!
//! Role 是一个**闭枚举**——Planner 在 plan 阶段只能从已加载的 Role 中选择，
//! 不允许 LLM 凭空发明新角色（避免角色失控泛滥）。但用户可在
//! `<data_dir>/role_templates.json` 中**覆盖或新增** Role（开放给用户而非 LLM）。
//!
//! S1 范围：6 内置 Role 硬编码，仅承载 Planner 选择 / 校验 + UI 标识。
//! 默认 tools / skills / guardrails 等字段留空，S2/S3 再填充。
//! JSON 覆盖（FR-01.2 / FR-01.5）延到 S2 实现。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

/// 默认期望 Artifact 类型（用于 prompt 提示，非强制）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleTemplate {
    pub id: String,
    pub display_name: String,
    /// CSS 颜色字符串，用于前端 RoleBadge / DAG 节点边框
    pub ui_color: String,
    /// emoji 或 icon 名（前端可自由映射）
    pub ui_icon: String,
    pub description: String,
    /// S2/S3 填充：默认 tools 白名单（None = 全部 tools）
    #[serde(default)]
    pub default_tools: Option<Vec<String>>,
    /// S2/S3 填充：默认装载 skill id 列表
    #[serde(default)]
    pub default_skills: Vec<String>,
    /// S2/S3 填充：期望产物类型（提示用）
    #[serde(default)]
    pub expected_artifact_types: Vec<String>,
}

fn builtin_role_templates() -> Vec<RoleTemplate> {
    vec![
        RoleTemplate {
            id: "architect".into(),
            display_name: "Architect".into(),
            ui_color: "#a78bfa".into(), // violet-400
            ui_icon: "📐".into(),
            description: "高层设计、模块拆分、API 契约设计。\
                通常产出 design_doc / api_spec，不直接写实现代码。".into(),
            default_tools: None,
            default_skills: vec![],
            expected_artifact_types: vec!["design_doc".into(), "api_spec".into()],
        },
        RoleTemplate {
            id: "implementer".into(),
            display_name: "Implementer".into(),
            ui_color: "#60a5fa".into(), // blue-400
            ui_icon: "🛠".into(),
            description: "实际编码实现。基于上游 architect 的 design_doc / api_spec，\
                把功能写出来。期望产出 code_module 并通过 build。".into(),
            default_tools: None,
            default_skills: vec![],
            expected_artifact_types: vec!["code_module".into()],
        },
        RoleTemplate {
            id: "refactorer".into(),
            display_name: "Refactorer".into(),
            ui_color: "#22d3ee".into(), // cyan-400
            ui_icon: "♻️".into(),
            description: "在不改变外部行为的前提下重构既有代码。\
                必须保证既有 test 不被破坏。".into(),
            default_tools: None,
            default_skills: vec![],
            expected_artifact_types: vec!["code_module".into()],
        },
        RoleTemplate {
            id: "tester".into(),
            display_name: "Tester".into(),
            ui_color: "#34d399".into(), // emerald-400
            ui_icon: "🧪".into(),
            description: "为指定模块编写或补充测试，确保新增 test 全部通过。".into(),
            default_tools: None,
            default_skills: vec![],
            expected_artifact_types: vec!["test_module".into()],
        },
        RoleTemplate {
            id: "integrator".into(),
            display_name: "Integrator".into(),
            ui_color: "#fb923c".into(), // orange-400
            ui_icon: "🔌".into(),
            description: "接线、配置、CI/CD、依赖管理。\
                把 implementer 的产出整合进项目骨架。".into(),
            default_tools: None,
            default_skills: vec![],
            expected_artifact_types: vec!["config".into(), "code_module".into()],
        },
        RoleTemplate {
            id: "researcher".into(),
            display_name: "Researcher".into(),
            ui_color: "#94a3b8".into(), // slate-400
            ui_icon: "🔍".into(),
            description: "调研、原型、读外部资料、产出 report。\
                通常作为 architect/implementer 的前置环节。".into(),
            default_tools: None,
            default_skills: vec![],
            expected_artifact_types: vec!["report".into()],
        },
    ]
}

/// 进程级 Role 注册表，启动时加载一次。
pub struct RoleRegistry {
    by_id: HashMap<String, RoleTemplate>,
    /// 保留插入顺序，用于 prompt 列举与 UI 列表
    ordered_ids: Vec<String>,
}

impl RoleRegistry {
    fn from_templates(templates: Vec<RoleTemplate>) -> Self {
        let mut by_id = HashMap::with_capacity(templates.len());
        let mut ordered_ids = Vec::with_capacity(templates.len());
        for tpl in templates {
            ordered_ids.push(tpl.id.clone());
            by_id.insert(tpl.id.clone(), tpl);
        }
        Self { by_id, ordered_ids }
    }

    pub fn get(&self, id: &str) -> Option<&RoleTemplate> {
        self.by_id.get(id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    pub fn all(&self) -> Vec<&RoleTemplate> {
        self.ordered_ids
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .collect()
    }

    /// 返回小写、逗号分隔的合法 id 列表，用于 Planner prompt 与 guardrail 错误信息。
    pub fn ids_csv(&self) -> String {
        self.ordered_ids.join(", ")
    }
}

static REGISTRY: OnceLock<RoleRegistry> = OnceLock::new();

/// 进程启动时调用一次。S1：直接加载内置 6 角色；S2：在此处叠加 JSON 覆盖。
pub fn init() -> &'static RoleRegistry {
    REGISTRY.get_or_init(|| RoleRegistry::from_templates(builtin_role_templates()))
}

/// 测试 / 模块内访问。生产代码统一通过 `init()` 拿。
pub fn registry() -> &'static RoleRegistry {
    init()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_six_roles_loaded() {
        let reg = RoleRegistry::from_templates(builtin_role_templates());
        assert_eq!(reg.all().len(), 6);
        for id in [
            "architect",
            "implementer",
            "refactorer",
            "tester",
            "integrator",
            "researcher",
        ] {
            assert!(reg.contains(id), "missing builtin role: {}", id);
        }
    }

    #[test]
    fn ids_csv_preserves_order() {
        let reg = RoleRegistry::from_templates(builtin_role_templates());
        let csv = reg.ids_csv();
        assert!(csv.starts_with("architect, implementer"));
    }

    #[test]
    fn unknown_role_not_contained() {
        let reg = RoleRegistry::from_templates(builtin_role_templates());
        assert!(!reg.contains("ceo"));
        assert!(reg.get("ceo").is_none());
    }
}
