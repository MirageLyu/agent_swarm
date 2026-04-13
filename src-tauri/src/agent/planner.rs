use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::mpsc;

use crate::llm::{ContentBlock, LlmProvider, LlmRequest, Message, MessageRole, StreamChunk, StreamChunkKind, ToolDefinition, ModelCapabilities, CacheControl, TokenUsage};
use crate::agent::belief_state::{PreflightBeliefState, ConversationPhase, SlotStatus, default_slot_definitions};

// ---------------------------------------------------------------------------
// FM-10.3: Dynamic System Prompt — Static Prefix + Dynamic Suffix
// ---------------------------------------------------------------------------

/// Static prefix: role definition + dialogue strategy + tool usage + output format.
/// MUST remain byte-stable within a session for FM-10.4 caching.
const STATIC_PREFIX: &str = r#"你是 Miragenty 的 Pre-flight Planner Agent，负责通过多轮对话澄清需求，构建 Mission Contract（Scope / Constraints / Exclusions / Assumptions）。

# 对话策略
- 每轮聚焦 1 个维度，使用 present_choices 工具提供结构化选项
- 用户确认后立即用 add_contract_item 写入 Contract
- 随着澄清深入，减少开放式问题，增加确认式问题
- 永远不要使用 ---CHOICES--- 分隔符，所有结构化输出通过工具完成
- 在工具调用之外的文本用于向用户解释推理过程
- add_contract_item 必须包含 rationale 字段，说明为什么做出这个决策

# 工具使用规范
- present_choices: 需要用户选择时调用，每轮最多 1 次。始终包含一个"你决定"选项。
- add_contract_item: 用户确认后写入，标注 confidence（confirmed/tentative/inferred）和 rationale
- update_contract_item: 后续讨论推翻了之前的假设时使用，必须注明 reason
- suggest_sign: 仅在收敛分数 > 85% 或 phase=ReadyToSign 时使用
- switch_clarification_mode: 当前模式效率低下时切换

# 输出规范
- 文本部分使用中文，保持简洁专业
- 每条消息文本 ≤ 300 字，避免冗长解释
- 每轮只问一个问题，不要捆绑多个问题"#;

/// Token padding to ensure static prefix >= 1024 tokens for DashScope caching.
const STATIC_PADDING: &str = r#"

# 决策质量要求
- 每个写入 Contract 的条目必须有明确的来源：用户明确选择 (confirmed)、用户未反对的推断 (inferred)、或暂定共识 (tentative)
- 修改已有条目时，必须通过 update_contract_item 并注明修改原因
- 不要重复提问已经确认过的领域
- 参考下方的 Contract 状态和信念状态，避免重复已有内容"#;

/// Build the complete system prompt with static prefix + dynamic suffix.
/// The static prefix is byte-stable across rounds for caching (FM-10.4).
pub fn build_preflight_system_prompt(
    mode: &str,
    contract_items: &[(String, String, String)], // (section, text, confidence)
    belief_state: &PreflightBeliefState,
    rejected_alternatives: &[(String, u32, String)], // (description, round, reason)
    caps: &ModelCapabilities,
) -> String {
    let static_part = build_static_prefix(caps);
    let dynamic_part = build_dynamic_suffix(mode, contract_items, belief_state, rejected_alternatives);
    format!("{static_part}\n\n═══ __DYNAMIC_BOUNDARY__ ═══\n\n{dynamic_part}")
}

/// Static prefix portion — byte-stable within a session.
pub fn build_static_prefix(caps: &ModelCapabilities) -> String {
    let mut prefix = String::from(STATIC_PREFIX);
    prefix.push_str(STATIC_PADDING);

    if !caps.supports_thinking {
        prefix.push_str("\n\n# 推理过程\n在回复前，请先在 <analysis> 标签中进行内部分析，然后在标签外输出结论和工具调用。");
    }

    prefix
}

