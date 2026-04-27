//! FM-15 v2.2 Slice 1: Planner Agent Loop 引擎。
//!
//! S1 范围（最小可工作）：
//! - 创建 `planner_session` 行
//! - 装载 `planner_tool_definitions`（read-only filesystem + DAG state machine 工具）
//! - 主循环：LLM 调用 → 处理 tool_use → 持久化 step → 发事件
//! - 终止：`finalize_plan` 工具返回 `Some(PlannerOutput)`
//! - 兜底校验：`finalize` 后再走 `parse_and_validate`（DAG 一致性 guardrail）
//! - 上限：`max_steps`（默认 30 次 LLM 迭代）+ `PLANNER_TIMEOUT`
//!
//! 与现有 `AgentEngine` 暂时**并行**存在：S2 提取共享 trait 后再统一。
//! 这样避免 S1 把 Coding Agent 主路径搅乱。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use tauri::{Emitter, Manager};
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use crate::agent::planner::{parse_and_validate, PlannerError, PlannerOutput};
use crate::agent::planner_fetch::{FetchPolicy, PlannerFetchCoordinator};
use crate::agent::planner_state::{ContractGuardrail, PlannerState};
use crate::agent::planner_tools::{
    planner_tool_definitions, PlannerFetchRuntime, PlannerToolExecutor,
};
use crate::commands::ConfigManager;
use crate::db::{queries, Database};
use crate::llm::{
    ContentBlock, LlmProvider, LlmRequest, Message, MessageRole, StreamChunk, StreamChunkKind,
};

/// 单次 plan_mission 的总超时
const PLANNER_LOOP_TIMEOUT: Duration = Duration::from_secs(180);
/// 最大 LLM 迭代次数（每次 = 一轮 stream_chat）
const PLANNER_MAX_STEPS: u32 = 30;
/// 单次 LLM 输出 token 上限
const PLANNER_MAX_OUTPUT_TOKENS: u32 = 4096;

/// FM-15 FR-06.1 (S1 缩减版): Planner Agent Loop system prompt。
/// 不含 skill / artifact / contract / fetch_url——这些将在 S2/S3/S4 加入。
pub const PLANNER_SYSTEM_PROMPT_V2: &str = r#"# Mission Planner Agent (Miragenty)

You are Miragenty's Mission Planner. Your job is to break a user requirement
into a directed acyclic Task graph that downstream coding agents will execute
autonomously.

## Workflow

1. **Scout** — Use `list_directory` (start at ".") and `read_file` to understand
   the repository: tech stack, existing modules, conventions. Always start here.
2. **Pick roles & skills** — Call `list_roles` ONCE for the closed role set,
   and `list_skills` ONCE to see the available skill ids and which roles they
   are compatible with.
3. **Propose** — Use `propose_task` one task at a time. Each call is validated
   immediately; on validation error, fix the input and retry the same tool.
4. **Wire** — Use `add_dependency` to declare upstream relationships.
   A → B means B consumes information / artifacts from A. Pure time-ordering
   without information flow is NOT a dependency.
5. **Validate** — Call `validate_plan`; if any issue is reported, fix it
   (revise_task / drop_task / add_dependency) and re-validate.
6. **Finalize** — When `validate_plan` returns zero issues, call
   `finalize_plan` with a 5–10 word `mission_title`. This exits the loop.

## Decision principles

- **Self-contained tasks**: Each `description` must contain enough detail for a
  coding agent to execute without re-asking the user.
- **Verifiable contract**: Each `expected_output` must be objectively checkable
  (e.g. "build passes", "endpoint returns 200 for /api/auth/login",
  "design doc at docs/design/auth.md exists with sections X, Y, Z").
- **Right role**: Pick from architect / implementer / refactorer / tester /
  integrator / researcher based on the actual nature of the work.
- **Granularity**: Aim for 3–10 tasks. Each task should be ~20–50 steps of
  coding-agent work. Don't make 1-line tasks; don't make month-long tasks.
- **Tester pairing**: For meaningful new logic, pair an `implementer` task with
  a downstream `tester` task that depends on it.

## Skills (FR-02)

- Each role already loads its default skills automatically. Use
  `additional_skills` ONLY when the task needs domain knowledge beyond the
  role default — e.g. attach `system-design` to an architect task that
  produces an API spec, attach `api-design` to an implementer that designs
  a public REST surface.
