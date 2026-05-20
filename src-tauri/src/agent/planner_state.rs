//! FM-15 FR-04 / FR-05 (S1 部分): Planner Agent Loop 的内存 DAG 状态机。
//!
//! 设计要点：
//! - 每次 `propose_task` / `add_dependency` / `revise_task` / `drop_task` 都立刻校验，
//!   失败抛错供 LLM 修正——避免"批量提交后整体 reject"那种对话流断裂。
//! - `validate_plan()` 做整体性快照检查（环、悬空依赖、孤儿等）。
//! - `finalize()` 把内存图转成 `PlannerOutput`，由 `parse_and_validate` 链路再走一遍兜底。
//!
//! S1 只承载 id/title/description/complexity/role/expected_output/depends_on。
//! Skill / artifact / file_scope_hints / guardrails 等字段在 S2/S3 增量加入此结构。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agent::artifacts::{self, ArtifactDecl};
use crate::agent::planner::{FileScopeHints, PlannerOutput, PlannerTask};
use crate::agent::roles;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposeTaskInput {
    pub id: String,
    pub title: String,
    pub description: String,
    /// 默认 medium；目前仅用于排序/展示
    #[serde(default = "default_complexity")]
    pub complexity: String,
    /// FM-15 FR-04: 验收契约
    pub expected_output: String,
    /// FM-15 FR-01: role id
    pub role: String,
    /// 可选：在 propose 时同时声明依赖（也可后续 add_dependency 补）
    #[serde(default)]
    pub depends_on: Vec<String>,

    // ---- FM-15 v2.2 (S3-5): 富语义字段 ----
    #[serde(default)]
    pub additional_skills: Vec<String>,
    #[serde(default)]
    pub produces_artifacts: Vec<ArtifactDecl>,
    #[serde(default)]
    pub consumes_artifacts: Vec<String>,
    #[serde(default)]
    pub file_scope_hints: FileScopeHints,
}

fn default_complexity() -> String {
    "medium".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviseTaskInput {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    // 富语义字段：Option<Vec> 表示 None=不动 / Some=整体替换。
    // 单条 add/remove 留给未来增量 API；先保证最小可用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produces_artifacts: Option<Vec<ArtifactDecl>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumes_artifacts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_scope_hints: Option<FileScopeHints>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    /// 阻止 finalize_plan：DAG 拓扑性 / 必填字段缺失 / 富字段非法等
    Error,
    /// 不阻止，但在 validate_plan 反馈给 LLM，提示其可能违反契约/最佳实践
    Warn,
}

impl Default for IssueSeverity {
    fn default() -> Self {
        IssueSeverity::Error
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
    /// 关联的 task id（若适用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// FM-15 v2.2 (S4): error vs warn。旧字段缺省走 error 兼容历史调用。
    #[serde(default)]
    pub severity: IssueSeverity,
}

impl ValidationIssue {
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            task_id: None,
            severity: IssueSeverity::Error,
        }
    }

    pub fn error_for(
        code: impl Into<String>,
        message: impl Into<String>,
        task_id: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            task_id: Some(task_id.into()),
            severity: IssueSeverity::Error,
        }
    }

    pub fn warn(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            task_id: None,
            severity: IssueSeverity::Warn,
        }
    }

    pub fn warn_for(
        code: impl Into<String>,
        message: impl Into<String>,
        task_id: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            task_id: Some(task_id.into()),
            severity: IssueSeverity::Warn,
        }
    }
}

/// FM-15 v2.2 (S4 / FR-PF-04): 来自 Pre-flight 的结构化契约，PlannerState 用它做
/// scope 覆盖度与 exclusions 触碰的轻量 guardrail。
/// 字段都是已经 trim 过的纯文本 bullet。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContractGuardrail {
    pub scope: Vec<String>,
    pub exclusions: Vec<String>,
    pub constraints: Vec<String>,
    pub assumptions: Vec<String>,
}

impl ContractGuardrail {
    pub fn is_empty(&self) -> bool {
        self.scope.is_empty()
            && self.exclusions.is_empty()
            && self.constraints.is_empty()
            && self.assumptions.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum PlannerStateError {
    #[error("Task id '{0}' is empty or invalid (must be non-empty, e.g. 'T1')")]
    InvalidTaskId(String),
    #[error("Task '{0}' already exists; use revise_task to modify or pick a unique id")]
    DuplicateTaskId(String),
    #[error("Task '{0}' not found")]
    TaskNotFound(String),
    #[error("Title for task '{0}' must not be empty")]
    EmptyTitle(String),
    #[error("Description for task '{0}' must not be empty")]
    EmptyDescription(String),
    #[error("Expected output for task '{0}' must not be empty (it is the acceptance contract)")]
    EmptyExpectedOutput(String),
    #[error("Invalid complexity '{value}' for task '{task_id}' (allowed: low | medium | high)")]
    InvalidComplexity { task_id: String, value: String },
    #[error("Invalid role '{role}' for task '{task_id}' (allowed: {allowed})")]
    InvalidRole {
        task_id: String,
        role: String,
        allowed: String,
    },
    #[error("Self dependency: task '{0}' cannot depend on itself")]
    SelfDependency(String),
    #[error("Dependency '{depends_on}' for task '{task_id}' does not exist (propose it first)")]
    UnknownDependency {
        task_id: String,
        depends_on: String,
    },
    #[error("Adding dependency from '{from}' to '{to}' would create a cycle")]
    CyclicDependency { from: String, to: String },
    #[error("Plan must contain at least one task before finalize")]
    EmptyPlan,
    // ---- FM-15 v2.2 (S3-5) 富语义校验 ----
    #[error("Invalid additional skill for task '{task_id}': {message}")]
    InvalidSkill { task_id: String, message: String },
    #[error("Invalid produced artifact for task '{task_id}': {message}")]
    InvalidProducedArtifact { task_id: String, message: String },
    #[error("Duplicate produced artifact local_name '{local_name}' within task '{task_id}'")]
    DuplicateProducedArtifact {
        task_id: String,
        local_name: String,
    },
    #[error(
        "consumed artifact '{artifact_id}' for task '{task_id}' is not produced by any task in the plan \
         (expected format `<task_id>.<local_name>` referencing a previously declared artifact)"
    )]
    UnknownConsumedArtifact {
        task_id: String,
        artifact_id: String,
    },
    #[error(
        "consumed artifact '{artifact_id}' for task '{task_id}' is produced by '{producer}', \
         which is not (transitively) in depends_on; declare the dependency or drop the consumption"
    )]
    ConsumedArtifactWithoutDependency {
        task_id: String,
        artifact_id: String,
        producer: String,
    },
    #[error("Invalid file_scope_hints path '{path}' for task '{task_id}': {message}")]
    InvalidFileScopePath {
        task_id: String,
        path: String,
        message: String,
    },
}

