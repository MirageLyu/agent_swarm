use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::mpsc;

use crate::llm::{ContentBlock, LlmProvider, LlmRequest, Message, MessageRole, StreamChunk, StreamChunkKind, ToolDefinition};

// ---------------------------------------------------------------------------
// Pre-flight mode system prompts
// ---------------------------------------------------------------------------

const SCENARIO_WALK_PROMPT: &str = r#"You are a requirements analyst helping a developer clarify their project requirements through scenario walk-through.

Your role:
- Walk through the user's requirement step by step, asking about user flows, data models, and edge cases
- For each decision point, provide 2-4 concrete choices as structured options
- When the user says "你决定" or "you decide", mark it as an agent decision and call add_contract_item with confidence="inferred"

CRITICAL RULES:
1. Ask ONE question per message. Never bundle multiple questions.
2. Keep your analysis brief (2-4 sentences), then present choices using the present_choices tool.
3. Call present_choices at most ONCE per message.

## Tool Usage
You MUST use the provided tools for structured output. Do NOT use text separators like ---CHOICES---.

- `present_choices`: Present 2-4 choices for ONE decision point. Set the `dimension` to the relevant contract section.
  Always include a "你决定" option as the last choice (let the agent decide based on best practices).
- `add_contract_item`: When the user confirms a decision, add it to the contract.
  Use confidence="confirmed" for explicit user choices, "tentative" for implied decisions, "inferred" for agent decisions.
- `suggest_sign`: When requirements are sufficiently clarified, suggest signing the contract.
- `switch_clarification_mode`: Suggest switching mode if the current approach is not effective.

Language: ALWAYS respond in Chinese. All choice labels, descriptions, and analysis text must be in Chinese. Do NOT include English translations."#;

const DEVILS_ADVOCATE_PROMPT: &str = r#"You are a devil's advocate reviewing a developer's requirements. Your job is to find gaps, ambiguities, and unstated assumptions.

Your role:
- Challenge the user's assumptions and find spec holes
- Ask about edge cases, error handling, and unstated requirements
- Identify what should be explicitly excluded to prevent scope creep

CRITICAL RULES:
1. Ask ONE question per message. Never bundle multiple questions.
2. Keep your challenge brief (2-4 sentences), then present choices using the present_choices tool.
3. Call present_choices at most ONCE per message.

## Tool Usage
You MUST use the provided tools for structured output. Do NOT use text separators like ---CHOICES---.

- `present_choices`: Present 2-4 choices for ONE issue. Set `dimension` to the relevant section.
- `add_contract_item`: Record confirmed decisions. Use appropriate confidence level.
- `suggest_sign`: Suggest signing when requirements are sufficiently clarified.

Language: ALWAYS respond in Chinese. All choice labels, descriptions, and analysis text must be in Chinese. Do NOT include English translations."#;

const RISK_HIGHLIGHTER_PROMPT: &str = r#"You are a risk analyst evaluating a software development task. Focus on the highest-impact risks.

Your role:
- Identify technical risks, dependency risks, and security risks
- Focus on ONE risk at a time, starting from the highest-impact risk
- For each risk, ask the user to confirm mitigations or prerequisites

CRITICAL RULES:
1. Present ONE risk per message. Never bundle multiple risks.
2. Keep your analysis brief (2-4 sentences), then present choices using the present_choices tool.
3. Call present_choices at most ONCE per message.

## Tool Usage
You MUST use the provided tools for structured output. Do NOT use text separators like ---CHOICES---.

- `present_choices`: Present 2-4 choices for ONE risk. Set `dimension` to the relevant section (usually "assumptions").
- `add_contract_item`: Record confirmed mitigations and assumptions.
- `suggest_sign`: Suggest signing when risk coverage is sufficient.

Language: ALWAYS respond in Chinese. All choice labels, descriptions, and analysis text must be in Chinese. Do NOT include English translations."#;

const PLANNER_TIMEOUT: Duration = Duration::from_secs(90);

const PLANNER_SYSTEM_PROMPT: &str = r#"You are a task planner for a software development project. Given a high-level requirement description, decompose it into concrete, independently executable sub-tasks.

Output ONLY a valid JSON object with this structure:
{
  "mission_title": "concise title for the overall mission",
  "tasks": [
    {
      "id": "T1",
      "title": "short task title",
      "description": "detailed description of what this task should accomplish",
      "complexity": "low|medium|high",
      "depends_on": []
    }
  ]
}

