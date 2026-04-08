use std::collections::HashMap;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::agent::planner::{validate_task_graph, PlannerTask};
use crate::commands::{DependencyInfo, MissionDetail};

pub const CURRENT_SCHEMA_VERSION: &str = "1";
pub const TEMPLATE_KIND: &str = "miragenty/mission-template";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionTemplate {
    pub schema_version: String,
    pub kind: String,
    pub mission: MissionMeta,
    pub tasks: Vec<TemplateTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionMeta {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub complexity: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl TemplateTask {
    fn to_planner_task(&self) -> PlannerTask {
        PlannerTask {
            id: self.id.clone(),
            title: self.title.clone(),
            description: self.description.clone(),
            complexity: self.complexity.clone(),
            depends_on: self.depends_on.clone(),
        }
    }
}

/// Convert a MissionDetail (DB UUIDs) into a portable MissionTemplate (T1, T2...).
pub fn build_template(detail: &MissionDetail) -> MissionTemplate {
    let mut uuid_to_portable: HashMap<&str, String> = HashMap::new();
    for (i, task) in detail.tasks.iter().enumerate() {
        uuid_to_portable.insert(&task.id, format!("T{}", i + 1));
    }

    let dep_lookup = build_dep_lookup(&detail.dependencies);

    let tasks = detail
        .tasks
        .iter()
        .map(|t| {
            let portable_id = uuid_to_portable.get(t.id.as_str()).unwrap().clone();
            let depends_on = dep_lookup
                .get(t.id.as_str())
                .map(|deps| {
                    deps.iter()
                        .filter_map(|d| uuid_to_portable.get(*d).cloned())
                        .collect()
                })
                .unwrap_or_default();

            TemplateTask {
                id: portable_id,
                title: t.title.clone(),
                description: t.description.clone(),
                complexity: t.complexity.clone(),
                depends_on,
            }
        })
        .collect();

    MissionTemplate {
        schema_version: CURRENT_SCHEMA_VERSION.to_string(),
        kind: TEMPLATE_KIND.to_string(),
        mission: MissionMeta {
            title: detail.mission.title.clone(),
            description: detail.mission.description.clone(),
        },
        tasks,
    }
}

fn build_dep_lookup(dependencies: &[DependencyInfo]) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    for dep in dependencies {
        map.entry(&dep.task_id).or_default().push(&dep.depends_on);
    }
    map
}

/// Serialize a template to YAML.
pub fn serialize_yaml(template: &MissionTemplate) -> Result<String> {
    serde_yml::to_string(template).map_err(|e| anyhow::anyhow!("YAML serialization failed: {e}"))
}

/// Deserialize and validate a YAML string into a MissionTemplate.
pub fn parse_and_validate_yaml(yaml_str: &str) -> Result<MissionTemplate> {
    let template: MissionTemplate =
        serde_yml::from_str(yaml_str).map_err(|e| anyhow::anyhow!("Invalid YAML: {e}"))?;

    validate_template(&template)?;
    Ok(template)
}

/// Validate an already-deserialized template.
pub fn validate_template(template: &MissionTemplate) -> Result<()> {
    if template.schema_version.trim().is_empty() {
        bail!("Missing schema_version");
    }

    let version = template
        .schema_version
        .trim()
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("Invalid schema_version: must be a positive integer"))?;

    if version < 1 {
        bail!("schema_version must be >= 1");
    }

    if version > CURRENT_SCHEMA_VERSION.parse::<u32>().unwrap() {
        bail!(
            "Unsupported schema_version {version}. This version of Miragenty supports up to {CURRENT_SCHEMA_VERSION}"
        );
    }

    if template.kind != TEMPLATE_KIND {
        bail!(
            "Invalid kind: expected '{}', got '{}'",
            TEMPLATE_KIND,
            template.kind
        );
    }

    if template.mission.title.trim().is_empty() {
        bail!("Mission title is required");
    }

    let planner_tasks: Vec<PlannerTask> =
        template.tasks.iter().map(|t| t.to_planner_task()).collect();
    validate_task_graph(&planner_tasks).map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok(())
}