- Every skill in `additional_skills` must (a) appear in `list_skills` and
  (b) be compatible with the chosen `role`. The runtime rejects unknown or
  incompatible skills.
- Don't carpet-bomb skills. Two well-chosen skills beat five generic ones.

## Artifacts (FR-03)

Artifacts are the *named, durable* outputs of a task that downstream tasks may
consume. They are how information flows along the DAG.

- Declare every meaningful output in `produces_artifacts`. Use a
  `local_name` in `snake_case` that is unique within the task, plus a
  `type` from this closed set: `design_doc | api_spec | schema |
  code_module | test_module | config | docs | report`.
- A downstream task that needs an upstream output declares the artifact id in
  `consumes_artifacts` using the form `<upstream_task_id>.<local_name>`
  (e.g. `T1.api_spec`).
- The runtime enforces that the producing task is in the consumer's
  *transitive* `depends_on` closure. If you declare a consumes-edge, also
  declare the depends_on edge — otherwise validation will reject the plan.
- Self-contained tasks may have an empty `produces_artifacts`; that's fine.

## File scope hints (FR-04)

`file_scope_hints` is a best-effort signal used for conflict prediction and
worktree warm-up — NOT a hard sandbox.

- `definite`: relative paths the task is essentially guaranteed to modify.
- `possible`: paths it might touch.
- All paths are repo-relative. No leading `/`, no `..` segments. Globs are
  not supported here; just enumerate the most relevant files / directories.
- It's better to leave both empty than to lie. The runtime trusts these
  hints only as a tiebreaker.

## Tool discipline

- One change per tool call. Don't try to batch 5 propose_task calls in a single
  turn — submit one, see the result, then submit the next.
- On validation error: read the error message carefully, fix the specific
  problem, re-call the same tool. Do not argue or rationalize.
- Read tools (`list_directory`, `read_file`) have no quota, but re-reading the
  same file is wasteful — remember what you've already seen.
- `fetch_url` is **expensive** and almost always requires the human at the
  keyboard to click "allow". Only call it when (a) you genuinely need an
  external spec / docs that is NOT in the repo, AND (b) you can name the
  reason in one short sentence. Local hosts are blocked. Per-session quota
  applies. Prefer reading the repo first.
- `finalize_plan` exits the loop. Only call it after `validate_plan` is clean.

## Hard constraints (will fail finalize)

- The final DAG must contain at least one task.
- No cycles, no dangling dependencies, no duplicate task ids.
- Every task must have a non-empty `expected_output` and a valid `role`.
- Every `additional_skills` entry must be a known skill id compatible with
  the task's role.
- Every `consumes_artifacts` entry must reference an artifact declared by
  some task in the consumer's transitive depends_on closure.

## Contract guardrails (warnings, not blockers)

If a Mission Contract was signed via Pre-flight, `validate_plan` may surface
`severity: "warn"` issues:
- `WARN_EXCLUSION_TOUCHED`: a task's `file_scope_hints` overlaps with an item
  in the Exclusions section. Re-scope the task or drop the offending path.
- `WARN_SCOPE_NOT_COVERED`: a Scope item is not visibly covered by any task's
  title / description / expected_output. Either add a task that addresses it,
  or revise an existing task to call it out.

These warnings DO NOT block `finalize_plan`, but you should resolve them when
the cause is a real oversight. If the contract item is intentionally out of
this DAG (e.g. it's a multi-mission roadmap item), explain it in the final
text answer rather than silently ignoring it.
"#;

/// 给前端的流式事件 payload。前端按 `kind` 区分渲染。
#[derive(Debug, Clone, Serialize)]
pub struct PlannerStepEvent {
    pub session_id: String,
    pub step_no: i64,
    pub kind: String, // tool_call | tool_result | text | thinking | error | status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_args: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Planner Loop 的最终结果。
#[derive(Debug, Clone)]
pub struct PlannerLoopOutcome {
    pub session_id: String,
    pub output: PlannerOutput,
    pub total_steps: u32,
    pub total_tokens: u64,
}

/// Planner session 的来源类型，决定写入 DB 的 `kind` 与 prompt 拼装。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerKind {
    /// 直接来自 Quick Plan：只有用户的自然语言描述。
    Planner,
    /// 来自 Pre-flight 签约后：会读取并注入 mission_contracts / contract_items。
    Preflight,
}

impl PlannerKind {
    fn db_value(self) -> &'static str {
        match self {
            PlannerKind::Planner => "planner",
            PlannerKind::Preflight => "preflight",
        }
    }
}