Rules:
- Each task should be completable by a single AI agent
- IDs must be sequential: T1, T2, T3...
- depends_on references must be valid task IDs defined earlier
- No circular dependencies
- Aim for 3-10 tasks depending on complexity
- Distinguish frontend/backend/test tasks where applicable
- Order dependencies logically (data model before API, API before UI)"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerOutput {
    pub mission_title: String,
    pub tasks: Vec<PlannerTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub complexity: String,
    pub depends_on: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlannerError {
    #[error("LLM call failed: {0}")]
    LlmError(String),
    #[error("JSON parse error: {0}")]
    JsonParseError(String),
    #[error("Empty task list")]
    EmptyTaskList,
    #[error("Missing field: {0}")]
    MissingField(String),
    #[error("Invalid complexity value: {0}")]
    InvalidComplexity(String),
    #[error("Invalid dependency reference: {task_id} depends on non-existent {ref_id}")]
    InvalidDependencyRef { task_id: String, ref_id: String },
    #[error("Self dependency: {0}")]
    SelfDependency(String),
    #[error("Cyclic dependency detected")]
    CyclicDependency,
    #[error("API key not configured. Please configure your API key in Settings first.")]
    ApiKeyNotConfigured,
}

pub fn parse_and_validate(json_str: &str) -> Result<PlannerOutput, PlannerError> {
    let json_str = extract_json(json_str);

    let output: PlannerOutput = serde_json::from_str(json_str)
        .map_err(|e| PlannerError::JsonParseError(e.to_string()))?;

    if output.mission_title.trim().is_empty() {
        return Err(PlannerError::MissingField("mission_title".into()));
    }

    validate_task_graph(&output.tasks)?;

    Ok(output)
}

/// Validate a list of tasks as a valid DAG.
/// Checks: non-empty, valid complexity, valid dependency refs, no self-deps, no cycles.
/// Shared by planner JSON parsing and mission template import.
pub fn validate_task_graph(tasks: &[PlannerTask]) -> Result<(), PlannerError> {
    if tasks.is_empty() {
        return Err(PlannerError::EmptyTaskList);
    }

    let valid_complexities = ["low", "medium", "high"];
    let task_ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();

    for task in tasks {
        if task.title.trim().is_empty() {
            return Err(PlannerError::MissingField(format!(
                "title for task {}",
                task.id
            )));
        }

        if !valid_complexities.contains(&task.complexity.as_str()) {
            return Err(PlannerError::InvalidComplexity(task.complexity.clone()));
        }

        for dep in &task.depends_on {
            if dep == &task.id {
                return Err(PlannerError::SelfDependency(task.id.clone()));
            }
            if !task_ids.contains(dep.as_str()) {
                return Err(PlannerError::InvalidDependencyRef {
                    task_id: task.id.clone(),
                    ref_id: dep.clone(),
                });
            }
        }
    }

    detect_cycles(tasks)?;

    Ok(())
}

fn extract_json(s: &str) -> &str {
    let trimmed = s.trim();
    if trimmed.starts_with('{') {
        return trimmed;
    }
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(newline) = after.find('\n') {
            let after = &after[newline + 1..];
            if let Some(end) = after.find("```") {
                return after[..end].trim();
            }
        }
    }
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }
    trimmed
}