/// Convert template tasks back into DB-ready structures.
/// Returns (mission_title, mission_description, tasks_with_uuid, dependencies).
pub fn template_to_db_records(
    template: &MissionTemplate,
) -> (
    String,
    String,
    Vec<(String, TemplateTask)>,
    Vec<(String, String)>,
) {
    let mut portable_to_uuid: HashMap<String, String> = HashMap::new();
    let mut tasks_with_uuid = Vec::new();

    for task in &template.tasks {
        let uuid = uuid::Uuid::new_v4().to_string();
        portable_to_uuid.insert(task.id.clone(), uuid.clone());
        tasks_with_uuid.push((uuid, task.clone()));
    }

    let mut dependencies = Vec::new();
    for task in &template.tasks {
        let task_uuid = &portable_to_uuid[&task.id];
        for dep_id in &task.depends_on {
            if let Some(dep_uuid) = portable_to_uuid.get(dep_id) {
                dependencies.push((task_uuid.clone(), dep_uuid.clone()));
            }
        }
    }

    (
        template.mission.title.clone(),
        template.mission.description.clone(),
        tasks_with_uuid,
        dependencies,
    )
}

// ─── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{MissionInfo, TaskInfo};

    fn make_detail(
        tasks: Vec<(&str, &str, &str, &str)>,
        deps: Vec<(&str, &str)>,
    ) -> MissionDetail {
        MissionDetail {
            mission: MissionInfo {
                id: "m1".into(),
                title: "Test Mission".into(),
                description: "A test".into(),
                status: "draft".into(),
                total_cost_usd: 0.0,
                created_at: "2026-01-01".into(),
                task_count: tasks.len() as i64,
                completed_count: 0,
            },
            tasks: tasks
                .iter()
                .map(|(id, title, desc, cx)| TaskInfo {
                    id: id.to_string(),
                    mission_id: "m1".into(),
                    title: title.to_string(),
                    description: desc.to_string(),
                    status: "pending".into(),
                    complexity: cx.to_string(),
                    assigned_agent_id: None,
                    created_at: "2026-01-01".into(),
                    completed_at: None,
                })
                .collect(),
            dependencies: deps
                .iter()
                .map(|(tid, dep)| DependencyInfo {
                    task_id: tid.to_string(),
                    depends_on: dep.to_string(),
                })
                .collect(),
        }
    }

    fn minimal_yaml() -> String {
        r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test Mission
  description: A test
tasks:
  - id: T1
    title: Task One
    description: Do something
    complexity: low
    depends_on: []
"#
        .to_string()
    }

    // ──────────── build_template tests ────────────

    #[test]
    fn build_template_single_task_no_deps() {
        let detail = make_detail(vec![("uuid-a", "Task A", "desc a", "low")], vec![]);
        let tpl = build_template(&detail);

        assert_eq!(tpl.schema_version, "1");
        assert_eq!(tpl.kind, TEMPLATE_KIND);
        assert_eq!(tpl.mission.title, "Test Mission");
        assert_eq!(tpl.tasks.len(), 1);
        assert_eq!(tpl.tasks[0].id, "T1");
        assert_eq!(tpl.tasks[0].title, "Task A");
        assert!(tpl.tasks[0].depends_on.is_empty());
    }

    #[test]
    fn build_template_linear_chain() {
        let detail = make_detail(
            vec![
                ("uuid-a", "Task A", "desc a", "low"),
                ("uuid-b", "Task B", "desc b", "medium"),
                ("uuid-c", "Task C", "desc c", "high"),
            ],
            vec![("uuid-b", "uuid-a"), ("uuid-c", "uuid-b")],
        );
        let tpl = build_template(&detail);

        assert_eq!(tpl.tasks.len(), 3);
        assert_eq!(tpl.tasks[0].id, "T1");
        assert_eq!(tpl.tasks[1].id, "T2");
        assert_eq!(tpl.tasks[2].id, "T3");
        assert_eq!(tpl.tasks[1].depends_on, vec!["T1"]);
        assert_eq!(tpl.tasks[2].depends_on, vec!["T2"]);
    }

    #[test]
    fn build_template_diamond_deps() {
        let detail = make_detail(
            vec![
                ("u1", "Root", "root", "low"),
                ("u2", "Left", "left", "medium"),
                ("u3", "Right", "right", "medium"),
                ("u4", "Merge", "merge", "high"),
            ],
            vec![
                ("u2", "u1"),
                ("u3", "u1"),
                ("u4", "u2"),
                ("u4", "u3"),
            ],
        );
        let tpl = build_template(&detail);

        assert_eq!(tpl.tasks[1].depends_on, vec!["T1"]);
        assert_eq!(tpl.tasks[2].depends_on, vec!["T1"]);
        let mut merge_deps = tpl.tasks[3].depends_on.clone();
        merge_deps.sort();
        assert_eq!(merge_deps, vec!["T2", "T3"]);
    }

    #[test]
    fn build_template_preserves_description() {
        let detail = make_detail(vec![("u1", "T", "d", "low")], vec![]);
        let tpl = build_template(&detail);
        assert_eq!(tpl.mission.description, "A test");
    }

    #[test]
    fn build_template_empty_tasks() {
        let detail = make_detail(vec![], vec![]);
        let tpl = build_template(&detail);
        assert!(tpl.tasks.is_empty());
    }

    // ──────────── serialize/deserialize round-trip ────────────

    #[test]
    fn roundtrip_yaml_simple() {
        let detail = make_detail(
            vec![
                ("u1", "Task A", "desc a", "low"),
                ("u2", "Task B", "desc b", "high"),
            ],
            vec![("u2", "u1")],
        );
        let tpl = build_template(&detail);
        let yaml = serialize_yaml(&tpl).unwrap();
        let parsed = parse_and_validate_yaml(&yaml).unwrap();

        assert_eq!(parsed.schema_version, "1");
        assert_eq!(parsed.mission.title, tpl.mission.title);
        assert_eq!(parsed.tasks.len(), 2);
        assert_eq!(parsed.tasks[1].depends_on, vec!["T1"]);
    }

    #[test]
    fn roundtrip_yaml_many_tasks() {
        let tasks: Vec<(&str, &str, &str, &str)> = (1..=20)
            .map(|i| {
                // Leak strings to get &str with 'static lifetime for test convenience
                let id: &str = Box::leak(format!("u{i}").into_boxed_str());
                let title: &str = Box::leak(format!("Task {i}").into_boxed_str());
                let cx = if i % 3 == 0 {
                    "high"
                } else if i % 2 == 0 {
                    "medium"
                } else {
                    "low"
                };
                (id, title, "desc", cx)
            })
            .collect();
        let deps: Vec<(&str, &str)> = (2..=20)
            .map(|i| {
                let tid: &str = Box::leak(format!("u{i}").into_boxed_str());
                let dep: &str = Box::leak(format!("u{}", i - 1).into_boxed_str());
                (tid, dep)
            })
            .collect();

        let detail = make_detail(tasks, deps);
        let tpl = build_template(&detail);
        let yaml = serialize_yaml(&tpl).unwrap();
        let parsed = parse_and_validate_yaml(&yaml).unwrap();
        assert_eq!(parsed.tasks.len(), 20);
    }

    #[test]
    fn roundtrip_preserves_special_chars_in_strings() {
        let detail = make_detail(
            vec![("u1", "Task: \"hello\" & 'world'", "line1\nline2\ttab", "low")],
            vec![],
        );
        let tpl = build_template(&detail);
        let yaml = serialize_yaml(&tpl).unwrap();
        let parsed = parse_and_validate_yaml(&yaml).unwrap();
        assert_eq!(parsed.tasks[0].title, "Task: \"hello\" & 'world'");
        assert!(parsed.tasks[0].description.contains("line1\nline2\ttab"));
    }

    #[test]
    fn roundtrip_unicode_content() {
        let detail = make_detail(
            vec![("u1", "设计数据库模式", "创建用户表和会话表 🚀", "low")],
            vec![],
        );
        let tpl = build_template(&detail);
        let yaml = serialize_yaml(&tpl).unwrap();
        let parsed = parse_and_validate_yaml(&yaml).unwrap();
        assert_eq!(parsed.tasks[0].title, "设计数据库模式");
        assert!(parsed.tasks[0].description.contains("🚀"));
    }

    // ──────────── parse_and_validate_yaml tests ────────────

    #[test]
    fn parse_valid_minimal() {
        let tpl = parse_and_validate_yaml(&minimal_yaml()).unwrap();
        assert_eq!(tpl.tasks.len(), 1);
        assert_eq!(tpl.tasks[0].id, "T1");
    }

    #[test]
    fn parse_valid_with_deps() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Multi-task
  description: test
