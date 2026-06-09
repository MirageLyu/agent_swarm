use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read as IoRead;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskContract {
    #[serde(default)]
    pub final_response: Option<FinalResponseContract>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactContract>,
    #[serde(default)]
    pub source_grounding: Option<SourceGroundingContract>,
    #[serde(default)]
    pub completion_policy: CompletionPolicy,
}

impl TaskContract {
    pub fn is_empty(&self) -> bool {
        self.final_response.is_none()
            && self.artifacts.is_empty()
            && self.source_grounding.is_none()
            && self.completion_policy == CompletionPolicy::default()
    }

    pub fn requires_direct_response(&self) -> bool {
        self.final_response
            .as_ref()
            .map(|contract| contract.required)
            .unwrap_or(false)
            && self.artifacts.is_empty()
    }

    pub fn required_artifact_paths(&self) -> Vec<String> {
        self.artifacts
            .iter()
            .filter(|artifact| artifact.required)
            .map(|artifact| artifact.path.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FinalResponseContract {
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub format: FinalResponseFormat,
    #[serde(default)]
    pub fenced: bool,
    #[serde(default)]
    pub exact_text: Option<String>,
    #[serde(default)]
    pub required_json_keys: Vec<String>,
    #[serde(default)]
    pub array_lengths: Vec<JsonArrayLengthContract>,
    #[serde(default)]
    pub require_non_empty: bool,
    #[serde(default)]
    pub no_extra_explanation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FinalResponseFormat {
    #[default]
    Any,
    Json,
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JsonArrayLengthContract {
    pub key: String,
    pub len: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactContract {
    pub path: String,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub kind: ArtifactKind,
    #[serde(default)]
    pub require_non_empty: bool,
    #[serde(default)]
    pub required_json_keys: Vec<String>,
    #[serde(default)]
    pub csv_header: Option<Vec<String>>,
    #[serde(default)]
    pub min_rows: Option<usize>,
    #[serde(default)]
    pub min_non_ws_chars: Option<usize>,
    #[serde(default)]
    pub max_non_ws_chars: Option<usize>,
    #[serde(default)]
    pub min_text_chars: Option<usize>,
    #[serde(default)]
    pub required_headings: Vec<String>,
    #[serde(default)]
    pub required_terms_any: Vec<TermRequirement>,
    #[serde(default)]
    pub forbidden_placeholders: bool,
    #[serde(default)]
    pub require_static_visible_derived_values: bool,
    #[serde(default)]
    pub pptx: Option<PptxContract>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    #[default]
    Infer,
    Json,
    Csv,
    Markdown,
    Notebook,
    Pptx,
    Image,
    Binary,
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TermRequirement {
    pub label: String,
    pub any: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PptxContract {
    #[serde(default)]
    pub require_slides: bool,
    #[serde(default)]
    pub require_media: bool,
    #[serde(default)]
    pub min_text_chars: Option<usize>,
    #[serde(default)]
    pub final_slide_required_terms_any: Vec<TermRequirement>,
    #[serde(default)]
    pub final_slide_min_number_markers: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceGroundingContract {
    #[serde(default)]
    pub required_markers: Vec<String>,
    #[serde(default)]
    pub evidence_files: Vec<String>,
    #[serde(default)]
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionPolicy {
    #[serde(default)]
    pub self_check_before_complete: bool,
    #[serde(default)]
    pub create_artifacts_early: bool,
    #[serde(default)]
    pub stop_exploration_during_repair: bool,
}

impl Default for CompletionPolicy {
    fn default() -> Self {
        Self {
            self_check_before_complete: false,
            create_artifacts_early: false,
            stop_exploration_during_repair: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ContractReport {
    pub name: String,
    pub target: String,
    pub passed: bool,
    pub error: Option<String>,
    pub repair_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContractRunResult {
    pub all_passed: bool,
    pub reports: Vec<ContractReport>,
}

impl ContractRunResult {
    pub fn format_failure_for_agent(&self) -> String {
        let mut out = String::from(
            "[Task Contract Validation Failed]\nYou called task_complete, but the declared task contract is not satisfied. Repair only the failing final answer or artifact(s), then call task_complete again.\n\n",
        );
        for report in &self.reports {
            if report.passed {
                out.push_str(&format!("- {}:{} ✓ passed\n", report.name, report.target));
            } else {
                out.push_str(&format!(
                    "- {}:{} ✗ FAILED: {}\n",
                    report.name,
                    report.target,
                    report.error.as_deref().unwrap_or("unknown error")
                ));
                if let Some(hint) = &report.repair_hint {
                    out.push_str(&format!("  repair: {hint}\n"));
                }
            }
        }
        out
    }
}

pub struct ContractContext<'a> {
    pub repo_root: &'a Path,
    pub completion_summary: Option<&'a str>,
}

pub fn validate_task_contract(
    contract: &TaskContract,
    ctx: &ContractContext<'_>,
) -> ContractRunResult {
    let mut reports = Vec::new();
    if let Some(final_response) = &contract.final_response {
        reports.extend(validate_final_response(final_response, ctx));
    }
    for artifact in &contract.artifacts {
        reports.extend(validate_artifact(artifact, ctx));
    }
    if let Some(source_grounding) = &contract.source_grounding {
        reports.push(validate_source_grounding(source_grounding, ctx));
    }
    ContractRunResult {
        all_passed: reports.iter().all(|report| report.passed),
        reports,
    }
}

fn validate_final_response(
    contract: &FinalResponseContract,
    ctx: &ContractContext<'_>,
) -> Vec<ContractReport> {
    let mut reports = Vec::new();
    let summary = ctx.completion_summary.unwrap_or("").trim();
    let target = "task_complete.summary".to_string();
    if contract.required && summary.is_empty() {
        reports.push(fail(
            "final_response",
            &target,
            "task_complete.summary is empty",
            "Provide the actual final answer in task_complete.summary.",
        ));
        return reports;
    }
    if contract.require_non_empty && summary.is_empty() {
        reports.push(fail(
            "final_response",
            &target,
            "task_complete.summary must be non-empty",
            "Return the requested final answer, not an empty completion.",
        ));
        return reports;
    }
    if let Some(expected) = &contract.exact_text {
        if summary != expected {
            reports.push(fail(
                "final_response",
                &target,
                &format!("task_complete.summary must be exactly `{expected}`"),
                "Replace the summary with the exact required text and no extra explanation.",
            ));
        }
    }
    match contract.format {
        FinalResponseFormat::Json => match extract_response_json(summary, contract.fenced) {
            Ok((json_text, value)) => {
                if contract.no_extra_explanation && !is_exact_json_payload(summary, json_text, contract.fenced) {
                    reports.push(fail(
                        "final_response",
                        &target,
                        "task_complete.summary contains extra explanation around the requested JSON",
                        "Return only the requested JSON payload, preserving required fences if requested.",
                    ));
                }
                for key in &contract.required_json_keys {
                    if !value.get(key).is_some() {
                        reports.push(fail(
                            "final_response_json_key",
                            key,
                            &format!("missing required JSON key `{key}`"),
                            "Add the missing key to the final JSON answer and keep the JSON parseable.",
                        ));
                    }
                }
                for array_contract in &contract.array_lengths {
                    match value.get(&array_contract.key).and_then(|v| v.as_array()) {
                        Some(values) if values.len() == array_contract.len => {}
                        Some(values) => reports.push(fail(
                            "final_response_json_array_length",
                            &array_contract.key,
                            &format!(
                                "JSON array `{}` must contain exactly {} item(s), got {}",
                                array_contract.key,
                                array_contract.len,
                                values.len()
                            ),
                            "Adjust the array length while preserving all required content.",
                        )),
                        None => reports.push(fail(
                            "final_response_json_array_length",
                            &array_contract.key,
                            &format!("JSON key `{}` must be an array", array_contract.key),
                            "Make the field an array with the required number of items.",
                        )),
                    }
                }
            }
            Err(err) => reports.push(fail(
                "final_response_json",
                &target,
                &err.to_string(),
                "Call task_complete with the requested valid JSON answer; include markdown fences only if the task contract requires them.",
            )),
        },
        FinalResponseFormat::Text => {
            if contract.fenced && extract_fenced_block(summary, "text").is_none() {
                reports.push(fail(
                    "final_response_text_block",
                    &target,
                    "task_complete.summary missing required fenced text code block",
                    "Wrap the requested final text in a ```text fenced block and remove unrelated prose.",
                ));
            }
        }
        FinalResponseFormat::Any => {}
    }
    if reports.is_empty() {
        reports.push(pass("final_response", &target));
    }
    reports
}

fn validate_artifact(
    artifact: &ArtifactContract,
    ctx: &ContractContext<'_>,
) -> Vec<ContractReport> {
    let mut reports = Vec::new();
    let rel_path = match checked_relative_path(&artifact.path) {
        Ok(path) => path,
        Err(err) => {
            reports.push(fail(
                "artifact_path",
                &artifact.path,
                &err,
                "Use a non-empty relative artifact path inside the workspace; do not use absolute paths or `..` traversal.",
            ));
            return reports;
        }
    };
    let path = ctx.repo_root.join(&rel_path);
    if artifact.required && !path.exists() {
        reports.push(fail(
            "artifact_exists",
            &artifact.path,
            &format!("required artifact `{}` does not exist", artifact.path),
            "Create the required artifact at the declared path before task_complete.",
        ));
        return reports;
    }
    if !path.exists() {
        return reports;
    }
    if artifact.require_non_empty {
        match fs::metadata(&path) {
            Ok(meta) if meta.len() > 0 => reports.push(pass("artifact_non_empty", &artifact.path)),
            Ok(_) => reports.push(fail(
                "artifact_non_empty",
                &artifact.path,
                "artifact is empty",
                "Write non-empty task-relevant content to the artifact.",
            )),
            Err(err) => reports.push(fail(
                "artifact_non_empty",
                &artifact.path,
                &format!("cannot stat artifact: {err}"),
                "Ensure the artifact exists and is readable.",
            )),
        }
    }
    let kind = infer_artifact_kind(artifact, &path);
    match kind {
        ArtifactKind::Json => reports.extend(validate_json_artifact(artifact, &path)),
        ArtifactKind::Csv => reports.extend(validate_csv_artifact(artifact, &path)),
        ArtifactKind::Markdown | ArtifactKind::Text => {
            reports.extend(validate_text_artifact(artifact, &path))
        }
        ArtifactKind::Notebook => reports.extend(validate_notebook_artifact(artifact, &path)),
        ArtifactKind::Pptx => reports.extend(validate_pptx_artifact(artifact, &path)),
        ArtifactKind::Image => reports.extend(validate_image_artifact(artifact, &path)),
        ArtifactKind::Binary | ArtifactKind::Infer => {
            reports.extend(validate_binary_artifact(artifact, &path))
        }
    }
    if reports.is_empty() {
        reports.push(pass("artifact", &artifact.path));
    }
    reports
}

fn validate_json_artifact(artifact: &ArtifactContract, path: &Path) -> Vec<ContractReport> {
    let target = artifact.path.as_str();
    let mut reports = Vec::new();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            reports.push(fail(
                "artifact_json",
                target,
                &format!("cannot read JSON artifact: {err}"),
                "Rewrite the artifact as valid UTF-8 JSON.",
            ));
            return reports;
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            reports.push(fail(
                "artifact_json",
                target,
                &format!("invalid JSON: {err}"),
                "Rewrite the artifact as parseable JSON.",
            ));
            return reports;
        }
    };
    reports.push(pass("artifact_json", target));
    for key in &artifact.required_json_keys {
        if value.get(key).is_none() {
            reports.push(fail(
                "artifact_json_key",
                key,
                &format!("{} missing required key `{key}`", artifact.path),
                "Add the missing key and keep the JSON parseable.",
            ));
        }
    }
    reports
}

fn validate_csv_artifact(artifact: &ArtifactContract, path: &Path) -> Vec<ContractReport> {
    let target = artifact.path.as_str();
    let mut reports = Vec::new();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            reports.push(fail(
                "artifact_csv",
                target,
                &format!("cannot read CSV artifact: {err}"),
                "Rewrite the CSV as readable UTF-8 text.",
            ));
            return reports;
        }
    };
    let rows = parse_csv_rows(&text);
    if rows.is_empty() {
        reports.push(fail(
            "artifact_csv",
            target,
            "CSV must include a header row",
            "Write a header row and the required data rows.",
        ));
        return reports;
    }
    if rows[0].is_empty() || rows[0].iter().all(|cell| cell.trim().is_empty()) {
        reports.push(fail(
            "artifact_csv",
            target,
            "CSV header is empty",
            "Write the requested CSV column header.",
        ));
    } else {
        reports.push(pass("artifact_csv", target));
    }
    if let Some(min_rows) = artifact.min_rows {
        let data_rows = rows.len().saturating_sub(1);
        if data_rows < min_rows {
            reports.push(fail(
                "artifact_csv_rows",
                target,
                &format!("CSV must include at least {min_rows} data row(s), got {data_rows}"),
                "Add the missing data rows while preserving the requested header.",
            ));
        }
    }
    if let Some(expected) = &artifact.csv_header {
        if rows.first() != Some(expected) {
            reports.push(fail(
                "artifact_csv_header",
                target,
                &format!(
                    "CSV header mismatch: expected {expected:?}, got {:?}",
                    rows.first()
                ),
                "Make the CSV header exactly match the declared columns.",
            ));
        }
    }
    reports
}

fn validate_text_artifact(artifact: &ArtifactContract, path: &Path) -> Vec<ContractReport> {
    let target = artifact.path.as_str();
    let mut reports = Vec::new();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            reports.push(fail(
                "artifact_text",
                target,
                &format!("cannot read text artifact: {err}"),
                "Rewrite the artifact as readable UTF-8 text.",
            ));
            return reports;
        }
    };
    let trimmed = text.trim();
    if let Some(min) = artifact.min_text_chars {
        if trimmed.chars().count() < min {
            reports.push(fail(
                "artifact_text_length",
                target,
                &format!("text must be at least {min} characters"),
                "Add concise task-relevant content to satisfy the required shape.",
            ));
        }
    }
    let non_ws = text.chars().filter(|c| !c.is_whitespace()).count();
    if let Some(min) = artifact.min_non_ws_chars {
        if non_ws < min {
            reports.push(fail(
                "artifact_non_ws_length",
                target,
                &format!(
                    "artifact must contain at least {min} non-whitespace characters, got {non_ws}"
                ),
                "Add task-relevant substance while preserving required structure.",
            ));
        }
    }
    if let Some(max) = artifact.max_non_ws_chars {
        if non_ws > max {
            reports.push(fail(
                "artifact_non_ws_length",
                target,
                &format!(
                    "artifact must contain at most {max} non-whitespace characters, got {non_ws}"
                ),
                "Condense low-priority prose while preserving required headings, facts, and keys.",
            ));
        }
    }
    for heading in &artifact.required_headings {
        if !text.contains(heading) {
            reports.push(fail(
                "artifact_heading",
                heading,
                &format!("missing required heading: {heading}"),
                "Add the missing heading exactly as declared.",
            ));
        }
    }
    for requirement in &artifact.required_terms_any {
        if !requirement
            .any
            .iter()
            .any(|term| text.to_lowercase().contains(&term.to_lowercase()))
        {
            reports.push(fail(
                "artifact_required_terms",
                &requirement.label,
                &format!("missing required content for `{}`", requirement.label),
                "Add concrete content that satisfies the declared requirement.",
            ));
        }
    }
    if artifact.forbidden_placeholders {
        let placeholders = placeholder_markers_in(&text);
        if !placeholders.is_empty() {
            reports.push(fail(
                "artifact_placeholders",
                target,
                &format!(
                    "artifact still contains placeholder markers: {}",
                    placeholders.join(", ")
                ),
                "Replace placeholder text with concrete task-relevant content.",
            ));
        }
    }
    if reports.is_empty() {
        reports.push(pass("artifact_text", target));
    }
    reports
}

fn validate_notebook_artifact(artifact: &ArtifactContract, path: &Path) -> Vec<ContractReport> {
    let target = artifact.path.as_str();
    let mut reports = Vec::new();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            reports.push(fail(
                "artifact_notebook",
                target,
                &format!("cannot read notebook artifact: {err}"),
                "Rewrite the notebook as readable UTF-8 `.ipynb` JSON.",
            ));
            return reports;
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            reports.push(fail(
                "artifact_notebook",
                target,
                &format!("invalid notebook JSON: {err}"),
                "Regenerate a valid `.ipynb` JSON file.",
            ));
            return reports;
        }
    };
    let Some(cells) = value.get("cells").and_then(|v| v.as_array()) else {
        reports.push(fail(
            "artifact_notebook_cells",
            target,
            "notebook JSON must contain a top-level `cells` array",
            "Create a valid notebook with a `cells` array and task-relevant cell source or outputs.",
        ));
        return reports;
    };
    if artifact.require_non_empty && cells.is_empty() {
        reports.push(fail(
            "artifact_notebook_cells",
            target,
            "notebook `cells` array is empty",
            "Add at least one task-relevant code or markdown cell before completing.",
        ));
    }
    let mut substantive = false;
    for (idx, cell) in cells.iter().enumerate() {
        let Some(obj) = cell.as_object() else {
            reports.push(fail(
                "artifact_notebook_cell",
                target,
                &format!("notebook cell {idx} must be an object"),
                "Rewrite the notebook with standard Jupyter cell objects.",
            ));
            continue;
        };
        let cell_type = obj.get("cell_type").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(cell_type, "code" | "markdown" | "raw") {
            reports.push(fail(
                "artifact_notebook_cell_type",
                target,
                &format!("notebook cell {idx} has invalid cell_type `{cell_type}`"),
                "Use standard Jupyter cell_type values such as `code` or `markdown`.",
            ));
        }
        let mut cell_source_text = String::new();
        if let Some(source) = obj.get("source") {
            match source {
                serde_json::Value::String(s) => {
                    cell_source_text = s.clone();
                    if !s.trim().is_empty() {
                        substantive = true;
                    }
                }
                serde_json::Value::Array(lines) => {
                    if lines.iter().all(|line| line.as_str().is_some()) {
                        cell_source_text = lines
                            .iter()
                            .filter_map(|line| line.as_str())
                            .collect::<String>();
                        if lines
                            .iter()
                            .filter_map(|line| line.as_str())
                            .any(|line| !line.trim().is_empty())
                        {
                            substantive = true;
                        }
                    } else {
                        reports.push(fail(
                            "artifact_notebook_source",
                            target,
                            &format!("notebook cell {idx} source array must contain only strings"),
                            "Store notebook cell source as a string or an array of strings.",
                        ));
                    }
                }
                _ => reports.push(fail(
                    "artifact_notebook_source",
                    target,
                    &format!("notebook cell {idx} source must be a string or string array"),
                    "Store notebook cell source as a string or an array of strings.",
                )),
            }
        }
        let outputs_text = obj
            .get("outputs")
            .and_then(|v| serde_json::to_string(v).ok())
            .unwrap_or_default();
        if obj
            .get("outputs")
            .and_then(|v| v.as_array())
            .map(|outputs| !outputs.is_empty())
            .unwrap_or(false)
        {
            substantive = true;
        }
        if cell_type == "code" {
            reports.extend(validate_notebook_static_visibility(
                artifact,
                target,
                idx,
                &cell_source_text,
                &outputs_text,
            ));
        }
    }
    if artifact.require_non_empty && !substantive {
        reports.push(fail(
            "artifact_notebook_content",
            target,
            "notebook has no substantive cell source or outputs",
            "Add task-relevant notebook source, markdown, or outputs before completing.",
        ));
    }
    if reports.is_empty() {
        reports.push(pass("artifact_notebook", target));
    }
    reports
}

fn validate_notebook_static_visibility(
    artifact: &ArtifactContract,
    target: &str,
    cell_idx: usize,
    source: &str,
    outputs: &str,
) -> Vec<ContractReport> {
    if !artifact.require_static_visible_derived_values || notebook_outputs_are_visible(outputs) {
        return Vec::new();
    }
    let derived_lines_without_visible_values = source
        .lines()
        .filter(|line| {
            (line.contains("statistics.mean(")
                || line.contains("mean(")
                || line.contains("summary[")
                || line.contains("summary.get("))
                && (line.contains(':') || line.contains('='))
        })
        .filter(|line| !line.contains('#'))
        .count();
    if derived_lines_without_visible_values == 0 {
        return Vec::new();
    }
    vec![fail(
        "artifact_notebook_static_value",
        target,
        &format!(
            "notebook cell {cell_idx} contains {derived_lines_without_visible_values} derived expression line(s) whose final values are not visible in source comments/assertions/literals or saved outputs"
        ),
        "Keep the derivations, but update the notebook so each derived final value is auditable without executing the notebook, for example by adding inline comments such as `# mean_value = <computed value>` or by saving cell outputs.",
    )]
}

fn notebook_outputs_are_visible(outputs: &str) -> bool {
    let trimmed = outputs.trim();
    !trimmed.is_empty() && trimmed != "[]" && trimmed != "null"
}

fn validate_pptx_artifact(artifact: &ArtifactContract, path: &Path) -> Vec<ContractReport> {
    let target = artifact.path.as_str();
    let mut reports = Vec::new();
    let Some(contract) = &artifact.pptx else {
        reports.push(pass("artifact_pptx", target));
        return reports;
    };
    let info = match inspect_pptx(path) {
        Ok(info) => info,
        Err(err) => {
            reports.push(fail(
                "artifact_pptx",
                target,
                &format!("cannot inspect PPTX: {err}"),
                "Regenerate a valid PPTX file and validate the saved artifact.",
            ));
            return reports;
        }
    };
    if contract.require_slides && info.slide_count == 0 {
        reports.push(fail(
            "artifact_pptx_slides",
            target,
            "PPTX contains no slides",
            "Regenerate the presentation with at least one slide.",
        ));
    }
    if contract.require_media && info.media_count == 0 {
        reports.push(fail(
            "artifact_pptx_media",
            target,
            "PPTX should include at least one real embedded image under ppt/media/",
            "Embed the required image into the PPTX, not only beside it on disk.",
        ));
    }
    if let Some(min) = contract.min_text_chars {
        if info.all_text.chars().count() < min {
            reports.push(fail(
                "artifact_pptx_text",
                target,
                &format!("PPTX slide text must contain at least {min} characters"),
                "Add task-relevant, machine-readable slide text to the saved presentation.",
            ));
        }
    }
    for requirement in &contract.final_slide_required_terms_any {
        if !requirement.any.iter().any(|term| {
            info.final_slide_text
                .to_lowercase()
                .contains(&term.to_lowercase())
        }) {
            reports.push(fail(
                "artifact_pptx_final_slide_terms",
                &requirement.label,
                &format!(
                    "final slide missing required content for `{}`",
                    requirement.label
                ),
                "Put the required content on the literal last slide of the saved presentation.",
            ));
        }
    }
    if let Some(min) = contract.final_slide_min_number_markers {
        let count = count_number_markers(&info.final_slide_text);
        if count < min {
            reports.push(fail(
                "artifact_pptx_final_slide_numbering",
                target,
                &format!("final slide must contain at least {min} machine-readable number marker(s), got {count}"),
                "Use plain numbering such as 1/2/3 or ①②③ on the literal last slide.",
            ));
        }
    }
    if reports.is_empty() {
        reports.push(pass("artifact_pptx", target));
    }
    reports
}

fn validate_source_grounding(
    contract: &SourceGroundingContract,
    ctx: &ContractContext<'_>,
) -> ContractReport {
    let summary = ctx.completion_summary.unwrap_or("");
    let mut markers = contract.required_markers.clone();
    for file in &contract.evidence_files {
        let Ok(rel_path) = checked_relative_path(file) else {
            continue;
        };
        let path = ctx.repo_root.join(rel_path);
        if let Ok(text) = fs::read_to_string(path) {
            markers.extend(extract_evidence_markers(&text));
        }
    }
    markers.sort();
    markers.dedup();
    let haystack = if contract.case_sensitive {
        summary.to_string()
    } else {
        summary.to_lowercase()
    };
    let missing = markers
        .iter()
        .filter(|marker| {
            let needle = if contract.case_sensitive {
                (*marker).clone()
            } else {
                marker.to_lowercase()
            };
            !haystack.contains(&needle)
        })
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        pass("source_grounding", "task_complete.summary")
    } else {
        fail(
            "source_grounding",
            "task_complete.summary",
            &format!(
                "final response should preserve exact source marker(s): {}",
                missing.join(", ")
            ),
            "Copy all missing source marker substrings verbatim into the relevant final answer fields; do not paraphrase or fix only the latest missing marker.",
        )
    }
}

fn extract_response_json<'a>(
    summary: &'a str,
    fenced: bool,
) -> Result<(&'a str, serde_json::Value)> {
    let json_text = if fenced {
        extract_fenced_block(summary, "json")
            .or_else(|| extract_fenced_block(summary, ""))
            .ok_or_else(|| anyhow!("task_complete.summary missing final JSON code block"))?
    } else {
        extract_json_value_slice(summary)?
    };
    let value = serde_json::from_str(json_text).with_context(|| {
        format!(
            "task_complete.summary contains invalid JSON: {}",
            truncate(json_text, 400)
        )
    })?;
    Ok((json_text, value))
}

fn extract_fenced_block<'a>(text: &'a str, lang: &str) -> Option<&'a str> {
    let fence = if lang.is_empty() {
        "```".to_string()
    } else {
        format!("```{lang}")
    };
    let start = text.find(&fence)?;
    let after_fence = &text[start + fence.len()..];
    let after_newline = after_fence.strip_prefix('\n').unwrap_or(after_fence);
    let end = after_newline.find("```")?;
    Some(after_newline[..end].trim())
}

fn extract_json_value_slice(text: &str) -> Result<&str> {
    let trimmed = text.trim();
    let object_start = trimmed.find('{');
    let array_start = trimmed.find('[');
    let (start, end_char) = match (object_start, array_start) {
        (Some(o), Some(a)) if a < o => (a, ']'),
        (Some(o), _) => (o, '}'),
        (None, Some(a)) => (a, ']'),
        (None, None) => anyhow::bail!("no JSON object or array in response"),
    };
    let end = trimmed
        .rfind(end_char)
        .ok_or_else(|| anyhow!("no closing `{end_char}` in response"))?;
    if end < start {
        anyhow::bail!("malformed JSON delimiters");
    }
    Ok(&trimmed[start..=end])
}

fn is_exact_json_payload(summary: &str, json_text: &str, fenced: bool) -> bool {
    let trimmed = summary.trim();
    if fenced {
        let expected = format!("```json\n{json_text}\n```");
        trimmed == expected || trimmed == format!("```\n{json_text}\n```")
    } else {
        trimmed == json_text
    }
}

fn infer_artifact_kind(artifact: &ArtifactContract, path: &Path) -> ArtifactKind {
    if artifact.kind != ArtifactKind::Infer {
        return artifact.kind.clone();
    }
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => ArtifactKind::Json,
        "csv" => ArtifactKind::Csv,
        "md" | "markdown" => ArtifactKind::Markdown,
        "ipynb" => ArtifactKind::Notebook,
        "pptx" => ArtifactKind::Pptx,
        "png" | "jpg" | "jpeg" | "gif" | "webp" => ArtifactKind::Image,
        "txt" => ArtifactKind::Text,
        _ => ArtifactKind::Binary,
    }
}

