use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use uuid::Uuid;

use crate::db::{queries, Database};
use crate::git::{DiffFile, WorktreeManager};
use crate::llm::{LlmProvider, LlmRequest, Message, MessageRole, ContentBlock};

// ---- Evaluator output types ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationOutput {
    pub file_reviews: Vec<FileReview>,
    pub overall_score: f64,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReview {
    pub file_path: String,
    pub score: f64,
    pub annotations: Vec<AnnotationOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationOutput {
    pub line: i64,
    #[serde(rename = "type")]
    pub ann_type: String,
    #[serde(default = "default_severity")]
    pub severity: String,
    pub message: String,
    #[serde(default)]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub auto_fixable: bool,
    #[serde(default)]
    pub original_code: Option<String>,
    #[serde(default)]
    pub fixed_code: Option<String>,
}

fn default_severity() -> String {
    "info".to_string()
}

// ---- Event payload ----

#[derive(Debug, Clone, Serialize)]
pub struct EvaluationCompletePayload {
    pub agent_id: String,
    pub overall_score: f64,
    pub annotation_count: u32,
}

// ---- Evaluator Agent ----

pub struct EvaluatorAgent {
    provider: Arc<dyn LlmProvider>,
    model: String,
    app_handle: tauri::AppHandle,
}

impl EvaluatorAgent {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: String,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            provider,
            model,
            app_handle,
        }
    }

    pub async fn evaluate(
        &self,
        agent_id: &str,
        mission_id: &str,
        repo_path: &PathBuf,
    ) -> Result<()> {
        let db = self.app_handle.state::<Database>();

        // Prevent duplicate evaluation (BT-07)
        let already_evaluated = db.with_conn(|conn| queries::has_evaluator_review(conn, agent_id))?;
        if already_evaluated {
            tracing::info!("Evaluator: agent {agent_id} already has a review, skipping");
            return Ok(());
        }

        let wt_manager = WorktreeManager::new(repo_path.clone());
        let diff_files = self.get_diff_for_agent(agent_id, &wt_manager)?;

        // BT-03: no diff → skip, score=10
        if diff_files.is_empty() {
            tracing::info!("Evaluator: agent {agent_id} produced no changes, auto-scoring 10");
            let review_id = Uuid::new_v4().to_string();
            db.with_conn(|conn| {
                queries::insert_evaluator_review(
                    conn, &review_id, agent_id, mission_id,
                    10.0, "No changes to review.", None,
                )
            })?;
            self.emit_complete(agent_id, 10.0, 0);
            return Ok(());
        }

        let contract_criteria = self.get_contract_criteria(mission_id);
        let diff_text = Self::format_diff(&diff_files);
        let system_prompt = Self::build_system_prompt(&diff_text, &contract_criteria);

        let output = self.call_llm_for_evaluation(&system_prompt).await?;
        let review_id = Uuid::new_v4().to_string();

        let score = output.overall_score.clamp(0.0, 10.0);

        let contract_compliance = if !contract_criteria.is_empty() {
            Some(format!("Evaluated against contract criteria"))
        } else {
            None
        };

        db.with_conn(|conn| {
            queries::insert_evaluator_review(
                conn, &review_id, agent_id, mission_id,
                score, &output.summary, contract_compliance.as_deref(),
            )
        })?;

        let mut annotation_count: u32 = 0;
        for file_review in &output.file_reviews {
            for ann in &file_review.annotations {
                let ann_id = Uuid::new_v4().to_string();
                let ann_type = Self::normalize_type(&ann.ann_type);
                let severity = Self::normalize_severity(&ann.severity);

                db.with_conn(|conn| {
                    queries::insert_evaluator_annotation(
                        conn, &ann_id, &review_id, agent_id,
                        &file_review.file_path, ann.line,
                        &ann_type, &severity, &ann.message,
                        ann.suggestion.as_deref(), ann.auto_fixable,
                        ann.original_code.as_deref(), ann.fixed_code.as_deref(),
                    )
                })?;
                annotation_count += 1;
            }
        }

        // Auto-fix
        let auto_fix_count = self.apply_auto_fixes(agent_id, &review_id, repo_path)?;
        if auto_fix_count > 0 {
            tracing::info!("Evaluator: auto-fixed {auto_fix_count} issue(s) for agent {agent_id}");
        }

        // Contract threshold check (FR-02.4)
        self.check_quality_threshold(agent_id, mission_id, score)?;

        self.emit_complete(agent_id, score, annotation_count);
        tracing::info!(
            "Evaluator: completed review for agent {agent_id} — score={score}, annotations={annotation_count}"
        );

        Ok(())
    }

    fn get_diff_for_agent(&self, agent_id: &str, wt_manager: &WorktreeManager) -> Result<Vec<DiffFile>> {
        match wt_manager.get_structured_diff(agent_id) {
            Ok(files) => Ok(files),
            Err(_) => {
                let db = self.app_handle.state::<Database>();
                let hashes = db.with_conn(|conn| queries::get_agent_commit_hashes(conn, agent_id))?;
                match (hashes.base_commit_hash, hashes.head_commit_hash) {
                    (Some(base), Some(head)) => wt_manager
                        .get_structured_diff_by_hashes(&base, &head)
                        .context("Failed to get diff by commit hashes"),
                    _ => Ok(Vec::new()),
                }
            }
        }
    }

    fn get_contract_criteria(&self, mission_id: &str) -> String {
        let db = self.app_handle.state::<Database>();
        db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT ci.section, ci.text FROM contract_items ci
                 JOIN mission_contracts mc ON mc.id = ci.contract_id
                 WHERE mc.mission_id = ?1 AND mc.status = 'signed'
                 ORDER BY ci.section",
            )?;
            let items: Vec<String> = stmt
                .query_map(rusqlite::params![mission_id], |row| {
                    let section: String = row.get(0)?;
                    let text: String = row.get(1)?;
                    Ok(format!("- [{section}] {text}"))
                })?
                .filter_map(|r| r.ok())
                .collect();
            Ok(items.join("\n"))
        })
        .unwrap_or_default()
    }

    fn format_diff(files: &[DiffFile]) -> String {
        let mut out = String::new();
        for file in files {
            out.push_str(&format!("=== {} ({}) ===\n", file.path, file.status));
            if let Some(ref new) = file.new_content {
                for (i, line) in new.lines().enumerate() {
                    out.push_str(&format!("{:>4} | {}\n", i + 1, line));
                }
            }
            out.push('\n');
        }
        out
    }

    fn build_system_prompt(diff_text: &str, contract_criteria: &str) -> String {
        let contract_section = if contract_criteria.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n# Contract Acceptance Criteria\n\
                 Evaluate the code against these criteria and note any non-compliance:\n{contract_criteria}"
            )
        };

        format!(
            "You are a code review expert (Evaluator Agent). Your task is to review the following \
             code changes (diff) and provide line-level annotations with quality scores.\n\n\
             # Review Dimensions\n\
             - bug: logic errors, null pointers, unhandled edge cases\n\
             - style: code style, naming conventions, duplicate code\n\
             - performance: performance issues, unnecessary loops/allocations\n\
             - security: security vulnerabilities, sensitive data exposure\n\
             - suggestion: improvement suggestions\n\n\
             # Severity Levels\n\
             - error: must fix, blocks quality\n\
             - warning: should fix, potential problem\n\
             - info: nice to have, minor improvement\n\n\
             # Output Format\n\
             You MUST output strict JSON (no markdown code block markers):\n\
             {{\n\
               \"file_reviews\": [\n\
                 {{\n\
                   \"file_path\": \"path/to/file\",\n\
                   \"score\": 8.5,\n\
                   \"annotations\": [\n\
                     {{\n\
                       \"line\": 42,\n\
                       \"type\": \"bug|style|performance|security|suggestion\",\n\
                       \"severity\": \"error|warning|info\",\n\
                       \"message\": \"description of the issue\",\n\
                       \"suggestion\": \"how to fix it\",\n\
                       \"auto_fixable\": false,\n\
                       \"original_code\": \"original line if auto_fixable\",\n\
                       \"fixed_code\": \"fixed line if auto_fixable\"\n\
                     }}\n\
                   ]\n\
                 }}\n\
               ],\n\
               \"overall_score\": 8.0,\n\
               \"summary\": \"brief summary of findings\"\n\
             }}\n\n\
             Rules:\n\
             - score is 0-10 (10 = perfect)\n\
             - Only mark auto_fixable=true for trivial fixes (add import, fix typo, add semicolon)\n\
             - If auto_fixable=true, you MUST provide original_code and fixed_code\n\
             - Output ONLY the JSON, no other text\
             {contract_section}\n\n\
             # Code Changes\n{diff_text}"
        )
    }

    async fn call_llm_for_evaluation(&self, system_prompt: &str) -> Result<EvaluationOutput> {
        let request = LlmRequest {
            model: self.model.clone(),
            system: Some(system_prompt.to_string()),
            messages: vec![Message {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "Please review the code changes above and output your evaluation as JSON.".to_string(),
                }],
                cache_control: None,
            }],
            tools: vec![],
            max_tokens: 4096,
        };

        // First attempt
        match self.try_parse_llm_response(&request).await {
            Ok(output) => return Ok(output),
            Err(e) => {
                tracing::warn!("Evaluator: first LLM parse attempt failed: {e}, retrying (BT-01)");
            }
        }

        // Retry once (BT-01)
        self.try_parse_llm_response(&request)
            .await
            .context("Evaluator: LLM output parse failed after retry")
    }

    async fn try_parse_llm_response(&self, request: &LlmRequest) -> Result<EvaluationOutput> {
        let response = self.provider.chat(request).await?;

        let text = response
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        Self::parse_evaluation_json(&text)
    }

    pub fn parse_evaluation_json(raw: &str) -> Result<EvaluationOutput> {
        let cleaned = Self::strip_markdown_fences(raw);
        let mut output: EvaluationOutput =
            serde_json::from_str(&cleaned).context("Failed to parse evaluation JSON")?;

        output.overall_score = output.overall_score.clamp(0.0, 10.0);
        for fr in &mut output.file_reviews {
            fr.score = fr.score.clamp(0.0, 10.0);
            for ann in &mut fr.annotations {
                ann.ann_type = Self::normalize_type(&ann.ann_type);
                ann.severity = Self::normalize_severity(&ann.severity);
            }
        }

        Ok(output)
    }

    fn strip_markdown_fences(raw: &str) -> String {
        let trimmed = raw.trim();
        if trimmed.starts_with("```") {
            let after_first_fence = if let Some(pos) = trimmed.find('\n') {
                &trimmed[pos + 1..]
            } else {
                trimmed
            };
            if let Some(pos) = after_first_fence.rfind("```") {
                return after_first_fence[..pos].trim().to_string();
            }
        }
        trimmed.to_string()
    }

    fn normalize_type(t: &str) -> String {
        match t {
            "bug" | "style" | "performance" | "security" | "suggestion" => t.to_string(),
            _ => "suggestion".to_string(),
        }
    }

    fn normalize_severity(s: &str) -> String {
        match s {
            "error" | "warning" | "info" => s.to_string(),
            _ => "info".to_string(),
        }
    }

    fn apply_auto_fixes(
        &self,
        agent_id: &str,
        _review_id: &str,
        repo_path: &PathBuf,
    ) -> Result<u32> {
        let db = self.app_handle.state::<Database>();
        let annotations = db.with_conn(|conn| {
            queries::get_annotations_for_agent(conn, agent_id, None)
        })?;

        let fixable: Vec<_> = annotations
            .iter()
            .filter(|a| a.auto_fixable && a.original_code.is_some() && a.fixed_code.is_some())
            .collect();

        if fixable.is_empty() {
            return Ok(0);
        }

        let worktree_path = self.get_worktree_path(agent_id)?;
        let mut fixed_count = 0u32;
        let mut fixed_files = std::collections::HashSet::new();

        for ann in &fixable {
            let file_full_path = worktree_path.join(&ann.file_path);
            if !file_full_path.exists() {
                continue;
            }

            let original = ann.original_code.as_deref().unwrap();
            let fixed = ann.fixed_code.as_deref().unwrap();

            match std::fs::read_to_string(&file_full_path) {
                Ok(content) => {
                    // BT-04: check target line still matches
                    if content.contains(original) {
                        let new_content = content.replacen(original, fixed, 1);
                        if let Err(e) = std::fs::write(&file_full_path, &new_content) {
                            tracing::warn!("Evaluator: failed to write fix to {}: {e}", ann.file_path);
                            continue;
                        }
                        db.with_conn(|conn| {
                            queries::update_annotation_status(conn, &ann.id, "auto_fixed")
                        })?;
                        fixed_count += 1;
                        fixed_files.insert(ann.file_path.clone());
                    } else {
                        tracing::info!(
                            "Evaluator: skipping auto-fix for {}:{} — original code not found (BT-04)",
                            ann.file_path, ann.line_number
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("Evaluator: cannot read {} for auto-fix: {e}", ann.file_path);
                }
            }
        }

        // Commit all auto-fixes in one commit (UT-02.3)
        if fixed_count > 0 {
            let wt_manager = WorktreeManager::new(repo_path.clone());
            let commit_msg = format!(
                "[evaluator-auto-fix] fix {} issue(s) in {} file(s)",
                fixed_count,
                fixed_files.len()
            );
            match wt_manager.commit_worktree(agent_id, &commit_msg) {
                Ok(Some(hash)) => {
                    tracing::info!("Evaluator: auto-fix commit {hash}");
                    let _ = db.with_conn(|conn| queries::save_agent_head_commit(conn, agent_id, &hash));
                }
                Ok(None) => {
                    tracing::info!("Evaluator: auto-fix commit empty (no actual changes)");
                }
                Err(e) => {
                    tracing::error!("Evaluator: failed to commit auto-fixes: {e}");
                }
            }
        }

        Ok(fixed_count)
    }

    fn get_worktree_path(&self, agent_id: &str) -> Result<PathBuf> {
        let db = self.app_handle.state::<Database>();
        let path: String = db.with_conn(|conn| {
            conn.query_row(
                "SELECT worktree_path FROM agents WHERE id = ?1",
                rusqlite::params![agent_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("Agent worktree not found: {e}"))
        })?;
        Ok(PathBuf::from(path))
    }

    fn check_quality_threshold(
        &self,
        agent_id: &str,
        mission_id: &str,
        score: f64,
    ) -> Result<()> {
        let db = self.app_handle.state::<Database>();
        let threshold = db.with_conn(|conn| {
            queries::get_contract_quality_threshold(conn, mission_id)
        })?;

        if let Some(threshold) = threshold {
            if score < threshold {
                tracing::info!(
                    "Evaluator: agent {agent_id} score {score} < threshold {threshold}, marking needs_revision"
                );
                let task_id = db.with_conn(|conn| queries::get_task_id_for_agent(conn, agent_id))?;
                if let Some(tid) = task_id {
                    let _ = db.with_conn(|conn| queries::mark_task_needs_revision(conn, &tid));
                }
            }
        }
        Ok(())
    }

    fn emit_complete(&self, agent_id: &str, score: f64, annotation_count: u32) {
        let _ = self.app_handle.emit(
            "evaluation-complete",
            EvaluationCompletePayload {
                agent_id: agent_id.to_string(),
                overall_score: score,
                annotation_count,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ut01_1_valid_evaluation_json() {
        let json = r#"{
            "file_reviews": [
                {
                    "file_path": "src/auth/handler.rs",
                    "score": 8.5,
                    "annotations": [
                        {
                            "line": 42,
                            "type": "security",
                            "severity": "warning",
                            "message": "JWT expiration is hardcoded to 24h",
                            "suggestion": "Use environment variable JWT_EXPIRES_IN",
                            "auto_fixable": false
                        }
                    ]
                },
                {
                    "file_path": "src/db/pool.rs",
                    "score": 9.0,
                    "annotations": [
                        {
                            "line": 10,
                            "type": "style",
                            "severity": "info",
                            "message": "Consider using a constant for pool size",
                            "auto_fixable": false
                        }
                    ]
                }
            ],
            "overall_score": 8.5,
            "summary": "Good quality code with minor concerns."
        }"#;

        let result = EvaluatorAgent::parse_evaluation_json(json).unwrap();
        assert_eq!(result.file_reviews.len(), 2);
        assert_eq!(result.overall_score, 8.5);
        assert_eq!(result.file_reviews[0].annotations.len(), 1);
        assert_eq!(result.file_reviews[0].annotations[0].ann_type, "security");
    }

    #[test]
    fn ut01_2_empty_annotations() {
        let json = r#"{
            "file_reviews": [
                {
                    "file_path": "src/main.rs",
                    "score": 10.0,
                    "annotations": []
                }
            ],
            "overall_score": 10.0,
            "summary": "Perfect code."
        }"#;

        let result = EvaluatorAgent::parse_evaluation_json(json).unwrap();
        assert_eq!(result.file_reviews.len(), 1);
        assert!(result.file_reviews[0].annotations.is_empty());
        assert_eq!(result.overall_score, 10.0);
    }

    #[test]
    fn ut01_3_unknown_type_normalized() {
        let json = r#"{
            "file_reviews": [
                {
                    "file_path": "src/lib.rs",
                    "score": 7.0,
                    "annotations": [
                        {
                            "line": 1,
                            "type": "unknown_category",
                            "severity": "info",
                            "message": "test",
                            "auto_fixable": false
                        }
                    ]
                }
            ],
            "overall_score": 7.0,
            "summary": "Some issues."
        }"#;

        let result = EvaluatorAgent::parse_evaluation_json(json).unwrap();
        assert_eq!(result.file_reviews[0].annotations[0].ann_type, "suggestion");
    }

    #[test]
    fn ut01_4_missing_line_field() {
        let json = r#"{
            "file_reviews": [
                {
                    "file_path": "src/lib.rs",
                    "score": 7.0,
                    "annotations": [
                        {
                            "type": "bug",
                            "severity": "error",
                            "message": "missing line"
                        }
                    ]
                }
            ],
            "overall_score": 7.0,
            "summary": "Missing field."
        }"#;

        let result = EvaluatorAgent::parse_evaluation_json(json);
        assert!(result.is_err());
    }

    #[test]
    fn ut01_5_score_clamped() {
        let json = r#"{
            "file_reviews": [],
            "overall_score": 15.0,
            "summary": "Out of range."
        }"#;

        let result = EvaluatorAgent::parse_evaluation_json(json).unwrap();
        assert_eq!(result.overall_score, 10.0);
    }

    #[test]
    fn strip_markdown_json() {
        let wrapped = "```json\n{\"file_reviews\":[],\"overall_score\":8.0,\"summary\":\"ok\"}\n```";
        let result = EvaluatorAgent::parse_evaluation_json(wrapped).unwrap();
        assert_eq!(result.overall_score, 8.0);
    }
}
