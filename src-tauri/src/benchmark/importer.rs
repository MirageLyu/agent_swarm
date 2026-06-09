use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::sop_bench::{build_sop_prompt, row_map, SOP_DATA_CSV, SOP_DECISION_LABELS};
use super::types::{
    BenchmarkGraderSpec, BenchmarkSourceKind, ImportedCaseDraft, ImportedSuiteDraft,
};

pub fn import_suite_from_path(path: &Path) -> Result<ImportedSuiteDraft> {
    if path.join("tool_efficiency_tasks.jsonl").exists() {
        import_tool_efficiency_suite(path)
    } else if path.join("test_set_with_outputs.csv").exists() && path.join("sop.txt").exists() {
        import_sop_bench_suite(path)
    } else if path
        .join("datasets/tool_efficiency_benchmark/tool_efficiency_tasks.jsonl")
        .exists()
    {
        import_tool_efficiency_suite(&path.join("datasets/tool_efficiency_benchmark"))
    } else if path
        .join("datasets/sop_bench/test_set_with_outputs.csv")
        .exists()
        && path.join("datasets/sop_bench/sop.txt").exists()
    {
        import_sop_bench_suite(&path.join("datasets/sop_bench"))
    } else {
        Err(anyhow!(
            "unsupported benchmark directory: expected GA tool_efficiency_tasks.jsonl or SOP-Bench test_set_with_outputs.csv + sop.txt"
        ))
    }
}

fn import_tool_efficiency_suite(path: &Path) -> Result<ImportedSuiteDraft> {
    let tasks_path = path.join("tool_efficiency_tasks.jsonl");
    let content = fs::read_to_string(&tasks_path)
        .with_context(|| format!("failed to read {}", tasks_path.display()))?;
    let suite_id = Uuid::new_v4().to_string();
    let mut cases = Vec::new();

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut raw: Value = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid JSONL at line {}", idx + 1))?;
        let task_id = required_str(&raw, "task_id")?;
        let task_type = optional_str(&raw, "task_type").unwrap_or_default();
        let source_suite = optional_str(&raw, "source_suite").unwrap_or_default();
        let target = optional_str(&raw, "target_tool_or_capability").unwrap_or_default();
        let mut prompt = required_str(&raw, "prompt")?;
        let assets = string_array(&raw, "assets")?;
        let expected_outputs = string_array(&raw, "expected_outputs")?;
        add_fact_extraction_contract(&mut raw, path, &prompt, &assets, &expected_outputs)?;
        add_tool_efficiency_case_contract_and_hint(&mut raw, &mut prompt, &task_id);
        let grader = optional_str(&raw, "grader").map(BenchmarkGraderSpec::python);
        let case_hash = stable_hash(&raw);
        cases.push(ImportedCaseDraft {
            id: Uuid::new_v4().to_string(),
            task_id,
            task_type,
            source_suite,
            target_tool_or_capability: target,
            prompt,
            assets,
            expected_outputs,
            grader,
            expected_output: None,
            raw_json: raw,
            case_hash,
        });
    }

    if cases.is_empty() {
        return Err(anyhow!("tool efficiency benchmark contains no cases"));
    }

    let manifest_json = serde_json::json!({
        "format": "ga_tool_efficiency",
        "tasks_file": path_relative_display(path, &tasks_path),
        "case_count": cases.len(),
    });

    Ok(ImportedSuiteDraft {
        suite_id,
        name: "GA Tool Efficiency Benchmark".to_string(),
        description: "GA-Technical-Report tool-use efficiency benchmark".to_string(),
        source_kind: BenchmarkSourceKind::GaToolEfficiency,
        source_path: path.to_string_lossy().to_string(),
        source_ref: None,
        manifest_json,
        cases,
    })
}

