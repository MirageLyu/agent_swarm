use crate::llm::ContentBlock;
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};

pub const TOOL_RESULT_MANIFEST_PREFIX: &str = "[tool_result_manifest]";
pub const TOOL_RESULT_REF_PREFIX: &str = "[tool_result_ref]";
pub const TOOL_RESULT_REPEAT_PREFIX: &str = "[tool_result_repeat]";

const POLICY_SOFT_CAP_CHARS: usize = 6 * 1024;
const POLICY_TAIL_CHARS: usize = 1200;
const TINY_INLINE_CHARS: usize = 1536;
const REPEAT_MIN_CHARS: usize = 512;
const MIN_EVIDENCE_REF_SAVED_CHARS: usize = 512;
const EVIDENCE_REF_EXCERPT_CHARS: usize = 900;
const TOOL_RESULT_MESSAGE_BUDGET_CHARS: usize = 12 * 1024;
const SAME_SOURCE_REPEAT_MIN_CHARS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultPolicyReport {
    pub tool_use_id: String,
    pub tool_name: String,
    pub original_chars: usize,
    pub compacted_chars: usize,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextRenderMode {
    Inline,
    EvidenceRef,
    RepeatRef,
    Manifest,
}

impl ContextRenderMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContextRenderMode::Inline => "inline",
            ContextRenderMode::EvidenceRef => "evidence_ref",
            ContextRenderMode::RepeatRef => "repeat_ref",
            ContextRenderMode::Manifest => "manifest",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRecord {
    pub tool_use_id: String,
    pub tool_name: String,
    pub step: u32,
    pub source_fingerprint: String,
    pub content_fingerprint: String,
    pub original_chars: usize,
    pub evidence_path: Option<String>,
    pub source_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultContextReport {
    pub tool_use_id: String,
    pub tool_name: String,
    pub mode: ContextRenderMode,
    pub original_chars: usize,
    pub context_chars: usize,
    pub content_fingerprint: String,
    pub source_fingerprint: String,
    pub repeat_of: Option<String>,
    pub evidence_path: Option<String>,
    pub persisted_path: Option<String>,
    pub saved_chars: usize,
    pub per_message_budget_replaced: bool,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ToolResultContextInput<'a> {
    pub step: u32,
    pub tool_use_id: &'a str,
    pub tool_name: &'a str,
    pub input: &'a serde_json::Value,
    pub output: &'a crate::tools::ToolOutput,
}

#[derive(Debug, Clone)]
pub struct ToolResultContextRendered {
    pub content: String,
    pub report: Option<ToolResultContextReport>,
}

#[derive(Debug, Default, Clone)]
pub struct ToolResultContextState {
    by_content: HashMap<String, EvidenceRecord>,
    by_source: HashMap<String, EvidenceRecord>,
    replacements_by_tool_use: HashMap<String, String>,
}

pub struct ToolResultContextPolicy;

pub fn apply_tool_result_context_policy(
    state: &mut ToolResultContextState,
    step: u32,
    tool_use_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
    output: &crate::tools::ToolOutput,
) -> (String, Option<ToolResultContextReport>) {
    ToolResultContextPolicy::apply(state, step, tool_use_id, tool_name, input, output)
}

impl ToolResultContextPolicy {
    pub fn apply(
        state: &mut ToolResultContextState,
        step: u32,
        tool_use_id: &str,
        tool_name: &str,
        input: &serde_json::Value,
        output: &crate::tools::ToolOutput,
    ) -> (String, Option<ToolResultContextReport>) {
        let rendered =
            render_one_tool_result(state, step, tool_use_id, tool_name, input, output, false);
        let report = rendered
            .report
            .filter(|report| report.mode != ContextRenderMode::Inline);
        (rendered.content, report)
    }

    pub fn apply_batch(
        state: &mut ToolResultContextState,
        inputs: &[ToolResultContextInput<'_>],
    ) -> Vec<ToolResultContextRendered> {
        let mut rendered = inputs
            .iter()
            .map(|item| {
                render_one_tool_result(
                    state,
                    item.step,
                    item.tool_use_id,
                    item.tool_name,
                    item.input,
                    item.output,
                    false,
                )
            })
            .collect::<Vec<_>>();

        let total_chars: usize = rendered.iter().map(|r| r.content.chars().count()).sum();
        if total_chars <= TOOL_RESULT_MESSAGE_BUDGET_CHARS {
            return rendered;
        }

        let mut over_by = total_chars - TOOL_RESULT_MESSAGE_BUDGET_CHARS;
        let mut candidates = rendered
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                let report = item.report.as_ref()?;
                if report.mode != ContextRenderMode::Inline {
                    return None;
                }
                if report.original_chars <= TINY_INLINE_CHARS {
                    return None;
                }
                Some((idx, report.original_chars))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(_, original_chars)| std::cmp::Reverse(*original_chars));

        for (idx, _) in candidates {
            if over_by == 0 {
                break;
            }
            let item = &inputs[idx];
            let replacement = render_one_tool_result(
                state,
                item.step,
                item.tool_use_id,
                item.tool_name,
                item.input,
                item.output,
                true,
            );
            let old_chars = rendered[idx].content.chars().count();
            let new_chars = replacement.content.chars().count();
            if new_chars >= old_chars {
                continue;
            }
            over_by = over_by.saturating_sub(old_chars - new_chars);
            rendered[idx] = replacement;
            if let Some(report) = rendered[idx].report.as_mut() {
                report.per_message_budget_replaced = true;
                if !report.reason.contains("per-message") {
                    report
                        .reason
                        .push_str("; selected by per-message tool_result budget");
                }
            }
        }

        rendered
    }
}

fn render_one_tool_result(
    state: &mut ToolResultContextState,
    step: u32,
    tool_use_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
    output: &crate::tools::ToolOutput,
    force_reference: bool,
) -> ToolResultContextRendered {
    if let Some(cached) = state.replacements_by_tool_use.get(tool_use_id).cloned() {
        return ToolResultContextRendered {
            content: cached,
            report: None,
        };
    }

    let original_chars = output.content.chars().count();
    let content_fingerprint = fingerprint_normalized(&output.content);
    let source_fingerprint = source_fingerprint(tool_name, input, output);
    let evidence_path = evidence_path(output);
    let persisted_path = persisted_path(output);
    let source_label = source_label(tool_name, input, output);

    if is_already_compacted(&output.content) {
        let record = EvidenceRecord {
            tool_use_id: tool_use_id.to_string(),
            tool_name: tool_name.to_string(),
            step,
            source_fingerprint: source_fingerprint.clone(),
            content_fingerprint: content_fingerprint.clone(),
            original_chars,
            evidence_path: evidence_path.clone().or_else(|| persisted_path.clone()),
            source_label: source_label.clone(),
        };
        state.record(record);

        if is_legacy_truncated(&output.content)
            && should_render_evidence_ref(tool_name, input, output, original_chars)
        {
            let rendered = render_evidence_ref(
                tool_name,
                input,
                output,
                original_chars,
                &source_label,
                &evidence_path.clone().or_else(|| persisted_path.clone()),
            );
            let report = make_context_report(
                tool_use_id,
                tool_name,
                ContextRenderMode::EvidenceRef,
                original_chars,
                &rendered,
                content_fingerprint,
                source_fingerprint,
                None,
                evidence_path,
                persisted_path,
                false,
                "legacy truncated tool_result re-rendered as evidence reference".to_string(),
            );
            state
                .replacements_by_tool_use
                .insert(tool_use_id.to_string(), rendered.clone());
            return ToolResultContextRendered {
                content: rendered,
                report: Some(report),
            };
        }

        return ToolResultContextRendered {
            content: output.content.clone(),
            report: None,
        };
    }

    if let Some(prior) = state.by_content.get(&content_fingerprint).cloned() {
        if prior.tool_use_id != tool_use_id && original_chars >= REPEAT_MIN_CHARS {
            let rendered = render_repeat_ref(tool_name, &prior, original_chars, &source_label);
            let report = make_context_report(
                tool_use_id,
                tool_name,
                ContextRenderMode::RepeatRef,
                original_chars,
                &rendered,
                content_fingerprint,
                source_fingerprint,
                Some(format!("step {} / {}", prior.step, prior.tool_use_id)),
                evidence_path,
                persisted_path,
                false,
                "exact output repeat suppressed".to_string(),
            );
            state
                .replacements_by_tool_use
                .insert(tool_use_id.to_string(), rendered.clone());
            return ToolResultContextRendered {
                content: rendered,
                report: Some(report),
            };
        }
    }

    if let Some(prior) = state.by_source.get(&source_fingerprint).cloned() {
        if prior.tool_use_id != tool_use_id
            && original_chars >= SAME_SOURCE_REPEAT_MIN_CHARS
            && source_fingerprint != fingerprint_normalized(tool_name)
            && same_source_repeat_is_safe(tool_name, input, output, &prior)
        {
            let rendered = render_same_source_ref(tool_name, &prior, original_chars, &source_label);
            let report = make_context_report(
                tool_use_id,
                tool_name,
                ContextRenderMode::RepeatRef,
                original_chars,
                &rendered,
                content_fingerprint.clone(),
                source_fingerprint.clone(),
                Some(format!("step {} / {}", prior.step, prior.tool_use_id)),
                evidence_path.clone(),
                persisted_path.clone(),
                false,
                "same-source broad output suppressed".to_string(),
            );
            let record = EvidenceRecord {
                tool_use_id: tool_use_id.to_string(),
                tool_name: tool_name.to_string(),
                step,
                source_fingerprint,
                content_fingerprint,
                original_chars,
                evidence_path: evidence_path.or_else(|| persisted_path.clone()),
                source_label,
            };
            state.record(record);
            state
                .replacements_by_tool_use
                .insert(tool_use_id.to_string(), rendered.clone());
            return ToolResultContextRendered {
                content: rendered,
                report: Some(report),
            };
        }
    }

    let should_ref =
        force_reference || should_render_evidence_ref(tool_name, input, output, original_chars);
    let mut mode = ContextRenderMode::Inline;
    let mut rendered = output.content.clone();
    let mut reason = String::new();

    if should_ref {
        let candidate = render_evidence_ref(
            tool_name,
            input,
            output,
            original_chars,
            &source_label,
            &evidence_path.clone().or_else(|| persisted_path.clone()),
        );
        let candidate_chars = candidate.chars().count();
        if force_reference || candidate_chars + MIN_EVIDENCE_REF_SAVED_CHARS <= original_chars {
            mode = ContextRenderMode::EvidenceRef;
            rendered = candidate;
            reason = if force_reference {
                "tool_result rendered as reference by per-message budget".to_string()
            } else {
                "medium evidence/content-shaped tool_result rendered as reference".to_string()
            };
        }
    } else if original_chars > cap_for_tool(tool_name) {
        mode = ContextRenderMode::Manifest;
        rendered = render_manifest(
            tool_name,
            input,
            output,
            original_chars,
            cap_for_tool(tool_name),
        );
        reason = format!(
            "tool_result exceeded deterministic cap ({}>{} chars)",
            original_chars,
            cap_for_tool(tool_name)
        );
    }

    let record = EvidenceRecord {
        tool_use_id: tool_use_id.to_string(),
        tool_name: tool_name.to_string(),
        step,
        source_fingerprint: source_fingerprint.clone(),
        content_fingerprint: content_fingerprint.clone(),
        original_chars,
        evidence_path: evidence_path.clone().or_else(|| persisted_path.clone()),
        source_label,
    };
    state.record(record);

    let report = make_context_report(
        tool_use_id,
        tool_name,
        mode.clone(),
        original_chars,
        &rendered,
        content_fingerprint,
        source_fingerprint,
        None,
        evidence_path,
        persisted_path,
        false,
        reason,
    );

    if mode != ContextRenderMode::Inline {
        state
            .replacements_by_tool_use
            .insert(tool_use_id.to_string(), rendered.clone());
    }

    ToolResultContextRendered {
        content: rendered,
        report: Some(report),
    }
}

fn make_context_report(
    tool_use_id: &str,
    tool_name: &str,
    mode: ContextRenderMode,
    original_chars: usize,
    rendered: &str,
    content_fingerprint: String,
    source_fingerprint: String,
    repeat_of: Option<String>,
    evidence_path: Option<String>,
    persisted_path: Option<String>,
    per_message_budget_replaced: bool,
    reason: String,
) -> ToolResultContextReport {
    let context_chars = rendered.chars().count();
    ToolResultContextReport {
        tool_use_id: tool_use_id.to_string(),
        tool_name: tool_name.to_string(),
        mode,
        original_chars,
        context_chars,
        content_fingerprint,
        source_fingerprint,
        repeat_of,
        evidence_path,
        persisted_path,
        saved_chars: original_chars.saturating_sub(context_chars),
        per_message_budget_replaced,
        reason,
    }
}

impl ToolResultContextState {
    fn record(&mut self, record: EvidenceRecord) {
        self.by_content
            .entry(record.content_fingerprint.clone())
            .or_insert_with(|| record.clone());
        self.by_source
            .entry(record.source_fingerprint.clone())
            .or_insert(record);
    }
}

pub fn apply_tool_result_policy(
    tool_use_id: &str,
    tool_name: &str,
    input: &serde_json::Value,
    output: &mut crate::tools::ToolOutput,
) -> Option<ToolResultPolicyReport> {
    if is_already_compacted(&output.content) {
        return None;
    }

    let original_chars = output.content.chars().count();
    let cap = cap_for_tool(tool_name);
    if original_chars <= cap {
        return None;
    }

    let manifest = render_manifest(tool_name, input, output, original_chars, cap);
    let compacted_chars = manifest.chars().count();
    output.content = manifest;
    Some(ToolResultPolicyReport {
        tool_use_id: tool_use_id.to_string(),
        tool_name: tool_name.to_string(),
        original_chars,
        compacted_chars,
        reason: format!("tool_result exceeded deterministic cap ({original_chars}>{cap} chars)"),
    })
}

pub fn is_already_compacted(content: &str) -> bool {
    content.starts_with("[shell_exec_output_compacted]")
        || content.starts_with(TOOL_RESULT_MANIFEST_PREFIX)
        || content.starts_with(TOOL_RESULT_REF_PREFIX)
        || content.starts_with(TOOL_RESULT_REPEAT_PREFIX)
        || content.starts_with("[tool_summary]")
        || content.starts_with("[result truncated to keep context lean.")
        || is_legacy_truncated(content)
}

fn is_legacy_truncated(content: &str) -> bool {
    content.starts_with("[... truncated ") && content.contains(" bytes from head ...]")
}

pub fn apply_policy_to_messages(
    messages: &mut [crate::llm::Message],
) -> Vec<ToolResultPolicyReport> {
    let tool_lookup = tool_lookup_from_messages(messages);
    let mut reports = Vec::new();
    for msg in messages.iter_mut() {
        for block in msg.content.iter_mut() {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            else {
                continue;
            };
            let (tool_name, input) = tool_lookup
                .get(tool_use_id)
                .cloned()
                .unwrap_or_else(|| (tool_use_id.clone(), serde_json::Value::Null));
            let mut output = crate::tools::ToolOutput {
                content: std::mem::take(content),
                is_error: *is_error,
                meta: None,
            };
            if let Some(report) =
                apply_tool_result_policy(tool_use_id, &tool_name, &input, &mut output)
            {
                reports.push(report);
            }
            *content = output.content;
        }
    }
    reports
}

fn should_render_evidence_ref(
    tool_name: &str,
    input: &serde_json::Value,
    output: &crate::tools::ToolOutput,
    original_chars: usize,
) -> bool {
    if output.is_error {
        return original_chars > cap_for_tool(tool_name);
    }
    if original_chars <= TINY_INLINE_CHARS {
        return false;
    }
    if evidence_path(output).is_some() {
        return matches!(
            tool_name,
            "shell_exec" | "read_file" | "grep" | "search_files" | "glob" | "list_files"
        );
    }
    if matches!(
        tool_name,
        "shell_exec" | "read_file" | "grep" | "search_files" | "glob" | "list_files"
    ) {
        return true;
    }
    if looks_like_artifact_dump(tool_name, input) {
        return true;
    }
    false
}

fn render_evidence_ref(
    tool_name: &str,
    input: &serde_json::Value,
    output: &crate::tools::ToolOutput,
    original_chars: usize,
    source_label: &Option<String>,
    evidence_path: &Option<String>,
) -> String {
    let mut out = format!(
        "{TOOL_RESULT_REF_PREFIX}\ntool: {tool_name}\nstatus: {}\noriginal_chars: {original_chars}\ncontent_hash: {}\n",
        if output.is_error { "error" } else { "ok" },
        fingerprint_normalized(&output.content)
    );
    if let Some(source) = source_label {
        out.push_str(&format!("source: {source}\n"));
    }
    if let Some(path) = evidence_path {
        out.push_str(&format!("evidence: {path}\n"));
    }
    if let Some(shape) =
        output_meta_str(output, "content_shape").or_else(|| output_meta_str(output, "output_kind"))
    {
        out.push_str(&format!("content_shape: {shape}\n"));
    }
    if let Some(family) = output_meta_str(output, "command_family") {
        out.push_str(&format!("command_family: {family}\n"));
    }
    if let Some(summary) = render_artifact_summary(tool_name, input, output) {
        out.push_str("\nArtifact summary:\n");
        out.push_str(&summary);
        out.push('\n');
    }
    out.push_str("\nObservation excerpt:\n");
    out.push_str(&head_tail_excerpt(
        &output.content,
        EVIDENCE_REF_EXCERPT_CHARS,
    ));
    out.push_str("\n\nSuggested next steps:\n");
    out.push_str(&suggested_next_steps(tool_name, input));
    out
}

fn render_repeat_ref(
    tool_name: &str,
    prior: &EvidenceRecord,
    original_chars: usize,
    source_label: &Option<String>,
) -> String {
    let mut out = format!(
        "{TOOL_RESULT_REPEAT_PREFIX}\ntool: {tool_name}\nsame_output_as: step {} / {}\nbytes: {original_chars}\ncontent_hash: {}\n",
        prior.step, prior.tool_use_id, prior.content_fingerprint
    );
    if let Some(source) = source_label.as_ref().or(prior.source_label.as_ref()) {
        out.push_str(&format!("source: {source}\n"));
    }
    if let Some(path) = &prior.evidence_path {
        out.push_str(&format!("evidence: {path}\n"));
    }
    out.push_str("note: The same output was already provided earlier; use the prior evidence ref or run a narrower grep/read if needed.\n");
    out
}

fn render_same_source_ref(
    tool_name: &str,
    prior: &EvidenceRecord,
    original_chars: usize,
    source_label: &Option<String>,
) -> String {
    let mut out = format!(
        "{TOOL_RESULT_REPEAT_PREFIX}\ntool: {tool_name}\nsame_source_as: step {} / {}\nbytes: {original_chars}\nsource_hash: {}\n",
        prior.step, prior.tool_use_id, prior.source_fingerprint
    );
    if let Some(source) = source_label.as_ref().or(prior.source_label.as_ref()) {
        out.push_str(&format!("source: {source}\n"));
    }
    if let Some(path) = &prior.evidence_path {
        out.push_str(&format!("evidence: {path}\n"));
    }
    out.push_str("note: A broad result from this source was already observed; use a narrower grep/read offset/targeted extraction instead of reloading the full source.\n");
    out
}

fn same_source_repeat_is_safe(
    tool_name: &str,
    input: &serde_json::Value,
    output: &crate::tools::ToolOutput,
    prior: &EvidenceRecord,
) -> bool {
    // Mutable workspace files can change between two reads/dumps. Do not suppress
    // a later result from the same path unless the producer supplies an unchanged
    // version token (for example, read_file's mtime/size metadata) that matches
    // the earlier observation. Exact byte-for-byte repeats are still handled by
    // the by_content path above.
    if source_label_is_mutable_workspace_path(source_label(tool_name, input, output).as_deref()) {
        return false;
    }
    if source_label_is_mutable_workspace_path(prior.source_label.as_deref()) {
        return false;
    }
    true
}

fn source_label_is_mutable_workspace_path(label: Option<&str>) -> bool {
    let Some(label) = label.map(str::trim).filter(|label| !label.is_empty()) else {
        return false;
    };
    if label.starts_with("http://")
        || label.starts_with("https://")
        || label.starts_with("custom://")
        || label.starts_with("pattern:")
    {
        return false;
    }
    true
}

fn source_fingerprint(
    tool_name: &str,
    input: &serde_json::Value,
    output: &crate::tools::ToolOutput,
) -> String {
    let source = source_label(tool_name, input, output)
        .unwrap_or_else(|| serde_json::to_string(input).unwrap_or_else(|_| "null".to_string()));
    fingerprint_normalized(&format!("{tool_name}:{source}"))
}

fn source_label(
    tool_name: &str,
    input: &serde_json::Value,
    output: &crate::tools::ToolOutput,
) -> Option<String> {
    output_meta_str(output, "content_source")
        .or_else(|| {
            input
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            input
                .get("pattern")
                .and_then(|v| v.as_str())
                .map(|s| format!("pattern:{s}"))
        })
        .or_else(|| {
            input
                .get("command")
                .or_else(|| input.get("cmd"))
                .and_then(|v| v.as_str())
                .map(command_source_label)
        })
        .or_else(|| Some(tool_name.to_string()))
}

fn command_source_label(command: &str) -> String {
    if let Some(url) = extract_url(command) {
        return url;
    }
    command
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_url(text: &str) -> Option<String> {
    for token in
        text.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ')' | '(' | '<' | '>'))
    {
        if token.starts_with("http://") || token.starts_with("https://") {
            return Some(
                token
                    .trim_end_matches(|c: char| matches!(c, ',' | ';'))
                    .to_string(),
            );
        }
    }
    None
}

fn evidence_path(output: &crate::tools::ToolOutput) -> Option<String> {
    output_meta_str(output, "stdout_path")
        .or_else(|| output_meta_str(output, "path"))
        .or_else(|| output_meta_str(output, "evidence_path"))
}

fn persisted_path(output: &crate::tools::ToolOutput) -> Option<String> {
    output_meta_str(output, "persisted_path")
        .or_else(|| output_meta_str(output, "tool_result_path"))
        .or_else(|| output_meta_str(output, "output_path"))
}

fn output_meta_str(output: &crate::tools::ToolOutput, key: &str) -> Option<String> {
    output
        .meta
        .as_ref()
        .and_then(|m| m.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn looks_like_artifact_dump(tool_name: &str, input: &serde_json::Value) -> bool {
    if tool_name != "shell_exec" {
        return false;
    }
    let Some(command) = input
        .get("command")
        .or_else(|| input.get("cmd"))
        .and_then(|v| v.as_str())
    else {
        return false;
    };
    let lower = command.to_ascii_lowercase();
    ["cat ", "head ", "tail ", "jq "]
        .iter()
        .any(|prefix| lower.contains(prefix))
        && [
            ".json",
            ".csv",
            ".md",
            ".markdown",
            ".ipynb",
            ".txt",
            ".html",
            ".htm",
            ".xml",
            ".pptx",
            ".zip",
            ".png",
            ".jpg",
            ".jpeg",
            ".gif",
            ".webp",
            ".pdf",
            ".bin",
        ]
        .iter()
        .any(|ext| lower.contains(ext))
}

fn render_artifact_summary(
    tool_name: &str,
    input: &serde_json::Value,
    output: &crate::tools::ToolOutput,
) -> Option<String> {
    if !(looks_like_artifact_dump(tool_name, input)
        || matches!(tool_name, "read_file" | "shell_exec"))
    {
        return None;
    }

    let source = source_label(tool_name, input, output).unwrap_or_default();
    let shape = output_meta_str(output, "content_shape")
        .or_else(|| shape_from_source(&source).map(str::to_string))
        .unwrap_or_else(|| classify_content_shape(&output.content).to_string());
    let bytes = output.content.len();
    let lines = output.content.lines().count();

    let mut summary = format!("- bytes: {bytes}\n- lines: {lines}\n- inferred_shape: {shape}\n");
    match shape.as_str() {
        "json" => append_json_summary(&mut summary, &output.content),
        "csv" => append_csv_summary(&mut summary, &output.content),
        "markdown" => append_markdown_summary(&mut summary, &output.content),
        "notebook" => append_notebook_summary(&mut summary, &output.content),
        "pptx" | "zip" => append_zip_like_summary(&mut summary, &output.content, &shape),
        "image" | "binary" | "pdf" => {
            append_media_or_binary_summary(&mut summary, &output.content, &source, &shape)
        }
        "html" | "xml" => append_markup_summary(&mut summary, &output.content, &shape),
        _ if looks_binary_like(&output.content) => summary.push_str("- binary_like: true\n"),
        _ => {}
    }
    Some(summary.trim_end().to_string())
}

fn append_json_summary(summary: &mut String, content: &str) {
    match serde_json::from_str::<serde_json::Value>(content) {
        Ok(value) => {
            summary.push_str("- json_parse: ok\n");
            match value {
                serde_json::Value::Object(map) => {
                    let keys = map.keys().take(12).cloned().collect::<Vec<_>>().join(", ");
                    summary.push_str(&format!("- top_level: object keys={}\n", map.len()));
                    if !keys.is_empty() {
                        summary.push_str(&format!("- first_keys: {keys}\n"));
                    }
                }
                serde_json::Value::Array(items) => {
                    summary.push_str(&format!("- top_level: array items={}\n", items.len()));
                }
                other => {
                    summary.push_str(&format!("- top_level: {}\n", json_type_name(&other)));
                }
            }
        }
        Err(err) => summary.push_str(&format!(
            "- json_parse: error: {}\n",
            one_line(&err.to_string())
        )),
    }
}

fn append_csv_summary(summary: &mut String, content: &str) {
    let mut non_empty = content.lines().filter(|line| !line.trim().is_empty());
    if let Some(header) = non_empty.next() {
        let columns = split_csv_line(header).len();
        let rows = non_empty.count();
        summary.push_str(&format!("- csv_columns: {columns}\n"));
        summary.push_str(&format!("- csv_data_rows: {rows}\n"));
        summary.push_str(&format!("- csv_header: {}\n", one_line(header)));
    } else {
        summary.push_str("- csv_empty: true\n");
    }
}

fn append_markdown_summary(summary: &mut String, content: &str) {
    let headings = content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                Some(one_line(trimmed))
            } else {
                None
            }
        })
        .take(8)
        .collect::<Vec<_>>();
    let heading_count = content
        .lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .count();
    let non_ws_chars = content.chars().filter(|c| !c.is_whitespace()).count();
    summary.push_str(&format!("- markdown_headings: {heading_count}\n"));
    summary.push_str(&format!("- non_whitespace_chars: {non_ws_chars}\n"));
    if !headings.is_empty() {
        summary.push_str(&format!("- first_headings: {}\n", headings.join(" | ")));
    }
}

