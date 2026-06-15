use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

pub const DELIVERY_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HandoffConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandResultStatus {
    Passed,
    Failed,
    Skipped,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedFileSummary {
    pub path: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub change_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionSummary {
    pub title: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub tradeoffs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandRunSummary {
    pub command: String,
    pub status: CommandResultStatus,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryArtifactRef {
    #[serde(default)]
    pub artifact_id: Option<String>,
    pub local_name: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub file_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskHandoffPacket {
    pub mission_id: String,
    pub task_id: String,
    pub task_title: String,
    pub summary: String,
    pub confidence: HandoffConfidence,
    #[serde(default)]
    pub changed_files: Vec<ChangedFileSummary>,
    #[serde(default)]
    pub decisions: Vec<DecisionSummary>,
    #[serde(default)]
    pub commands: Vec<CommandRunSummary>,
    #[serde(default)]
    pub artifacts: Vec<DeliveryArtifactRef>,
    #[serde(default)]
    pub direct_context: Vec<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
}

impl TaskHandoffPacket {
    pub fn from_task_complete_value(
        mission_id: impl Into<String>,
        task_id: impl Into<String>,
        title: impl Into<String>,
        objective: impl Into<String>,
        value: &serde_json::Value,
        published_artifacts: Vec<DeliveryArtifactRef>,
    ) -> Result<Self> {
        let mission_id = mission_id.into();
        let task_id = task_id.into();
        let task_title = title.into();
        let objective = objective.into();
        let summary = value
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let Some(handoff) = value.get("handoff") else {
            return Ok(Self::fallback(
                mission_id,
                task_id,
                task_title,
                non_empty_or(summary, || objective.clone()),
                published_artifacts,
            ));
        };

        #[derive(Deserialize)]
        struct TaskCompleteHandoff {
            #[serde(default)]
            summary: String,
            #[serde(default)]
            confidence: Option<HandoffConfidence>,
            #[serde(default)]
            changed_files: Vec<ChangedFileSummary>,
            #[serde(default)]
            decisions: Vec<DecisionSummary>,
            #[serde(default)]
            commands_run: Vec<CommandRunSummary>,
            #[serde(default)]
            artifacts: Vec<DeliveryArtifactRef>,
            #[serde(default)]
            reusable_context: Vec<String>,
            #[serde(default)]
            caveats: Vec<String>,
            #[serde(default)]
            downstream_hints: Vec<String>,
        }

        let mut parsed: TaskCompleteHandoff = serde_json::from_value(handoff.clone())
            .context("task_complete.handoff must match the handoff packet schema")?;
        parsed.artifacts.extend(published_artifacts);
        parsed.artifacts.sort_by(compare_artifacts);
        dedup_artifacts(&mut parsed.artifacts);

        Ok(Self {
            mission_id,
            task_id,
            task_title,
            summary: non_empty_or(parsed.summary, || non_empty_or(summary, || objective)),
            confidence: parsed.confidence.unwrap_or(HandoffConfidence::Medium),
            changed_files: parsed.changed_files,
            decisions: parsed.decisions,
            commands: parsed.commands_run,
            artifacts: parsed.artifacts,
            direct_context: parsed.reusable_context,
            caveats: parsed.caveats,
            next_steps: parsed.downstream_hints,
        })
    }
    pub fn fallback(
        mission_id: impl Into<String>,
        task_id: impl Into<String>,
        task_title: impl Into<String>,
        task_summary: impl Into<String>,
        mut artifacts: Vec<DeliveryArtifactRef>,
    ) -> Self {
        artifacts.sort_by(compare_artifacts);
        dedup_artifacts(&mut artifacts);

        let mission_id = mission_id.into();
        let task_id = task_id.into();
        let task_title = task_title.into();
        let summary = non_empty_or(task_summary.into(), || {
            format!(
                "Task `{}` completed without an authored handoff summary.",
                task_title
            )
        });

        let artifact_names = artifacts
            .iter()
            .map(|artifact| artifact.local_name.as_str())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>()
            .join(", ");

        let mut direct_context = vec![if artifact_names.is_empty() {
            summary.clone()
        } else {
            format!("{summary} Artifacts: {artifact_names}.")
        }];
        direct_context.extend(artifacts.iter().flat_map(|artifact| {
            let mut lines = Vec::new();
            if !artifact.summary.trim().is_empty() {
                lines.push(format!(
                    "Artifact `{}`: {}",
                    artifact.local_name,
                    artifact.summary.trim()
                ));
            }
            if !artifact.file_paths.is_empty() {
                lines.push(format!(
                    "Artifact `{}` files: {}",
                    artifact.local_name,
                    artifact.file_paths.join(", ")
                ));
            }
            lines
        }));

        Self {
            mission_id,
            task_id,
            task_title,
            summary,
            confidence: HandoffConfidence::Low,
            changed_files: vec![],
            decisions: vec![],
            commands: vec![],
            artifacts,
            direct_context,
            caveats: vec![
                "Generated fallback handoff because no agent-authored handoff packet was available."
                    .to_string(),
            ],
            next_steps: vec![
                "Review the task output and artifacts before depending on this handoff."
                    .to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryCandidateSource {
    Artifact,
    Handoff,
    Git,
    Filesystem,
    Manifest,
    ModelHint,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryCandidate {
    pub id: String,
    pub source: DeliveryCandidateSource,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub file_paths: Vec<String>,
    pub confidence: DeliveryConfidence,
}

impl DeliveryCandidate {
    pub fn from_artifact(task_id: &str, artifact: &DeliveryArtifactRef) -> Self {
        let id = artifact
            .artifact_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "{}.{}",
                    safe_id_part(task_id),
                    safe_id_part(&artifact.local_name)
                )
            });

        Self {
            id,
            source: DeliveryCandidateSource::Artifact,
            title: non_empty_or(artifact.local_name.clone(), || "artifact".to_string()),
            summary: artifact.summary.clone(),
            file_paths: artifact.file_paths.clone(),
            confidence: DeliveryConfidence::Medium,
        }
    }

    fn into_item(mut self) -> DeliveryItem {
        sort_and_dedup_strings(&mut self.file_paths);
        DeliveryItem {
            id: self.id,
            source: self.source,
            title: non_empty_or(self.title, || "Untitled deliverable".to_string()),
            summary: self.summary,
            file_paths: self.file_paths,
            confidence: self.confidence,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Completed,
    CompletedWithWarnings,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryItem {
    pub id: String,
    pub source: DeliveryCandidateSource,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub file_paths: Vec<String>,
    pub confidence: DeliveryConfidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryOverview {
    pub title: String,
    pub summary: String,
    pub status: DeliveryStatus,
    pub confidence: DeliveryConfidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HowToUseStep {
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationResultStatus {
    Passed,
    Failed,
    NotRun,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationEvidence {
    pub status: ValidationResultStatus,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeSummary {
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionDeliverySnapshot {
    pub schema_version: u32,
    pub mission_id: String,
    pub status: DeliveryStatus,
    pub confidence: DeliveryConfidence,
    pub overview: DeliveryOverview,
    #[serde(default)]
    pub items: Vec<DeliveryItem>,
    #[serde(default)]
    pub how_to_use: Vec<HowToUseStep>,
    #[serde(default)]
    pub validation: Vec<ValidationEvidence>,
    #[serde(default)]
    pub changes: Vec<ChangeSummary>,
    #[serde(default)]
    pub caveats: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
}

impl MissionDeliverySnapshot {
    pub fn degraded(mission_id: impl Into<String>, candidates: Vec<DeliveryCandidate>) -> Self {
        let mission_id = mission_id.into();
        let mut items = candidates
            .into_iter()
            .map(DeliveryCandidate::into_item)
            .collect::<Vec<_>>();
        items.sort_by(compare_delivery_items);
        dedup_items(&mut items);

        if items.is_empty() {
            items.push(DeliveryItem {
                id: "delivery-summary".to_string(),
                source: DeliveryCandidateSource::Manifest,
                title: "Delivery summary".to_string(),
                summary: "No concrete deliverable candidates were collected; inspect mission changes and task outputs manually."
                    .to_string(),
                file_paths: vec![],
                confidence: DeliveryConfidence::Low,
            });
        }

        let item_count = items.len();
        let primary_title = items
            .first()
            .map(|item| item.title.clone())
            .unwrap_or_else(|| "Delivery summary".to_string());

        Self {
            schema_version: DELIVERY_SNAPSHOT_SCHEMA_VERSION,
            mission_id,
            status: DeliveryStatus::CompletedWithWarnings,
            confidence: DeliveryConfidence::Low,
            overview: DeliveryOverview {
                title: "Degraded mission delivery snapshot".to_string(),
                summary: format!(
                    "Deterministic fallback snapshot with {item_count} deliverable candidate(s). Primary candidate: {primary_title}."
                ),
                status: DeliveryStatus::CompletedWithWarnings,
                confidence: DeliveryConfidence::Low,
            },
            items,
            how_to_use: vec![HowToUseStep {
                title: "Review deliverables".to_string(),
                detail: "Open the listed files or artifact references and validate they satisfy the mission request."
                    .to_string(),
            }],
            validation: vec![ValidationEvidence {
                status: ValidationResultStatus::NotRun,
                summary: "No curator validation was run for this degraded snapshot.".to_string(),
                command: None,
            }],
            changes: vec![ChangeSummary {
                title: "Fallback delivery generated".to_string(),
                detail: "Delivery was assembled from deterministic candidates instead of curated mission output."
                    .to_string(),
                files: vec![],
            }],
            caveats: vec![
                "This degraded snapshot was generated without model curation; verify important outputs manually."
                    .to_string(),
            ],
            next_steps: vec![
                "Validate the listed candidates against the original mission goal.".to_string(),
                "Regenerate a curated delivery snapshot when the delivery curator is available.".to_string(),
            ],
        }
    }
}

pub fn render_handoffs_for_prompt(handoffs: &[TaskHandoffPacket], max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let mut ordered = handoffs.to_vec();
    ordered.sort_by(|a, b| {
        a.task_id
            .cmp(&b.task_id)
            .then_with(|| a.task_title.cmp(&b.task_title))
    });

    let mut out = String::from("## Parent task handoffs\n");
    if ordered.is_empty() {
        out.push_str("No parent handoff packets were available.\n");
    }

    for handoff in ordered {
        push_line(
            &mut out,
            format!(
                "- Task `{}` — {} (confidence: {:?})",
                handoff.task_id, handoff.task_title, handoff.confidence
            ),
        );
        push_line(&mut out, format!("  Summary: {}", handoff.summary));
        if !handoff.direct_context.is_empty() {
            push_line(&mut out, "  Direct context:".to_string());
            for context in &handoff.direct_context {
                push_line(&mut out, format!("  - {}", context));
            }
        }
        if !handoff.artifacts.is_empty() {
            push_line(&mut out, "  Artifacts:".to_string());
            for artifact in &handoff.artifacts {
                let files = if artifact.file_paths.is_empty() {
                    "no files listed".to_string()
                } else {
                    artifact.file_paths.join(", ")
                };
                push_line(
                    &mut out,
                    format!(
                        "  - {} [{}]: {} ({})",
                        artifact.local_name, artifact.artifact_type, artifact.summary, files
                    ),
                );
            }
        }
        if !handoff.caveats.is_empty() {
            push_line(
                &mut out,
                format!("  Caveats: {}", handoff.caveats.join("; ")),
            );
        }
    }

    truncate_chars(&out, max_chars)
}

pub fn render_delivery_for_prompt(snapshot: &MissionDeliverySnapshot, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let mut out = String::from("## Mission delivery snapshot\n");
    push_line(&mut out, format!("Mission: {}", snapshot.mission_id));
    push_line(&mut out, format!("Status: {:?}", snapshot.status));
    push_line(&mut out, format!("Overview: {}", snapshot.overview.summary));

    if !snapshot.items.is_empty() {
        push_line(&mut out, "Deliverables:".to_string());
        for item in &snapshot.items {
            let files = if item.file_paths.is_empty() {
                "no files listed".to_string()
            } else {
                item.file_paths.join(", ")
            };
            push_line(
                &mut out,
                format!(
                    "- {} [{:?}]: {} ({})",
                    item.title, item.source, item.summary, files
                ),
            );
        }
    }

    if !snapshot.how_to_use.is_empty() {
        push_line(&mut out, "How to use:".to_string());
        for step in &snapshot.how_to_use {
            push_line(&mut out, format!("- {}: {}", step.title, step.detail));
        }
    }

    if !snapshot.caveats.is_empty() {
        push_line(
            &mut out,
            format!("Caveats: {}", snapshot.caveats.join("; ")),
        );
    }

    truncate_chars(&out, max_chars)
}

fn compare_artifacts(a: &DeliveryArtifactRef, b: &DeliveryArtifactRef) -> Ordering {
    a.local_name
        .cmp(&b.local_name)
        .then_with(|| a.artifact_type.cmp(&b.artifact_type))
        .then_with(|| a.artifact_id.cmp(&b.artifact_id))
}

fn compare_delivery_items(a: &DeliveryItem, b: &DeliveryItem) -> Ordering {
    source_rank(a.source)
        .cmp(&source_rank(b.source))
        .then_with(|| a.id.cmp(&b.id))
        .then_with(|| a.title.cmp(&b.title))
}

fn source_rank(source: DeliveryCandidateSource) -> u8 {
    match source {
        DeliveryCandidateSource::Artifact => 0,
        DeliveryCandidateSource::Handoff => 1,
        DeliveryCandidateSource::Manifest => 2,
        DeliveryCandidateSource::Filesystem => 3,
        DeliveryCandidateSource::Git => 4,
        DeliveryCandidateSource::ModelHint => 5,
    }
}


fn dedup_items(items: &mut Vec<DeliveryItem>) {
    items.dedup_by(|a, b| a.id == b.id && a.source == b.source);
}

fn dedup_artifacts(artifacts: &mut Vec<DeliveryArtifactRef>) {
    artifacts.dedup_by(|a, b| {
        a.artifact_id == b.artifact_id
            && a.local_name == b.local_name
            && a.artifact_type == b.artifact_type
    });
}

fn sort_and_dedup_strings(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn non_empty_or(value: String, fallback: impl FnOnce() -> String) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback()
    } else if trimmed.len() == value.len() {
        value
    } else {
        trimmed.to_string()
    }
}

fn safe_id_part(value: &str) -> String {
    let id = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    non_empty_or(id, || "unknown".to_string())
}

fn push_line(out: &mut String, line: String) {
    out.push_str(&line);
    out.push('\n');
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(local_name: &str, summary: &str, paths: &[&str]) -> DeliveryArtifactRef {
        DeliveryArtifactRef {
            artifact_id: Some(format!("task-1.{local_name}")),
            local_name: local_name.to_string(),
            artifact_type: "report".to_string(),
            summary: summary.to_string(),
            file_paths: paths.iter().map(|path| path.to_string()).collect(),
        }
    }

    #[test]
    fn parses_agent_authored_handoff_from_task_complete_input() {
        let value = serde_json::json!({
            "summary": "Implemented delivery handoff persistence.",
            "handoff": {
                "summary": "Task completion now records reusable handoff context.",
                "confidence": "high",
                "changed_files": [
                    {
                        "path": "src-tauri/src/agent/engine.rs",
                        "summary": "Persists handoff packets after task_complete.",
                        "change_type": "modified"
                    }
                ],
                "decisions": [
                    {
                        "title": "Persist from task_complete input",
                        "rationale": "The tool input is the agent-authored source of truth.",
                        "tradeoffs": ["Fallback packets remain available when handoff is omitted."]
                    }
                ],
                "commands_run": [
                    {
                        "command": "cargo test parses_agent_authored_handoff_from_task_complete_input --lib",
                        "status": "passed",
                        "summary": "Focused parser test passed.",
                        "exit_code": 0
                    }
                ],
                "artifacts": [
                    {
                        "artifact_id": "agent-artifact",
                        "local_name": "handoff_notes",
                        "type": "markdown",
                        "summary": "Agent-authored handoff notes.",
                        "file_paths": ["docs/handoff.md"]
                    }
                ],
                "reusable_context": ["Engine can derive status from handoff presence."],
                "caveats": ["Fallback rows should not overwrite agent-authored rows."],
                "downstream_hints": ["Use this packet for child task prompts."]
            }
        });

        let handoff = TaskHandoffPacket::from_task_complete_value(
            "mission-1",
            "task-1",
            "Persist handoffs",
            "Persist task handoff packets from task_complete.",
            &value,
            vec![artifact(
                "published_report",
                "Artifact published before completion.",
                &["dist/report.md"],
            )],
        )
        .expect("handoff parses");

        assert_eq!(handoff.mission_id, "mission-1");
        assert_eq!(handoff.task_id, "task-1");
        assert_eq!(handoff.task_title, "Persist handoffs");
        assert_eq!(
            handoff.summary,
            "Task completion now records reusable handoff context."
        );
        assert_eq!(handoff.confidence, HandoffConfidence::High);
        assert_eq!(handoff.changed_files[0].path, "src-tauri/src/agent/engine.rs");
        assert_eq!(handoff.decisions[0].title, "Persist from task_complete input");
        assert_eq!(handoff.commands[0].status, CommandResultStatus::Passed);
        assert_eq!(handoff.direct_context, vec!["Engine can derive status from handoff presence."]);
        assert_eq!(
            handoff.caveats,
            vec!["Fallback rows should not overwrite agent-authored rows."]
        );
        assert_eq!(handoff.next_steps, vec!["Use this packet for child task prompts."]);
        assert_eq!(
            handoff
                .artifacts
                .iter()
                .map(|artifact| artifact.local_name.as_str())
                .collect::<Vec<_>>(),
            vec!["handoff_notes", "published_report"]
        );
    }

    #[test]
    fn fallback_handoff_uses_task_summary_and_artifacts() {
        let handoff = TaskHandoffPacket::fallback(
            "mission-1",
            "task-1",
            "Summarize release outputs",
            "Built the release notes and checksum manifest.",
            vec![artifact(
                "release_notes",
                "Markdown release notes for operators.",
                &["dist/release-notes.md"],
            )],
        );

        assert_eq!(handoff.mission_id, "mission-1");
        assert_eq!(handoff.task_id, "task-1");
        assert_eq!(handoff.task_title, "Summarize release outputs");
        assert_eq!(
            handoff.summary,
            "Built the release notes and checksum manifest."
        );
        assert_eq!(handoff.confidence, HandoffConfidence::Low);
        assert_eq!(handoff.artifacts.len(), 1);
        assert_eq!(handoff.artifacts[0].local_name, "release_notes");
        assert!(handoff.direct_context.iter().any(|line| {
            line.contains("Built the release notes") && line.contains("release_notes")
        }));
        assert!(handoff
            .caveats
            .iter()
            .any(|caveat| caveat.contains("fallback")));
    }

    #[test]
    fn degraded_snapshot_prefers_artifacts_but_never_empty() {
        let without_candidates = MissionDeliverySnapshot::degraded("mission-1", vec![]);
        assert_eq!(without_candidates.mission_id, "mission-1");
        assert_eq!(
            without_candidates.status,
            DeliveryStatus::CompletedWithWarnings
        );
        assert!(!without_candidates.overview.title.is_empty());
        assert!(!without_candidates.overview.summary.is_empty());
        assert!(!without_candidates.caveats.is_empty());
        assert!(!without_candidates.how_to_use.is_empty());
        assert!(!without_candidates.next_steps.is_empty());
        assert!(!without_candidates.items.is_empty());

        let candidates = vec![
            DeliveryCandidate {
                id: "git-diff".to_string(),
                source: DeliveryCandidateSource::Git,
                title: "Raw git diff".to_string(),
                summary: "Repository changes are available for review.".to_string(),
                file_paths: vec!["src/lib.rs".to_string()],
                confidence: DeliveryConfidence::Low,
            },
            DeliveryCandidate::from_artifact(
                "task-1",
                &artifact(
                    "release_notes",
                    "Markdown release notes for operators.",
                    &["dist/release-notes.md"],
                ),
            ),
        ];

        let snapshot = MissionDeliverySnapshot::degraded("mission-1", candidates);
        assert_eq!(snapshot.items[0].source, DeliveryCandidateSource::Artifact);
        assert_eq!(snapshot.items[0].title, "release_notes");
        assert_eq!(snapshot.items[0].file_paths, vec!["dist/release-notes.md"]);
        assert!(snapshot
            .caveats
            .iter()
            .any(|caveat| caveat.contains("degraded")));
    }

    #[test]
    fn degraded_snapshot_deduplicates_same_source_and_id_even_with_interleaved_titles() {
        let candidates = vec![
            DeliveryCandidate {
                id: "x".to_string(),
                source: DeliveryCandidateSource::Artifact,
                title: "A".to_string(),
                summary: "First artifact candidate.".to_string(),
                file_paths: vec!["dist/a.md".to_string()],
                confidence: DeliveryConfidence::Medium,
            },
            DeliveryCandidate {
                id: "y".to_string(),
                source: DeliveryCandidateSource::Artifact,
                title: "B".to_string(),
                summary: "Different artifact candidate.".to_string(),
                file_paths: vec!["dist/b.md".to_string()],
                confidence: DeliveryConfidence::Medium,
            },
            DeliveryCandidate {
                id: "x".to_string(),
                source: DeliveryCandidateSource::Artifact,
                title: "C".to_string(),
                summary: "Duplicate artifact candidate.".to_string(),
                file_paths: vec!["dist/c.md".to_string()],
                confidence: DeliveryConfidence::Medium,
            },
        ];

        let snapshot = MissionDeliverySnapshot::degraded("mission-1", candidates);
        let duplicate_count = snapshot
            .items
            .iter()
            .filter(|item| item.source == DeliveryCandidateSource::Artifact && item.id == "x")
            .count();

        assert_eq!(duplicate_count, 1);
    }

    #[test]
    fn artifact_candidate_trims_explicit_artifact_id() {
        let candidate = DeliveryCandidate::from_artifact(
            "task-1",
            &DeliveryArtifactRef {
                artifact_id: Some("  explicit-id  ".to_string()),
                local_name: "release_notes".to_string(),
                artifact_type: "report".to_string(),
                summary: "Markdown release notes for operators.".to_string(),
                file_paths: vec!["dist/release-notes.md".to_string()],
            },
        );

        assert_eq!(candidate.id, "explicit-id");
    }

    #[test]
    fn render_handoffs_for_prompt_mentions_direct_context() {
        let mut handoff = TaskHandoffPacket::fallback(
            "mission-1",
            "task-1",
            "Build CLI",
            "Implemented command parsing.",
            vec![artifact("cli_patch", "CLI patch", &["src/cli.rs"])],
        );
        handoff
            .direct_context
            .push("Direct context: pass --dry-run to preview changes.".into());

        let rendered = render_handoffs_for_prompt(&[handoff], 2_000);

        assert!(rendered.contains("Parent task handoffs"));
        assert!(rendered.contains("task-1"));
        assert!(rendered.contains("Direct context"));
        assert!(rendered.contains("--dry-run"));
        assert!(rendered.len() <= 2_000);
    }
}