fn build_dynamic_suffix(
    mode: &str,
    contract_items: &[(String, String, String)],
    belief_state: &PreflightBeliefState,
    rejected_alternatives: &[(String, u32, String)],
) -> String {
    let mut sections = Vec::new();

    // § Mode guidance
    sections.push(render_mode_section(mode));

    // § Contract state
    sections.push(compact_contract_json(contract_items));

    // § Belief state
    sections.push(render_belief_state_section(belief_state));

    // § Convergence directive
    sections.push(get_convergence_directive(belief_state));

    // § Rejected alternatives (FM-10.6)
    if !rejected_alternatives.is_empty() {
        sections.push(render_rejected_alternatives(rejected_alternatives));
    }

    // § Round info
    sections.push(format!("# 当前轮次\n第 {} 轮", belief_state.round));

    sections.join("\n\n")
}

fn render_mode_section(mode: &str) -> String {
    match mode {
        "scenario_walk" => "# 当前模式：场景走查\n重点关注：\n- 通过具体用户旅程引导思考边界\n- 模拟真实使用流程，发现遗漏的异常路径\n- 针对每个功能点追问「如果用户这样做会怎样?」\n- 从正向场景到边界场景，逐步深入".to_string(),
        "devils_advocate" => "# 当前模式：魔鬼代言人\n重点关注：\n- 质疑每一个隐含假设，提出反例\n- 寻找需求中的模糊地带和矛盾点\n- 追问「如果不是这样呢?」\n- 挑战最乐观的估计，揭示潜在的范围蔓延".to_string(),
        "risk_highlighter" => "# 当前模式：风险标记\n重点关注：\n- 识别技术风险（性能瓶颈、复杂度）\n- 评估安全风险（权限、数据泄露）\n- 标记依赖风险（第三方服务、兼容性）\n- 按影响程度排序，每次只讨论一个风险".to_string(),
        _ => render_mode_section("scenario_walk"),
    }
}

/// Compact JSON representation of current contract items (FR-10.3.3).
pub fn compact_contract_json(items: &[(String, String, String)]) -> String {
    let mut scope = Vec::new();
    let mut constraints = Vec::new();
    let mut exclusions = Vec::new();
    let mut assumptions = Vec::new();

    for (section, text, confidence) in items {
        let entry = format!("{}({})", text, confidence);
        match section.as_str() {
            "scope" => scope.push(entry),
            "constraints" => constraints.push(entry),
            "exclusions" => exclusions.push(entry),
            "assumptions" => assumptions.push(entry),
            _ => {}
        }
    }

    // Truncate if > 20 items total
    let total = scope.len() + constraints.len() + exclusions.len() + assumptions.len();
    let truncated = if total > 20 {
        let truncate_vec = |v: &mut Vec<String>| {
            if v.len() > 5 {
                let extra = v.len() - 5;
                v.truncate(5);
                v.push(format!("...及另外 {} 条", extra));
            }
        };
        truncate_vec(&mut scope);
        truncate_vec(&mut constraints);
        truncate_vec(&mut exclusions);
        truncate_vec(&mut assumptions);
        true
    } else {
        false
    };

    let json = serde_json::json!({
        "scope": scope,
        "constraints": constraints,
        "exclusions": exclusions,
        "assumptions": assumptions,
    });

    let header = if items.is_empty() {
        "# Contract 当前状态\n尚无条目"
    } else if truncated {
        "# Contract 当前状态（已省略部分旧条目，参考下方 JSON 仅作为上下文，不要复述）"
    } else {
        "# Contract 当前状态（仅作参考，不要复述）"
    };

    format!("{}\n{}", header, serde_json::to_string(&json).unwrap_or_default())
}