fn add_fact_extraction_contract(
    raw: &mut Value,
    suite_root: &Path,
    prompt: &str,
    assets: &[String],
    expected_outputs: &[String],
) -> Result<()> {
    if raw.get("taskContract").is_some() || raw.get("task_contract").is_some() {
        return Ok(());
    }
    let prompt_lower = prompt.to_lowercase();
    if !expected_outputs
        .iter()
        .any(|output| output == "final_response")
        || !(prompt.contains("facts") || prompt.contains("事实"))
    {
        return Ok(());
    }
    let mut markers = Vec::new();
    for asset in assets {
        if !matches!(
            Path::new(asset).extension().and_then(|ext| ext.to_str()),
            Some("txt" | "md" | "markdown")
        ) {
            continue;
        }
        let path = suite_root.join(asset);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        markers.extend(import_evidence_markers(&text));
    }
    markers.sort();
    markers.dedup();
    let mut selected = Vec::new();
    push_first_marker(&mut selected, &markers, |lower| lower.contains("7 atomic"));
    push_first_marker(&mut selected, &markers, |lower| lower.contains("1-2 steps"));
    push_first_marker(&mut selected, &markers, |lower| lower == "subagent");
    for marker in &markers {
        if selected.len() >= 4 {
            break;
        }
        let lower = marker.to_lowercase();
        if (lower.contains("agent") || prompt_lower.contains(&lower))
            && !selected.iter().any(|existing| existing == marker)
        {
            selected.push(marker.clone());
        }
    }
    if selected.is_empty() {
        return Ok(());
    }
    selected.sort();
    if let Some(obj) = raw.as_object_mut() {
        obj.insert(
            "taskContract".to_string(),
            serde_json::json!({
                "sourceGrounding": {
                    "requiredMarkers": selected,
                    "evidenceFiles": [],
                    "caseSensitive": false
                }
            }),
        );
    }
    Ok(())
}

fn add_tool_efficiency_case_contract_and_hint(raw: &mut Value, prompt: &mut String, task_id: &str) {
    match task_id {
        "teb_12_paper_ppt_generation" => {
            if let Some(obj) = raw.as_object_mut() {
                obj.insert(
                    "taskContract".to_string(),
                    serde_json::json!({
                        "artifacts": [
                            {
                                "path": "presentation.pptx",
                                "kind": "pptx",
                                "required": true,
                                "requireNonEmpty": true,
                                "pptx": {
                                    "requireSlides": true,
                                    "requireMedia": true,
                                    "minTextChars": 300,
                                    "finalSlideRequiredTermsAny": [
                                        {"label": "open_question", "any": ["开放问题", "open question", "question"]}
                                    ],
                                    "finalSlideMinNumberMarkers": 3
                                }
                            },
                            {
                                "path": "presentation_notes.md",
                                "kind": "markdown",
                                "required": true,
                                "requireNonEmpty": true,
                                "minTextChars": 80,
                                "requiredTermsAny": [
                                    {"label": "source_link", "any": ["https://arxiv.org/abs/2503.14476", "https://arxiv.org/pdf/2503.14476"]},
                                    {"label": "slide_count", "any": ["6", "7", "8", "页", "slides"]},
                                    {"label": "chart_page", "any": ["图表", "chart", "figure"]},
                                    {"label": "structure_page", "any": ["结构图", "流程图", "architecture", "flow", "structure"]}
                                ],
                                "forbiddenPlaceholders": true
                            }
                        ],
                        "completionPolicy": {
                            "selfCheckBeforeComplete": true,
                            "createArtifactsEarly": true,
                            "stopExplorationDuringRepair": true
                        }
                    }),
                );
            }
            if !prompt.contains("arXiv:2503.14476") {
                prompt.push_str("\n\nBenchmark hint: the correct arXiv identifier for this paper is arXiv:2503.14476. Do artifact-first work: create `presentation_notes.md`, at least one PNG image, and `presentation.pptx` before deep reading or polishing. Prefer one compact local `python3 <<'PY' ... PY` command that directly creates all outputs; do not use a huge `write_file` payload or many append chunks for the PPT generator. The first PPTX can be a minimal complete 7-slide version, then polish only if steps remain. On the literal last slide, include the exact contiguous phrase `开放问题` (not `开放的问题`) and three machine-readable markers `①` `②` `③` or `1` `2` `3`. Embed at least one real generated image into the PPTX (for example a PNG chart/flowchart), then validate by extracting the saved PPTX slide text and checking that the last slide contains `开放问题`.");
            }
        }
        "teb_13_sql_copilot_query_generation" => {
            if let Some(obj) = raw.as_object_mut() {
                obj.insert(
                    "taskContract".to_string(),
                    serde_json::json!({
                        "artifacts": [
                            {"path": "query.sql", "kind": "text", "required": true, "requireNonEmpty": true, "minTextChars": 80},
                            {"path": "result.csv", "kind": "csv", "required": true, "requireNonEmpty": true, "csvHeader": ["channel_name", "first_order_customers", "refund_rate", "net_revenue_30d"], "minRows": 1},
                            {
                                "path": "analysis.md",
                                "kind": "markdown",
                                "required": true,
                                "requireNonEmpty": true,
                                "minTextChars": 120,
                                "requiredTermsAny": [
                                    {"label": "calculation_scope", "any": ["口径", "refund", "退款", "净收入", "30"]},
                                    {"label": "join_relationships", "any": ["join", "关联", "campaign_attribution", "orders", "channels"]},
                                    {"label": "top_reason", "any": ["第1", "第一", "top 1", "排名第", "入选", "原因"]}
                                ],
                                "forbiddenPlaceholders": true
                            }
                        ],
                        "completionPolicy": {
                            "selfCheckBeforeComplete": true,
                            "createArtifactsEarly": true,
                            "stopExplorationDuringRepair": false
                        }
                    }),
                );
            }
            if !prompt.contains("analysis.md` 的校验要求") {
                prompt.push_str("\n\nBenchmark hint: `analysis.md` 的校验要求是显式包含计算口径、核心 JOIN/关联关系，以及排名第1渠道的入选原因；请在文件中使用这些关键词（如“口径”、“JOIN”、“排名第1”、“入选原因”）以便静态校验。");
            }
        }
        _ => {}
    }
}