fn checked_relative_path(path: &str) -> std::result::Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("artifact path is empty".to_string());
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err("artifact path must be relative to the workspace".to_string());
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err("artifact path must not contain `..` traversal".to_string())
            }
            _ => return Err("artifact path must stay inside the workspace".to_string()),
        }
    }
    if out.as_os_str().is_empty() {
        return Err("artifact path is empty".to_string());
    }
    Ok(out)
}

fn parse_csv_rows(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.split(',')
                .map(|cell| cell.trim().trim_matches('"').to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum ImageMagic {
    Png,
    Jpeg,
    Gif,
    Webp,
}

fn image_magic_for_path(path: &Path) -> Option<ImageMagic> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some(ImageMagic::Png),
        "jpg" | "jpeg" => Some(ImageMagic::Jpeg),
        "gif" => Some(ImageMagic::Gif),
        "webp" => Some(ImageMagic::Webp),
        _ => None,
    }
}

fn looks_like_known_image(header: &[u8]) -> bool {
    header.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
        || header.starts_with(&[0xff, 0xd8, 0xff])
        || header.starts_with(b"GIF87a")
        || header.starts_with(b"GIF89a")
        || (header.len() >= 12 && header.starts_with(b"RIFF") && &header[8..12] == b"WEBP")
}

fn read_prefix(path: &Path, max_len: usize) -> Result<Vec<u8>> {
    let mut file =
        fs::File::open(path).with_context(|| format!("open `{}` failed", path.display()))?;
    let mut buf = vec![0u8; max_len];
    let n = file
        .read(&mut buf)
        .with_context(|| format!("read `{}` failed", path.display()))?;
    buf.truncate(n);
    Ok(buf)
}