/// 内存 DAG 表达；保持 id 插入顺序，对外暴露的 `tasks()` 也按插入序返回。
#[derive(Debug, Default)]
pub struct PlannerState {
    /// 按插入顺序保留 id，便于稳定遍历 / 序列化
    order: Vec<String>,
    tasks: HashMap<String, PlannerTask>,
    /// finalize 时写入；finalize 前可由 LLM 通过 propose 时附带或 revise 中暂不支持
    mission_title: Option<String>,
    finalized: bool,
    /// FM-15 v2.2 (S4): 来自 Pre-flight 的契约。Quick-Plan 路径为 None。
    contract: Option<ContractGuardrail>,
}

impl PlannerState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_finalized(&self) -> bool {
        self.finalized
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn tasks(&self) -> Vec<&PlannerTask> {
        self.order
            .iter()
            .filter_map(|id| self.tasks.get(id))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<&PlannerTask> {
        self.tasks.get(id)
    }

    pub fn propose_task(&mut self, input: ProposeTaskInput) -> Result<String, PlannerStateError> {
        validate_id(&input.id)?;
        if self.tasks.contains_key(&input.id) {
            return Err(PlannerStateError::DuplicateTaskId(input.id));
        }
        if input.title.trim().is_empty() {
            return Err(PlannerStateError::EmptyTitle(input.id));
        }
        if input.description.trim().is_empty() {
            return Err(PlannerStateError::EmptyDescription(input.id));
        }
        if input.expected_output.trim().is_empty() {
            return Err(PlannerStateError::EmptyExpectedOutput(input.id));
        }
        validate_complexity(&input.id, &input.complexity)?;
        validate_role(&input.id, &input.role)?;

        for dep in &input.depends_on {
            if dep == &input.id {
                return Err(PlannerStateError::SelfDependency(input.id.clone()));
            }
            if !self.tasks.contains_key(dep) {
                return Err(PlannerStateError::UnknownDependency {
                    task_id: input.id.clone(),
                    depends_on: dep.clone(),
                });
            }
        }

        // FM-15 v2.2 (S3-5) 富字段校验
        validate_additional_skills(&input.id, &input.role, &input.additional_skills)?;
        validate_produced_artifacts(&input.id, &input.produces_artifacts)?;
        // consumes 校验需要 plan 上下文 + 依赖闭包：先把任务放进去再校验
        validate_file_scope_hints(&input.id, &input.file_scope_hints)?;

        // 由于全部依赖必须先存在，这里不可能形成新环，但保留双保险检查
        let task = PlannerTask {
            id: input.id.clone(),
            title: input.title,
            description: input.description,
            complexity: input.complexity,
            depends_on: input.depends_on.clone(),
            expected_output: Some(input.expected_output),
            role: Some(input.role),
            additional_skills: input.additional_skills,
            produces_artifacts: input.produces_artifacts,
            consumes_artifacts: input.consumes_artifacts.clone(),
            file_scope_hints: input.file_scope_hints,
            // Explicit Merge Node v1：planner LLM 输出永远是 Work；Merge 由
            // inject_merge_nodes 后处理生成。
            kind: crate::agent::planner::NodeKind::Work,
            merge_parents: Vec::new(),
        };
        let id = task.id.clone();
        self.tasks.insert(id.clone(), task);
        self.order.push(id.clone());

        // consumes 校验必须在插入后跑，因为 ancestor 闭包可能包括 self（不允许）
        if let Err(e) = self.validate_consumes_for(&id, &input.consumes_artifacts) {
            self.tasks.remove(&id);
            self.order.pop();
            return Err(e);
        }

        if self.has_cycle() {
            // 回滚（理论不可达）
            self.tasks.remove(&id);
            self.order.pop();
            return Err(PlannerStateError::CyclicDependency {
                from: id.clone(),
                to: id.clone(),
            });
        }
        Ok(id)
    }

    pub fn add_dependency(&mut self, task_id: &str, depends_on: &str) -> Result<(), PlannerStateError> {
        if task_id == depends_on {
            return Err(PlannerStateError::SelfDependency(task_id.to_string()));
        }
        if !self.tasks.contains_key(task_id) {
            return Err(PlannerStateError::TaskNotFound(task_id.to_string()));
        }
        if !self.tasks.contains_key(depends_on) {
            return Err(PlannerStateError::UnknownDependency {
                task_id: task_id.to_string(),
                depends_on: depends_on.to_string(),
            });
        }
        // 已存在则幂等
        let task = self.tasks.get_mut(task_id).unwrap();
        if task.depends_on.iter().any(|d| d == depends_on) {
            return Ok(());
        }
        task.depends_on.push(depends_on.to_string());
        if self.has_cycle() {
            // 回滚
            let task = self.tasks.get_mut(task_id).unwrap();
            task.depends_on.pop();
            return Err(PlannerStateError::CyclicDependency {
                from: task_id.to_string(),
                to: depends_on.to_string(),
            });
        }
        Ok(())
    }

    pub fn revise_task(&mut self, input: ReviseTaskInput) -> Result<(), PlannerStateError> {
        if !self.tasks.contains_key(&input.task_id) {
            return Err(PlannerStateError::TaskNotFound(input.task_id));
        }
        if let Some(role) = input.role.as_deref() {
            validate_role(&input.task_id, role)?;
        }
        if let Some(cx) = input.complexity.as_deref() {
            validate_complexity(&input.task_id, cx)?;
        }
        if let Some(t) = input.title.as_deref() {
            if t.trim().is_empty() {
                return Err(PlannerStateError::EmptyTitle(input.task_id.clone()));
            }
        }
        if let Some(d) = input.description.as_deref() {
            if d.trim().is_empty() {
                return Err(PlannerStateError::EmptyDescription(input.task_id.clone()));
            }
        }
        if let Some(eo) = input.expected_output.as_deref() {
            if eo.trim().is_empty() {
                return Err(PlannerStateError::EmptyExpectedOutput(input.task_id.clone()));
            }
        }

        // FM-15 v2.2 (S3-5) 富字段校验：在落入 task 前先验
        let effective_role = input
            .role
            .as_deref()
            .unwrap_or_else(|| self.tasks.get(&input.task_id).unwrap().effective_role());
        if let Some(skills) = input.additional_skills.as_ref() {
            validate_additional_skills(&input.task_id, effective_role, skills)?;
        }
        if let Some(prod) = input.produces_artifacts.as_ref() {
            validate_produced_artifacts(&input.task_id, prod)?;
        }
        if let Some(hints) = input.file_scope_hints.as_ref() {
            validate_file_scope_hints(&input.task_id, hints)?;
        }

        // 应用变更
        let task = self.tasks.get_mut(&input.task_id).unwrap();
        if let Some(t) = input.title {
            task.title = t;
        }
        if let Some(d) = input.description {
            task.description = d;
        }
        if let Some(c) = input.complexity {
            task.complexity = c;
        }
        if let Some(eo) = input.expected_output {
            task.expected_output = Some(eo);
        }
        if let Some(r) = input.role {
            task.role = Some(r);
        }
        if let Some(skills) = input.additional_skills {
            task.additional_skills = skills;
        }
        if let Some(prod) = input.produces_artifacts {
            task.produces_artifacts = prod;
        }
        if let Some(hints) = input.file_scope_hints {
            task.file_scope_hints = hints;
        }

        // consumes 校验需要看 plan 全图（包括其他 task 的 produces）；放在最后跑
        if let Some(consumes) = input.consumes_artifacts {
            // 先暂存旧值，校验失败回滚
            let prev_consumes = self.tasks.get(&input.task_id).unwrap().consumes_artifacts.clone();
            self.tasks.get_mut(&input.task_id).unwrap().consumes_artifacts = consumes.clone();
            if let Err(e) = self.validate_consumes_for(&input.task_id, &consumes) {
                self.tasks.get_mut(&input.task_id).unwrap().consumes_artifacts = prev_consumes;
                return Err(e);
            }
        }
        Ok(())
    }

    pub fn drop_task(&mut self, task_id: &str) -> Result<(), PlannerStateError> {
        if !self.tasks.contains_key(task_id) {
            return Err(PlannerStateError::TaskNotFound(task_id.to_string()));
        }
        self.tasks.remove(task_id);
        self.order.retain(|id| id != task_id);
        // 清理任意 task 对它的引用
        for t in self.tasks.values_mut() {
            t.depends_on.retain(|d| d != task_id);
        }
        Ok(())
    }

    pub fn set_mission_title(&mut self, title: String) {
        self.mission_title = Some(title);
    }

    /// FM-15 v2.2 (S4): 注入 Pre-flight 契约。后续 validate_plan 会基于此发出
    /// `WARN_EXCLUSION_TOUCHED` / `WARN_SCOPE_NOT_COVERED` 提示性 issue。
    pub fn set_contract(&mut self, contract: ContractGuardrail) {
        self.contract = Some(contract);
    }

    pub fn contract(&self) -> Option<&ContractGuardrail> {
        self.contract.as_ref()
    }

    pub fn validate_plan(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if self.tasks.is_empty() {
            issues.push(ValidationIssue::error(
                "EMPTY_PLAN",
                "Plan has no tasks; propose at least one before finalize",
            ));
            return issues;
        }
        // 悬空依赖（理论 propose/add_dependency 已挡，但 revise/drop 后再验一遍）
        for task in self.tasks.values() {
            for dep in &task.depends_on {
                if !self.tasks.contains_key(dep) {
                    issues.push(ValidationIssue::error_for(
                        "DANGLING_DEPENDENCY",
                        format!(
                            "Task '{}' depends on '{}' which does not exist",
                            task.id, dep
                        ),
                        task.id.clone(),
                    ));
                }
            }
        }
        if self.has_cycle() {
            issues.push(ValidationIssue::error(
                "CYCLE",
                "Plan contains a dependency cycle",
            ));
        }

        // FM-15 v2.2 (S3-5): 富字段二次扫描——drop_task 后可能让 consumes 失效
        for task in self.tasks.values() {
            if let Err(e) = self.validate_consumes_for(&task.id, &task.consumes_artifacts) {
                issues.push(ValidationIssue::error_for(
                    "INVALID_CONSUMES",
                    e.to_string(),
                    task.id.clone(),
                ));
            }
        }

        // FM-15 v2.2 (S4 / FR-PF-04): 契约 guardrail（仅 warn，不阻止 finalize）
        if let Some(contract) = &self.contract {
            self.assess_contract(contract, &mut issues);
        }
        issues
    }

    /// 仅做"提示型"扫描，全部以 `WARN_*` code 落盘，severity=warn。
    /// 设计上保守且可解释，避免 LLM 因为词法误判而陷入死循环。
    fn assess_contract(&self, contract: &ContractGuardrail, issues: &mut Vec<ValidationIssue>) {
        // 1) Exclusions：扫描每个任务的 file_scope_hints 与 description，
        //    如果命中 exclusion 关键词就 warn。匹配规则：以 `path/` / 文件名 / 关键词
        //    任一形式做大小写不敏感子串匹配。
        for excl in &contract.exclusions {
            let needle = excl.trim();
            if needle.is_empty() {
                continue;
            }
            let needle_lc = needle.to_lowercase();
            for task in self.tasks.values() {
                let mut hit_path: Option<String> = None;
                for p in task
                    .file_scope_hints
                    .definite
                    .iter()
                    .chain(task.file_scope_hints.possible.iter())
                {
                    if p.to_lowercase().contains(&needle_lc) {
                        hit_path = Some(p.clone());
                        break;
                    }
                }
                if let Some(path) = hit_path {
                    issues.push(ValidationIssue::warn_for(
                        "WARN_EXCLUSION_TOUCHED",
                        format!(
                            "Task '{}' file scope path '{}' overlaps with exclusion: \"{}\". \
                             Re-scope the task or drop the path.",
                            task.id, path, needle
                        ),
                        task.id.clone(),
                    ));
                }
            }
        }

        // 2) Scope coverage：每条 scope 应当至少在一个 task 的 title/description/
        //    expected_output 里被语义性地涉及。退化为关键词匹配——取 scope 句子里
        //    长度 ≥ 4 的非常用词作为 anchor，至少有一个 anchor 被命中即视为覆盖。
        if !contract.scope.is_empty() {
            for scope_item in &contract.scope {
                let anchors = extract_anchors(scope_item);
                if anchors.is_empty() {
                    continue; // 用户给的 scope 太短/太通用，跳过避免假阳性
                }
                let covered = self.tasks.values().any(|t| {
                    let blob = format!(
                        "{} {} {}",
                        t.title,
                        t.description,
                        t.expected_output.as_deref().unwrap_or("")
                    )
                    .to_lowercase();
                    anchors.iter().any(|a| blob.contains(a))
                });
                if !covered {
                    issues.push(ValidationIssue::warn(
                        "WARN_SCOPE_NOT_COVERED",
                        format!(
                            "Scope item not visibly covered by any task: \"{}\". \
                             Either add a task that addresses it, or revise an existing one.",
                            scope_item
                        ),
                    ));
                }
            }
        }
    }

    /// Sub-helper: 校验某个 task 的 consumes_artifacts 列表是否合法。
    /// 规则：
    /// 1. 每条形如 `<task_id>.<local_name>`，其 task_id 必须存在；
    /// 2. 该 producer task 必须声明 `produces_artifacts` 包含相应 local_name；
    /// 3. producer 必须在当前 task 的（传递）depends_on 闭包中——否则 artifact
    ///    不会通过 worktree 合并到当前 task 的初始工作区里。
    fn validate_consumes_for(
        &self,
        task_id: &str,
        consumes: &[String],
    ) -> Result<(), PlannerStateError> {
        if consumes.is_empty() {
            return Ok(());
        }
        let ancestors = self.transitive_dependencies(task_id);
        for art_id in consumes {
            let (producer, local_name) = match art_id.split_once('.') {
                Some((p, l)) if !p.is_empty() && !l.is_empty() => (p, l),
                _ => {
                    return Err(PlannerStateError::UnknownConsumedArtifact {
                        task_id: task_id.to_string(),
                        artifact_id: art_id.clone(),
                    });
                }
            };
            let producer_task = self.tasks.get(producer).ok_or_else(|| {
                PlannerStateError::UnknownConsumedArtifact {
                    task_id: task_id.to_string(),
                    artifact_id: art_id.clone(),
                }
            })?;
            let declared = producer_task
                .produces_artifacts
                .iter()
                .any(|d| d.local_name == local_name);
            if !declared {
                return Err(PlannerStateError::UnknownConsumedArtifact {
                    task_id: task_id.to_string(),
                    artifact_id: art_id.clone(),
                });
            }
            if producer == task_id {
                return Err(PlannerStateError::UnknownConsumedArtifact {
                    task_id: task_id.to_string(),
                    artifact_id: art_id.clone(),
                });
            }
            if !ancestors.contains(producer) {
                return Err(PlannerStateError::ConsumedArtifactWithoutDependency {
                    task_id: task_id.to_string(),
                    artifact_id: art_id.clone(),
                    producer: producer.to_string(),
                });
            }
        }
        Ok(())
    }

    /// 传递依赖闭包（不含自身）。
    fn transitive_dependencies(&self, task_id: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        let mut stack: Vec<String> = self
            .tasks
            .get(task_id)
            .map(|t| t.depends_on.clone())
            .unwrap_or_default();
        while let Some(cur) = stack.pop() {
            if !out.insert(cur.clone()) {
                continue;
            }
            if let Some(t) = self.tasks.get(&cur) {
                for d in &t.depends_on {
                    if !out.contains(d) {
                        stack.push(d.clone());
                    }
                }
            }
        }
        out
    }

    /// 终态：把内存 DAG 物化为 `PlannerOutput`。要求 mission_title 已设置且 validate 全过。
    pub fn finalize(&mut self, mission_title: String) -> Result<PlannerOutput, PlannerStateError> {
        if self.tasks.is_empty() {
            return Err(PlannerStateError::EmptyPlan);
        }
        let issues = self.validate_plan();
        // FM-15 v2.2 (S4): warn 类 issue 不阻塞 finalize；只针对 error 兜底
        let blocking: Vec<&ValidationIssue> = issues
            .iter()
            .filter(|i| i.severity == IssueSeverity::Error)
            .collect();
        if !blocking.is_empty() {
            // 把首个 error 转成 state error；调用方应先调 validate_plan 做精细引导
            // 这里只在 LLM 跳过 validate 直接 finalize 时兜底
            let first = blocking[0];
            return Err(match first.code.as_str() {
                "CYCLE" => PlannerStateError::CyclicDependency {
                    from: "<unknown>".into(),
                    to: "<unknown>".into(),
                },
                _ => PlannerStateError::TaskNotFound(
                    first.task_id.clone().unwrap_or_else(|| "<unknown>".into()),
                ),
            });
        }
        self.mission_title = Some(mission_title.clone());
        self.finalized = true;
        Ok(PlannerOutput {
            mission_title,
            tasks: self.tasks().into_iter().cloned().collect(),
        })
    }

    /// Kahn 拓扑排序 + cycle detection。返回 true 表示存在环。
    fn has_cycle(&self) -> bool {
        let mut indegree: HashMap<&str, usize> =
            self.tasks.keys().map(|k| (k.as_str(), 0)).collect();
        for t in self.tasks.values() {
            for d in &t.depends_on {
                if self.tasks.contains_key(d) {
                    *indegree.entry(t.id.as_str()).or_insert(0) += 1;
                }
            }
        }
        let mut queue: Vec<&str> = indegree
            .iter()
            .filter_map(|(k, v)| if *v == 0 { Some(*k) } else { None })
            .collect();
        let mut visited = HashSet::new();
        while let Some(node) = queue.pop() {
            visited.insert(node.to_string());
            for t in self.tasks.values() {
                if t.depends_on.iter().any(|d| d == node) {
                    let entry = indegree.get_mut(t.id.as_str()).unwrap();
                    *entry -= 1;
                    if *entry == 0 {
                        queue.push(t.id.as_str());
                    }
                }
            }
        }
        visited.len() != self.tasks.len()
    }
}

fn validate_id(id: &str) -> Result<(), PlannerStateError> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(PlannerStateError::InvalidTaskId(id.to_string()));
    }
    if trimmed != id {
        return Err(PlannerStateError::InvalidTaskId(id.to_string()));
    }
    Ok(())
}