fn detect_cycles(tasks: &[PlannerTask]) -> Result<(), PlannerError> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for task in tasks {
        adj.entry(task.id.as_str()).or_default();
        for dep in &task.depends_on {
            adj.entry(dep.as_str()).or_default().push(task.id.as_str());
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let mut colors: HashMap<&str, Color> = tasks.iter().map(|t| (t.id.as_str(), Color::White)).collect();

    fn dfs<'a>(
        node: &'a str,
        adj: &HashMap<&str, Vec<&'a str>>,
        colors: &mut HashMap<&'a str, Color>,
    ) -> bool {
        colors.insert(node, Color::Gray);
        if let Some(neighbors) = adj.get(node) {
            for &next in neighbors {
                match colors.get(next) {
                    Some(Color::Gray) => return true,
                    Some(Color::White) => {
                        if dfs(next, adj, colors) {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
        }
        colors.insert(node, Color::Black);
        false
    }

    let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    for &id in &ids {
        if colors.get(id) == Some(&Color::White) {
            if dfs(id, &adj, &mut colors) {
                return Err(PlannerError::CyclicDependency);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct PlannerStreamEvent {
    kind: String,
    content: String,
}

fn emit_planner_event(app: &tauri::AppHandle, kind: &str, content: &str) {
    let _ = app.emit(
        "planner-stream",
        PlannerStreamEvent {
            kind: kind.to_string(),
            content: content.to_string(),
        },
    );
}

pub fn emit_planner_event_pub(app: &tauri::AppHandle, kind: &str, content: &str) {
    emit_planner_event(app, kind, content);
}

async fn stream_planner_call(
    provider: Arc<dyn LlmProvider>,
    request: &LlmRequest,
    app: &tauri::AppHandle,
) -> Result<String, PlannerError> {
    let (tx, mut rx) = mpsc::channel::<StreamChunk>(256);

    let provider_clone = provider.clone();
    let request_clone = LlmRequest {
        model: request.model.clone(),
        system: request.system.clone(),
        messages: request.messages.clone(),
        tools: request.tools.clone(),
        max_tokens: request.max_tokens,
    };

    let stream_handle = tokio::spawn(async move {
        provider_clone.stream_chat(&request_clone, tx).await
    });

    let app_clone = app.clone();
    let mut full_text = String::new();

    // Forward stream chunks as Tauri events
    while let Some(chunk) = rx.recv().await {
        match chunk.kind {
            StreamChunkKind::ReasoningDelta => {
                emit_planner_event(&app_clone, "reasoning_delta", &chunk.content);
            }
            StreamChunkKind::TextDelta => {
                full_text.push_str(&chunk.content);
                emit_planner_event(&app_clone, "text_delta", &chunk.content);
            }
            StreamChunkKind::MessageStop => {
                // Will emit "done" after parsing
            }
            _ => {}
        }
    }

    let response = tokio::time::timeout(Duration::from_secs(5), stream_handle)
        .await
        .map_err(|_| PlannerError::LlmError("Stream handle timed out".into()))?
        .map_err(|e| PlannerError::LlmError(format!("Stream task failed: {e}")))?
        .map_err(|e| PlannerError::LlmError(e.to_string()))?;

    // If full_text is empty, extract from response content blocks
    if full_text.is_empty() {
        full_text = response
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
    }

    Ok(full_text)
}

pub async fn call_planner(
    provider: Arc<dyn LlmProvider>,
    model: &str,
    description: &str,
    app: &tauri::AppHandle,
) -> Result<PlannerOutput, PlannerError> {
    let messages = vec![Message {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: description.to_string(),
        }],
    }];

    let request = LlmRequest {
        model: model.to_string(),
        system: Some(PLANNER_SYSTEM_PROMPT.to_string()),
        messages: messages.clone(),
        tools: vec![],
        max_tokens: 4096,
    };

    tracing::info!("Planner: calling LLM (streaming) model={model}");
    let text = tokio::time::timeout(
        PLANNER_TIMEOUT,
        stream_planner_call(provider.clone(), &request, app),
    )
    .await
    .map_err(|_| PlannerError::LlmError("Planning timed out, please retry".into()))?
    .map_err(|e| PlannerError::LlmError(e.to_string()))?;
    tracing::info!("Planner: stream complete, parsing output");

    match parse_and_validate(&text) {
        Ok(output) => {
            emit_planner_event(app, "done", "");
            Ok(output)
        }
        Err(first_err) => {
            tracing::warn!("Planner first attempt failed: {first_err}, retrying with error feedback");
            emit_planner_event(app, "error", &format!("Parse error, retrying: {first_err}"));

            let retry_messages = vec![
                Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: description.to_string(),
                    }],
                },
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::Text {
                        text: text.clone(),
                    }],
                },
                Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: format!(
                            "Your previous output had an error: {first_err}\n\
                             Please fix it and output ONLY a valid JSON object."
                        ),
                    }],
                },
            ];

            let retry_request = LlmRequest {
                model: model.to_string(),
                system: Some(PLANNER_SYSTEM_PROMPT.to_string()),
                messages: retry_messages,
                tools: vec![],
                max_tokens: 4096,
            };

            let retry_text = tokio::time::timeout(
                PLANNER_TIMEOUT,
                stream_planner_call(provider, &retry_request, app),
            )
            .await
            .map_err(|_| PlannerError::LlmError("Planning retry timed out".into()))?
            .map_err(|e| PlannerError::LlmError(e.to_string()))?;

            let result = parse_and_validate(&retry_text)?;
            emit_planner_event(app, "done", "");
            Ok(result)
        }
    }
}