/// Render belief state section for dynamic suffix (FR-10.3.4).
pub fn render_belief_state_section(bs: &PreflightBeliefState) -> String {
    let phase_label = match bs.phase {
        ConversationPhase::Exploring => "探索阶段",
        ConversationPhase::Narrowing => "收窄阶段",
        ConversationPhase::Confirming => "确认阶段",
        ConversationPhase::ReadyToSign => "就绪阶段",
    };

    let defs = default_slot_definitions();
    let mut confirmed = Vec::new();
    let mut tentative = Vec::new();
    let mut unfilled = Vec::new();

    for def in &defs {
        if let Some(slot) = bs.slots.get(&def.id) {
            let name = slot_display_name(&def.id);
            match slot.status {
                SlotStatus::Confirmed | SlotStatus::Skipped => confirmed.push(name),
                SlotStatus::Tentative => tentative.push(name),
                SlotStatus::Unfilled => unfilled.push(name),
            }
        }
    }

    let mut lines = vec![
        "# 信念状态".to_string(),
        format!("收敛分数: {}%", (bs.convergence_score * 100.0).round()),
        format!("当前阶段: {}", phase_label),
    ];

    if !confirmed.is_empty() {
        lines.push(format!("已确认: {}", confirmed.join(", ")));
    }
    if !tentative.is_empty() {
        lines.push(format!("待确认: {}", tentative.join(", ")));
    }
    if !unfilled.is_empty() {
        lines.push(format!("未触及: {}", unfilled.join(", ")));
    }

    lines.join("\n")
}

fn slot_display_name(id: &str) -> &'static str {
    match id {
        "primary_goal" => "核心目标",
        "target_users" => "目标用户",
        "key_features" => "关键功能",
        "tech_constraints" => "技术约束",
        "performance_targets" => "性能目标",
        "security_requirements" => "安全需求",
        "integration_points" => "集成接口",
        "out_of_scope" => "排除范围",
        "risk_assumptions" => "风险假设",
        "timeline_budget" => "时间预算",
        _ => "未知",
    }
}

/// Phase-driven convergence directive (FR-10.3.5).
pub fn get_convergence_directive(bs: &PreflightBeliefState) -> String {
    match bs.phase {
        ConversationPhase::Exploring => {
            "# 收敛指令\n当前处于探索阶段，广泛覆盖各维度。优先确认核心目标和关键功能。".to_string()
        }
        ConversationPhase::Narrowing => {
            "# 收敛指令\n已进入收窄阶段。聚焦未确认的维度，减少开放式问题，增加二选一或三选一的结构化选项。".to_string()
        }
        ConversationPhase::Confirming => {
            "# 收敛指令\n进入确认阶段。仅针对剩余 1-2 个关键空白点提问。如果主要领域已覆盖，考虑调用 suggest_sign。".to_string()
        }
        ConversationPhase::ReadyToSign => {
            "# 收敛指令\n澄清已充分完成！你必须立即调用 suggest_sign 工具建议用户签署 Contract。不要再提出新问题。".to_string()
        }
    }
}

/// Render rejected alternatives for prompt injection (FM-10.6.5).
fn render_rejected_alternatives(alts: &[(String, u32, String)]) -> String {
    let mut lines = vec!["# 已否决方案（请勿再建议）".to_string()];
    for (desc, round, reason) in alts.iter().take(10) {
        lines.push(format!("- {} (第{}轮否决，原因: {})", desc, round, reason));
    }
    lines.join("\n")
}

/// Extract reasoning from LLM response — unified interface (FM-10.3.10b).
pub fn extract_reasoning(response_text: &str, caps: &ModelCapabilities) -> Option<String> {
    if caps.supports_thinking {
        None // thinking block extracted separately from content blocks
    } else {
        extract_between_tags(response_text, "analysis")
    }
}