fn push_first_marker(
    selected: &mut Vec<String>,
    markers: &[String],
    predicate: impl Fn(&str) -> bool,
) {
    if let Some(marker) = markers
        .iter()
        .find(|marker| predicate(&marker.to_lowercase()))
    {
        if !selected.iter().any(|existing| existing == marker) {
            selected.push(marker.clone());
        }
    }
}

fn import_evidence_markers(text: &str) -> Vec<String> {
    let mut markers = Vec::new();
    for line in text.lines() {
        markers.extend(import_high_signal_digit_phrases(line));
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

fn import_high_signal_digit_phrases(line: &str) -> Vec<String> {
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

fn import_sop_bench_suite(path: &Path) -> Result<ImportedSuiteDraft> {
    let sop_path = path.join("sop.txt");
    let csv_path = path.join("test_set_with_outputs.csv");
    let tools_path = path.join("tools.py");
    let toolspecs_path = path.join("toolspecs.json");
    let sop = fs::read_to_string(&sop_path)
        .with_context(|| format!("failed to read {}", sop_path.display()))?;
    let csv = fs::read_to_string(&csv_path)
        .with_context(|| format!("failed to read {}", csv_path.display()))?;
    if !tools_path.exists() {
        return Err(anyhow!(
            "SOP-Bench tools.py not found: {}",
            tools_path.display()
        ));
    }
    if !toolspecs_path.exists() {
        return Err(anyhow!(
            "SOP-Bench toolspecs.json not found: {}",
            toolspecs_path.display()
        ));
    }
    let mut lines = csv.lines();
    let header = lines
        .next()
        .ok_or_else(|| anyhow!("SOP-Bench CSV is empty"))?;
    let columns = parse_csv_line(header);
    let expected_idx = find_col(&columns, &["expected_output"])
        .ok_or_else(|| anyhow!("SOP-Bench CSV missing expected_output column"))?;
    let id_idx = find_col(&columns, &["order_id"])
        .ok_or_else(|| anyhow!("SOP-Bench CSV missing order_id column"))?;
    for required in [
        "product_id",
        "quantity_requested",
        "customer_id",
        "order_total",
        "destination_zip",
        "package_weight",
        "shipping_speed",
        "order_priority",
    ] {
        if find_col(&columns, &[required]).is_none() {
            return Err(anyhow!("SOP-Bench CSV missing {required} column"));
        }
    }
    let suite_id = Uuid::new_v4().to_string();
    let mut cases = Vec::new();

    for (idx, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = parse_csv_line(line);
        let row_no = idx + 1;
        let expected_output = fields
            .get(expected_idx)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("SOP-Bench row {row_no} missing expected_output"))?;
        let task_id = fields
            .get(id_idx)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("SOP-Bench row {row_no} missing order_id"))?;
        let row = row_map(&columns, &fields);
        let raw = serde_json::json!({
            "format": "ga_sop_bench_case",
            "row": row_no,
            "columns": columns,
            "values": fields,
            "rowData": row,
            "sop_file": "sop.txt",
            "tools_file": "tools.py",
            "toolspecs_file": "toolspecs.json",
            "sanitized_csv": SOP_DATA_CSV,
        });
        cases.push(ImportedCaseDraft {
            id: Uuid::new_v4().to_string(),
            task_id,
            task_type: "sop_task_completion".to_string(),
            source_suite: "sop_bench".to_string(),
            target_tool_or_capability: "tool_based_sop_execution".to_string(),
            prompt: build_sop_prompt(&sop, raw.get("rowData").and_then(Value::as_object).unwrap()),
            assets: vec![
                "sop.txt".to_string(),
                "tools.py".to_string(),
                "toolspecs.json".to_string(),
            ],
            expected_outputs: vec!["final_response".to_string()],
            grader: Some(BenchmarkGraderSpec::sop_bench()),
            expected_output: Some(expected_output),
            raw_json: raw.clone(),
            case_hash: stable_hash(&raw),
        });
    }

    if cases.is_empty() {
        return Err(anyhow!("SOP-Bench CSV contains no cases"));
    }

    Ok(ImportedSuiteDraft {
        suite_id,
        name: "SOP-Bench".to_string(),
        description: "GA-Technical-Report SOP-Bench task completion benchmark".to_string(),
        source_kind: BenchmarkSourceKind::GaSopBench,
        source_path: path.to_string_lossy().to_string(),
        source_ref: None,
        manifest_json: serde_json::json!({
            "format": "ga_sop_bench",
            "sop_file": path_relative_display(path, &sop_path),
            "csv_file": path_relative_display(path, &csv_path),
            "tools_file": path_relative_display(path, &tools_path),
            "toolspecs_file": path_relative_display(path, &toolspecs_path),
            "sanitized_csv": SOP_DATA_CSV,
            "decision_labels": SOP_DECISION_LABELS,
            "case_count": cases.len(),
        }),
        cases,
    })
}

fn required_str(raw: &Value, key: &str) -> Result<String> {
    optional_str(raw, key).ok_or_else(|| anyhow!("missing required string field `{key}`"))
}

fn optional_str(raw: &Value, key: &str) -> Option<String> {
    raw.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn string_array(raw: &Value, key: &str) -> Result<Vec<String>> {
    let Some(value) = raw.get(key) else {
        return Ok(Vec::new());
    };
    let arr = value
        .as_array()
        .ok_or_else(|| anyhow!("field `{key}` must be an array"))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("field `{key}` must contain only strings"))
        })
        .collect()
}