fn validate_complexity(task_id: &str, value: &str) -> Result<(), PlannerStateError> {
    if !matches!(value, "low" | "medium" | "high") {
        return Err(PlannerStateError::InvalidComplexity {
            task_id: task_id.to_string(),
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_role(task_id: &str, role: &str) -> Result<(), PlannerStateError> {
    let reg = roles::registry();
    if !reg.contains(role) {
        return Err(PlannerStateError::InvalidRole {
            task_id: task_id.to_string(),
            role: role.to_string(),
            allowed: reg.ids_csv(),
        });
    }
    Ok(())
}

fn validate_additional_skills(
    task_id: &str,
    role: &str,
    skills: &[String],
) -> Result<(), PlannerStateError> {
    let reg = crate::skills::registry::global();
    let mut seen = HashSet::new();
    for s in skills {
        if !seen.insert(s.clone()) {
            return Err(PlannerStateError::InvalidSkill {
                task_id: task_id.to_string(),
                message: format!("duplicate skill `{s}`"),
            });
        }
        reg.validate_skill_role(s, role)
            .map_err(|m| PlannerStateError::InvalidSkill {
                task_id: task_id.to_string(),
                message: m,
            })?;
    }
    Ok(())
}

fn validate_produced_artifacts(
    task_id: &str,
    decls: &[ArtifactDecl],
) -> Result<(), PlannerStateError> {
    let mut seen: HashSet<&str> = HashSet::new();
    for d in decls {
        artifacts::validate_decl(&d.local_name, &d.artifact_type).map_err(|e| {
            PlannerStateError::InvalidProducedArtifact {
                task_id: task_id.to_string(),
                message: e.to_string(),
            }
        })?;
        if !seen.insert(d.local_name.as_str()) {
            return Err(PlannerStateError::DuplicateProducedArtifact {
                task_id: task_id.to_string(),
                local_name: d.local_name.clone(),
            });
        }
    }
    Ok(())
}

fn validate_file_scope_hints(
    task_id: &str,
    hints: &FileScopeHints,
) -> Result<(), PlannerStateError> {
    for p in hints.definite.iter().chain(hints.possible.iter()) {
        validate_repo_relative_path(task_id, p)?;
    }
    Ok(())
}

/// FM-15 v2.2 (S4): 从一句 scope/exclusion 文本里抽取 anchor 词——
/// 用作 plan 覆盖度的提示性匹配。规则：
/// - 拆词后取长度 ≥ 4 的 token；
/// - 过滤常见停用词（中英混排，简单白名单足够避免噪声）；
/// - 全部小写；最多保留 8 个，避免单条 scope 反复触发命中。
/// 这是有意识的"轻量启发式"——只用作 warn，不阻 finalize。
fn extract_anchors(text: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "should", "shall", "must", "make", "build", "create", "support", "feature",
        "system", "module", "user", "users", "with", "when", "that", "from", "into",
        "your", "this", "those", "their", "these", "task", "tasks", "agent", "agents",
        "需要", "实现", "支持", "完成", "提供", "用户", "可以", "能够", "应当", "保证",
        "处理", "功能", "模块", "系统",
    ];
    let mut out: Vec<String> = Vec::new();
    let lower = text.to_lowercase();
    let raw = lower.replace(
        |c: char| !c.is_alphanumeric() && !matches!(c, '_' | '-' | '.' | '/'),
        " ",
    );
    for tok in raw.split_whitespace() {
        if tok.chars().count() < 4 {
            continue;
        }
        if STOP.contains(&tok) {
            continue;
        }
        if !out.iter().any(|e| e == tok) {
            out.push(tok.to_string());
            if out.len() >= 8 {
                break;
            }
        }
    }
    out
}

fn validate_repo_relative_path(task_id: &str, raw: &str) -> Result<(), PlannerStateError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PlannerStateError::InvalidFileScopePath {
            task_id: task_id.to_string(),
            path: raw.to_string(),
            message: "empty path".into(),
        });
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return Err(PlannerStateError::InvalidFileScopePath {
            task_id: task_id.to_string(),
            path: raw.to_string(),
            message: "must be repo-relative (no leading `/`)".into(),
        });
    }
    for seg in trimmed.split(['/', '\\']) {
        if seg == ".." {
            return Err(PlannerStateError::InvalidFileScopePath {
                task_id: task_id.to_string(),
                path: raw.to_string(),
                message: "path segment `..` is not allowed (no escaping repo root)".into(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, role: &str) -> ProposeTaskInput {
        ProposeTaskInput {
            id: id.to_string(),
            title: format!("Title {id}"),
            description: format!("Desc {id}"),
            complexity: "medium".to_string(),
            expected_output: format!("Output {id}"),
            role: role.to_string(),
            depends_on: vec![],
            additional_skills: vec![],
            produces_artifacts: vec![],
            consumes_artifacts: vec![],
            file_scope_hints: FileScopeHints::default(),
        }
    }

    #[test]
    fn propose_and_finalize_single_task() {
        let mut s = PlannerState::new();
        s.propose_task(p("T1", "implementer")).unwrap();
        let out = s.finalize("Demo".into()).unwrap();
        assert_eq!(out.tasks.len(), 1);
        assert_eq!(out.tasks[0].effective_role(), "implementer");
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut s = PlannerState::new();
        s.propose_task(p("T1", "implementer")).unwrap();
        let err = s.propose_task(p("T1", "implementer")).unwrap_err();
        assert!(matches!(err, PlannerStateError::DuplicateTaskId(_)));
    }

    #[test]
    fn unknown_role_rejected() {
        let mut s = PlannerState::new();
        let err = s.propose_task(p("T1", "ceo")).unwrap_err();
        assert!(matches!(err, PlannerStateError::InvalidRole { .. }));
    }

    #[test]
    fn empty_expected_output_rejected() {
        let mut s = PlannerState::new();
        let mut input = p("T1", "implementer");
        input.expected_output = "   ".into();
        let err = s.propose_task(input).unwrap_err();
        assert!(matches!(err, PlannerStateError::EmptyExpectedOutput(_)));
    }

    #[test]
    fn add_dependency_creates_edge() {
        let mut s = PlannerState::new();
        s.propose_task(p("T1", "architect")).unwrap();
        s.propose_task(p("T2", "implementer")).unwrap();
        s.add_dependency("T2", "T1").unwrap();
        assert_eq!(s.get("T2").unwrap().depends_on, vec!["T1"]);
    }

    #[test]
    fn add_dependency_idempotent() {
        let mut s = PlannerState::new();
        s.propose_task(p("T1", "architect")).unwrap();
        s.propose_task(p("T2", "implementer")).unwrap();
        s.add_dependency("T2", "T1").unwrap();
        s.add_dependency("T2", "T1").unwrap();
        assert_eq!(s.get("T2").unwrap().depends_on.len(), 1);
    }

    #[test]
    fn add_dependency_cycle_rejected_and_rolled_back() {
        let mut s = PlannerState::new();
        s.propose_task(p("T1", "architect")).unwrap();
        s.propose_task(p("T2", "implementer")).unwrap();
        s.add_dependency("T2", "T1").unwrap();
        let err = s.add_dependency("T1", "T2").unwrap_err();
        assert!(matches!(err, PlannerStateError::CyclicDependency { .. }));
        // 回滚后 T1 不应有依赖
        assert!(s.get("T1").unwrap().depends_on.is_empty());
    }

    #[test]
    fn unknown_dependency_rejected() {
        let mut s = PlannerState::new();
        s.propose_task(p("T1", "architect")).unwrap();
        let err = s.add_dependency("T1", "T9").unwrap_err();
        assert!(matches!(err, PlannerStateError::UnknownDependency { .. }));
    }

    #[test]
    fn drop_task_cleans_up_references() {
        let mut s = PlannerState::new();
        s.propose_task(p("T1", "architect")).unwrap();
        s.propose_task(p("T2", "implementer")).unwrap();
        s.add_dependency("T2", "T1").unwrap();
        s.drop_task("T1").unwrap();
        assert!(s.get("T1").is_none());
        assert!(s.get("T2").unwrap().depends_on.is_empty());
    }

    #[test]
    fn revise_task_updates_fields() {
        let mut s = PlannerState::new();
        s.propose_task(p("T1", "implementer")).unwrap();
        s.revise_task(ReviseTaskInput {
            task_id: "T1".into(),
            title: Some("New title".into()),
            description: None,
            complexity: Some("high".into()),
            expected_output: None,
            role: Some("architect".into()),
            additional_skills: None,
            produces_artifacts: None,
            consumes_artifacts: None,
            file_scope_hints: None,
        })
        .unwrap();
        let t = s.get("T1").unwrap();
        assert_eq!(t.title, "New title");
        assert_eq!(t.complexity, "high");
        assert_eq!(t.effective_role(), "architect");
    }

    #[test]
    fn validate_plan_empty_returns_issue() {
        let s = PlannerState::new();
        let issues = s.validate_plan();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "EMPTY_PLAN");
    }

    #[test]
    fn finalize_empty_plan_errors() {
        let mut s = PlannerState::new();
        let err = s.finalize("Demo".into()).unwrap_err();
        assert!(matches!(err, PlannerStateError::EmptyPlan));
    }

    #[test]
    fn finalize_diamond_dag_succeeds() {
        let mut s = PlannerState::new();
        s.propose_task(p("T1", "architect")).unwrap();
        s.propose_task(p("T2", "implementer")).unwrap();
        s.propose_task(p("T3", "implementer")).unwrap();
        s.propose_task(p("T4", "tester")).unwrap();
        s.add_dependency("T2", "T1").unwrap();
        s.add_dependency("T3", "T1").unwrap();
        s.add_dependency("T4", "T2").unwrap();
        s.add_dependency("T4", "T3").unwrap();
        let out = s.finalize("Diamond".into()).unwrap();
        assert_eq!(out.tasks.len(), 4);
    }

    // ---- FM-15 v2.2 (S3-5) 富语义字段单测 ----

    fn produces(local_name: &str, ty: &str) -> ArtifactDecl {
        ArtifactDecl {
            local_name: local_name.into(),
            artifact_type: ty.into(),
            summary: format!("{local_name} summary"),
        }
    }

    #[test]
    fn ut15_propose_with_artifacts_round_trips() {
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "architect");
        t1.produces_artifacts = vec![produces("api_spec", "api_spec")];
        s.propose_task(t1).unwrap();

        let mut t2 = p("T2", "implementer");
        t2.depends_on = vec!["T1".into()];
        t2.consumes_artifacts = vec!["T1.api_spec".into()];
        t2.file_scope_hints = FileScopeHints {
            definite: vec!["src/api/mod.rs".into()],
            possible: vec!["src/api/types.rs".into()],
        };
        s.propose_task(t2).unwrap();

        let out = s.finalize("Two stage".into()).unwrap();
        assert_eq!(out.tasks.len(), 2);
        assert_eq!(out.tasks[0].produces_artifacts.len(), 1);
        assert_eq!(out.tasks[1].consumes_artifacts, vec!["T1.api_spec"]);
        assert_eq!(out.tasks[1].file_scope_hints.definite.len(), 1);
    }

    #[test]
    fn ut15_consumes_unknown_producer_rejected() {
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "implementer");
        t1.consumes_artifacts = vec!["T0.api_spec".into()];
        let err = s.propose_task(t1).unwrap_err();
        assert!(matches!(
            err,
            PlannerStateError::UnknownConsumedArtifact { .. }
        ));
    }

    #[test]
    fn ut15_consumes_without_dependency_rejected() {
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "architect");
        t1.produces_artifacts = vec![produces("api_spec", "api_spec")];
        s.propose_task(t1).unwrap();

        // T2 不在 T1 的依赖闭包里，应该被拒绝
        let mut t2 = p("T2", "implementer");
        t2.consumes_artifacts = vec!["T1.api_spec".into()];
        let err = s.propose_task(t2).unwrap_err();
        assert!(matches!(
            err,
            PlannerStateError::ConsumedArtifactWithoutDependency { .. }
        ));
    }

    #[test]
    fn ut15_invalid_skill_rejected() {
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "implementer");
        t1.additional_skills = vec!["non-existent-skill".into()];
        let err = s.propose_task(t1).unwrap_err();
        assert!(matches!(err, PlannerStateError::InvalidSkill { .. }));
    }

    #[test]
    fn ut15_duplicate_local_name_rejected() {
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "architect");
        t1.produces_artifacts = vec![
            produces("design", "design_doc"),
            produces("design", "docs"),
        ];
        let err = s.propose_task(t1).unwrap_err();
        assert!(matches!(
            err,
            PlannerStateError::DuplicateProducedArtifact { .. }
        ));
    }

    #[test]
    fn ut15_invalid_file_scope_path_rejected() {
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "implementer");
        t1.file_scope_hints = FileScopeHints {
            definite: vec!["../escape.rs".into()],
            possible: vec![],
        };
        let err = s.propose_task(t1).unwrap_err();
        assert!(matches!(
            err,
            PlannerStateError::InvalidFileScopePath { .. }
        ));
    }

    #[test]
    fn ut15_drop_producer_invalidates_consumes() {
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "architect");
        t1.produces_artifacts = vec![produces("api_spec", "api_spec")];
        s.propose_task(t1).unwrap();

        let mut t2 = p("T2", "implementer");
        t2.depends_on = vec!["T1".into()];
        t2.consumes_artifacts = vec!["T1.api_spec".into()];
        s.propose_task(t2).unwrap();

        s.drop_task("T1").unwrap();
        let issues = s.validate_plan();
        assert!(issues.iter().any(|i| i.code == "INVALID_CONSUMES"));
    }

    // ---- FM-15 v2.2 (S4): Contract guardrail ---------------------------------

    #[test]
    fn ut15_s4_exclusion_touched_emits_warn() {
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "implementer");
        t1.file_scope_hints = FileScopeHints {
            definite: vec!["src/legacy/payment.ts".into()],
            possible: vec![],
        };
        s.propose_task(t1).unwrap();

        s.set_contract(ContractGuardrail {
            scope: vec![],
            exclusions: vec!["src/legacy/".into()],
            constraints: vec![],
            assumptions: vec![],
        });

        let issues = s.validate_plan();
        let warns: Vec<_> = issues
            .iter()
            .filter(|i| i.code == "WARN_EXCLUSION_TOUCHED")
            .collect();
        assert_eq!(warns.len(), 1, "expected exactly one exclusion-touch warn");
        assert_eq!(warns[0].severity, IssueSeverity::Warn);
        assert_eq!(warns[0].task_id.as_deref(), Some("T1"));
    }

    #[test]
    fn ut15_s4_exclusion_warn_does_not_block_finalize_eligibility() {
        // 仅 warn 不应该让 validate_plan 报告 ERROR，因此 finalize 视角的"无错误"应当成立
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "implementer");
        t1.file_scope_hints = FileScopeHints {
            definite: vec!["src/legacy/foo.ts".into()],
            possible: vec![],
        };
        s.propose_task(t1).unwrap();
        s.set_contract(ContractGuardrail {
            scope: vec![],
            exclusions: vec!["src/legacy/".into()],
            constraints: vec![],
            assumptions: vec![],
        });
        let issues = s.validate_plan();
        let any_error = issues.iter().any(|i| i.severity == IssueSeverity::Error);
        assert!(!any_error, "guardrail warns should not produce error issues");
    }

    #[test]
    fn ut15_s4_scope_not_covered_emits_warn() {
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "implementer");
        t1.title = "Login form".into();
        t1.description = "Build the login form UI".into();
        s.propose_task(t1).unwrap();

        s.set_contract(ContractGuardrail {
            scope: vec!["Implement password reset flow".into()],
            exclusions: vec![],
            constraints: vec![],
            assumptions: vec![],
        });
        let issues = s.validate_plan();
        assert!(
            issues
                .iter()
                .any(|i| i.code == "WARN_SCOPE_NOT_COVERED" && i.severity == IssueSeverity::Warn),
            "expected WARN_SCOPE_NOT_COVERED, got {:?}",
            issues
        );
    }

    #[test]
    fn ut15_s4_scope_covered_no_warn() {
        let mut s = PlannerState::new();
        let mut t1 = p("T1", "implementer");
        t1.title = "Password reset email".into();
        t1.description = "Implement password reset flow with email verification".into();
        s.propose_task(t1).unwrap();

        s.set_contract(ContractGuardrail {
            scope: vec!["Implement password reset flow".into()],
            exclusions: vec![],
            constraints: vec![],
            assumptions: vec![],
        });
        let issues = s.validate_plan();
        assert!(
            !issues.iter().any(|i| i.code == "WARN_SCOPE_NOT_COVERED"),
            "expected no scope-not-covered warn, got {:?}",
            issues
        );
    }

    #[test]
    fn ut15_s4_extract_anchors_strips_stopwords() {
        let anchors = extract_anchors("System should support password reset flow");
        // "should" / "system" / "support" 是停用词，"reset" / "flow" / "password" 应当保留
        assert!(anchors.iter().any(|a| a == "password"));
        assert!(anchors.iter().any(|a| a == "reset"));
        assert!(anchors.iter().any(|a| a == "flow"));
        assert!(!anchors.iter().any(|a| a == "should"));
        assert!(!anchors.iter().any(|a| a == "system"));
    }
}