fn extract_between_tags(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    if let Some(start) = text.find(&open) {
        if let Some(end) = text[start..].find(&close) {
            let inner = &text[start + open.len()..start + end];
            return Some(inner.trim().to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// FM-10.4: Cache marker application
// ---------------------------------------------------------------------------

/// Apply cache_control markers to messages and tools (max 4 markers per request).
pub fn apply_cache_markers(
    messages: &mut [Message],
    tools: &mut [ToolDefinition],
    caps: &ModelCapabilities,
) {
    if !caps.supports_prompt_caching {
        return;
    }

    // Marker 1: system prompt (first message if role is implicit system via content)
    // In our architecture, system prompt is passed via LlmRequest.system,
    // so we mark the first user message that acts as context carrier.
    // Actually, for OpenAI-compatible API, system is a separate message.
    // We'll handle this in the provider layer.

    // Marker 2: last tool definition
    if let Some(last_tool) = tools.last_mut() {
        last_tool.cache_control = Some(CacheControl::ephemeral());
    }

    // Marker 3: last user message
    if let Some(last_user) = messages.iter_mut().rev().find(|m| m.role == MessageRole::User) {
        last_user.cache_control = Some(CacheControl::ephemeral());
    }
}

// ---------------------------------------------------------------------------
// FM-10.5: Context Compression
// ---------------------------------------------------------------------------

/// Micro-compact: compress old tool_results for present_choices (FR-10.5.1).
/// Only modifies the copy sent to LLM — DB originals are untouched.
pub fn micro_compact_messages(
    messages: &[Message],
    current_round: u32,
    keep_recent: u32,
) -> Vec<Message> {
    let threshold_round = current_round.saturating_sub(keep_recent);
    let mut round_counter: u32 = 0;

    // First pass: assign approximate round numbers based on user messages
    let mut msg_rounds: Vec<u32> = Vec::with_capacity(messages.len());
    for msg in messages {
        if msg.role == MessageRole::User {
            let is_tool_result = msg.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }));
            if !is_tool_result {
                round_counter += 1;
            }
        }
        msg_rounds.push(round_counter);
    }

    let meta_patterns = [
        "好的，让我们",
        "好的，接下来",
        "很高兴您选择了",
        "很好，让我们",
        "感谢您的选择",
        "明白了，",
        "了解，让我们",
        "好的，那我们",
    ];

    messages.iter().enumerate().map(|(i, msg)| {
        let msg_round = msg_rounds[i];

        // Compress old present_choices tool_results
        if msg.role == MessageRole::User && msg_round < threshold_round {
            let new_content: Vec<ContentBlock> = msg.content.iter().map(|block| {
                if let ContentBlock::ToolResult { tool_use_id, content, is_error } = block {
                    if content.contains("\"presented\":true") || content.contains("choices_count") {
                        return ContentBlock::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content: "[选项已呈现，用户已选择]".to_string(),
                            is_error: *is_error,
                        };
                    }
                }
                block.clone()
            }).collect();

            return Message {
                role: msg.role.clone(),
                content: new_content,
                cache_control: msg.cache_control.clone(),
            };
        }

        // Clean meta-dialogue from old assistant messages (FR-10.5.3)
        let meta_threshold = current_round.saturating_sub(5);
        if msg.role == MessageRole::Assistant && msg_round < meta_threshold {
            let new_content: Vec<ContentBlock> = msg.content.iter().map(|block| {
                if let ContentBlock::Text { text } = block {
                    let cleaned = clean_meta_dialogue(text, &meta_patterns);
                    if cleaned != *text {
                        return ContentBlock::Text { text: cleaned };
                    }
                }
                block.clone()
            }).collect();

            return Message {
                role: msg.role.clone(),
                content: new_content,
                cache_control: msg.cache_control.clone(),
            };
        }

        msg.clone()
    }).collect()
}

fn clean_meta_dialogue(text: &str, patterns: &[&str]) -> String {
    let mut result = text.to_string();
    for pattern in patterns {
        if result.starts_with(pattern) {
            if let Some(pos) = result.find('\n') {
                result = result[pos..].trim_start().to_string();
            }
        }
    }
    result
}

/// Estimate token count from text (rough heuristic for when API doesn't return usage).
pub fn estimate_tokens(text: &str) -> u64 {
    let chinese_chars = text.chars().filter(|c| *c >= '\u{4e00}' && *c <= '\u{9fff}').count();
    let other_chars = text.len() - chinese_chars;
    (chinese_chars as f64 * 1.5 + other_chars as f64 / 4.0).ceil() as u64
}