// ---------------------------------------------------------------------------
// Pre-flight streaming
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PreflightStreamEvent {
    pub session_id: String,
    pub chunk: PreflightChunk,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreflightChunk {
    pub kind: String,
    pub content: String,
}

fn emit_preflight_event(app: &tauri::AppHandle, session_id: &str, kind: &str, content: &str) {
    let _ = app.emit(
        "preflight-stream",
        PreflightStreamEvent {
            session_id: session_id.to_string(),
            chunk: PreflightChunk {
                kind: kind.to_string(),
                content: content.to_string(),
            },
        },
    );
}

pub fn emit_preflight_event_pub(app: &tauri::AppHandle, session_id: &str, kind: &str, content: &str) {
    emit_preflight_event(app, session_id, kind, content);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightChoice {
    pub id: String,
    pub label: String,
    pub contract_impact: Option<ContractImpact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractImpact {
    pub section: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightResponse {
    pub text: String,
    pub choices: Vec<PreflightChoice>,
    #[serde(default)]
    pub tool_calls: Vec<PreflightToolCall>,
    #[serde(default)]
    pub fallback_used: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Pre-flight tool argument types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresentChoicesArgs {
    pub question: String,
    pub dimension: String,
    pub choices: Vec<ToolChoice>,
    pub allow_multiple: Option<bool>,
    pub allow_custom: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolChoice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub impact: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddContractItemArgs {
    pub section: String,
    pub item: String,
    pub confidence: String,
    pub source_round: Option<u32>,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateContractItemArgs {
    pub item_id: String,
    pub new_content: String,
    pub new_confidence: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessAssessment {
    pub scope_completeness: f64,
    pub constraints_completeness: f64,
    pub risk_coverage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestSignArgs {
    pub readiness_assessment: ReadinessAssessment,
    pub remaining_concerns: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchClarificationModeArgs {
    pub mode: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PreflightAction {
    PresentChoices { id: String, args: PresentChoicesArgs },
    AddContractItem { id: String, args: AddContractItemArgs },
    UpdateContractItem { id: String, args: UpdateContractItemArgs },
    SuggestSign { id: String, args: SuggestSignArgs },
    SwitchClarificationMode { id: String, args: SwitchClarificationModeArgs },
}

// ---------------------------------------------------------------------------
// Pre-flight tool definitions (OpenAI function calling format)
// ---------------------------------------------------------------------------

pub fn preflight_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "present_choices".into(),
            description: "Present structured choices to the user for a single decision point.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string"},
                    "dimension": {"type": "string", "enum": ["scope", "constraints", "exclusions", "assumptions"]},
                    "choices": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "label": {"type": "string"},
                                "description": {"type": "string"},
                                "impact": {"type": "string"}
                            },
                            "required": ["id", "label"]
                        },
                        "minItems": 2
                    },
                    "allow_multiple": {"type": "boolean"},
                    "allow_custom": {"type": "boolean"}
                },
                "required": ["question", "dimension", "choices"]
            }),
        },
        ToolDefinition {
            name: "add_contract_item".into(),
            description: "Add a confirmed or tentative item to the mission contract.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "section": {"type": "string", "enum": ["scope", "constraints", "exclusions", "assumptions"]},
                    "item": {"type": "string"},
                    "confidence": {"type": "string", "enum": ["confirmed", "tentative", "inferred"]},
                    "source_round": {"type": "integer"},
                    "rationale": {"type": "string"}
                },
                "required": ["section", "item", "confidence"]
            }),
        },
        ToolDefinition {
            name: "update_contract_item".into(),
            description: "Update an existing contract item.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "item_id": {"type": "string"},
                    "new_content": {"type": "string"},
                    "new_confidence": {"type": "string", "enum": ["confirmed", "tentative", "inferred"]},
                    "reason": {"type": "string"}
                },
                "required": ["item_id", "new_content"]
            }),
        },
        ToolDefinition {
            name: "suggest_sign".into(),
            description: "Suggest signing the contract when requirements are sufficiently clarified.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "readiness_assessment": {
                        "type": "object",
                        "properties": {
                            "scope_completeness": {"type": "number"},
                            "constraints_completeness": {"type": "number"},
                            "risk_coverage": {"type": "number"}
                        },
                        "required": ["scope_completeness", "constraints_completeness", "risk_coverage"]
                    },
                    "remaining_concerns": {"type": "array", "items": {"type": "string"}},
                    "summary": {"type": "string"}
                },
                "required": ["readiness_assessment", "remaining_concerns", "summary"]
            }),
        },
        ToolDefinition {
            name: "switch_clarification_mode".into(),
            description: "Switch to a different clarification mode.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {"type": "string", "enum": ["scenario_walkthrough", "devils_advocate", "risk_tagging"]},
                    "reason": {"type": "string"}
                },
                "required": ["mode"]
            }),
        },
    ]
}

/// Parse tool_calls from LLM response ContentBlocks into typed actions.
pub fn parse_tool_calls(content: &[ContentBlock]) -> (Vec<PreflightAction>, Vec<PreflightToolCall>) {
    let mut actions = Vec::new();
    let mut raw_calls = Vec::new();

    for block in content {
        if let ContentBlock::ToolUse { id, name, input } = block {
            raw_calls.push(PreflightToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: input.clone(),
            });

            match name.as_str() {
                "present_choices" => {
                    match serde_json::from_value::<PresentChoicesArgs>(input.clone()) {
                        Ok(args) => actions.push(PreflightAction::PresentChoices {
                            id: id.clone(),
                            args,
                        }),
                        Err(e) => tracing::warn!("Failed to parse present_choices args: {e}"),
                    }
                }
                "add_contract_item" => {
                    match serde_json::from_value::<AddContractItemArgs>(input.clone()) {
                        Ok(args) => actions.push(PreflightAction::AddContractItem {
                            id: id.clone(),
                            args,
                        }),
                        Err(e) => tracing::warn!("Failed to parse add_contract_item args: {e}"),
                    }
                }
                "update_contract_item" => {
                    match serde_json::from_value::<UpdateContractItemArgs>(input.clone()) {
                        Ok(args) => actions.push(PreflightAction::UpdateContractItem {
                            id: id.clone(),
                            args,
                        }),
                        Err(e) => tracing::warn!("Failed to parse update_contract_item args: {e}"),
                    }
                }
                "suggest_sign" => {
                    match serde_json::from_value::<SuggestSignArgs>(input.clone()) {
                        Ok(args) => actions.push(PreflightAction::SuggestSign {
                            id: id.clone(),
                            args,
                        }),
                        Err(e) => tracing::warn!("Failed to parse suggest_sign args: {e}"),
                    }
                }
                "switch_clarification_mode" => {
                    match serde_json::from_value::<SwitchClarificationModeArgs>(input.clone()) {
                        Ok(args) => actions.push(PreflightAction::SwitchClarificationMode {
                            id: id.clone(),
                            args,
                        }),
                        Err(e) => tracing::warn!("Failed to parse switch_clarification_mode args: {e}"),
                    }
                }
                other => {
                    tracing::warn!("Unknown preflight tool: {other}");
                }
            }
        }
    }

    (actions, raw_calls)
}