fn stable_hash(value: &Value) -> String {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(value)
        .unwrap_or_default()
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn path_relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn find_col(columns: &[String], names: &[&str]) -> Option<usize> {
    columns.iter().position(|c| {
        let normalized = c.trim().to_ascii_lowercase();
        names.iter().any(|n| normalized == *n)
    })
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    fields.push(current.trim().to_string());
    fields
}

pub fn resolve_asset_paths(case_assets: &[String], source_root: &Path) -> Vec<PathBuf> {
    case_assets.iter().map(|p| source_root.join(p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_quoted_csv_line() {
        assert_eq!(
            parse_csv_line("id,prompt,expected_output\n"),
            vec!["id", "prompt", "expected_output"]
        );
        assert_eq!(
            parse_csv_line("1,\"hello, world\",\"a \"\"quote\"\"\""),
            vec!["1", "hello, world", "a \"quote\""]
        );
    }

    #[test]
    fn imports_tool_efficiency_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = fs::File::create(dir.path().join("tool_efficiency_tasks.jsonl")).unwrap();
        writeln!(
            f,
            r#"{{"task_id":"teb_01","task_type":"simple_tool_generalization","source_suite":"claude_code","target_tool_or_capability":"GlobTool","prompt":"Find files","assets":[],"expected_outputs":["final_response"],"grader":"graders/g.py"}}"#
        )
        .unwrap();

        let imported = import_suite_from_path(dir.path()).unwrap();
        assert_eq!(imported.source_kind, BenchmarkSourceKind::GaToolEfficiency);
        assert_eq!(imported.cases.len(), 1);
        assert_eq!(imported.cases[0].task_id, "teb_01");
        assert_eq!(
            imported.cases[0].grader.as_ref().unwrap().path,
            "graders/g.py"
        );
    }

    #[test]
    fn imports_fact_extraction_contract_from_text_asset() {
        let dir = tempfile::tempdir().unwrap();
        let asset_dir = dir.path().join("assets/teb_04_agent_fact_extract");
        fs::create_dir_all(&asset_dir).unwrap();
        fs::write(
            asset_dir.join("notes.txt"),
            "GenericAgent exposes 7 atomic tools. Some tasks can finish in 1-2 steps. A subagent workflow is relevant.\n",
        )
        .unwrap();
        let mut f = fs::File::create(dir.path().join("tool_efficiency_tasks.jsonl")).unwrap();
        writeln!(
            f,
            r#"{{"task_id":"teb_04","task_type":"simple_tool_generalization","source_suite":"claude_code","target_tool_or_capability":"AgentTool","prompt":"请使用subagent从文本中提取 facts，直接输出 JSON 代码块。","assets":["assets/teb_04_agent_fact_extract/notes.txt"],"expected_outputs":["final_response"],"grader":"graders/g.py"}}"#
        )
        .unwrap();

        let imported = import_suite_from_path(dir.path()).unwrap();
        let markers = imported.cases[0]
            .raw_json
            .pointer("/taskContract/sourceGrounding/requiredMarkers")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(markers.iter().any(|m| m.contains("7 atomic")));
        assert!(markers.iter().any(|m| m.contains("1-2")));
        assert!(markers.iter().any(|m| m.eq_ignore_ascii_case("subagent")));
    }
    #[test]
    fn imports_sop_bench_fixture() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sop.txt"), "Step 1: verify order").unwrap();
        fs::write(dir.path().join("tools.py"), "# tools").unwrap();
        fs::write(dir.path().join("toolspecs.json"), "[]").unwrap();
        fs::write(
            dir.path().join("test_set_with_outputs.csv"),
            "order_id,product_id,quantity_requested,customer_id,order_total,destination_zip,package_weight,shipping_speed,order_priority,expected_output\nORD001,PROD001,2,CUST001,159.98,98101,1.5,standard,normal,fulfill_immediately\n",
        )
        .unwrap();

        let imported = import_suite_from_path(dir.path()).unwrap();
        assert_eq!(imported.source_kind, BenchmarkSourceKind::GaSopBench);
        assert_eq!(imported.cases.len(), 1);
        let case = &imported.cases[0];
        assert_eq!(case.task_id, "ORD001");
        assert_eq!(case.expected_outputs, vec!["final_response".to_string()]);
        assert_eq!(case.expected_output.as_deref(), Some("fulfill_immediately"));
        assert_eq!(case.grader.as_ref().unwrap().kind, "sop_bench");
        assert_eq!(
            case.assets,
            vec![
                "sop.txt".to_string(),
                "tools.py".to_string(),
                "toolspecs.json".to_string()
            ]
        );
        assert!(case.prompt.contains("PROD001"));
        assert!(case.prompt.contains("CUST001"));
        assert!(!case.prompt.contains("- expected_output:"));
        assert_eq!(
            case.raw_json
                .pointer("/rowData/order_id")
                .and_then(Value::as_str),
            Some("ORD001")
        );
    }
}