/// Check if full compaction should be triggered (FR-10.5.4).
pub fn should_compact(
    last_input_tokens: Option<u64>,
    context_window: u64,
    round: u32,
    compaction_failures: u32,
    already_compacting: bool,
) -> (bool, bool) {
    // Returns (should_compact, should_warn)
    if already_compacting || compaction_failures >= 3 {
        return (false, false);
    }

    if round >= 12 {
        return (true, false);
    }

    if let Some(tokens) = last_input_tokens {
        let ratio = tokens as f64 / context_window as f64;
        if ratio >= 0.70 {
            return (true, false);
        }
        if ratio >= 0.55 {
            return (false, true);
        }
    }

    (false, false)
}

/// Build the compaction prompt for full history compression (FR-10.5.5).
pub fn build_compaction_prompt(
    scope_count: usize,
    constraints_count: usize,
    exclusions_count: usize,
    assumptions_count: usize,
) -> String {
    format!(
        r#"请将以上 Pre-flight 澄清对话压缩为结构化摘要。

当前 Contract 状态（已独立持久化，无需在摘要中复述条目详情）：
- Scope: {} 条
- Constraints: {} 条
- Exclusions: {} 条
- Assumptions: {} 条

请按以下结构输出，仅输出文本，不要调用任何工具：

1. 用户原始需求：[原文引用]
2. 关键决策及理由：[决策 → 理由 列表，最多 10 条]
3. 仍待澄清的问题：[列表]
4. 用户偏好与风格：[观察到的沟通偏好、技术倾向]
5. 对话中明确被否决的方案：[列表，防止 Agent 重复建议]"#,
        scope_count, constraints_count, exclusions_count, assumptions_count
    )
}

/// Build compacted message list after full compaction (FR-10.5.6).
pub fn build_compacted_messages(
    summary: &str,
    original_requirement: Option<&Message>,
    recent_messages: &[Message],
) -> Vec<Message> {
    let mut result = Vec::new();

    // Summary as user message
    result.push(Message {
        role: MessageRole::User,
        content: vec![ContentBlock::Text {
            text: format!(
                "本次对话是对之前澄清的延续。以下是之前讨论的结构化摘要：\n\n{}\n\n[完整对话可在 session 日志中查看]",
                summary
            ),
        }],
        cache_control: None,
    });

    // Original requirement message
    if let Some(req) = original_requirement {
        result.push(req.clone());
    }

    // Recent messages (last 3 rounds preserved intact)
    result.extend_from_slice(recent_messages);

    result
}

/// Truncation fallback: keep the latest 50% + original requirement (FR-10.5.8).
pub fn truncate_messages(messages: &[Message]) -> Vec<Message> {
    if messages.len() <= 2 {
        return messages.to_vec();
    }

    let keep = messages.len() / 2;
    let mut result = Vec::new();

    // Always keep the first message (original requirement)
    result.push(messages[0].clone());

    // Keep the latest half
    result.extend_from_slice(&messages[messages.len() - keep..]);

    result
}

// ---------------------------------------------------------------------------
// FM-10.6: Decision Log types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionEntry {
    pub id: String,
    pub session_id: String,
    pub round: u32,
    pub decision_type: String,
    pub description: String,
    pub rationale: String,
    pub alternatives: Vec<Alternative>,
    pub contract_item_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alternative {
    pub label: String,
    pub reason_rejected: String,
}

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
        cache_control: None,
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
                    cache_control: None,
                },
                Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::Text {
                        text: text.clone(),
                    }],
                    cache_control: None,
                },
                Message {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: format!(
                            "Your previous output had an error: {first_err}\n\
                             Please fix it and output ONLY a valid JSON object."
                        ),
                    }],
                    cache_control: None,
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
            cache_control: None,
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
            cache_control: None,
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
            cache_control: None,
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
            cache_control: None,
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
            cache_control: None,
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