tasks:
  - id: T1
    title: First
    description: d
    complexity: low
    depends_on: []
  - id: T2
    title: Second
    description: d
    complexity: medium
    depends_on: [T1]
  - id: T3
    title: Third
    description: d
    complexity: high
    depends_on: [T1, T2]
"#;
        let tpl = parse_and_validate_yaml(yaml).unwrap();
        assert_eq!(tpl.tasks.len(), 3);
        assert_eq!(tpl.tasks[2].depends_on, vec!["T1", "T2"]);
    }

    #[test]
    fn parse_missing_description_defaults_empty() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: No Desc
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
"#;
        let tpl = parse_and_validate_yaml(yaml).unwrap();
        assert_eq!(tpl.mission.description, "");
    }

    #[test]
    fn parse_depends_on_defaults_empty() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
"#;
        let tpl = parse_and_validate_yaml(yaml).unwrap();
        assert!(tpl.tasks[0].depends_on.is_empty());
    }

    // ──────────── validation error tests ────────────

    #[test]
    fn reject_missing_schema_version() {
        let yaml = r#"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
"#;
        let err = parse_and_validate_yaml(yaml);
        assert!(err.is_err());
    }

    #[test]
    fn reject_empty_schema_version() {
        let yaml = r#"
schema_version: ""
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("schema_version"));
    }

    #[test]
    fn reject_non_numeric_schema_version() {
        let yaml = r#"
schema_version: "abc"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("schema_version"));
    }

    #[test]
    fn reject_future_schema_version() {
        let yaml = r#"
schema_version: "999"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("Unsupported"));
    }

    #[test]
    fn reject_zero_schema_version() {
        let yaml = r#"
schema_version: "0"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("schema_version"));
    }

    #[test]
    fn reject_wrong_kind() {
        let yaml = r#"
schema_version: "1"
kind: something/else
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("kind"));
    }

    #[test]
    fn reject_empty_mission_title() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: "   "
  description: d
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("title"));
    }

    #[test]
    fn reject_empty_task_list() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks: []
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("empty"));
    }

    #[test]
    fn reject_empty_task_title() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: ""
    description: d
    complexity: low
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("title"));
    }

    #[test]
    fn reject_invalid_complexity() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: extreme
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("complexity"));
    }

    #[test]
    fn reject_self_dependency() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
    depends_on: [T1]
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("self"));
    }

    #[test]
    fn reject_invalid_dependency_ref() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
    depends_on: [T99]
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().contains("T99"));
    }

    #[test]
    fn reject_cyclic_dependency() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: A
    description: d
    complexity: low
    depends_on: [T2]
  - id: T2
    title: B
    description: d
    complexity: low
    depends_on: [T1]
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("cycl"));
    }

    #[test]
    fn reject_three_node_cycle() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: A
    description: d
    complexity: low
    depends_on: [T3]
  - id: T2
    title: B
    description: d
    complexity: low
    depends_on: [T1]
  - id: T3
    title: C
    description: d
    complexity: low
    depends_on: [T2]