/// Convenience wrapper: parse tool_calls from a PreflightResponse's raw tool_call data.
pub fn parse_tool_calls_from_response(
    response: &PreflightResponse,
) -> (Vec<PreflightAction>, Vec<PreflightToolCall>) {
    if response.tool_calls.is_empty() {
        return (vec![], vec![]);
    }

    let content_blocks: Vec<ContentBlock> = response
        .tool_calls
        .iter()
        .map(|tc| ContentBlock::ToolUse {
            id: tc.id.clone(),
            name: tc.name.clone(),
            input: tc.arguments.clone(),
        })
        .collect();

    parse_tool_calls(&content_blocks)
}

/// Convert PresentChoicesArgs into PreflightChoice vec for the frontend.
/// IDs are normalized to sequential letters (A, B, C, …) for consistent UI display.
pub fn tool_choices_to_preflight_choices(args: &PresentChoicesArgs) -> Vec<PreflightChoice> {
    args.choices
        .iter()
        .enumerate()
        .map(|(i, tc)| {
            let id = String::from((b'A' + i as u8) as char);
            let label = if let Some(desc) = &tc.description {
                if desc.is_empty() {
                    tc.label.clone()
                } else {
                    format!("{} — {}", tc.label, desc)
                }
            } else {
                tc.label.clone()
            };
            PreflightChoice {
                id,
                label,
                contract_impact: None,
            }
        })
        .collect()
}

fn parse_preflight_response(raw: &str) -> PreflightResponse {
    let separator = "---CHOICES---";
    if let Some(idx) = raw.find(separator) {
        let text = raw[..idx].trim().to_string();
        let json_part = raw[idx + separator.len()..].trim();
        if let Ok(choices) = serde_json::from_str::<Vec<PreflightChoice>>(json_part) {
            if !choices.is_empty() {
                return PreflightResponse { text, choices, tool_calls: vec![], fallback_used: "text".into() };
            }
        }
        let json_part = extract_json(json_part);
        if let Ok(choices) = serde_json::from_str::<Vec<PreflightChoice>>(json_part) {
            if !choices.is_empty() {
                return PreflightResponse { text, choices, tool_calls: vec![], fallback_used: "text".into() };
            }
        }
        let fallback = extract_choices_from_markdown(&text);
        if !fallback.is_empty() {
            return PreflightResponse { text, choices: fallback, tool_calls: vec![], fallback_used: "markdown".into() };
        }
        PreflightResponse { text, choices: vec![], tool_calls: vec![], fallback_used: "none".into() }
    } else {
        let text = raw.trim().to_string();
        let fallback = extract_choices_from_markdown(&text);
        let fb = if fallback.is_empty() { "none" } else { "markdown" };
        PreflightResponse { text, choices: fallback, tool_calls: vec![], fallback_used: fb.into() }
    }
}

/// Fallback: extract choices from Markdown patterns like:
///   - **A. description** — detail
///   - **A.** description
///   - **A)** description
///   - A. description (at line start)
fn extract_choices_from_markdown(text: &str) -> Vec<PreflightChoice> {
    use std::collections::BTreeMap;

    let mut choices: BTreeMap<String, String> = BTreeMap::new();

    for line in text.lines() {
        let trimmed = line.trim().trim_start_matches('-').trim_start_matches('*').trim();

        // Pattern 1: **A. label** or **A) label** or **A、label**
        if let Some(rest) = trimmed.strip_prefix("**") {
            if let Some((id_part, after)) = split_choice_id(rest) {
                let label = after
                    .trim_end_matches("**")
                    .trim_start_matches("**")
                    .trim()
                    .trim_start_matches('—')
                    .trim_start_matches('-')
                    .trim()
                    .to_string();
                if !label.is_empty() {
                    choices.entry(id_part).or_insert(label);
                }
            }
            continue;
        }

        // Pattern 2: A. label or A) label (plain, line starts with single letter/word)
        if let Some((id_part, after)) = split_choice_id(trimmed) {
            let label = after.trim().to_string();
            if !label.is_empty() && label.len() > 2 {
                choices.entry(id_part).or_insert(label);
            }
        }
    }

    choices
        .into_iter()
        .map(|(id, label)| PreflightChoice {
            id: id.clone(),
            label,
            contract_impact: None,
        })
        .collect()
}

