use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::mpsc;

use crate::llm::{ContentBlock, LlmProvider, LlmRequest, Message, MessageRole, StreamChunk, StreamChunkKind};

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

    if output.tasks.is_empty() {
        return Err(PlannerError::EmptyTaskList);
    }

    let valid_complexities = ["low", "medium", "high"];
    let task_ids: HashSet<&str> = output.tasks.iter().map(|t| t.id.as_str()).collect();

    for task in &output.tasks {
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

    detect_cycles(&output.tasks)?;

    Ok(output)
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
