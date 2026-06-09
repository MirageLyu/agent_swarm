use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use super::types::{BenchmarkGraderSpec, GraderArtifact};

#[derive(Debug, Clone)]
pub struct GraderExecutionOutput {
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout_json: Option<Value>,
    pub stderr: String,
    pub duration_ms: i64,
    pub task_success: Option<bool>,
}

pub async fn execute_python_grader(
    source_root: &Path,
    spec: &BenchmarkGraderSpec,
    workspace: &Path,
    response_file: &Path,
    timeout_seconds: u64,
) -> Result<GraderExecutionOutput> {
    if spec.kind != "python" {
        return Err(anyhow!("unsupported grader kind: {}", spec.kind));
    }
    let grader_path = source_root.join(&spec.path);
    if !grader_path.exists() {
        return Err(anyhow!("grader not found: {}", grader_path.display()));
    }

    let response_file_arg = grader_response_file_arg(&grader_path)?;
    let workspace_arg = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let response_file_arg_path = response_file
        .canonicalize()
        .unwrap_or_else(|_| response_file.to_path_buf());
    let mut command_args = vec![
        "python3".to_string(),
        grader_path.to_string_lossy().to_string(),
        "--workspace".to_string(),
        workspace_arg.to_string_lossy().to_string(),
    ];
    if let Some(flag) = response_file_arg {
        command_args.push(flag.to_string());
        command_args.push(response_file_arg_path.to_string_lossy().to_string());
    }

    let started = Instant::now();
    let mut cmd = Command::new("python3");
    cmd.arg(&grader_path).arg("--workspace").arg(&workspace_arg);
    if let Some(flag) = response_file_arg {
        cmd.arg(flag).arg(&response_file_arg_path);
    }
    cmd.current_dir(&workspace_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = timeout(Duration::from_secs(timeout_seconds.max(1)), cmd.output())
        .await
        .map_err(|_| anyhow!("grader timed out after {timeout_seconds}s"))?
        .context("failed to execute grader")?;
    let duration_ms = started.elapsed().as_millis() as i64;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let stdout_json = if stdout.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&stdout).context("grader stdout is not valid JSON")?)
    };
    let task_success = stdout_json
        .as_ref()
        .and_then(|v: &Value| v.get("task_success"))
        .and_then(Value::as_bool);

    Ok(GraderExecutionOutput {
        command: command_args,
        exit_code: output.status.code(),
        stdout_json,
        stderr,
        duration_ms,
        task_success,
    })
}

fn grader_response_file_arg(grader_path: &Path) -> Result<Option<&'static str>> {
    let source = fs::read_to_string(grader_path)
        .with_context(|| format!("failed to inspect grader {}", grader_path.display()))?;
    if source.contains("--response-file") {
        Ok(Some("--response-file"))
    } else if source.contains("--response_file") {
        Ok(Some("--response_file"))
    } else {
        Ok(None)
    }
}

pub fn artifact_from_output(
    id: String,
    result_id: String,
    output: GraderExecutionOutput,
    created_at: String,
) -> GraderArtifact {
    GraderArtifact {
        id,
        result_id,
        grader_kind: "python".to_string(),
        command: output.command,
        exit_code: output.exit_code,
        stdout_json: output.stdout_json,
        stderr: output.stderr,
        duration_ms: output.duration_ms,
        created_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn omits_response_file_for_workspace_only_grader() {
        let dir = tempfile::tempdir().unwrap();
        let grader = dir.path().join("grader.py");
        fs::write(
            &grader,
            r#"import argparse, json
p = argparse.ArgumentParser()
p.add_argument('--workspace', required=True)
args = p.parse_args()
print(json.dumps({'task_success': True}))
"#,
        )
        .unwrap();
        let response = dir.path().join("final_response.txt");
        fs::write(&response, "done").unwrap();
        let output = execute_python_grader(
            dir.path(),
            &BenchmarkGraderSpec::python("grader.py"),
            dir.path(),
            &response,
            5,
        )
        .await
        .unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert!(!output.command.iter().any(|arg| arg == "--response-file"));
        assert_eq!(output.task_success, Some(true));
    }

    #[tokio::test]
    async fn passes_response_file_when_grader_declares_it() {
        let dir = tempfile::tempdir().unwrap();
        let grader = dir.path().join("grader.py");
        fs::write(
            &grader,
            r#"import argparse, json, pathlib
p = argparse.ArgumentParser()
p.add_argument('--workspace', required=True)
p.add_argument('--response-file', required=True)
args = p.parse_args()
print(json.dumps({'task_success': pathlib.Path(args.response_file).read_text() == 'done', 'response_is_absolute': pathlib.Path(args.response_file).is_absolute()}))
"#,
        )
        .unwrap();
        let response = dir.path().join("final_response.txt");
        fs::write(&response, "done").unwrap();
        let output = execute_python_grader(
            dir.path(),
            &BenchmarkGraderSpec::python("grader.py"),
            dir.path(),
            &response,
            5,
        )
        .await
        .unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert!(output.command.iter().any(|arg| arg == "--response-file"));
        assert_eq!(output.task_success, Some(true));
        assert_eq!(
            output
                .stdout_json
                .as_ref()
                .and_then(|value| value.get("response_is_absolute"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }
}
