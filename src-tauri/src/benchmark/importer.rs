use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use uuid::Uuid;

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
        let raw: Value = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid JSONL at line {}", idx + 1))?;
        let task_id = required_str(&raw, "task_id")?;
        let task_type = optional_str(&raw, "task_type").unwrap_or_default();
        let source_suite = optional_str(&raw, "source_suite").unwrap_or_default();
        let target = optional_str(&raw, "target_tool_or_capability").unwrap_or_default();
        let prompt = required_str(&raw, "prompt")?;
        let assets = string_array(&raw, "assets")?;
        let expected_outputs = string_array(&raw, "expected_outputs")?;
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

fn import_sop_bench_suite(path: &Path) -> Result<ImportedSuiteDraft> {
    let sop_path = path.join("sop.txt");
    let csv_path = path.join("test_set_with_outputs.csv");
    let sop = fs::read_to_string(&sop_path)
        .with_context(|| format!("failed to read {}", sop_path.display()))?;
    let csv = fs::read_to_string(&csv_path)
        .with_context(|| format!("failed to read {}", csv_path.display()))?;
    let mut lines = csv.lines();
    let header = lines
        .next()
        .ok_or_else(|| anyhow!("SOP-Bench CSV is empty"))?;
    let columns = parse_csv_line(header);
    let prompt_idx = find_col(&columns, &["prompt", "input", "question", "task"]);
    let expected_idx = find_col(
        &columns,
        &["expected_output", "output", "label", "expected"],
    );
    let id_idx = find_col(&columns, &["task_id", "id", "case_id"]);
    let suite_id = Uuid::new_v4().to_string();
    let mut cases = Vec::new();

    for (idx, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = parse_csv_line(line);
        let row_no = idx + 1;
        let prompt_value = prompt_idx
            .and_then(|i| fields.get(i).cloned())
            .or_else(|| fields.first().cloned())
            .ok_or_else(|| anyhow!("SOP-Bench row {row_no} has no prompt field"))?;
        let expected_output = expected_idx.and_then(|i| fields.get(i).cloned());
        let task_id = id_idx
            .and_then(|i| fields.get(i).cloned())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("sop_{row_no:02}"));
        let raw = serde_json::json!({
            "row": row_no,
            "columns": columns,
            "values": fields,
            "sop": sop,
        });
        cases.push(ImportedCaseDraft {
            id: Uuid::new_v4().to_string(),
            task_id,
            task_type: "sop_task_completion".to_string(),
            source_suite: "sop_bench".to_string(),
            target_tool_or_capability: "task_completion_token_efficiency".to_string(),
            prompt: format!("Follow the SOP below and complete the task.\n\nSOP:\n{sop}\n\nTask:\n{prompt_value}"),
            assets: Vec::new(),
            expected_outputs: expected_output.clone().into_iter().collect(),
            grader: None,
            expected_output,
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
    fn imports_sop_bench_fixture() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sop.txt"), "Step 1: verify order").unwrap();
        fs::write(
            dir.path().join("test_set_with_outputs.csv"),
            "task_id,prompt,expected_output\nsop_a,Check order,approve\n",
        )
        .unwrap();

        let imported = import_suite_from_path(dir.path()).unwrap();
        assert_eq!(imported.source_kind, BenchmarkSourceKind::GaSopBench);
        assert_eq!(imported.cases.len(), 1);
        assert_eq!(imported.cases[0].task_id, "sop_a");
        assert_eq!(
            imported.cases[0].expected_output.as_deref(),
            Some("approve")
        );
    }
}