/// Try to split "A. rest" or "A) rest" or "A、rest" into (id, rest).
fn split_choice_id(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    if bytes.is_empty() { return None; }

    // Find the id part: 1-2 uppercase ASCII chars
    let mut id_end = 0;
    while id_end < bytes.len() && id_end < 3 && bytes[id_end].is_ascii_alphanumeric() {
        id_end += 1;
    }
    if id_end == 0 { return None; }

    let after_id = &s[id_end..];
    // Must be followed by a delimiter: . ) 、 :
    let rest = if let Some(r) = after_id.strip_prefix('.')
        .or_else(|| after_id.strip_prefix(')'))
        .or_else(|| after_id.strip_prefix(':'))
    {
        r
    } else if let Some(r) = after_id.strip_prefix('、') {
        r
    } else {
        return None;
    };

    let id = s[..id_end].to_uppercase();
    // Only accept single-letter or two-char IDs
    if id.len() > 2 { return None; }
    Some((id, rest))
}

fn get_mode_prompt(mode: &str) -> &'static str {
    match mode {
        "scenario_walk" => SCENARIO_WALK_PROMPT,
        "devils_advocate" => DEVILS_ADVOCATE_PROMPT,
        "risk_highlighter" => RISK_HIGHLIGHTER_PROMPT,
        _ => SCENARIO_WALK_PROMPT,
    }
}

pub async fn preflight_chat(
    provider: Arc<dyn LlmProvider>,
    model: &str,
    mode: &str,
    history: Vec<Message>,
    session_id: &str,
    app: &tauri::AppHandle,
) -> Result<PreflightResponse, PlannerError> {
    let base_prompt = get_mode_prompt(mode);

    let user_rounds = history.iter().filter(|m| m.role == MessageRole::User).count();
    let convergence = if user_rounds >= 8 {
        "\n\nIMPORTANT: The clarification has been going on for a while. You MUST now call suggest_sign to recommend the user sign the Contract. Do NOT ask new questions."
    } else if user_rounds >= 5 {
        "\n\nNOTE: You are in the later stage of clarification. Start wrapping up — only ask about critical remaining gaps. If the main areas are covered, call suggest_sign."
    } else {
        ""
    };

    let system_prompt = format!(
        "{base_prompt}\n\nCurrent round: {user_rounds} (of user messages so far).{convergence}"
    );

    let tools = preflight_tools();

    let request = LlmRequest {
        model: model.to_string(),
        system: Some(system_prompt),
        messages: history,
        tools,
        max_tokens: 4096,
    };

    tracing::info!("Preflight: calling LLM (streaming) mode={mode} model={model}");
    // NOTE: "start" event is emitted by the caller (commands/preflight.rs)
    // so continuation calls don't re-trigger the loading state.

    let (tx, mut rx) = mpsc::channel::<StreamChunk>(256);
    let provider_clone = provider.clone();
    let request_clone = LlmRequest {
        model: request.model.clone(),
        system: request.system.clone(),
        messages: request.messages.clone(),
        tools: request.tools.clone(),
        max_tokens: request.max_tokens,
    };

    let stream_handle = tokio::spawn(async move {
        provider_clone.stream_chat(&request_clone, tx).await
    });

    let app_clone = app.clone();
    let sid = session_id.to_string();
    let mut full_text = String::new();

    while let Some(chunk) = rx.recv().await {
        match chunk.kind {
            StreamChunkKind::TextDelta => {
                full_text.push_str(&chunk.content);
                emit_preflight_event(&app_clone, &sid, "text_delta", &chunk.content);
            }
            StreamChunkKind::ReasoningDelta => {
                emit_preflight_event(&app_clone, &sid, "reasoning_delta", &chunk.content);
            }
            StreamChunkKind::MessageStop => {}
            _ => {}
        }
    }

    let response = tokio::time::timeout(Duration::from_secs(5), stream_handle)
        .await
        .map_err(|_| PlannerError::LlmError("Preflight stream handle timed out".into()))?
        .map_err(|e| PlannerError::LlmError(format!("Preflight stream task failed: {e}")))?
        .map_err(|e| PlannerError::LlmError(e.to_string()))?;

    if full_text.is_empty() {
        full_text = response
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
    }

    // Check for tool_calls in the response
    let has_tool_calls = response.content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }));

    let (choices, tool_calls, fallback_used) = if has_tool_calls {
        let (actions, raw_calls) = parse_tool_calls(&response.content);

        let mut choices = Vec::new();
        for action in &actions {
            if let PreflightAction::PresentChoices { args, .. } = action {
                choices = tool_choices_to_preflight_choices(args);
                // When LLM returned only tool_calls with no text, use the question as text
                if full_text.trim().is_empty() {
                    full_text = args.question.clone();
                }
            }
        }

        // Generate summary text when LLM returned only non-interactive tool_calls
        if full_text.trim().is_empty() {
            let mut summaries = Vec::new();
            for action in &actions {
                match action {
                    PreflightAction::AddContractItem { args, .. } => {
                        let section_label = match args.section.as_str() {
                            "scope" => "范围",
                            "constraints" => "约束",
                            "exclusions" => "排除项",
                            "assumptions" => "假设",
                            _ => &args.section,
                        };
                        summaries.push(format!("已将「{}」记录到合同的 **{}** 区块。", args.item, section_label));
                    }
                    PreflightAction::UpdateContractItem { args, .. } => {
                        summaries.push(format!("已更新合同条目为：「{}」。", args.new_content));
                    }
                    PreflightAction::SuggestSign { args, .. } => {
                        summaries.push(args.summary.clone());
                    }
                    PreflightAction::SwitchClarificationMode { args, .. } => {
                        let mode_label = match args.mode.as_str() {
                            "devils_advocate" => "魔鬼代言人",
                            "risk_tagging" => "风险标记",
                            _ => "场景走查",
                        };
                        summaries.push(format!("已切换到「{}」模式。", mode_label));
                    }
                    _ => {}
                }
            }
            if !summaries.is_empty() {
                full_text = summaries.join("\n\n");
            }
        }

        let tool_names: Vec<&str> = raw_calls.iter().map(|tc| tc.name.as_str()).collect();
        tracing::info!(
            tool_calls_count = raw_calls.len(),
            tool_names = tool_names.join(","),
            "preflight tool_calls parsed"
        );

        (choices, raw_calls, "none".to_string())
    } else {
        // Three-layer fallback
        let parsed = parse_preflight_response(&full_text);
        let fallback = if full_text.contains("---CHOICES---") && !parsed.choices.is_empty() {
            "text"
        } else if !parsed.choices.is_empty() {
            "markdown"
        } else {
            "none"
        };
        tracing::info!(fallback_used = fallback, "preflight using text fallback");
        (parsed.choices, vec![], fallback.to_string())
    };

    let result = PreflightResponse {
        text: full_text,
        choices,
        tool_calls,
        fallback_used,
    };

    // NOTE: "done" event is now emitted by the caller (commands/preflight.rs)
    // so it can include belief_state data.

    tracing::info!(
        choices_count = result.choices.len(),
        tool_calls_count = result.tool_calls.len(),
        fallback_used = %result.fallback_used,
        "preflight stream complete"
    );

    Ok(result)
}