fn append_notebook_summary(summary: &mut String, content: &str) {
    match serde_json::from_str::<serde_json::Value>(content) {
        Ok(value) => {
            let Some(cells) = value.get("cells").and_then(|v| v.as_array()) else {
                summary.push_str("- notebook_parse: ok_without_cells\n");
                return;
            };
            let code = cells
                .iter()
                .filter(|cell| cell.get("cell_type").and_then(|v| v.as_str()) == Some("code"))
                .count();
            let markdown = cells
                .iter()
                .filter(|cell| cell.get("cell_type").and_then(|v| v.as_str()) == Some("markdown"))
                .count();
            summary.push_str("- notebook_parse: ok\n");
            summary.push_str(&format!("- notebook_cells: {}\n", cells.len()));
            summary.push_str(&format!("- code_cells: {code}\n"));
            summary.push_str(&format!("- markdown_cells: {markdown}\n"));
        }
        Err(err) => summary.push_str(&format!(
            "- notebook_parse: error: {}\n",
            one_line(&err.to_string())
        )),
    }
}

fn append_markup_summary(summary: &mut String, content: &str, shape: &str) {
    let tags = content.matches('<').count();
    summary.push_str(&format!("- {shape}_tag_markers: {tags}\n"));
}

fn append_zip_like_summary(summary: &mut String, content: &str, shape: &str) {
    let entries = content
        .lines()
        .filter_map(|line| line.split_whitespace().last())
        .filter(|name| name.contains('/') || name.contains('.'))
        .map(|name| {
            name.trim_matches(|c: char| c == '/' || c == '\\')
                .to_string()
        })
        .collect::<Vec<_>>();
    let slide_count = entries
        .iter()
        .filter(|entry| entry.starts_with("ppt/slides/slide") && entry.ends_with(".xml"))
        .count();
    let media_count = entries
        .iter()
        .filter(|entry| entry.starts_with("ppt/media/"))
        .count();
    let first_entries = entries
        .iter()
        .take(8)
        .cloned()
        .collect::<Vec<_>>()
        .join(" | ");
    summary.push_str(&format!("- {shape}_entries: {}\n", entries.len()));
    if shape == "pptx" {
        summary.push_str(&format!("- pptx_slides: {slide_count}\n"));
        summary.push_str(&format!("- pptx_media: {media_count}\n"));
    }
    if !first_entries.is_empty() {
        summary.push_str(&format!("- first_entries: {first_entries}\n"));
    }
}