"#;
        let err = parse_and_validate_yaml(yaml).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("cycl"));
    }

    #[test]
    fn reject_completely_invalid_yaml() {
        let err = parse_and_validate_yaml("{{{{not yaml at all::::").unwrap_err();
        assert!(err.to_string().contains("YAML"));
    }

    #[test]
    fn reject_yaml_wrong_structure() {
        let yaml = r#"
just_a_string: hello
"#;
        let err = parse_and_validate_yaml(yaml);
        assert!(err.is_err());
    }

    #[test]
    fn reject_yaml_tasks_not_array() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks: "not an array"
"#;
        let err = parse_and_validate_yaml(yaml);
        assert!(err.is_err());
    }

    #[test]
    fn reject_empty_string() {
        assert!(parse_and_validate_yaml("").is_err());
    }

    #[test]
    fn reject_json_input() {
        let json = r#"{"schema_version": "1", "kind": "miragenty/mission-template", "mission": {"title": "T"}, "tasks": [{"id": "T1", "title": "T", "description": "d", "complexity": "low"}]}"#;
        // YAML is a superset of JSON, so valid JSON should parse as valid YAML
        let result = parse_and_validate_yaml(json);
        assert!(result.is_ok());
    }

    // ──────────── template_to_db_records tests ────────────

    #[test]
    fn db_records_generates_unique_uuids() {
        let tpl = parse_and_validate_yaml(&minimal_yaml()).unwrap();
        let (title, desc, tasks, _deps) = template_to_db_records(&tpl);

        assert_eq!(title, "Test Mission");
        assert_eq!(desc, "A test");
        assert_eq!(tasks.len(), 1);
        assert!(!tasks[0].0.is_empty());
        assert_ne!(tasks[0].0, "T1"); // should be UUID, not portable id
    }

    #[test]
    fn db_records_maps_deps_to_uuids() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
  description: d