struct PptxInfo {
    slide_count: usize,
    media_count: usize,
    all_text: String,
    final_slide_text: String,
}

fn validate_image_artifact(artifact: &ArtifactContract, path: &Path) -> Vec<ContractReport> {
    let target = artifact.path.as_str();
    let mut reports = Vec::new();
    let header = match read_prefix(path, 32) {
        Ok(header) => header,
        Err(err) => {
            reports.push(fail(
                "artifact_image",
                target,
                &format!("cannot inspect image artifact: {err}"),
                "Regenerate a readable image file at the declared path.",
            ));
            return reports;
        }
    };
    if header.is_empty() {
        reports.push(fail(
            "artifact_image",
            target,
            "image file is empty",
            "Regenerate a non-empty image file.",
        ));
        return reports;
    }
    let expected = image_magic_for_path(path);
    let valid = match expected {
        Some(ImageMagic::Png) => {
            header.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
        }
        Some(ImageMagic::Jpeg) => header.starts_with(&[0xff, 0xd8, 0xff]),
        Some(ImageMagic::Gif) => header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a"),
        Some(ImageMagic::Webp) => {
            header.len() >= 12 && header.starts_with(b"RIFF") && &header[8..12] == b"WEBP"
        }
        None => looks_like_known_image(&header),
    };
    if !valid {
        reports.push(fail(
            "artifact_image_magic",
            target,
            "image header does not match a supported PNG/JPEG/GIF/WebP file",
            "Regenerate the artifact as a real image file, not a text placeholder or broken binary.",
        ));
    }
    if reports.is_empty() {
        reports.push(pass("artifact_image", target));
    }
    reports
}