fn append_media_or_binary_summary(summary: &mut String, content: &str, source: &str, shape: &str) {
    summary.push_str(&format!("- {shape}_bytes_observed: {}\n", content.len()));
    if let Some(ext) = source_extension(source) {
        summary.push_str(&format!("- extension: {ext}\n"));
    }
    summary.push_str(&format!(
        "- content_hash: {}\n",
        fingerprint_normalized(content)
    ));
    if looks_binary_like(content) || shape != "pdf" {
        summary.push_str("- binary_like: true\n");
    }
}

fn classify_content_shape(content: &str) -> &'static str {
    let trimmed = content.trim_start();
    if trimmed.starts_with("{") || trimmed.starts_with("[") {
        return "json";
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
        return "html";
    }
    if lower.starts_with("<?xml") || lower.starts_with("<feed") || lower.starts_with("<rss") {
        return "xml";
    }
    if trimmed.starts_with('#') {
        return "markdown";
    }
    let mut lines = content.lines().filter(|line| !line.trim().is_empty());
    if let (Some(first), Some(second)) = (lines.next(), lines.next()) {
        if first.contains(',') && second.contains(',') {
            return "csv";
        }
    }
    "text"
}

fn shape_from_source(source: &str) -> Option<&'static str> {
    let lower = source.to_ascii_lowercase();
    if lower.ends_with(".json") {
        Some("json")
    } else if lower.ends_with(".csv") {
        Some("csv")
    } else if lower.ends_with(".md") || lower.ends_with(".markdown") {
        Some("markdown")
    } else if lower.ends_with(".ipynb") {
        Some("notebook")
    } else if lower.ends_with(".pptx") {
        Some("pptx")
    } else if lower.ends_with(".zip") || lower.ends_with(".jar") || lower.ends_with(".docx") {
        Some("zip")
    } else if matches!(
        source_extension(&lower).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")
    ) {
        Some("image")
    } else if lower.ends_with(".pdf") {
        Some("pdf")
    } else if matches!(
        source_extension(&lower).as_deref(),
        Some("bin" | "dat" | "parquet" | "sqlite" | "db")
    ) {
        Some("binary")
    } else if lower.ends_with(".html") || lower.ends_with(".htm") {
        Some("html")
    } else if lower.ends_with(".xml") {
        Some("xml")
    } else {
        None
    }
}