tasks:
  - id: T1
    title: First
    description: d
    complexity: low
  - id: T2
    title: Second
    description: d
    complexity: medium
    depends_on: [T1]
"#;
        let tpl = parse_and_validate_yaml(yaml).unwrap();
        let (_title, _desc, tasks, deps) = template_to_db_records(&tpl);

        assert_eq!(deps.len(), 1);
        let (dep_task_uuid, dep_on_uuid) = &deps[0];
        assert_eq!(dep_task_uuid, &tasks[1].0); // T2's uuid
        assert_eq!(dep_on_uuid, &tasks[0].0); // T1's uuid
    }

    #[test]
    fn db_records_all_uuids_unique() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: A
    description: d
    complexity: low
  - id: T2
    title: B
    description: d
    complexity: low
  - id: T3
    title: C
    description: d
    complexity: low
"#;
        let tpl = parse_and_validate_yaml(yaml).unwrap();
        let (_, _, tasks, _) = template_to_db_records(&tpl);

        let uuids: Vec<&str> = tasks.iter().map(|(uuid, _)| uuid.as_str()).collect();
        let unique: std::collections::HashSet<&str> = uuids.iter().copied().collect();
        assert_eq!(uuids.len(), unique.len());
    }

    #[test]
    fn db_records_diamond_deps() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Diamond
tasks:
  - id: T1
    title: Root
    description: d
    complexity: low
  - id: T2
    title: Left
    description: d
    complexity: medium
    depends_on: [T1]
  - id: T3
    title: Right
    description: d
    complexity: medium
    depends_on: [T1]
  - id: T4
    title: Merge
    description: d
    complexity: high
    depends_on: [T2, T3]