fn validate_binary_artifact(artifact: &ArtifactContract, path: &Path) -> Vec<ContractReport> {
    let target = artifact.path.as_str();
    let mut reports = Vec::new();
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) => {
            reports.push(fail(
                "artifact_binary",
                target,
                &format!("cannot stat binary artifact: {err}"),
                "Ensure the binary artifact exists and is readable.",
            ));
            return reports;
        }
    };
    if !metadata.is_file() {
        reports.push(fail(
            "artifact_binary",
            target,
            "artifact path is not a regular file",
            "Write the required artifact as a regular file.",
        ));
        return reports;
    }
    if metadata.len() == 0 {
        reports.push(fail(
            "artifact_binary",
            target,
            "binary artifact is empty",
            "Write non-empty binary content to the artifact.",
        ));
        return reports;
    }
    let prefix = match read_prefix(path, 256) {
        Ok(prefix) => prefix,
        Err(err) => {
            reports.push(fail(
                "artifact_binary",
                target,
                &format!("cannot read binary artifact prefix: {err}"),
                "Ensure the binary artifact is readable.",
            ));
            return reports;
        }
    };
    if prefix.iter().all(|b| *b == 0) {
        reports.push(fail(
            "artifact_binary_content",
            target,
            "binary artifact prefix is all zero bytes",
            "Regenerate the artifact with real task output, not an empty placeholder buffer.",
        ));
    }
    if let Ok(text) = std::str::from_utf8(&prefix) {
        let trimmed = text.trim_matches(char::from(0)).trim();
        if !trimmed.is_empty() && placeholder_markers_in(trimmed).len() > 0 {
            reports.push(fail(
                "artifact_binary_placeholder",
                target,
                "binary artifact prefix contains placeholder text",
                "Replace placeholder content with a real generated artifact.",
            ));
        }
    }
    if reports.is_empty() {
        reports.push(pass("artifact_binary", target));
    }
    reports
}