fn source_extension(source: &str) -> Option<String> {
    let clean = source
        .split(['?', '#'])
        .next()
        .unwrap_or(source)
        .trim_end_matches('/');
    let ext = clean.rsplit_once('.')?.1;
    if ext.is_empty() || ext.contains('/') || ext.len() > 12 {
        None
    } else {
        Some(ext.to_ascii_lowercase())
    }
}

fn split_csv_line(line: &str) -> Vec<&str> {
    line.split(',').collect()
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn looks_binary_like(content: &str) -> bool {
    content
        .chars()
        .take(512)
        .any(|c| c == '\0' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
}

fn fingerprint_normalized(content: &str) -> String {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn head_tail_excerpt(content: &str, max_chars: usize) -> String {
    let count = content.chars().count();
    if count <= max_chars {
        return content.to_string();
    }
    let head_len = max_chars / 2;
    let tail_len = max_chars.saturating_sub(head_len);
    let head: String = content.chars().take(head_len).collect();
    let tail: String = content
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!(
        "{head}\n... [middle omitted: {} chars] ...\n{tail}",
        count.saturating_sub(max_chars)
    )
}

fn cap_for_tool(tool_name: &str) -> usize {
    crate::tools::lookup_tool_spec(tool_name)
        .map(|spec| spec.max_result_size_chars)
        .unwrap_or(POLICY_SOFT_CAP_CHARS)
}

fn render_manifest(
    tool_name: &str,
    input: &serde_json::Value,
    output: &crate::tools::ToolOutput,
    original_chars: usize,
    cap: usize,
) -> String {
    let tail = tail_chars(&output.content, POLICY_TAIL_CHARS);
    let mut out = format!(
        "{TOOL_RESULT_MANIFEST_PREFIX}\ntool: {tool_name}\nstatus: {}\noriginal_chars: {original_chars}\ncap_chars: {cap}\n",
        if output.is_error { "error" } else { "ok" }
    );
    if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
        out.push_str(&format!("path: {path}\n"));
    }
    if let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) {
        out.push_str(&format!("pattern: {pattern}\n"));
    }
    out.push_str("\nTail excerpt:\n");
    out.push_str(&tail);
    out.push_str("\n\nSuggested next steps:\n");
    out.push_str(&suggested_next_steps(tool_name, input));
    out
}

fn suggested_next_steps(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "read_file" => {
            if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                format!(
                    "- read_file {{\"path\":\"{path}\",\"offset\":<line>,\"limit\":120}}\n- grep {{\"path\":\"{path}\",\"pattern\":\"<narrow-pattern>\",\"context\":4}}"
                )
            } else {
                "- Re-read a smaller offset/limit window or use grep with a narrower pattern.".to_string()
            }
        }
        "grep" | "search_files" => "- Use a narrower pattern, glob/type filter, output_mode=count/files_with_matches, or lower head_limit.".to_string(),
        "notebook_edit" => "- Re-read fewer cells with limit, then update/read a specific cell by index.".to_string(),
        "list_files" => "- Pass a more specific path or lower/raise limit within the tool maximum.".to_string(),
        "glob" => "- Use a narrower pattern/path or raise limit within the tool maximum.".to_string(),
        "shell_exec" => "- Do not re-read the full stdout/stderr just to recover the omitted text. Rerun a narrower command against the original source, or use grep/read_file only on an explicit evidence path when you need a small targeted slice.".to_string(),
        _ => "- Narrow the tool call or inspect a smaller slice of the result.".to_string(),
    }
}