"#;
        let tpl = parse_and_validate_yaml(yaml).unwrap();
        let (_, _, tasks, deps) = template_to_db_records(&tpl);

        assert_eq!(tasks.len(), 4);
        assert_eq!(deps.len(), 4); // T2->T1, T3->T1, T4->T2, T4->T3
    }

    // ──────────── edge cases ────────────

    #[test]
    fn accept_all_complexity_levels() {
        for cx in &["low", "medium", "high"] {
            let yaml = format!(
                r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: d
    complexity: {cx}
"#
            );
            assert!(
                parse_and_validate_yaml(&yaml).is_ok(),
                "should accept complexity={cx}"
            );
        }
    }

    #[test]
    fn accept_task_with_long_description() {
        let long_desc = "x".repeat(10_000);
        let yaml = format!(
            r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: Task
    description: "{long_desc}"
    complexity: low
"#
        );
        let tpl = parse_and_validate_yaml(&yaml).unwrap();
        assert_eq!(tpl.tasks[0].description.len(), 10_000);
    }

    #[test]
    fn accept_wide_fan_out_dag() {
        let mut tasks_yaml = String::new();
        tasks_yaml.push_str("  - id: T1\n    title: Root\n    description: d\n    complexity: low\n");
        for i in 2..=50 {
            tasks_yaml.push_str(&format!(
                "  - id: T{i}\n    title: Fan {i}\n    description: d\n    complexity: low\n    depends_on: [T1]\n"
            ));
        }
        let yaml = format!(
            r#"schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Fan-out
tasks:
{tasks_yaml}"#
        );
        let tpl = parse_and_validate_yaml(&yaml).unwrap();
        assert_eq!(tpl.tasks.len(), 50);
    }

    #[test]
    fn accept_wide_fan_in_dag() {
        let mut tasks_yaml = String::new();
        let mut deps = Vec::new();
        for i in 1..=20 {
            tasks_yaml.push_str(&format!(
                "  - id: T{i}\n    title: Source {i}\n    description: d\n    complexity: low\n"
            ));
            deps.push(format!("T{i}"));
        }
        let dep_list = deps.join(", ");
        tasks_yaml.push_str(&format!(
            "  - id: T21\n    title: Sink\n    description: d\n    complexity: high\n    depends_on: [{dep_list}]\n"
        ));
        let yaml = format!(
            r#"schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Fan-in
tasks:
{tasks_yaml}"#
        );
        let tpl = parse_and_validate_yaml(&yaml).unwrap();
        assert_eq!(tpl.tasks.len(), 21);
        assert_eq!(tpl.tasks[20].depends_on.len(), 20);
    }

    #[test]
    fn duplicate_dep_references_preserved() {
        // depends_on with duplicate entries - not ideal but should not crash
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: T1
    title: A
    description: d
    complexity: low
  - id: T2
    title: B
    description: d
    complexity: low
    depends_on: [T1, T1]
"#;
        let tpl = parse_and_validate_yaml(yaml).unwrap();
        assert_eq!(tpl.tasks[1].depends_on.len(), 2);
    }

    #[test]
    fn extra_yaml_fields_are_ignored() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
  description: d
  some_future_field: value
tasks:
  - id: T1
    title: Task
    description: d
    complexity: low
    labels: [backend]
    estimated_hours: 2
"#;
        // serde_yml by default ignores unknown fields, so this should succeed
        let result = parse_and_validate_yaml(yaml);
        assert!(result.is_ok());
    }

    #[test]
    fn task_id_ordering_does_not_matter() {
        let yaml = r#"
schema_version: "1"
kind: miragenty/mission-template
mission:
  title: Test
tasks:
  - id: Z1
    title: Zebra
    description: d
    complexity: low
  - id: A1
    title: Alpha
    description: d
    complexity: low
    depends_on: [Z1]
"#;
        let tpl = parse_and_validate_yaml(yaml).unwrap();
        assert_eq!(tpl.tasks[0].id, "Z1");
        assert_eq!(tpl.tasks[1].depends_on, vec!["Z1"]);
    }

    #[test]
    fn build_then_validate_roundtrip() {
        let detail = make_detail(
            vec![
                ("u1", "A", "desc", "low"),
                ("u2", "B", "desc", "medium"),
                ("u3", "C", "desc", "high"),
            ],
            vec![("u2", "u1"), ("u3", "u1"), ("u3", "u2")],
        );
        let tpl = build_template(&detail);
        assert!(validate_template(&tpl).is_ok());
    }

    #[test]
    fn full_export_import_roundtrip() {
        let detail = make_detail(
            vec![
                ("u1", "Design", "schema", "low"),
                ("u2", "Implement", "code", "high"),
                ("u3", "Test", "tests", "medium"),
            ],
            vec![("u2", "u1"), ("u3", "u2")],
        );

        let tpl = build_template(&detail);
        let yaml = serialize_yaml(&tpl).unwrap();
        let imported = parse_and_validate_yaml(&yaml).unwrap();
        let (title, desc, tasks, deps) = template_to_db_records(&imported);

        assert_eq!(title, "Test Mission");
        assert_eq!(desc, "A test");
        assert_eq!(tasks.len(), 3);
        assert_eq!(deps.len(), 2);

        // Verify task content is preserved
        assert_eq!(tasks[0].1.title, "Design");
        assert_eq!(tasks[1].1.title, "Implement");
        assert_eq!(tasks[2].1.title, "Test");
    }
}