fn inspect_pptx(path: &Path) -> Result<PptxInfo> {
    let prefix = read_prefix(path, 4)?;
    if !prefix.starts_with(b"PK\x03\x04")
        && !prefix.starts_with(b"PK\x05\x06")
        && !prefix.starts_with(b"PK\x07\x08")
    {
        anyhow::bail!("PPTX is not a valid ZIP container");
    }
    let output = std::process::Command::new("python3")
        .arg("-")
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                stdin.write_all(PPTX_INSPECT_SCRIPT.as_bytes())?;
            }
            child.wait_with_output()
        })
        .with_context(|| "failed to run python3 PPTX inspector")?;
    if !output.status.success() {
        anyhow::bail!(
            "PPTX inspector failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .with_context(|| "PPTX inspector returned invalid JSON")?;
    Ok(PptxInfo {
        slide_count: value
            .get("slide_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        media_count: value
            .get("media_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        all_text: value
            .get("all_text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        final_slide_text: value
            .get("final_slide_text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

const PPTX_INSPECT_SCRIPT: &str = r#"
import json, sys, zipfile, xml.etree.ElementTree as ET
path = sys.argv[1]
with zipfile.ZipFile(path) as zf:
    names = zf.namelist()
    slides = sorted(name for name in names if name.startswith('ppt/slides/slide') and name.endswith('.xml'))
    media = [name for name in names if name.startswith('ppt/media/') and not name.endswith('/')]
    slide_texts = []
    for name in slides:
        root = ET.fromstring(zf.read(name))
        slide_texts.append(' '.join(node.text or '' for node in root.iter() if node.tag.endswith('}t')))
print(json.dumps({
    'slide_count': len(slides),
    'media_count': len(media),
    'all_text': '\n'.join(slide_texts),
    'final_slide_text': slide_texts[-1] if slide_texts else '',
}))
"#;

fn count_number_markers(text: &str) -> usize {
    let lower = text.to_lowercase();
    let mut count = 0;
    for marker in ["1", "2", "3", "①", "②", "③"] {
        if lower.contains(marker) {
            count += 1;
        }
    }
    count
}

fn extract_evidence_markers(text: &str) -> Vec<String> {
    let mut markers = Vec::new();
    for line in text.lines() {
        markers.extend(high_signal_digit_phrases(line));
        for token in line.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')) {
            let token = token.trim();
            if token.len() >= 6
                && token.chars().any(|c| c.is_ascii_alphabetic())
                && token
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                && (token.contains('-') || token.to_ascii_lowercase().contains("agent"))
            {
                markers.push(token.to_string());
            }
        }
    }
    markers
}

fn high_signal_digit_phrases(line: &str) -> Vec<String> {
    let words = line
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_'))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let mut phrases = Vec::new();
    for window in 2..=4 {
        for chunk in words.windows(window) {
            let phrase = chunk.join(" ");
            let has_digit = phrase.chars().any(|c| c.is_ascii_digit());
            let has_alpha = phrase.chars().any(|c| c.is_ascii_alphabetic());
            let has_signal = phrase.contains('-')
                || chunk
                    .iter()
                    .any(|word| word.chars().any(|c| c.is_ascii_digit()));
            let starts_with_line_number = chunk
                .first()
                .and_then(|word| word.parse::<usize>().ok())
                .is_some();
            if has_digit && has_alpha && has_signal && !starts_with_line_number {
                phrases.push(phrase);
            }
        }
    }
    phrases
}

fn placeholder_markers_in(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    [
        "待获取",
        "待下载",
        "待定",
        "待补充",
        "待填写",
        "todo",
        "tbd",
        "placeholder",
        "to be filled",
    ]
    .iter()
    .filter(|marker| lower.contains(&marker.to_lowercase()))
    .map(|marker| marker.to_string())
    .collect()
}

fn pass(name: &str, target: &str) -> ContractReport {
    ContractReport {
        name: name.to_string(),
        target: target.to_string(),
        passed: true,
        error: None,
        repair_hint: None,
    }
}

fn fail(name: &str, target: &str, error: &str, repair_hint: &str) -> ContractReport {
    ContractReport {
        name: name.to_string(),
        target: target.to_string(),
        passed: false,
        error: Some(error.to_string()),
        repair_hint: Some(repair_hint.to_string()),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out = s.chars().take(n).collect::<String>();
        out.push('…');
        out
    }
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn validates_fenced_json_required_keys_and_array_lengths() {
        let contract = TaskContract {
            final_response: Some(FinalResponseContract {
                format: FinalResponseFormat::Json,
                fenced: true,
                required_json_keys: vec!["used_worker".to_string(), "facts".to_string()],
                array_lengths: vec![JsonArrayLengthContract {
                    key: "facts".to_string(),
                    len: 3,
                }],
                require_non_empty: true,
                no_extra_explanation: true,
                required: true,
                exact_text: None,
            }),
            ..TaskContract::default()
        };
        let dir = tempdir().unwrap();
        let ctx = ContractContext {
            repo_root: dir.path(),
            completion_summary: Some(
                "```json\n{\"used_worker\":true,\"facts\":[\"a\",\"b\",\"c\"]}\n```",
            ),
        };
        assert!(validate_task_contract(&contract, &ctx).all_passed);
    }

    #[test]
    fn reports_missing_source_markers_from_contract() {
        let contract = TaskContract {
            source_grounding: Some(SourceGroundingContract {
                required_markers: vec![
                    "release checklist".to_string(),
                    "1-2 steps through scripted validation".to_string(),
                ],
                evidence_files: Vec::new(),
                case_sensitive: false,
            }),
            ..TaskContract::default()
        };
        let dir = tempdir().unwrap();
        let ctx = ContractContext {
            repo_root: dir.path(),
            completion_summary: Some("facts mention 1-2 steps through scripted validation only"),
        };
        let result = validate_task_contract(&contract, &ctx);
        assert!(!result.all_passed);
        assert!(result
            .format_failure_for_agent()
            .contains("release checklist"));
    }

    #[test]
    fn evidence_marker_extraction_keeps_only_high_signal_phrases() {
        let text = "     1|Meeting notes:\n     2|The simple-task section can finish in 1-2 steps through scripted validation.\n     3|A delegated subagent workflow is relevant here.\n     4|A release-check workflow is relevant here.";
        let markers = extract_evidence_markers(text);
        assert!(markers.contains(&"1-2 steps through".to_string()));
        assert!(markers.contains(&"1-2 steps through scripted".to_string()));
        assert!(markers.contains(&"release-check".to_string()));
        assert!(markers.contains(&"subagent".to_string()));
        assert!(!markers.contains(&"2 The".to_string()));
        assert!(!markers.contains(&"Meeting".to_string()));
    }

    #[test]
    fn validates_artifact_json_csv_and_markdown_contracts() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("summary.json"), r#"{"winner":"A"}"#).unwrap();
        fs::write(dir.path().join("result.csv"), "name,value\na,1\n").unwrap();
        fs::write(
            dir.path().join("report.md"),
            "# Required\nConcrete final content is complete.",
        )
        .unwrap();
        let contract = TaskContract {
            artifacts: vec![
                ArtifactContract {
                    path: "summary.json".to_string(),
                    kind: ArtifactKind::Json,
                    require_non_empty: true,
                    required_json_keys: vec!["winner".to_string()],
                    required: true,
                    csv_header: None,
                    min_rows: None,
                    min_non_ws_chars: None,
                    max_non_ws_chars: None,
                    min_text_chars: None,
                    required_headings: Vec::new(),
                    required_terms_any: Vec::new(),
                    forbidden_placeholders: false,
                    require_static_visible_derived_values: false,
                    pptx: None,
                },
                ArtifactContract {
                    path: "result.csv".to_string(),
                    kind: ArtifactKind::Csv,
                    require_non_empty: true,
                    csv_header: Some(vec!["name".to_string(), "value".to_string()]),
                    min_rows: Some(1),
                    required: true,
                    required_json_keys: Vec::new(),
                    min_non_ws_chars: None,
                    max_non_ws_chars: None,
                    min_text_chars: None,
                    required_headings: Vec::new(),
                    required_terms_any: Vec::new(),
                    forbidden_placeholders: false,
                    require_static_visible_derived_values: false,
                    pptx: None,
                },
                ArtifactContract {
                    path: "report.md".to_string(),
                    kind: ArtifactKind::Markdown,
                    require_non_empty: true,
                    min_text_chars: Some(20),
                    required_headings: vec!["# Required".to_string()],
                    forbidden_placeholders: true,
                    required: true,
                    required_json_keys: Vec::new(),
                    csv_header: None,
                    min_rows: None,
                    min_non_ws_chars: None,
                    max_non_ws_chars: None,
                    required_terms_any: Vec::new(),
                    require_static_visible_derived_values: false,
                    pptx: None,
                },
            ],
            ..TaskContract::default()
        };
        let ctx = ContractContext {
            repo_root: dir.path(),
            completion_summary: None,
        };
        assert!(validate_task_contract(&contract, &ctx).all_passed);
    }

    fn artifact(path: &str, kind: ArtifactKind) -> ArtifactContract {
        ArtifactContract {
            path: path.to_string(),
            required: true,
            kind,
            require_non_empty: true,
            required_json_keys: Vec::new(),
            csv_header: None,
            min_rows: None,
            min_non_ws_chars: None,
            max_non_ws_chars: None,
            min_text_chars: None,
            required_headings: Vec::new(),
            required_terms_any: Vec::new(),
            forbidden_placeholders: false,
            require_static_visible_derived_values: false,
            pptx: None,
        }
    }

    #[test]
    fn validates_notebook_cells_and_source_shape() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("good.ipynb"),
            serde_json::json!({
                "cells": [{
                    "cell_type": "markdown",
                    "metadata": {},
                    "source": ["# Result\n", "Concrete analysis"]
                }],
                "metadata": {},
                "nbformat": 4,
                "nbformat_minor": 5
            })
            .to_string(),
        )
        .unwrap();
        fs::write(dir.path().join("missing.ipynb"), r#"{"metadata":{}}"#).unwrap();
        fs::write(
            dir.path().join("bad_source.ipynb"),
            serde_json::json!({"cells":[{"cell_type":"code","source":[1]}]}).to_string(),
        )
        .unwrap();
        fs::write(
            dir.path().join("computed_invisible.ipynb"),
            serde_json::json!({
                "cells": [{
                    "cell_type": "code",
                    "metadata": {},
                    "source": [
                        "result_snapshot = {\n",
                        "  \"mean_value\": statistics.mean(values),\n",
                        "  \"tool_count\": summary['tools'],\n",
                        "  \"step_claim\": summary['steps']\n",
                        "}\n",
                        "result_snapshot\n"
                    ],
                    "outputs": []
                }],
                "metadata": {},
                "nbformat": 4,
                "nbformat_minor": 5
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.path().join("computed_visible.ipynb"),
            serde_json::json!({
                "cells": [{
                    "cell_type": "code",
                    "metadata": {},
                    "source": [
                        "result_snapshot = {\n",
                        "  \"mean_value\": statistics.mean(values),  # mean_value = 7.25\n",
                        "  \"tool_count\": summary['tools'],  # tool_count = 7\n",
                        "  \"step_claim\": summary['steps']  # step_claim = 1-2\n",
                        "}\n",
                        "result_snapshot\n"
                    ],
                    "outputs": []
                }],
                "metadata": {},
                "nbformat": 4,
                "nbformat_minor": 5
            })
            .to_string(),
        )
        .unwrap();

        let ctx = ContractContext {
            repo_root: dir.path(),
            completion_summary: None,
        };
        assert!(
            validate_task_contract(
                &TaskContract {
                    artifacts: vec![artifact("good.ipynb", ArtifactKind::Notebook)],
                    ..TaskContract::default()
                },
                &ctx
            )
            .all_passed
        );
        let missing = validate_task_contract(
            &TaskContract {
                artifacts: vec![artifact("missing.ipynb", ArtifactKind::Notebook)],
                ..TaskContract::default()
            },
            &ctx,
        );
        assert!(!missing.all_passed);
        assert!(missing.format_failure_for_agent().contains("cells"));
        let bad_source = validate_task_contract(
            &TaskContract {
                artifacts: vec![artifact("bad_source.ipynb", ArtifactKind::Notebook)],
                ..TaskContract::default()
            },
            &ctx,
        );
        assert!(!bad_source.all_passed);
        assert!(bad_source.format_failure_for_agent().contains("source"));
        let invisible = validate_task_contract(
            &TaskContract {
                artifacts: vec![ArtifactContract {
                    require_static_visible_derived_values: true,
                    ..artifact("computed_invisible.ipynb", ArtifactKind::Notebook)
                }],
                ..TaskContract::default()
            },
            &ctx,
        );
        assert!(!invisible.all_passed);
        assert!(invisible
            .format_failure_for_agent()
            .contains("derived expression"));
        assert!(
            validate_task_contract(
                &TaskContract {
                    artifacts: vec![ArtifactContract {
                        require_static_visible_derived_values: true,
                        ..artifact("computed_visible.ipynb", ArtifactKind::Notebook)
                    }],
                    ..TaskContract::default()
                },
                &ctx,
            )
            .all_passed
        );
    }

    #[test]
    fn validates_image_magic_and_binary_sanity() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("ok.png"),
            [
                &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a][..],
                b"payload",
            ]
            .concat(),
        )
        .unwrap();
        fs::write(dir.path().join("bad.png"), b"not an image placeholder").unwrap();
        fs::write(dir.path().join("ok.bin"), b"\x01\x02real").unwrap();
        fs::write(dir.path().join("zero.bin"), [0u8; 32]).unwrap();

        let ctx = ContractContext {
            repo_root: dir.path(),
            completion_summary: None,
        };
        assert!(
            validate_task_contract(
                &TaskContract {
                    artifacts: vec![artifact("ok.png", ArtifactKind::Image)],
                    ..TaskContract::default()
                },
                &ctx
            )
            .all_passed
        );
        assert!(
            !validate_task_contract(
                &TaskContract {
                    artifacts: vec![artifact("bad.png", ArtifactKind::Image)],
                    ..TaskContract::default()
                },
                &ctx
            )
            .all_passed
        );
        assert!(
            validate_task_contract(
                &TaskContract {
                    artifacts: vec![artifact("ok.bin", ArtifactKind::Binary)],
                    ..TaskContract::default()
                },
                &ctx
            )
            .all_passed
        );
        let zero = validate_task_contract(
            &TaskContract {
                artifacts: vec![artifact("zero.bin", ArtifactKind::Binary)],
                ..TaskContract::default()
            },
            &ctx,
        );
        assert!(!zero.all_passed);
        assert!(zero.format_failure_for_agent().contains("zero"));
    }

    #[test]
    fn rejects_unsafe_artifact_paths() {
        let dir = tempdir().unwrap();
        let ctx = ContractContext {
            repo_root: dir.path(),
            completion_summary: None,
        };
        for path in ["", "../escape.json", "/tmp/escape.json"] {
            let result = validate_task_contract(
                &TaskContract {
                    artifacts: vec![artifact(path, ArtifactKind::Json)],
                    ..TaskContract::default()
                },
                &ctx,
            );
            assert!(!result.all_passed, "{path:?} should fail");
            assert!(result.format_failure_for_agent().contains("artifact_path"));
        }
    }
}