fn tail_chars(content: &str, max_chars: usize) -> String {
    let count = content.chars().count();
    if count <= max_chars {
        return content.to_string();
    }
    content.chars().skip(count - max_chars).collect()
}

pub fn tool_lookup_from_messages(
    messages: &[crate::llm::Message],
) -> std::collections::HashMap<String, (String, serde_json::Value)> {
    let mut lookup = std::collections::HashMap::new();
    for msg in messages {
        for block in &msg.content {
            if let ContentBlock::ToolUse { id, name, input } = block {
                lookup.insert(id.clone(), (name.clone(), input.clone()));
            }
        }
    }
    lookup
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_exec_refs_discourage_full_stdout_rereads() {
        let mut state = ToolResultContextState::default();
        let output = crate::tools::ToolOutput {
            content: "line\n".repeat(1000),
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/tmp/evidence/stdout.txt",
                "command_family": "fetch",
                "content_shape": "text",
            })),
        };

        let (content, report) = apply_tool_result_context_policy(
            &mut state,
            1,
            "call-1",
            "shell_exec",
            &serde_json::json!({"command": "curl https://example.com"}),
            &output,
        );

        assert!(report.is_some());
        assert!(content.contains("Do not re-read the full stdout/stderr"));
        assert!(content.contains("Rerun a narrower command against the original source"));
    }
    #[test]
    fn policy_skips_existing_sentinels() {
        for content in [
            "[shell_exec_output_compacted]\n...",
            "[tool_result_manifest]\n...",
            "[tool_result_ref]\n...",
            "[tool_result_repeat]\n...",
            "[tool_summary]\n...",
            "[result truncated to keep context lean. ...",
        ] {
            let mut output = crate::tools::ToolOutput {
                content: content.to_string(),
                is_error: false,
                meta: None,
            };
            let report = apply_tool_result_policy(
                "t1",
                "read_file",
                &serde_json::json!({"path": "a.txt"}),
                &mut output,
            );
            assert!(report.is_none());
            assert_eq!(output.content, content);
        }
    }

    #[test]
    fn policy_compacts_large_tool_result_with_tool_context() {
        let mut output = crate::tools::ToolOutput {
            content: "x".repeat(20 * 1024),
            is_error: false,
            meta: None,
        };
        let report = apply_tool_result_policy(
            "t1",
            "read_file",
            &serde_json::json!({"path": "src/lib.rs"}),
            &mut output,
        )
        .expect("large output should be compacted");

        assert_eq!(report.tool_name, "read_file");
        assert!(output.content.starts_with(TOOL_RESULT_MANIFEST_PREFIX));
        assert!(output.content.contains("tool: read_file"));
        assert!(output.content.contains("path: src/lib.rs"));
        assert!(output.content.contains("Suggested next steps"));
        assert!(output.content.chars().count() < report.original_chars);
    }

    #[test]
    fn context_policy_inlines_tiny_output() {
        let mut state = ToolResultContextState::default();
        let output = crate::tools::ToolOutput {
            content: "short result".to_string(),
            is_error: false,
            meta: None,
        };
        let (content, report) = apply_tool_result_context_policy(
            &mut state,
            1,
            "t1",
            "shell_exec",
            &serde_json::json!({"command": "printf short"}),
            &output,
        );
        assert_eq!(content, "short result");
        assert!(report.is_none());
    }

    #[test]
    fn context_policy_refs_medium_shell_output() {
        let mut state = ToolResultContextState::default();
        let output = crate::tools::ToolOutput {
            content: "line\n".repeat(700),
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/tmp/evidence/stdout.txt",
                "command_family": "fetch",
                "content_source": "https://example.com/page",
                "content_shape": "html"
            })),
        };
        let (content, report) = apply_tool_result_context_policy(
            &mut state,
            2,
            "t2",
            "shell_exec",
            &serde_json::json!({"command": "curl https://example.com/page"}),
            &output,
        );
        assert!(content.starts_with(TOOL_RESULT_REF_PREFIX));
        assert!(content.contains("evidence: /tmp/evidence/stdout.txt"));
        assert!(content.contains("command_family: fetch"));
        let report = report.expect("medium shell output should be referenced");
        assert_eq!(report.mode, ContextRenderMode::EvidenceRef);
        assert!(report.context_chars < report.original_chars);
    }

    #[test]
    fn context_policy_suppresses_exact_repeat() {
        let mut state = ToolResultContextState::default();
        let output = crate::tools::ToolOutput {
            content: "repeat me\n".repeat(400),
            is_error: false,
            meta: Some(serde_json::json!({"stdout_path": "/tmp/one/stdout.txt"})),
        };
        let _ = apply_tool_result_context_policy(
            &mut state,
            3,
            "first",
            "shell_exec",
            &serde_json::json!({"command": "cat a.txt"}),
            &output,
        );
        let (content, report) = apply_tool_result_context_policy(
            &mut state,
            4,
            "second",
            "shell_exec",
            &serde_json::json!({"command": "cat a.txt"}),
            &output,
        );
        assert!(content.starts_with(TOOL_RESULT_REPEAT_PREFIX));
        assert!(content.contains("same_output_as: step 3 / first"));
        assert_eq!(
            report.expect("repeat should report").mode,
            ContextRenderMode::RepeatRef
        );
    }

    #[test]
    fn context_policy_refs_medium_shell_output_above_new_inline_threshold() {
        let mut state = ToolResultContextState::default();
        let output = crate::tools::ToolOutput {
            content: "x".repeat(3200),
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/tmp/evidence/stdout.txt",
                "command_family": "file_dump",
                "content_shape": "text"
            })),
        };
        let (content, report) = apply_tool_result_context_policy(
            &mut state,
            6,
            "medium",
            "shell_exec",
            &serde_json::json!({"command": "cat report.txt"}),
            &output,
        );
        assert!(content.starts_with(TOOL_RESULT_REF_PREFIX));
        assert_eq!(
            report
                .expect("medium shell output should be referenced")
                .mode,
            ContextRenderMode::EvidenceRef
        );
    }

    #[test]
    fn context_policy_keeps_inline_when_reference_would_be_longer() {
        let mut state = ToolResultContextState::default();
        let output = crate::tools::ToolOutput {
            content: "x".repeat(1700),
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/very/long/evidence/path/that/makes/the/reference/card/expensive/stdout.txt",
                "command_family": "file_dump",
                "content_shape": "text"
            })),
        };
        let (content, report) = apply_tool_result_context_policy(
            &mut state,
            8,
            "inline-short-medium",
            "shell_exec",
            &serde_json::json!({"command": "cat report.txt"}),
            &output,
        );
        assert_eq!(content, output.content);
        assert!(report.is_none());
    }
    #[test]
    fn context_policy_rerenders_legacy_truncated_shell_output_as_ref() {
        let mut state = ToolResultContextState::default();
        let output = crate::tools::ToolOutput {
            content: format!(
                "[... truncated 7561 bytes from head ...]\n{}",
                "body\n".repeat(500)
            ),
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/tmp/evidence/stdout.txt",
                "command_family": "fetch",
                "content_source": "https://example.com/page",
                "content_shape": "html"
            })),
        };
        let (content, report) = apply_tool_result_context_policy(
            &mut state,
            7,
            "legacy",
            "shell_exec",
            &serde_json::json!({"command": "curl https://example.com/page"}),
            &output,
        );
        assert!(content.starts_with(TOOL_RESULT_REF_PREFIX));
        assert!(content.contains("source: https://example.com/page"));
        assert_eq!(
            report
                .expect("legacy truncated output should still report")
                .mode,
            ContextRenderMode::EvidenceRef
        );
    }

    #[test]
    fn context_policy_summarizes_json_artifact_dump() {
        let mut state = ToolResultContextState::default();
        let content = format!(
            "{}{}",
            serde_json::json!({"alpha": 1, "beta": true, "items": [1, 2, 3]}),
            "\n"
        )
        .repeat(260);
        let output = crate::tools::ToolOutput {
            content,
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/tmp/evidence/stdout.txt",
                "content_source": "reports/summary.json",
                "content_shape": "json"
            })),
        };
        let (rendered, report) = apply_tool_result_context_policy(
            &mut state,
            5,
            "artifact-json",
            "shell_exec",
            &serde_json::json!({"command": "cat reports/summary.json"}),
            &output,
        );
        assert!(rendered.starts_with(TOOL_RESULT_REF_PREFIX));
        assert!(rendered.contains("Artifact summary:"));
        assert!(rendered.contains("json_parse:"));
        assert_eq!(
            report.expect("artifact dump should be referenced").mode,
            ContextRenderMode::EvidenceRef
        );
    }

    #[test]
    fn context_policy_summarizes_csv_markdown_notebook_artifact_dumps() {
        let notebook_cells = (0..80)
            .map(|idx| {
                serde_json::json!({
                    "cell_type": if idx % 2 == 0 { "markdown" } else { "code" },
                    "source": [format!("# Cell {idx}\n"), "content line\n".repeat(20)]
                })
            })
            .collect::<Vec<_>>();
        let notebook_content = serde_json::json!({ "cells": notebook_cells }).to_string();
        let cases = [
            (
                "artifact-csv",
                "cat reports/data.csv",
                "a,b,c\n1,2,3\n4,5,6\n".repeat(260),
                "reports/data.csv",
                "csv_columns:",
            ),
            (
                "artifact-md",
                "cat reports/summary.md",
                "# Title\n\n## Finding\nbody\n".repeat(260),
                "reports/summary.md",
                "markdown_headings:",
            ),
            (
                "artifact-ipynb",
                "cat analysis.ipynb",
                notebook_content.clone(),
                "analysis.ipynb",
                "notebook_cells:",
            ),
        ];

        for (tool_use_id, command, content, source, expected) in cases {
            let mut state = ToolResultContextState::default();
            let output = crate::tools::ToolOutput {
                content,
                is_error: false,
                meta: Some(serde_json::json!({
                    "stdout_path": format!("/tmp/evidence/{tool_use_id}/stdout.txt"),
                    "content_source": source,
                })),
            };
            let (rendered, report) = apply_tool_result_context_policy(
                &mut state,
                5,
                tool_use_id,
                "shell_exec",
                &serde_json::json!({"command": command}),
                &output,
            );
            assert!(rendered.starts_with(TOOL_RESULT_REF_PREFIX));
            assert!(rendered.contains("Artifact summary:"));
            assert!(rendered.contains(expected), "rendered: {rendered}");
            assert_eq!(
                report.expect("artifact dump should be referenced").mode,
                ContextRenderMode::EvidenceRef
            );
        }
    }

    #[test]
    fn context_policy_summarizes_pptx_image_and_binary_artifacts() {
        let cases = [
            (
                "artifact-pptx",
                "unzip -l deck.pptx",
                "Archive: deck.pptx\n  120 ppt/slides/slide1.xml\n  130 ppt/slides/slide2.xml\n  80 ppt/media/image1.png\n".repeat(90),
                "deck.pptx",
                "pptx_slides:",
            ),
            (
                "artifact-image",
                "xxd image.png",
                "89504e470d0a1a0a\n".repeat(700),
                "image.png",
                "image_bytes_observed:",
            ),
            (
                "artifact-binary",
                "xxd data.bin",
                "00000000: 0001 0203 0405\n".repeat(700),
                "data.bin",
                "binary_bytes_observed:",
            ),
        ];

        for (tool_use_id, command, content, source, expected) in cases {
            let mut state = ToolResultContextState::default();
            let output = crate::tools::ToolOutput {
                content,
                is_error: false,
                meta: Some(serde_json::json!({
                    "stdout_path": format!("/tmp/evidence/{tool_use_id}/stdout.txt"),
                    "content_source": source,
                })),
            };
            let (rendered, report) = apply_tool_result_context_policy(
                &mut state,
                5,
                tool_use_id,
                "shell_exec",
                &serde_json::json!({"command": command}),
                &output,
            );
            assert!(rendered.starts_with(TOOL_RESULT_REF_PREFIX));
            assert!(rendered.contains("Artifact summary:"));
            assert!(rendered.contains(expected), "rendered: {rendered}");
            assert_eq!(
                report.expect("artifact dump should be referenced").mode,
                ContextRenderMode::EvidenceRef
            );
        }
    }

    #[test]
    fn context_policy_suppresses_same_source_repeat_for_immutable_sources() {
        let mut state = ToolResultContextState::default();
        let first = crate::tools::ToolOutput {
            content: "alpha\n".repeat(700),
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/tmp/one/stdout.txt",
                "content_source": "https://example.com/report",
                "content_shape": "html"
            })),
        };
        let second = crate::tools::ToolOutput {
            content: "beta\n".repeat(700),
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/tmp/two/stdout.txt",
                "content_source": "https://example.com/report",
                "content_shape": "html"
            })),
        };

        let _ = apply_tool_result_context_policy(
            &mut state,
            9,
            "source-first",
            "shell_exec",
            &serde_json::json!({"command": "curl https://example.com/report"}),
            &first,
        );
        let (content, report) = apply_tool_result_context_policy(
            &mut state,
            10,
            "source-second",
            "shell_exec",
            &serde_json::json!({"command": "curl https://example.com/report"}),
            &second,
        );

        assert!(content.starts_with(TOOL_RESULT_REPEAT_PREFIX));
        assert!(content.contains("same_source_as: step 9 / source-first"));
        assert!(content.contains("narrower grep/read"));
        let report = report.expect("same-source repeat should report");
        assert_eq!(report.mode, ContextRenderMode::RepeatRef);
        assert_eq!(report.repeat_of.as_deref(), Some("step 9 / source-first"));
    }

    #[test]
    fn context_policy_does_not_suppress_changed_workspace_source() {
        let mut state = ToolResultContextState::default();
        let first = crate::tools::ToolOutput {
            content: "old\n".repeat(700),
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/tmp/one/stdout.txt",
                "content_source": "reports/analysis.md",
                "content_shape": "text"
            })),
        };
        let second = crate::tools::ToolOutput {
            content: "new\n".repeat(700),
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/tmp/two/stdout.txt",
                "content_source": "reports/analysis.md",
                "content_shape": "text"
            })),
        };

        let _ = apply_tool_result_context_policy(
            &mut state,
            9,
            "source-first",
            "shell_exec",
            &serde_json::json!({"command": "cat reports/analysis.md"}),
            &first,
        );
        let (content, report) = apply_tool_result_context_policy(
            &mut state,
            10,
            "source-second",
            "shell_exec",
            &serde_json::json!({"command": "cat reports/analysis.md"}),
            &second,
        );

        assert!(!content.starts_with(TOOL_RESULT_REPEAT_PREFIX));
        assert!(report.is_some());
    }

    #[test]
    fn context_policy_batch_applies_per_message_budget() {
        let mut state = ToolResultContextState::default();
        let outputs = [
            crate::tools::ToolOutput {
                content: "a".repeat(5 * 1024),
                is_error: false,
                meta: Some(serde_json::json!({"content_source": "custom://a"})),
            },
            crate::tools::ToolOutput {
                content: "b".repeat(5 * 1024),
                is_error: false,
                meta: Some(serde_json::json!({"content_source": "custom://b"})),
            },
            crate::tools::ToolOutput {
                content: "c".repeat(5 * 1024),
                is_error: false,
                meta: Some(serde_json::json!({"content_source": "custom://c"})),
            },
        ];
        let input_values = [
            serde_json::json!({"name": "a"}),
            serde_json::json!({"name": "b"}),
            serde_json::json!({"name": "c"}),
        ];
        let inputs = outputs
            .iter()
            .zip(input_values.iter())
            .enumerate()
            .map(|(idx, (output, input))| ToolResultContextInput {
                step: 11,
                tool_use_id: match idx {
                    0 => "batch-a",
                    1 => "batch-b",
                    _ => "batch-c",
                },
                tool_name: "custom_tool",
                input,
                output,
            })
            .collect::<Vec<_>>();

        let rendered = ToolResultContextPolicy::apply_batch(&mut state, &inputs);
        assert_eq!(rendered.len(), outputs.len());
        assert!(rendered.iter().any(|item| item
            .report
            .as_ref()
            .is_some_and(|report| report.per_message_budget_replaced)));
        assert!(rendered
            .iter()
            .any(|item| item.content.starts_with(TOOL_RESULT_REF_PREFIX)));
        assert!(
            rendered
                .iter()
                .map(|item| item.content.chars().count())
                .sum::<usize>()
                <= TOOL_RESULT_MESSAGE_BUDGET_CHARS
        );
    }

    #[test]
    fn context_policy_replacement_is_stable_for_same_tool_use() {
        let mut state = ToolResultContextState::default();
        let output = crate::tools::ToolOutput {
            content: "stable\n".repeat(700),
            is_error: false,
            meta: Some(serde_json::json!({
                "stdout_path": "/tmp/stable/stdout.txt",
                "content_source": "stable.txt"
            })),
        };

        let (first, report) = apply_tool_result_context_policy(
            &mut state,
            12,
            "stable-tool-use",
            "shell_exec",
            &serde_json::json!({"command": "cat stable.txt"}),
            &output,
        );
        let (second, second_report) = apply_tool_result_context_policy(
            &mut state,
            13,
            "stable-tool-use",
            "shell_exec",
            &serde_json::json!({"command": "cat stable.txt"}),
            &output,
        );

        assert!(first.starts_with(TOOL_RESULT_REF_PREFIX));
        assert_eq!(first, second);
        assert!(report.is_some());
        assert!(second_report.is_none());
    }
    #[test]
    fn policy_to_messages_uses_tool_use_mapping() {
        let mut messages = vec![
            crate::llm::Message {
                role: crate::llm::MessageRole::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "tu-1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({"path": "src/main.rs"}),
                }],
                cache_control: None,
            },
            crate::llm::Message {
                role: crate::llm::MessageRole::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tu-1".to_string(),
                    content: "x".repeat(20 * 1024),
                    is_error: false,
                }],
                cache_control: None,
            },
        ];

        let reports = apply_policy_to_messages(&mut messages);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].tool_name, "read_file");
        let crate::llm::ContentBlock::ToolResult { content, .. } = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        assert!(content.contains("tool: read_file"));
        assert!(content.contains("path: src/main.rs"));
    }
}