/// `run()` 入参集合，避免参数列表无限增长。
pub struct PlannerRunRequest<'a> {
    pub kind: PlannerKind,
    pub description: &'a str,
    pub mission_id: Option<&'a str>,
    pub contract_id: Option<&'a str>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlannerEngineError {
    #[error("Planner LLM error: {0}")]
    Llm(String),
    #[error("Planner timed out after {0}s")]
    Timeout(u64),
    #[error("Planner exhausted {0} steps without finalizing the plan")]
    StepBudgetExhausted(u32),
    #[error("Planner finalize_plan output failed validation guardrail: {0}")]
    GuardrailFailed(PlannerError),
    #[error("Database error: {0}")]
    Db(String),
}

pub struct PlannerEngine {
    provider: Arc<dyn LlmProvider>,
    model: String,
    repo_root: PathBuf,
    app_handle: tauri::AppHandle,
}

impl PlannerEngine {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: String,
        repo_root: PathBuf,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            provider,
            model,
            repo_root,
            app_handle,
        }
    }

    /// Backward-compatible: 旧的 plan_mission 调用方一直用 `(description, mission_id)`，
    /// 默认按 `PlannerKind::Planner` 启动。
    pub async fn run(
        &self,
        description: &str,
        mission_id: Option<&str>,
    ) -> Result<PlannerLoopOutcome, PlannerEngineError> {
        self.run_with(PlannerRunRequest {
            kind: PlannerKind::Planner,
            description,
            mission_id,
            contract_id: None,
        })
        .await
    }

    /// 完整版入口：支持 `PlannerKind::Preflight` + 注入 contract 上下文。
    /// 返回 `(session_id, PlannerOutput)`。
    pub async fn run_with(
        &self,
        req: PlannerRunRequest<'_>,
    ) -> Result<PlannerLoopOutcome, PlannerEngineError> {
        let session_id = Uuid::new_v4().to_string();
        let repo_path_display = self.repo_root.display().to_string();

        let db = self.app_handle.try_state::<Database>().ok_or_else(|| {
            PlannerEngineError::Db("Database state not registered with Tauri".into())
        })?;
        db.with_conn(|conn| {
            queries::create_planner_session(
                conn,
                &session_id,
                req.mission_id,
                req.kind.db_value(),
                &repo_path_display,
                req.description,
            )
        })
        .map_err(|e| PlannerEngineError::Db(e.to_string()))?;

        // FM-15 v2.2 (S2 / FR-PF-01): preflight 模式下加载 contract，作为额外上下文塞进首条消息
        // FM-15 v2.2 (S4): 同时拿到结构化 ContractGuardrail，注入 PlannerState 做 guardrail
        let (contract_block, contract_guardrail): (Option<String>, Option<ContractGuardrail>) =
            if req.kind == PlannerKind::Preflight {
                match req.mission_id {
                    Some(mid) => match db.with_conn(|conn| Ok(load_contract_payload(conn, mid))) {
                        Ok((text, guardrail)) => (Some(text), Some(guardrail)),
                        Err(_) => (None, None),
                    },
                    None => (None, None),
                }
            } else {
                (None, None)
            };

        self.emit_status(&session_id, "started");

        let loop_result = tokio::time::timeout(
            PLANNER_LOOP_TIMEOUT,
            self.drive_loop(
                &session_id,
                req.description,
                contract_block.as_deref(),
                contract_guardrail,
            ),
        )
        .await;

        match loop_result {
            Ok(Ok(outcome)) => {
                if let Err(e) = db.with_conn(|conn| {
                    queries::complete_planner_session(
                        conn,
                        &session_id,
                        outcome.total_steps as i64,
                        outcome.total_tokens as i64,
                    )
                }) {
                    tracing::warn!("complete_planner_session failed: {e}");
                }
                self.emit_status(&session_id, "completed");
                Ok(outcome)
            }
            Ok(Err(e)) => {
                let msg = e.to_string();
                if let Err(db_err) = db.with_conn(|conn| {
                    queries::fail_planner_session(conn, &session_id, 0, 0, &msg)
                }) {
                    tracing::warn!("fail_planner_session failed: {db_err}");
                }
                self.emit_step(&PlannerStepEvent {
                    session_id: session_id.clone(),
                    step_no: -1,
                    kind: "error".into(),
                    tool_name: None,
                    tool_args: None,
                    tool_result: None,
                    text_content: None,
                    error: Some(msg),
                });
                self.emit_status(&session_id, "failed");
                Err(e)
            }
            Err(_) => {
                let secs = PLANNER_LOOP_TIMEOUT.as_secs();
                let msg = format!("Planner timed out after {secs}s");
                if let Err(db_err) = db.with_conn(|conn| {
                    queries::fail_planner_session(conn, &session_id, 0, 0, &msg)
                }) {
                    tracing::warn!("fail_planner_session failed: {db_err}");
                }
                self.emit_status(&session_id, "failed");
                Err(PlannerEngineError::Timeout(secs))
            }
        }
    }

    async fn drive_loop(
        &self,
        session_id: &str,
        description: &str,
        contract_block: Option<&str>,
        contract_guardrail: Option<ContractGuardrail>,
    ) -> Result<PlannerLoopOutcome, PlannerEngineError> {
        let state = Arc::new(Mutex::new(PlannerState::new()));
        if let Some(guardrail) = contract_guardrail {
            if !guardrail.is_empty() {
                state.lock().await.set_contract(guardrail);
            }
        }
        let mut executor = PlannerToolExecutor::new(self.repo_root.clone(), state.clone());

        // FM-15 v2.2 (S3-4): 装载 fetch_url runtime（policy 来自 AppConfig，
        // coordinator 是 Tauri global state，会被 IPC `confirm_planner_fetch` 唤醒）。
        if let (Some(coord), Some(cfg)) = (
            self.app_handle.try_state::<Arc<PlannerFetchCoordinator>>(),
            self.app_handle.try_state::<ConfigManager>(),
        ) {
            let policy = FetchPolicy::from_app_config(&cfg.get_config_snapshot());
            executor = executor.with_fetch_runtime(PlannerFetchRuntime {
                session_id: session_id.to_string(),
                app_handle: self.app_handle.clone(),
                coordinator: coord.inner().clone(),
                policy,
            });
        } else {
            tracing::warn!(
                "[planner_engine] fetch_url disabled: PlannerFetchCoordinator or ConfigManager not registered"
            );
        }

        let tools = planner_tool_definitions();

        // FM-15 v2.2 (S2): 当 mission 走 Pre-flight → 签约链路时，把已澄清的 contract
        // 作为首轮 user message 的一部分注入。S4 会做格式化 / guardrail，本期裸 dump 即可。
        let user_text = if let Some(contract_text) = contract_block {
            format!(
                "User requirement:\n\n{description}\n\nRepository root: {}\n\n\
                ## Mission Contract (signed via Pre-flight)\n\n{contract_text}\n\n\
                Treat the contract above as authoritative: do not propose tasks that violate \
                Exclusions, and ensure every Scope item is covered by at least one task.",
                self.repo_root.display()
            )
        } else {
            format!(
                "User requirement:\n\n{description}\n\nRepository root: {}",
                self.repo_root.display()
            )
        };

        let user_message = Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Text { text: user_text }],
            cache_control: None,
        };
        let mut messages: Vec<Message> = vec![user_message];

        let mut iteration: u32 = 0;
        let mut step_no: i64 = 0;
        let mut total_tokens: u64 = 0;

        loop {
            if iteration >= PLANNER_MAX_STEPS {
                return Err(PlannerEngineError::StepBudgetExhausted(PLANNER_MAX_STEPS));
            }
            iteration += 1;

            let request = LlmRequest {
                model: self.model.clone(),
                system: Some(PLANNER_SYSTEM_PROMPT_V2.to_string()),
                messages: messages.clone(),
                tools: tools.clone(),
                max_tokens: PLANNER_MAX_OUTPUT_TOKENS,
            };

            let response = self.call_llm_streaming(request).await?;
            total_tokens += response.usage.input_tokens + response.usage.output_tokens;

            // 持久化文本输出（供前端事后回溯）
            for block in &response.content {
                if let ContentBlock::Text { text } = block {
                    if !text.trim().is_empty() {
                        step_no += 1;
                        self.persist_step(
                            session_id,
                            step_no,
                            "text",
                            None,
                            None,
                            None,
                            Some(text),
                            response.usage.output_tokens as i64,
                        );
                        self.emit_step(&PlannerStepEvent {
                            session_id: session_id.to_string(),
                            step_no,
                            kind: "text".into(),
                            tool_name: None,
                            tool_args: None,
                            tool_result: None,
                            text_content: Some(text.clone()),
                            error: None,
                        });
                    }
                }
            }

            // 收集本轮所有 tool_use
            let tool_uses: Vec<(String, String, serde_json::Value)> = response
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { id, name, input } => {
                        Some((id.clone(), name.clone(), input.clone()))
                    }
                    _ => None,
                })
                .collect();

            // 把 assistant 消息推进 history
            messages.push(Message {
                role: MessageRole::Assistant,
                content: response.content.clone(),
                cache_control: None,
            });

            if tool_uses.is_empty() {
                // 没有 tool_use 也没 finalize → 模型可能在做 free-form 总结
                // 注入 nudge 重新拉回工具流程
                if iteration >= 2 {
                    return Err(PlannerEngineError::Llm(
                        "Planner produced text without using tools twice in a row. Aborting."
                            .into(),
                    ));
                }
                messages.push(Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: "Please continue using tools (list_directory / list_roles / \
                              propose_task / validate_plan / finalize_plan). Do not produce \
                              free-form text without a tool call."
                            .into(),
                    }],
                    cache_control: None,
                });
                continue;
            }

            // 依次执行每个 tool_use；finalize_plan 命中即终止
            let mut tool_results: Vec<ContentBlock> = Vec::new();
            let mut finalized: Option<PlannerOutput> = None;

            for (tool_use_id, name, input) in tool_uses {
                step_no += 1;
                let args_str = serde_json::to_string(&input).unwrap_or_default();
                self.persist_step(
                    session_id,
                    step_no,
                    "tool_call",
                    Some(&name),
                    Some(&args_str),
                    None,
                    None,
                    0,
                );
                self.emit_step(&PlannerStepEvent {
                    session_id: session_id.to_string(),
                    step_no,
                    kind: "tool_call".into(),
                    tool_name: Some(name.clone()),
                    tool_args: Some(args_str.clone()),
                    tool_result: None,
                    text_content: None,
                    error: None,
                });

                let result = executor.execute(&name, &input).await;

                step_no += 1;
                let result_content = result.output.content.clone();
                let kind = if result.output.is_error {
                    "tool_result_error"
                } else {
                    "tool_result"
                };
                self.persist_step(
                    session_id,
                    step_no,
                    kind,
                    Some(&name),
                    None,
                    Some(&result_content),
                    None,
                    0,
                );
                self.emit_step(&PlannerStepEvent {
                    session_id: session_id.to_string(),
                    step_no,
                    kind: kind.to_string(),
                    tool_name: Some(name.clone()),
                    tool_args: None,
                    tool_result: Some(result_content.clone()),
                    text_content: None,
                    error: None,
                });

                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id,
                    content: result_content,
                    is_error: result.output.is_error,
                });

                if let Some(out) = result.finalized {
                    finalized = Some(out);
                }
            }

            messages.push(Message {
                role: MessageRole::User,
                content: tool_results,
                cache_control: None,
            });

            if let Some(out) = finalized {
                // ---- DAG 一致性 guardrail (FR-06.3) ----
                // finalize_plan 内部已校验，但作为 belt-and-suspenders 再走一遍序列化-反序列化路径。
                let serialized = serde_json::to_string(&out).map_err(|e| {
                    PlannerEngineError::Llm(format!("Cannot serialize finalized plan: {e}"))
                })?;
                let revalidated = parse_and_validate(&serialized)
                    .map_err(PlannerEngineError::GuardrailFailed)?;
                return Ok(PlannerLoopOutcome {
                    session_id: session_id.to_string(),
                    output: revalidated,
                    total_steps: step_no as u32,
                    total_tokens,
                });
            }
        }
    }

    /// 走 stream_chat 拉响应：text_delta 通过 `planner-stream` 透传给前端
    /// （兼容现有 PlannerStreamPanel）；最终 LlmResponse 用于驱动主循环。
    async fn call_llm_streaming(
        &self,
        request: LlmRequest,
    ) -> Result<crate::llm::LlmResponse, PlannerEngineError> {
        let (tx, mut rx) = mpsc::channel::<StreamChunk>(256);
        let provider = self.provider.clone();
        let req = request.clone();

        let stream_handle =
            tokio::spawn(async move { provider.stream_chat(&req, tx).await });

        let app_handle = self.app_handle.clone();
        tokio::spawn(async move {
            while let Some(chunk) = rx.recv().await {
                if let StreamChunkKind::TextDelta = chunk.kind {
                    let _ = app_handle.emit(
                        "planner-stream",
                        serde_json::json!({
                            "kind": "text_delta",
                            "content": chunk.content,
                        }),
                    );
                }
            }
        });

        let response = stream_handle
            .await
            .map_err(|e| PlannerEngineError::Llm(format!("Stream task join error: {e}")))?
            .map_err(|e| PlannerEngineError::Llm(e.to_string()))?;
        Ok(response)
    }

    fn persist_step(
        &self,
        session_id: &str,
        step_no: i64,
        kind: &str,
        tool_name: Option<&str>,
        tool_args: Option<&str>,
        tool_result: Option<&str>,
        text_content: Option<&str>,
        tokens_used: i64,
    ) {
        let Some(db) = self.app_handle.try_state::<Database>() else {
            return;
        };
        let id = Uuid::new_v4().to_string();
        if let Err(e) = db.with_conn(|conn| {
            queries::insert_planner_step(
                conn,
                &id,
                session_id,
                step_no,
                kind,
                tool_name,
                tool_args,
                tool_result,
                text_content,
                tokens_used,
            )
        }) {
            tracing::warn!("insert_planner_step failed: {e}");
        }
    }

    fn emit_step(&self, payload: &PlannerStepEvent) {
        let _ = self.app_handle.emit("planner-step", payload);
    }

    fn emit_status(&self, session_id: &str, status: &str) {
        let _ = self.app_handle.emit(
            "planner-session-status",
            serde_json::json!({
                "session_id": session_id,
                "status": status,
            }),
        );
    }
}

