use crate::db::queries;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
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

impl TaskCompleteHandoff {
    fn is_empty(&self) -> bool {
        self.summary.trim().is_empty()
            && self.confidence.is_none()
            && self.changed_files.is_empty()
            && self.decisions.is_empty()
            && self.commands_run.is_empty()
            && self.artifacts.is_empty()
            && self.reusable_context.is_empty()
            && self.caveats.is_empty()
            && self.downstream_hints.is_empty()
    }
}

fn parse_task_complete_handoff(value: &serde_json::Value) -> Option<TaskCompleteHandoff> {
    serde_json::from_value::<TaskCompleteHandoff>(value.get("handoff")?.clone())
        .ok()
        .filter(|handoff| !handoff.is_empty())
}

pub fn task_complete_handoff_is_agent_authored(value: &serde_json::Value) -> bool {
    parse_task_complete_handoff(value).is_some()
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

        let Some(parsed) = parse_task_complete_handoff(value) else {
            return Ok(Self::fallback(
                mission_id,
                task_id,
                task_title,
                non_empty_or(summary, || objective.clone()),
                published_artifacts,
            ));
        };

        let mut artifacts = parsed.artifacts;
        artifacts.extend(published_artifacts);
        artifacts.sort_by(compare_artifacts);
        dedup_artifacts(&mut artifacts);

        Ok(Self {
            mission_id,
            task_id,
            task_title,
            summary: non_empty_or(parsed.summary, || non_empty_or(summary, || objective)),
            confidence: parsed.confidence.unwrap_or(HandoffConfidence::Medium),
            changed_files: parsed.changed_files,
            decisions: parsed.decisions,
            commands: parsed.commands_run,
            artifacts,
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
        Self::degraded_with_mission_status(mission_id, None::<String>, None::<String>, candidates)
    }

    pub fn degraded_with_mission_status(
        mission_id: impl Into<String>,
        mission_title: Option<impl Into<String>>,
        mission_status: Option<impl AsRef<str>>,
        candidates: Vec<DeliveryCandidate>,
    ) -> Self {
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
        let mission_title = mission_title
            .map(Into::into)
            .map(|title| non_empty_or(title, || mission_id.clone()));
        let mission_label = mission_title.as_deref().unwrap_or(&mission_id).to_string();
        let status = match mission_status.as_ref().map(|status| status.as_ref()) {
            Some("failed") => DeliveryStatus::Failed,
            Some("completed") => DeliveryStatus::CompletedWithWarnings,
            _ => DeliveryStatus::CompletedWithWarnings,
        };
        let overview_summary = format!(
            "Deterministic fallback snapshot for `{mission_label}` with {item_count} deliverable candidate(s). Primary candidate: {primary_title}."
        );

        Self {
            schema_version: DELIVERY_SNAPSHOT_SCHEMA_VERSION,
            mission_id,
            status,
            confidence: DeliveryConfidence::Low,
            overview: DeliveryOverview {
                title: "Degraded mission delivery snapshot".to_string(),
                summary: overview_summary,
                status,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverySnapshotGeneration {
    pub snapshot: MissionDeliverySnapshot,
    pub source_task_ids: Vec<String>,
}

pub fn candidates_from_artifacts(artifacts: &[queries::ArtifactRow]) -> Vec<DeliveryCandidate> {
    let mut candidates = artifacts
        .iter()
        .map(|artifact| {
            let mut file_paths = parse_artifact_file_paths(&artifact.file_paths, &artifact.id);
            sort_and_dedup_strings(&mut file_paths);
            DeliveryCandidate {
                id: artifact.id.trim().to_string(),
                source: DeliveryCandidateSource::Artifact,
                title: non_empty_or(artifact.local_name.clone(), || "artifact".to_string()),
                summary: artifact.summary.clone(),
                file_paths,
                confidence: DeliveryConfidence::Medium,
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_by(compare_delivery_candidates);
    candidates
}

pub fn generate_degraded_delivery_snapshot(
    conn: &Connection,
    mission_id: &str,
) -> Result<DeliverySnapshotGeneration> {
    let (mission_title, mission_status): (String, String) = conn
        .query_row(
            "SELECT title, status FROM missions WHERE id = ?1",
            [mission_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("mission not found: {mission_id}"))?;

    if !matches!(mission_status.as_str(), "completed" | "failed") {
        return Err(anyhow::anyhow!(
            "Mission `{mission_title}` is not ready for delivery yet. Delivery can be generated after the mission completes or fails."
        ));
    }

    let handoffs = queries::list_task_handoff_packets_for_mission(conn, mission_id)?;
    let mut candidates = Vec::new();
    let mut source_task_ids = Vec::new();
    for row in handoffs {
        match serde_json::from_str::<TaskHandoffPacket>(&row.packet_json) {
            Ok(mut packet) => {
                if packet.task_id != row.task_id {
                    tracing::warn!(
                        mission_id = %mission_id,
                        row_task_id = %row.task_id,
                        packet_task_id = %packet.task_id,
                        "task handoff packet task_id mismatches authoritative row task_id; using row task_id for delivery sources"
                    );
                    packet.task_id = row.task_id.clone();
                }
                source_task_ids.push(row.task_id.clone());
                candidates.extend(candidates_from_handoff(&packet));
            }
            Err(err) => tracing::warn!(
                mission_id = %mission_id,
                task_id = %row.task_id,
                error = %err,
                "skipping corrupt task handoff packet during delivery snapshot generation"
            ),
        }
    }

    let artifacts = queries::list_artifacts_for_mission(conn, mission_id)?;
    let published_artifacts = artifacts
        .into_iter()
        .filter(|artifact| artifact.published)
        .collect::<Vec<_>>();
    source_task_ids.extend(
        published_artifacts
            .iter()
            .map(|artifact| artifact.producer_task_id.clone())
            .filter(|task_id| !task_id.trim().is_empty()),
    );
    candidates.extend(candidates_from_artifacts(&published_artifacts));
    sort_and_dedup_strings(&mut source_task_ids);

    Ok(DeliverySnapshotGeneration {
        snapshot: MissionDeliverySnapshot::degraded_with_mission_status(
            mission_id.to_string(),
            Some(mission_title),
            Some(mission_status),
            candidates,
        ),
        source_task_ids,
    })
}

pub fn persist_delivery_snapshot(
    conn: &Connection,
    snapshot: &MissionDeliverySnapshot,
    generation_status: &str,
    curator_model: Option<&str>,
    source_task_ids: &[String],
) -> Result<()> {
    let snapshot_json = serde_json::to_string(snapshot)?;
    let source_task_ids = source_task_ids_json(source_task_ids)?;
    if let Some(existing) = queries::get_mission_delivery(conn, &snapshot.mission_id)? {
        if generation_status == "degraded"
            && existing.generation_status == "generated"
            && !existing.stale
            && existing.version >= snapshot.schema_version as i64
        {
            tracing::debug!(
                mission_id = %snapshot.mission_id,
                existing_version = existing.version,
                snapshot_version = snapshot.schema_version,
                "skipping degraded delivery persistence because a non-stale generated snapshot already exists"
            );
            return Ok(());
        }
    }
    queries::upsert_mission_delivery(
        conn,
        &snapshot.mission_id,
        snapshot.schema_version as i64,
        &snapshot_json,
        generation_status,
        curator_model,
        &source_task_ids,
        "[]",
        false,
    )
}

pub fn generate_and_persist_degraded_delivery_on_conn(
    conn: &Connection,
    mission_id: &str,
) -> Result<()> {
    let generation = generate_degraded_delivery_snapshot(conn, mission_id)?;
    persist_delivery_snapshot(
        conn,
        &generation.snapshot,
        "degraded",
        Some("deterministic"),
        &generation.source_task_ids,
    )
}

pub fn generate_and_persist_degraded_delivery(
    db: &crate::db::Database,
    mission_id: &str,
) -> Result<()> {
    db.with_conn(|conn| generate_and_persist_degraded_delivery_on_conn(conn, mission_id))
}

pub fn mark_mission_delivery_stale_best_effort(
    conn: &Connection,
    mission_id: &str,
    reason: &str,
) -> bool {
    match queries::mark_mission_delivery_stale(conn, mission_id) {
        Ok(marked) => marked,
        Err(err) => {
            tracing::warn!(
                mission_id = %mission_id,
                reason = %reason,
                error = %err,
                "failed to mark mission delivery stale after source input change"
            );
            false
        }
    }
}

pub fn parse_curator_snapshot_or_fallback(
    raw: &str,
    mut fallback: MissionDeliverySnapshot,
) -> MissionDeliverySnapshot {
    let trimmed = raw.trim();
    let candidate = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .or_else(|| {
            trimmed
                .strip_prefix("```")
                .and_then(|value| value.strip_suffix("```"))
        })
        .map(str::trim)
        .unwrap_or(trimmed);

    match serde_json::from_str::<MissionDeliverySnapshot>(candidate) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            fallback.caveats.push(format!(
                "Delivery model curation output was invalid; using fallback snapshot: {err}"
            ));
            fallback
        }
    }
}

pub async fn curate_delivery_with_llm(
    provider: std::sync::Arc<dyn crate::llm::LlmProvider>,
    model: &str,
    fallback: MissionDeliverySnapshot,
) -> MissionDeliverySnapshot {
    let prompt = format!(
        "You are Miragenty's Delivery Curator. Decide what the user actually received and how to use it. Return ONLY JSON matching the provided MissionDeliverySnapshot schema. Preserve schema_version and mission_id. Treat listed items as broad candidates, not rigid rules.\n\nFallback snapshot JSON:\n{}",
        serde_json::to_string_pretty(&fallback).unwrap_or_default(),
    );
    let request = crate::llm::LlmRequest {
        model: model.to_string(),
        system: Some("Return strict JSON only. Do not wrap in Markdown.".into()),
        messages: vec![crate::llm::Message {
            role: crate::llm::MessageRole::User,
            content: vec![crate::llm::ContentBlock::Text { text: prompt }],
            cache_control: None,
        }],
        tools: Vec::new(),
        max_tokens: 4_000,
        provider_extras: None,
    };

    match tokio::time::timeout(std::time::Duration::from_secs(30), provider.chat(&request)).await {
        Ok(Ok(response)) => {
            let raw = response
                .content
                .into_iter()
                .filter_map(|block| match block {
                    crate::llm::ContentBlock::Text { text } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            parse_curator_snapshot_or_fallback(&raw, fallback)
        }
        Ok(Err(err)) => {
            let mut snapshot = fallback;
            snapshot.caveats.push(format!(
                "Delivery model curation failed; using fallback snapshot: {err}"
            ));
            snapshot
        }
        Err(_) => {
            let mut snapshot = fallback;
            snapshot
                .caveats
                .push("Delivery model curation timed out; using fallback snapshot.".into());
            snapshot
        }
    }
}

fn candidates_from_handoff(packet: &TaskHandoffPacket) -> Vec<DeliveryCandidate> {
    let mut candidates = packet
        .artifacts
        .iter()
        .map(|artifact| DeliveryCandidate::from_artifact(&packet.task_id, artifact))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        if let Some(candidate) = handoff_summary_candidate(packet) {
            candidates.push(candidate);
        }
    }
    candidates.sort_by(compare_delivery_candidates);
    candidates
}

fn handoff_summary_candidate(packet: &TaskHandoffPacket) -> Option<DeliveryCandidate> {
    let mut summary_parts = Vec::new();
    push_non_empty(&mut summary_parts, packet.summary.trim());
    for file in &packet.changed_files {
        let mut line = String::new();
        if !file.path.trim().is_empty() {
            line.push_str(file.path.trim());
        }
        if !file.summary.trim().is_empty() {
            if !line.is_empty() {
                line.push_str(": ");
            }
            line.push_str(file.summary.trim());
        }
        push_non_empty(&mut summary_parts, line.trim());
    }
    for command in &packet.commands {
        let mut line = command.command.trim().to_string();
        if !command.summary.trim().is_empty() {
            if !line.is_empty() {
                line.push_str(": ");
            }
            line.push_str(command.summary.trim());
        }
        push_non_empty(&mut summary_parts, line.trim());
    }
    for context in &packet.direct_context {
        push_non_empty(&mut summary_parts, context.trim());
    }
    for caveat in &packet.caveats {
        push_non_empty(&mut summary_parts, caveat.trim());
    }
    for next_step in &packet.next_steps {
        push_non_empty(&mut summary_parts, next_step.trim());
    }

    if summary_parts.is_empty() {
        return None;
    }

    let mut file_paths = packet
        .changed_files
        .iter()
        .map(|file| file.path.trim().to_string())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    sort_and_dedup_strings(&mut file_paths);

    Some(DeliveryCandidate {
        id: format!("{}.handoff", safe_id_part(&packet.task_id)),
        source: DeliveryCandidateSource::Handoff,
        title: format!(
            "{} task output",
            non_empty_or(packet.task_title.clone(), || packet.task_id.clone())
        ),
        summary: summary_parts.join(" "),
        file_paths,
        confidence: match packet.confidence {
            HandoffConfidence::High => DeliveryConfidence::Medium,
            HandoffConfidence::Medium => DeliveryConfidence::Low,
            HandoffConfidence::Low => DeliveryConfidence::Low,
        },
    })
}

fn push_non_empty(out: &mut Vec<String>, value: &str) {
    if !value.trim().is_empty() {
        out.push(value.trim().to_string());
    }
}

fn parse_artifact_file_paths(file_paths_json: &str, artifact_id: &str) -> Vec<String> {
    match serde_json::from_str::<Vec<String>>(file_paths_json) {
        Ok(paths) => paths
            .into_iter()
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
            .collect(),
        Err(err) => {
            tracing::warn!(
                artifact_id = %artifact_id,
                error = %err,
                "skipping malformed artifact file_paths JSON during delivery snapshot generation"
            );
            Vec::new()
        }
    }
}

fn source_task_ids_json(source_task_ids: &[String]) -> Result<String> {
    let mut ids = source_task_ids
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    sort_and_dedup_strings(&mut ids);
    serde_json::to_string(&ids).map_err(Into::into)
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

fn compare_delivery_candidates(a: &DeliveryCandidate, b: &DeliveryCandidate) -> Ordering {
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
    if max_chars == 0 {
        return String::new();
    }

    const TRUNCATION_MARKER: &str = "\n…(truncated)";
    let marker_chars = TRUNCATION_MARKER.chars().count();
    if max_chars <= marker_chars {
        return TRUNCATION_MARKER
            .trim_start_matches('\n')
            .chars()
            .take(max_chars)
            .collect();
    }

    let keep_chars = max_chars - marker_chars;
    let mut truncated = value.chars().take(keep_chars).collect::<String>();
    truncated.push_str(TRUNCATION_MARKER);
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{migrations_run_on, queries, Database};
    use rusqlite::Connection;

    fn setup_delivery_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations_run_on(&conn).expect("run migrations");
        conn
    }

    fn insert_mission(conn: &Connection, id: &str, title: &str, status: &str) {
        conn.execute(
            "INSERT INTO missions (id, title, description, status) VALUES (?1, ?2, 'Build delivery outputs', ?3)",
            rusqlite::params![id, title, status],
        )
        .unwrap();
    }

    fn insert_task(conn: &Connection, id: &str, mission_id: &str, title: &str, status: &str) {
        conn.execute(
            "INSERT INTO tasks (id, mission_id, title, description, status) VALUES (?1, ?2, ?3, 'Produce output', ?4)",
            rusqlite::params![id, mission_id, title, status],
        )
        .unwrap();
    }

    fn insert_artifact_row(
        conn: &Connection,
        id: &str,
        mission_id: &str,
        task_id: &str,
        local_name: &str,
        summary: &str,
        file_paths_json: &str,
    ) {
        insert_artifact_row_with_published(
            conn,
            id,
            mission_id,
            task_id,
            local_name,
            summary,
            file_paths_json,
            true,
        );
    }

    fn insert_artifact_row_with_published(
        conn: &Connection,
        id: &str,
        mission_id: &str,
        task_id: &str,
        local_name: &str,
        summary: &str,
        file_paths_json: &str,
        published: bool,
    ) {
        conn.execute(
            "INSERT INTO artifacts (id, mission_id, producer_task_id, type, local_name, summary, file_paths, published)
             VALUES (?1, ?2, ?3, 'report', ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                mission_id,
                task_id,
                local_name,
                summary,
                file_paths_json,
                if published { 1i64 } else { 0i64 }
            ],
        )
        .unwrap();
    }

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
        assert_eq!(
            handoff.changed_files[0].path,
            "src-tauri/src/agent/engine.rs"
        );
        assert_eq!(
            handoff.decisions[0].title,
            "Persist from task_complete input"
        );
        assert_eq!(handoff.commands[0].status, CommandResultStatus::Passed);
        assert_eq!(
            handoff.direct_context,
            vec!["Engine can derive status from handoff presence."]
        );
        assert_eq!(
            handoff.caveats,
            vec!["Fallback rows should not overwrite agent-authored rows."]
        );
        assert_eq!(
            handoff.next_steps,
            vec!["Use this packet for child task prompts."]
        );
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
    fn malformed_handoff_falls_back_to_summary_and_artifacts() {
        for handoff_value in [
            serde_json::Value::Null,
            serde_json::json!("not an object"),
            serde_json::json!([]),
        ] {
            let value = serde_json::json!({
                "summary": "Completed with published outputs.",
                "handoff": handoff_value,
            });

            let handoff = TaskHandoffPacket::from_task_complete_value(
                "mission-1",
                "task-1",
                "Publish outputs",
                "Create downstream artifacts.",
                &value,
                vec![artifact(
                    "published_report",
                    "Published report for child tasks.",
                    &["dist/report.md"],
                )],
            )
            .expect("malformed handoff falls back instead of erroring");

            assert_eq!(handoff.summary, "Completed with published outputs.");
            assert_eq!(handoff.confidence, HandoffConfidence::Low);
            assert_eq!(handoff.artifacts[0].local_name, "published_report");
            assert!(handoff
                .caveats
                .iter()
                .any(|caveat| caveat.contains("fallback")));
        }
    }

    #[test]
    fn empty_handoff_object_falls_back_to_summary_and_artifacts() {
        let value = serde_json::json!({
            "summary": "Completed with published outputs.",
            "handoff": {},
        });

        let handoff = TaskHandoffPacket::from_task_complete_value(
            "mission-1",
            "task-1",
            "Publish outputs",
            "Create downstream artifacts.",
            &value,
            vec![artifact(
                "published_report",
                "Published report for child tasks.",
                &["dist/report.md"],
            )],
        )
        .expect("empty handoff object falls back instead of agent-authored packet");

        assert_eq!(handoff.summary, "Completed with published outputs.");
        assert_eq!(handoff.confidence, HandoffConfidence::Low);
        assert_eq!(handoff.artifacts[0].local_name, "published_report");
        assert!(handoff
            .caveats
            .iter()
            .any(|caveat| caveat.contains("fallback")));
    }

    #[test]
    fn delivery_snapshot_round_trips_json_for_frontend() {
        let snapshot = MissionDeliverySnapshot::degraded(
            "mission-1",
            vec![DeliveryCandidate::from_artifact(
                "task-1",
                &artifact(
                    "release_notes",
                    "Markdown release notes for operators.",
                    &["dist/release-notes.md"],
                ),
            )],
        );

        let json = serde_json::to_value(&snapshot).expect("snapshot serializes");
        assert_eq!(json["schema_version"], DELIVERY_SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(json["mission_id"], "mission-1");
        assert_eq!(json["status"], "completed_with_warnings");
        assert_eq!(json["confidence"], "low");
        assert_eq!(json["overview"]["status"], "completed_with_warnings");
        assert_eq!(json["overview"]["confidence"], "low");
        assert_eq!(json["items"][0]["source"], "artifact");
        assert_eq!(json["items"][0]["confidence"], "medium");
        assert_eq!(json["validation"][0]["status"], "not_run");

        let round_tripped: MissionDeliverySnapshot =
            serde_json::from_value(json).expect("frontend JSON shape deserializes");
        assert_eq!(round_tripped, snapshot);
    }

    #[test]
    fn candidates_from_artifacts_parses_paths_and_skips_malformed_path_lists() {
        let artifacts = vec![
            queries::ArtifactRow {
                id: "artifact-b".to_string(),
                mission_id: "mission-1".to_string(),
                producer_task_id: "task-b".to_string(),
                artifact_type: "report".to_string(),
                local_name: "beta".to_string(),
                summary: "Beta report".to_string(),
                file_paths: "[\"dist/beta.md\", \"dist/beta.md\", \"\"]".to_string(),
                published: true,
                created_at: "2026-06-15 10:00:00".to_string(),
            },
            queries::ArtifactRow {
                id: "artifact-a".to_string(),
                mission_id: "mission-1".to_string(),
                producer_task_id: "task-a".to_string(),
                artifact_type: "report".to_string(),
                local_name: "alpha".to_string(),
                summary: "Alpha report".to_string(),
                file_paths: "not json".to_string(),
                published: true,
                created_at: "2026-06-15 09:00:00".to_string(),
            },
        ];

        let candidates = candidates_from_artifacts(&artifacts);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].id, "artifact-a");
        assert_eq!(candidates[0].title, "alpha");
        assert!(candidates[0].file_paths.is_empty());
        assert_eq!(candidates[1].id, "artifact-b");
        assert_eq!(candidates[1].file_paths, vec!["dist/beta.md"]);
    }

    #[test]
    fn generate_degraded_delivery_snapshot_collects_artifacts_handoffs_and_failed_status() {
        let conn = setup_delivery_db();
        insert_mission(&conn, "mission-1", "Ship CLI", "failed");
        insert_task(
            &conn,
            "task-1",
            "mission-1",
            "Write release notes",
            "completed",
        );
        insert_task(
            &conn,
            "task-2",
            "mission-1",
            "Write malformed handoff",
            "completed",
        );
        insert_artifact_row(
            &conn,
            "artifact-1",
            "mission-1",
            "task-1",
            "release_notes",
            "Markdown release notes for operators.",
            "[\"dist/release-notes.md\"]",
        );
        queries::upsert_task_handoff_packet(
            &conn,
            "task-1",
            "mission-1",
            &serde_json::to_string(&TaskHandoffPacket::fallback(
                "mission-1",
                "task-1",
                "Write release notes",
                "Release notes are ready.",
                vec![artifact(
                    "handoff_manifest",
                    "Manifest from handoff.",
                    &["dist/manifest.json"],
                )],
            ))
            .unwrap(),
            "generated",
        )
        .unwrap();
        queries::upsert_task_handoff_packet(&conn, "task-2", "mission-1", "not json", "generated")
            .unwrap();

        let generation = generate_degraded_delivery_snapshot(&conn, "mission-1")
            .expect("snapshot generation succeeds with corrupt handoff skipped");
        let snapshot = generation.snapshot;

        assert_eq!(snapshot.mission_id, "mission-1");
        assert_eq!(snapshot.status, DeliveryStatus::Failed);
        assert_eq!(snapshot.overview.status, DeliveryStatus::Failed);
        assert!(snapshot.overview.summary.contains("Ship CLI"));
        assert!(snapshot.items.iter().any(
            |item| item.id == "artifact-1" && item.file_paths == vec!["dist/release-notes.md"]
        ));
    }

    #[test]
    fn generate_degraded_delivery_snapshot_excludes_unpublished_artifacts() {
        let conn = setup_delivery_db();
        insert_mission(&conn, "mission-1", "Ship CLI", "completed");
        insert_task(&conn, "task-1", "mission-1", "Write outputs", "completed");
        insert_artifact_row_with_published(
            &conn,
            "published-artifact",
            "mission-1",
            "task-1",
            "published_report",
            "Visible report.",
            "[\"dist/published.md\"]",
            true,
        );
        insert_artifact_row_with_published(
            &conn,
            "draft-artifact",
            "mission-1",
            "task-1",
            "draft_report",
            "Planner declaration not produced by an agent yet.",
            "[\"dist/draft.md\"]",
            false,
        );

        let generation = generate_degraded_delivery_snapshot(&conn, "mission-1")
            .expect("snapshot generation succeeds");

        assert!(generation
            .snapshot
            .items
            .iter()
            .any(|item| item.id == "published-artifact"));
        assert!(!generation
            .snapshot
            .items
            .iter()
            .any(|item| item.id == "draft-artifact"));
        assert_eq!(generation.source_task_ids, vec!["task-1".to_string()]);
    }

    #[test]
    fn delivery_source_task_ids_are_explicit_task_ids_not_parsed_item_ids() {
        let conn = setup_delivery_db();
        insert_mission(&conn, "mission-1", "Ship CLI", "completed");
        insert_task(
            &conn,
            "real-task",
            "mission-1",
            "Publish report",
            "completed",
        );
        insert_task(
            &conn,
            "handoff-task",
            "mission-1",
            "Publish manifest",
            "completed",
        );
        insert_artifact_row(
            &conn,
            "artifact.id.with.dots",
            "mission-1",
            "real-task",
            "release_notes",
            "Markdown release notes for operators.",
            "[\"dist/release-notes.md\"]",
        );
        queries::upsert_task_handoff_packet(
            &conn,
            "handoff-task",
            "mission-1",
            &serde_json::to_string(&TaskHandoffPacket::fallback(
                "mission-1",
                "handoff-task",
                "Publish manifest",
                "Manifest is ready.",
                vec![DeliveryArtifactRef {
                    artifact_id: Some("custom.handoff.item".to_string()),
                    local_name: "handoff_manifest".to_string(),
                    artifact_type: "report".to_string(),
                    summary: "Manifest from handoff.".to_string(),
                    file_paths: vec!["dist/manifest.json".to_string()],
                }],
            ))
            .unwrap(),
            "generated",
        )
        .unwrap();

        let generation = generate_degraded_delivery_snapshot(&conn, "mission-1")
            .expect("snapshot generation succeeds");
        persist_delivery_snapshot(
            &conn,
            &generation.snapshot,
            "degraded",
            Some("deterministic"),
            &generation.source_task_ids,
        )
        .expect("snapshot persists");

        let row = queries::get_mission_delivery(&conn, "mission-1")
            .unwrap()
            .expect("delivery row exists");
        let source_task_ids: Vec<String> =
            serde_json::from_str(&row.source_task_ids).expect("source task IDs parse");

        assert_eq!(
            source_task_ids,
            vec!["handoff-task".to_string(), "real-task".to_string()]
        );
    }

    #[test]
    fn generate_degraded_delivery_snapshot_rejects_nonterminal_missions() {
        let conn = setup_delivery_db();
        insert_mission(&conn, "mission-1", "Ship CLI", "running");

        let err = generate_degraded_delivery_snapshot(&conn, "mission-1")
            .expect_err("running mission should not generate on-demand delivery");

        assert!(
            err.to_string().contains("not ready for delivery"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn degraded_generation_does_not_overwrite_non_stale_generated_delivery() {
        let conn = setup_delivery_db();
        insert_mission(&conn, "mission-1", "Ship CLI", "completed");
        let curated = MissionDeliverySnapshot {
            schema_version: DELIVERY_SNAPSHOT_SCHEMA_VERSION,
            mission_id: "mission-1".to_string(),
            status: DeliveryStatus::Completed,
            confidence: DeliveryConfidence::High,
            overview: DeliveryOverview {
                title: "Curated delivery".to_string(),
                summary: "Curated generated snapshot should survive degraded refresh.".to_string(),
                status: DeliveryStatus::Completed,
                confidence: DeliveryConfidence::High,
            },
            items: vec![DeliveryItem {
                id: "curated-item".to_string(),
                source: DeliveryCandidateSource::ModelHint,
                title: "Curated output".to_string(),
                summary: "Generated curator output.".to_string(),
                file_paths: vec!["dist/curated.md".to_string()],
                confidence: DeliveryConfidence::High,
            }],
            how_to_use: vec![],
            validation: vec![],
            changes: vec![],
            caveats: vec![],
            next_steps: vec![],
        };
        persist_delivery_snapshot(&conn, &curated, "generated", Some("claude-curator"), &[])
            .expect("curated snapshot persists");

        let degraded = MissionDeliverySnapshot::degraded("mission-1", vec![]);
        persist_delivery_snapshot(&conn, &degraded, "degraded", Some("deterministic"), &[])
            .expect("degraded persistence is allowed to no-op");

        let row = queries::get_mission_delivery(&conn, "mission-1")
            .unwrap()
            .expect("delivery row exists");
        let persisted: MissionDeliverySnapshot =
            serde_json::from_str(&row.snapshot_json).expect("persisted snapshot parses");
        assert_eq!(row.generation_status, "generated");
        assert_eq!(row.curator_model.as_deref(), Some("claude-curator"));
        assert!(!row.stale);
        assert_eq!(persisted.overview.title, "Curated delivery");
        assert_eq!(persisted.items[0].id, "curated-item");
    }

    #[test]
    fn degraded_generation_conn_helper_persists_terminal_snapshot_without_database_relock() {
        let db = Database::open_in_memory().expect("open in-memory db");
        db.with_conn(|conn| {
            insert_mission(conn, "mission-1", "Ship CLI", "failed");
            insert_task(conn, "task-1", "mission-1", "Publish manifest", "completed");
            insert_artifact_row(
                conn,
                "artifact-1",
                "mission-1",
                "task-1",
                "manifest",
                "Release manifest for operators.",
                r#"[\"dist/manifest.json\"]"#,
            );

            generate_and_persist_degraded_delivery_on_conn(conn, "mission-1")
                .expect("terminal snapshot generation persists with existing connection");

            let row =
                queries::get_mission_delivery(conn, "mission-1")?.expect("delivery row exists");
            assert_eq!(row.generation_status, "degraded");
            assert_eq!(row.curator_model.as_deref(), Some("deterministic"));
            let persisted: MissionDeliverySnapshot =
                serde_json::from_str(&row.snapshot_json).expect("persisted snapshot parses");
            assert_eq!(persisted.status, DeliveryStatus::Failed);
            assert!(persisted.items.iter().any(|item| {
                item.id == "artifact-1" && item.file_paths == vec!["dist/manifest.json"]
            }));
            Ok(())
        })
        .expect("connection-level generation succeeds without re-locking database");
    }

    #[test]
    fn degraded_generation_helper_persists_terminal_snapshot_without_overwriting_generated() {
        let db = Database::open_in_memory().expect("open in-memory db");
        db.with_conn(|conn| {
            insert_mission(conn, "mission-1", "Ship CLI", "completed");
            insert_task(conn, "task-1", "mission-1", "Publish manifest", "completed");
            insert_artifact_row(
                conn,
                "artifact-1",
                "mission-1",
                "task-1",
                "manifest",
                "Release manifest for operators.",
                r#"[\"dist/manifest.json\"]"#,
            );
            Ok(())
        })
        .expect("seed delivery inputs");

        generate_and_persist_degraded_delivery(&db, "mission-1")
            .expect("terminal snapshot generation persists");

        let degraded_row = db
            .with_conn(|conn| queries::get_mission_delivery(conn, "mission-1"))
            .expect("read degraded delivery")
            .expect("degraded delivery row exists");
        assert_eq!(degraded_row.generation_status, "degraded");
        assert_eq!(degraded_row.curator_model.as_deref(), Some("deterministic"));
        assert!(!degraded_row.stale);

        let curated = MissionDeliverySnapshot {
            schema_version: DELIVERY_SNAPSHOT_SCHEMA_VERSION,
            mission_id: "mission-1".to_string(),
            status: DeliveryStatus::Completed,
            confidence: DeliveryConfidence::High,
            overview: DeliveryOverview {
                title: "Curated delivery".to_string(),
                summary: "Curated generated snapshot should survive degraded refresh.".to_string(),
                status: DeliveryStatus::Completed,
                confidence: DeliveryConfidence::High,
            },
            items: vec![DeliveryItem {
                id: "curated-item".to_string(),
                source: DeliveryCandidateSource::ModelHint,
                title: "Curated output".to_string(),
                summary: "Generated curator output.".to_string(),
                file_paths: vec!["dist/curated.md".to_string()],
                confidence: DeliveryConfidence::High,
            }],
            how_to_use: vec![],
            validation: vec![],
            changes: vec![],
            caveats: vec![],
            next_steps: vec![],
        };
        db.with_conn(|conn| {
            persist_delivery_snapshot(conn, &curated, "generated", Some("claude-curator"), &[])
        })
        .expect("curated snapshot persists");

        generate_and_persist_degraded_delivery(&db, "mission-1")
            .expect("degraded generation no-ops over fresh generated snapshot");

        let row = db
            .with_conn(|conn| queries::get_mission_delivery(conn, "mission-1"))
            .expect("read final delivery")
            .expect("delivery row exists");
        let persisted: MissionDeliverySnapshot =
            serde_json::from_str(&row.snapshot_json).expect("persisted snapshot parses");
        assert_eq!(row.generation_status, "generated");
        assert_eq!(row.curator_model.as_deref(), Some("claude-curator"));
        assert_eq!(persisted.overview.title, "Curated delivery");
        assert_eq!(persisted.items[0].id, "curated-item");
    }

    #[test]
    fn generate_degraded_delivery_snapshot_uses_row_task_id_for_handoff_sources() {
        let conn = setup_delivery_db();
        insert_mission(&conn, "mission-1", "Ship CLI", "completed");
        insert_task(
            &conn,
            "db-task",
            "mission-1",
            "Publish manifest",
            "completed",
        );
        let mut packet = TaskHandoffPacket::fallback(
            "mission-1",
            "embedded-task",
            "Publish manifest",
            "Manifest is ready.",
            vec![],
        );
        packet
            .direct_context
            .push("Use dist/manifest.json downstream.".to_string());
        queries::upsert_task_handoff_packet(
            &conn,
            "db-task",
            "mission-1",
            &serde_json::to_string(&packet).unwrap(),
            "agent_authored",
        )
        .unwrap();

        let generation = generate_degraded_delivery_snapshot(&conn, "mission-1")
            .expect("snapshot generation succeeds");

        assert_eq!(generation.source_task_ids, vec!["db-task".to_string()]);
        assert!(generation
            .snapshot
            .items
            .iter()
            .any(|item| item.id.starts_with("db-task")
                && item.source == DeliveryCandidateSource::Handoff));
    }

    #[test]
    fn handoff_without_artifacts_still_contributes_visible_delivery_candidate() {
        let conn = setup_delivery_db();
        insert_mission(&conn, "mission-1", "Ship CLI", "completed");
        insert_task(
            &conn,
            "task-1",
            "mission-1",
            "Document behavior",
            "completed",
        );
        let packet = TaskHandoffPacket {
            mission_id: "mission-1".to_string(),
            task_id: "task-1".to_string(),
            task_title: "Document behavior".to_string(),
            summary: "Documented the handoff-only behavior.".to_string(),
            confidence: HandoffConfidence::High,
            changed_files: vec![ChangedFileSummary {
                path: "src/lib.rs".to_string(),
                summary: "Updated delivery logic.".to_string(),
                change_type: Some("modified".to_string()),
            }],
            decisions: vec![],
            commands: vec![CommandRunSummary {
                command: "cargo test delivery_snapshot --lib".to_string(),
                status: CommandResultStatus::Passed,
                summary: "Delivery tests passed.".to_string(),
                exit_code: Some(0),
            }],
            artifacts: vec![],
            direct_context: vec!["No artifact was produced; use the task output.".to_string()],
            caveats: vec!["Manual review recommended.".to_string()],
            next_steps: vec!["Proceed to downstream delivery.".to_string()],
        };
        queries::upsert_task_handoff_packet(
            &conn,
            "task-1",
            "mission-1",
            &serde_json::to_string(&packet).unwrap(),
            "agent_authored",
        )
        .unwrap();

        let generation = generate_degraded_delivery_snapshot(&conn, "mission-1")
            .expect("snapshot generation succeeds");

        let item = generation
            .snapshot
            .items
            .iter()
            .find(|item| item.source == DeliveryCandidateSource::Handoff)
            .expect("handoff-only packet contributes a visible delivery item");
        assert_eq!(item.id, "task-1.handoff");
        assert!(item.title.contains("Document behavior"));
        assert!(item
            .summary
            .contains("Documented the handoff-only behavior"));
        assert!(item.summary.contains("No artifact was produced"));
        assert_eq!(item.file_paths, vec!["src/lib.rs".to_string()]);
    }

    #[test]
    fn stale_helper_marks_existing_delivery_stale_after_source_change() {
        let conn = setup_delivery_db();
        insert_mission(&conn, "mission-1", "Ship CLI", "completed");
        let snapshot = MissionDeliverySnapshot::degraded("mission-1", vec![]);
        persist_delivery_snapshot(&conn, &snapshot, "degraded", Some("deterministic"), &[])
            .expect("snapshot persists");

        assert!(mark_mission_delivery_stale_best_effort(
            &conn,
            "mission-1",
            "test source change"
        ));

        let row = queries::get_mission_delivery(&conn, "mission-1")
            .unwrap()
            .expect("delivery row exists");
        assert!(row.stale);
    }

    #[test]
    fn persist_delivery_snapshot_upserts_frontend_json() {
        let conn = setup_delivery_db();
        insert_mission(&conn, "mission-1", "Ship CLI", "completed");
        let snapshot = MissionDeliverySnapshot::degraded("mission-1", vec![]);

        persist_delivery_snapshot(&conn, &snapshot, "degraded", Some("deterministic"), &[])
            .expect("snapshot persists");

        let row = queries::get_mission_delivery(&conn, "mission-1")
            .unwrap()
            .expect("delivery row exists");
        assert_eq!(row.generation_status, "degraded");
        assert_eq!(row.curator_model.as_deref(), Some("deterministic"));
        assert!(!row.stale);
        let persisted: MissionDeliverySnapshot =
            serde_json::from_str(&row.snapshot_json).expect("persisted snapshot JSON parses");
        assert_eq!(persisted, snapshot);
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
    fn render_handoffs_for_prompt_marks_truncation() {
        let handoff = TaskHandoffPacket::fallback(
            "mission-1",
            "task-1",
            "Build CLI",
            &"A very long summary. ".repeat(200),
            vec![],
        );

        let rendered = render_handoffs_for_prompt(&[handoff], 120);

        assert!(
            rendered.contains("truncated") || rendered.contains('…'),
            "truncated handoff render lacked visible marker: {rendered:?}"
        );
        assert!(rendered.chars().count() <= 120);
    }

    #[test]
    fn invalid_curator_json_falls_back_to_degraded_snapshot() {
        let fallback = MissionDeliverySnapshot::degraded_with_mission_status(
            "mission-1",
            Some("Build app"),
            Some("completed"),
            vec![],
        );

        let parsed = parse_curator_snapshot_or_fallback("not json", fallback.clone());

        assert_eq!(parsed.mission_id, fallback.mission_id);
        assert_eq!(parsed.status, fallback.status);
        assert!(parsed
            .caveats
            .iter()
            .any(|caveat| caveat.contains("model curation output was invalid")));
    }

    #[test]
    fn render_delivery_for_prompt_includes_primary_deliverable_and_caveat() {
        let snapshot = MissionDeliverySnapshot {
            schema_version: DELIVERY_SNAPSHOT_SCHEMA_VERSION,
            mission_id: "mission-1".into(),
            status: DeliveryStatus::CompletedWithWarnings,
            confidence: DeliveryConfidence::Medium,
            overview: DeliveryOverview {
                title: "Build app".into(),
                summary: "Packaged the app for review.".into(),
                status: DeliveryStatus::CompletedWithWarnings,
                confidence: DeliveryConfidence::Medium,
            },
            items: vec![DeliveryItem {
                id: "app".into(),
                source: DeliveryCandidateSource::Artifact,
                title: "Miragenty.app".into(),
                summary: "Runnable app bundle.".into(),
                file_paths: vec!["target/release/bundle/macos/Miragenty.app".into()],
                confidence: DeliveryConfidence::High,
            }],
            how_to_use: vec![HowToUseStep {
                title: "Open app".into(),
                detail: "Open the app bundle from Finder.".into(),
            }],
            validation: vec![],
            changes: vec![],
            caveats: vec!["Not notarized".into()],
            next_steps: vec![],
        };

        let rendered = render_delivery_for_prompt(&snapshot, 2_000);

        assert!(rendered.contains("Miragenty.app"));
        assert!(rendered.contains("target/release/bundle/macos/Miragenty.app"));
        assert!(rendered.contains("Not notarized"));
    }
}