/// Pre-flight chat with dynamic prompt assembly (FM-10.3) and optional context compression (FM-10.5).
pub async fn preflight_chat(
    provider: Arc<dyn LlmProvider>,
    model: &str,
    mode: &str,
    mut history: Vec<Message>,
    session_id: &str,
    app: &tauri::AppHandle,
    contract_items: &[(String, String, String)],
    belief_state: &PreflightBeliefState,
    rejected_alternatives: &[(String, u32, String)],
    caps: &ModelCapabilities,
) -> Result<(PreflightResponse, TokenUsage), PlannerError> {
    let system_prompt = build_preflight_system_prompt(
        mode, contract_items, belief_state, rejected_alternatives, caps,
    );

    // Apply micro-compact to reduce token usage (FM-10.5)
    let current_round = belief_state.round;
    if current_round > 3 {
        history = micro_compact_messages(&history, current_round, 3);
        tracing::debug!(round = current_round, "micro-compact applied to message history");
    }

    let mut tools = preflight_tools();

    // Apply cache markers (FM-10.4)
    apply_cache_markers(&mut history, &mut tools, caps);

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

    // Log cache metrics (FM-10.4)
    if response.usage.cache_read_input_tokens > 0 || response.usage.cache_creation_input_tokens > 0 {
        tracing::info!(
            cache_creation_tokens = response.usage.cache_creation_input_tokens,
            cache_read_tokens = response.usage.cache_read_input_tokens,
            total_input_tokens = response.usage.input_tokens,
            cache_hit_ratio = %format!("{:.2}", response.usage.cache_hit_ratio()),
            "preflight cache metrics"
        );
    }

    tracing::info!(
        choices_count = result.choices.len(),
        tool_calls_count = result.tool_calls.len(),
        fallback_used = %result.fallback_used,
        input_tokens = response.usage.input_tokens,
        output_tokens = response.usage.output_tokens,
        "preflight stream complete"
    );

    Ok((result, response.usage))
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

    // --- FM-10.3 Dynamic System Prompt tests ---

    #[test]
    fn ut_10_3_1a_basic_assembly() {
        let bs = PreflightBeliefState::new();
        let caps = ModelCapabilities::default();
        let prompt = build_preflight_system_prompt("scenario_walk", &[], &bs, &[], &caps);
        assert!(prompt.contains("__DYNAMIC_BOUNDARY__"));
        assert!(prompt.contains("场景走查"));
    }

    #[test]
    fn ut_10_3_1b_with_contract_items() {
        let bs = PreflightBeliefState::new();
        let caps = ModelCapabilities::default();
        let items = vec![
            ("scope".into(), "实现OAuth登录".into(), "confirmed".into()),
            ("scope".into(), "支持GitHub".into(), "tentative".into()),
            ("constraints".into(), "使用React".into(), "confirmed".into()),
        ];
        let prompt = build_preflight_system_prompt("scenario_walk", &items, &bs, &[], &caps);
        assert!(prompt.contains("OAuth"));
        assert!(prompt.contains("confirmed"));
    }

    #[test]
    fn ut_10_3_1d_ready_to_sign_directive() {
        let mut bs = PreflightBeliefState::new();
        bs.phase = ConversationPhase::ReadyToSign;
        let caps = ModelCapabilities::default();
        let prompt = build_preflight_system_prompt("scenario_walk", &[], &bs, &[], &caps);
        assert!(prompt.contains("suggest_sign"));
    }

    #[test]
    fn ut_10_3_1e_mode_switch() {
        let bs = PreflightBeliefState::new();
        let caps = ModelCapabilities::default();
        let prompt = build_preflight_system_prompt("devils_advocate", &[], &bs, &[], &caps);
        assert!(prompt.contains("魔鬼代言人"));
        assert!(!prompt.contains("场景走查"));
    }

    #[test]
    fn ut_10_3_4a_static_prefix_stability() {
        let caps = ModelCapabilities::default();
        let p1 = build_static_prefix(&caps);
        let p2 = build_static_prefix(&caps);
        assert_eq!(p1, p2, "Static prefix must be byte-stable");
    }

    #[test]
    fn ut_10_3_4b_static_prefix_mode_independent() {
        let caps = ModelCapabilities::default();
        let bs1 = PreflightBeliefState::new();
        let mut bs2 = PreflightBeliefState::new();
        bs2.phase = ConversationPhase::Narrowing;

        let p1 = build_preflight_system_prompt("scenario_walk", &[], &bs1, &[], &caps);
        let p2 = build_preflight_system_prompt("devils_advocate", &[], &bs2, &[], &caps);

        let static1 = p1.split("__DYNAMIC_BOUNDARY__").next().unwrap();
        let static2 = p2.split("__DYNAMIC_BOUNDARY__").next().unwrap();
        assert_eq!(static1, static2, "Static prefix must not change with mode/state");
    }

    #[test]
    fn ut_10_3_8_thinking_cot_mutual_exclusion() {
        let mut caps = ModelCapabilities::default();
        caps.supports_thinking = true;
        let prefix = build_static_prefix(&caps);
        assert!(!prefix.contains("<analysis>"), "Thinking model must not have CoT in prompt");

        caps.supports_thinking = false;
        let prefix = build_static_prefix(&caps);
        assert!(prefix.contains("<analysis>"), "Non-thinking model must have CoT guidance");
    }

    #[test]
    fn ut_10_3_9_extract_reasoning() {
        let mut caps = ModelCapabilities::default();
        caps.supports_thinking = false;
        let text = "Some intro <analysis>深度分析内容</analysis> conclusion";
        assert_eq!(extract_reasoning(text, &caps), Some("深度分析内容".into()));

        caps.supports_thinking = true;
        assert_eq!(extract_reasoning(text, &caps), None);
    }

    // --- FM-10.3.3 Contract compact JSON tests ---

    #[test]
    fn ut_10_3_2a_empty_contract() {
        let json = compact_contract_json(&[]);
        assert!(json.contains("尚无条目"));
    }

    #[test]
    fn ut_10_3_2b_normal_contract() {
        let items = vec![
            ("scope".into(), "实现登录".into(), "confirmed".into()),
            ("constraints".into(), "使用React".into(), "confirmed".into()),
        ];
        let json = compact_contract_json(&items);
        assert!(json.contains("实现登录(confirmed)"));
    }

    #[test]
    fn ut_10_3_2c_truncation() {
        let mut items = Vec::new();
        for i in 0..25 {
            items.push(("scope".into(), format!("item_{i}"), "confirmed".into()));
        }
        let json = compact_contract_json(&items);
        assert!(json.contains("另外"));
    }

    // --- FM-10.5 Micro-compact tests ---

    #[test]
    fn ut_10_5_1a_recent_not_compressed() {
        let msgs = vec![
            Message { role: MessageRole::User, content: vec![ContentBlock::Text { text: "r1".into() }], cache_control: None },
            Message { role: MessageRole::User, content: vec![ContentBlock::Text { text: "r8".into() }], cache_control: None },
        ];
        let result = micro_compact_messages(&msgs, 10, 3);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn ut_10_5_4a_no_compact_low_usage() {
        let (compact, warn) = should_compact(Some(10000), 100000, 5, 0, false);
        assert!(!compact);
        assert!(!warn);
    }

    #[test]
    fn ut_10_5_4b_compact_high_usage() {
        let (compact, _) = should_compact(Some(75000), 100000, 5, 0, false);
        assert!(compact);
    }

    #[test]
    fn ut_10_5_4c_compact_many_rounds() {
        let (compact, _) = should_compact(Some(30000), 100000, 12, 0, false);
        assert!(compact);
    }

    #[test]
    fn ut_10_5_4e_circuit_breaker() {
        let (compact, _) = should_compact(Some(90000), 100000, 15, 3, false);
        assert!(!compact, "Should not compact after 3 failures");
    }

    // --- FM-10.6 Decision Log tests ---

    #[test]
    fn ut_10_6_3a_no_rejected() {
        let alts: Vec<(String, u32, String)> = vec![];
        let section = render_rejected_alternatives(&alts);
        assert!(section.contains("已否决方案"));
    }

    #[test]
    fn ut_10_6_3b_some_rejected() {
        let alts = vec![
            ("自建认证".into(), 3, "用户偏好OAuth".into()),
        ];
        let prompt = build_preflight_system_prompt("scenario_walk", &[], &PreflightBeliefState::new(), &alts, &ModelCapabilities::default());
        assert!(prompt.contains("自建认证"));
        assert!(prompt.contains("第3轮否决"));
    }
}