/// FM-15 v2.2 (S4): 把 mission 已签约的合同条目读取成结构化形式 + 文本 dump。
///
/// - 文本 dump 拼到首条 prompt 让 LLM 看到；
/// - 结构化 `ContractGuardrail` 注入 PlannerState，由 `validate_plan` 做轻量
///   guardrail（exclusion 触碰 / scope 未覆盖）。
fn load_contract_payload(
    conn: &rusqlite::Connection,
    mission_id: &str,
) -> (String, ContractGuardrail) {
    let rows = conn
        .prepare(
            "SELECT ci.section, ci.text \
             FROM contract_items ci \
             JOIN mission_contracts mc ON mc.id = ci.contract_id \
             WHERE mc.mission_id = ? \
             ORDER BY \
               CASE ci.section \
                 WHEN 'scope' THEN 0 \
                 WHEN 'constraints' THEN 1 \
                 WHEN 'exclusions' THEN 2 \
                 WHEN 'assumptions' THEN 3 \
                 ELSE 4 END, \
               ci.created_at",
        )
        .and_then(|mut stmt| {
            stmt.query_map([mission_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()
        });

    let items = match rows {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!("load_contract_payload failed: {e}");
            return ("(contract not available)".into(), ContractGuardrail::default());
        }
    };
    if items.is_empty() {
        return ("(contract is empty)".into(), ContractGuardrail::default());
    }

    let mut guardrail = ContractGuardrail::default();
    let mut out = String::new();
    let mut last_section: Option<String> = None;
    for (section, text) in items {
        if last_section.as_deref() != Some(section.as_str()) {
            if last_section.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("### {}\n", section));
            last_section = Some(section.clone());
        }
        out.push_str(&format!("- {}\n", text));
        match section.as_str() {
            "scope" => guardrail.scope.push(text),
            "exclusions" => guardrail.exclusions.push(text),
            "constraints" => guardrail.constraints.push(text),
            "assumptions" => guardrail.assumptions.push(text),
            _ => {}
        }
    }
    (out, guardrail)
}