// ---------------------------------------------------------------------------
// Contract-aware planner prompt
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractData {
    pub scope: Vec<String>,
    pub constraints: Vec<String>,
    pub exclusions: Vec<String>,
    pub assumptions: Vec<String>,
    pub budget_usd: Option<f64>,
    pub quality_threshold: Option<f64>,
    pub max_duration_hours: Option<f64>,
}

pub fn build_contract_aware_planner_prompt(contract: &ContractData) -> String {
    let mut prompt = String::from(PLANNER_SYSTEM_PROMPT);
    prompt.push_str("\n\n--- MISSION CONTRACT ---\n\n");

    if !contract.scope.is_empty() {
        prompt.push_str("## Scope (MUST implement):\n");
        for item in &contract.scope {
            prompt.push_str(&format!("- {item}\n"));
        }
        prompt.push('\n');
    }

    if !contract.constraints.is_empty() {
        prompt.push_str("## Constraints (Agent decisions, follow these):\n");
        for item in &contract.constraints {
            prompt.push_str(&format!("- {item}\n"));
        }
        prompt.push('\n');
    }

    if !contract.exclusions.is_empty() {
        prompt.push_str("## Exclusions (DO NOT implement any of these):\n");
        for item in &contract.exclusions {
            prompt.push_str(&format!("- {item}\n"));
        }
        prompt.push('\n');
    }

    if !contract.assumptions.is_empty() {
        prompt.push_str("## Assumptions (Confirmed environment facts):\n");
        for item in &contract.assumptions {
            prompt.push_str(&format!("- {item}\n"));
        }
        prompt.push('\n');
    }

    if let Some(budget) = contract.budget_usd {
        prompt.push_str(&format!("Budget limit: ${budget:.2}\n"));
    }
    if let Some(qt) = contract.quality_threshold {
        prompt.push_str(&format!("Quality threshold: {qt}/10 — ensure tasks include thorough testing.\n"));
    }
    if let Some(dur) = contract.max_duration_hours {
        prompt.push_str(&format!("Max duration: {dur} hours — keep task count proportional.\n"));
    }

    prompt.push_str("\nGenerate a Task DAG that covers ALL scope items, respects ALL constraints, and excludes ALL exclusion items.\n");
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ut01_1_valid_json_no_deps() {
        let json = r#"{
            "mission_title": "Build auth system",
            "tasks": [
                {"id": "T1", "title": "Design DB schema", "description": "Create user table", "complexity": "low", "depends_on": []},
                {"id": "T2", "title": "Implement API", "description": "REST endpoints", "complexity": "medium", "depends_on": []},
                {"id": "T3", "title": "Write tests", "description": "Unit tests", "complexity": "low", "depends_on": []}
            ]
        }"#;
        let result = parse_and_validate(json).unwrap();
        assert_eq!(result.tasks.len(), 3);
    }

    #[test]
    fn ut01_2_valid_dependencies() {
        let json = r#"{
            "mission_title": "Build feature",
            "tasks": [
                {"id": "T1", "title": "Task 1", "description": "d", "complexity": "low", "depends_on": []},
                {"id": "T2", "title": "Task 2", "description": "d", "complexity": "medium", "depends_on": ["T1"]},
                {"id": "T3", "title": "Task 3", "description": "d", "complexity": "high", "depends_on": ["T1", "T2"]}
            ]
        }"#;
        let result = parse_and_validate(json).unwrap();
        assert_eq!(result.tasks[2].depends_on, vec!["T1", "T2"]);
    }

    #[test]
    fn ut01_3_cyclic_dependency() {
        let json = r#"{
            "mission_title": "Test",
            "tasks": [
                {"id": "T1", "title": "A", "description": "d", "complexity": "low", "depends_on": ["T2"]},
                {"id": "T2", "title": "B", "description": "d", "complexity": "low", "depends_on": ["T1"]}
            ]
        }"#;
        let err = parse_and_validate(json).unwrap_err();
        assert!(matches!(err, PlannerError::CyclicDependency));
    }

    #[test]
    fn ut01_4_invalid_dependency_ref() {
        let json = r#"{
            "mission_title": "Test",
            "tasks": [
                {"id": "T1", "title": "A", "description": "d", "complexity": "low", "depends_on": []},
                {"id": "T2", "title": "B", "description": "d", "complexity": "low", "depends_on": ["T99"]}
            ]
        }"#;
        let err = parse_and_validate(json).unwrap_err();
        assert!(matches!(err, PlannerError::InvalidDependencyRef { .. }));
    }

    #[test]
    fn ut01_5_self_dependency() {
        let json = r#"{
            "mission_title": "Test",
            "tasks": [
                {"id": "T1", "title": "A", "description": "d", "complexity": "low", "depends_on": ["T1"]}
            ]
        }"#;
        let err = parse_and_validate(json).unwrap_err();
        assert!(matches!(err, PlannerError::SelfDependency(_)));
    }

    #[test]
    fn ut01_6_empty_task_list() {
        let json = r#"{"mission_title": "Test", "tasks": []}"#;
        let err = parse_and_validate(json).unwrap_err();
        assert!(matches!(err, PlannerError::EmptyTaskList));
    }

    #[test]
    fn ut01_7_non_json() {
        let err = parse_and_validate("这不是JSON").unwrap_err();
        assert!(matches!(err, PlannerError::JsonParseError(_)));
    }

    #[test]
    fn ut01_8_missing_title() {
        let json = r#"{
            "mission_title": "Test",
            "tasks": [{"id": "T1", "title": "", "description": "d", "complexity": "low", "depends_on": []}]
        }"#;
        let err = parse_and_validate(json).unwrap_err();
        assert!(matches!(err, PlannerError::MissingField(_)));
    }

    #[test]
    fn ut01_9_invalid_complexity() {
        let json = r#"{
            "mission_title": "Test",
            "tasks": [{"id": "T1", "title": "A", "description": "d", "complexity": "extreme", "depends_on": []}]
        }"#;
        let err = parse_and_validate(json).unwrap_err();
        assert!(matches!(err, PlannerError::InvalidComplexity(_)));
    }

    #[test]
    fn ut01_10_large_dag() {
        let mut tasks = Vec::new();
        for i in 1..=30 {
            let deps: Vec<String> = if i > 1 {
                vec![format!("T{}", i - 1)]
            } else {
                vec![]
            };
            tasks.push(serde_json::json!({
                "id": format!("T{i}"),
                "title": format!("Task {i}"),
                "description": "desc",
                "complexity": "medium",
                "depends_on": deps,
            }));
        }
        let json = serde_json::json!({
            "mission_title": "Large project",
            "tasks": tasks,
        });
        let result = parse_and_validate(&json.to_string()).unwrap();
        assert_eq!(result.tasks.len(), 30);
    }

    #[test]
    fn extract_json_from_markdown_fence() {
        let input = "Here's the plan:\n```json\n{\"mission_title\": \"t\", \"tasks\": [{\"id\": \"T1\", \"title\": \"a\", \"description\": \"d\", \"complexity\": \"low\", \"depends_on\": []}]}\n```";
        let result = parse_and_validate(input).unwrap();
        assert_eq!(result.tasks.len(), 1);
    }
}
