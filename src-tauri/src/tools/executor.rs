use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    fn ok(content: String) -> Self {
        Self {
            content,
            is_error: false,
        }
    }

    fn error(kind: &str, message: &str) -> Self {
        Self {
            content: serde_json::json!({ "error": kind, "message": message }).to_string(),
            is_error: true,
        }
    }
}

pub struct ToolExecutor {
    workspace_root: PathBuf,
}

impl ToolExecutor {
    pub fn new(workspace_root: PathBuf) -> Self {
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or(workspace_root);
        Self { workspace_root }
    }

    pub fn workspace_display(&self) -> String {
        self.workspace_root.display().to_string()
    }

    pub async fn execute(&self, tool_name: &str, input: &serde_json::Value) -> ToolOutput {
        match tool_name {
            "read_file" => self.read_file(input).await,
            "write_file" => self.write_file(input).await,
            "search_files" => self.search_files(input).await,
            "shell_exec" => self.shell_exec(input).await,
            "list_files" => self.list_files(input).await,
            _ => ToolOutput::error("unknown_tool", &format!("Unknown tool: {tool_name}")),
        }
    }

    async fn read_file(&self, input: &serde_json::Value) -> ToolOutput {
        let rel_path = match input["path"].as_str() {
            Some(p) => p,
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'path' parameter. Received: {input}"),
            ),
        };
        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };
        match tokio::fs::read_to_string(&full_path).await {
            Ok(content) => ToolOutput::ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ToolOutput::error("file_not_found", &format!("File not found: {rel_path}"))
            }
            Err(e) => ToolOutput::error("io_error", &e.to_string()),
        }
    }

    async fn write_file(&self, input: &serde_json::Value) -> ToolOutput {
        let rel_path = match input["path"].as_str() {
            Some(p) => p,
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'path' parameter. Received: {input}"),
            ),
        };
        let content = match input["content"].as_str() {
            Some(c) => c,
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'content' parameter. Received keys: {:?}", input.as_object().map(|o| o.keys().collect::<Vec<_>>())),
            ),
        };
        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };

        if let Some(parent) = full_path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::error("io_error", &e.to_string());
            }
        }
        match tokio::fs::write(&full_path, content).await {
            Ok(()) => ToolOutput::ok(format!("Written {} bytes to {rel_path}", content.len())),
            Err(e) => ToolOutput::error("io_error", &e.to_string()),
        }
    }

    async fn search_files(&self, input: &serde_json::Value) -> ToolOutput {
        let pattern = match input["pattern"].as_str() {
            Some(p) => p,
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'pattern' parameter. Received: {input}"),
            ),
        };
        let search_path = match input["path"].as_str() {
            Some(p) => match self.resolve_path(p) {
                Ok(path) => path,
                Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
            },
            None => self.workspace_root.clone(),
        };

        match Command::new("rg")
            .args(["--max-count", "50", "--line-number", pattern])
            .current_dir(&search_path)
            .output()
            .await
        {
            Ok(output) => ToolOutput::ok(String::from_utf8_lossy(&output.stdout).to_string()),
            Err(e) => ToolOutput::error("io_error", &e.to_string()),
        }
    }

    async fn shell_exec(&self, input: &serde_json::Value) -> ToolOutput {
        let command = match input["command"].as_str() {
            Some(c) => c,
            None => return ToolOutput::error(
                "parameter_error",
                &format!("Missing 'command' parameter. Received: {input}"),
            ),
        };

        let output = match Command::new("sh")
            .args(["-c", command])
            .current_dir(&self.workspace_root)
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => return ToolOutput::error("shell_error", &e.to_string()),
        };

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() {
            let mut result = stdout.to_string();
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str("[stderr] ");
                result.push_str(&stderr);
            }
            ToolOutput::ok(result)
        } else {
            let msg = format!(
                "Command failed (exit code {exit_code})\n[stdout] {stdout}\n[stderr] {stderr}"
            );
            ToolOutput::error("shell_error", &msg)
        }
    }

    async fn list_files(&self, input: &serde_json::Value) -> ToolOutput {
        let rel_path = input["path"].as_str().unwrap_or(".");
        let full_path = match self.resolve_path(rel_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error("sandbox_violation", &e.to_string()),
        };

        let mut dir = match tokio::fs::read_dir(&full_path).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutput::error(
                    "file_not_found",
                    &format!("Directory not found: {rel_path}"),
                );
            }
            Err(e) => return ToolOutput::error("io_error", &e.to_string()),
        };

        let mut entries = Vec::new();
        loop {
            match dir.next_entry().await {
                Ok(Some(entry)) => {
                    let file_type = match entry.file_type().await {
                        Ok(ft) => ft,
                        Err(_) => continue,
                    };
                    let name = entry.file_name().to_string_lossy().to_string();
                    let suffix = if file_type.is_dir() { "/" } else { "" };
                    entries.push(format!("{name}{suffix}"));
                }
                Ok(None) => break,
                Err(e) => return ToolOutput::error("io_error", &e.to_string()),
            }
        }
        entries.sort();
        ToolOutput::ok(entries.join("\n"))
    }

    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf> {
        let full = self.workspace_root.join(rel_path);
        let canonical = full
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(&full));

        let workspace_canonical = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| Self::normalize_lexical(&self.workspace_root));

        if !canonical.starts_with(&workspace_canonical) {
            bail!(
                "Path escapes workspace: {} is outside {}",
                canonical.display(),
                workspace_canonical.display()
            );
        }
        Ok(full)
    }

    /// Resolve `.` and `..` components lexically (without touching the filesystem).
    fn normalize_lexical(path: &std::path::Path) -> PathBuf {
        use std::path::Component;
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    if !parts.is_empty() {
                        parts.pop();
                    }
                }
                Component::CurDir => {}
                c => parts.push(c),
            }
        }
        parts.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ToolExecutor) {
        let dir = TempDir::new().unwrap();
        let exec = ToolExecutor::new(dir.path().to_path_buf());
        (dir, exec)
    }

    #[tokio::test]
    async fn shell_success_returns_stdout() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "shell_exec",
                &serde_json::json!({"command": "echo hello"}),
            )
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("hello"));
    }

    #[tokio::test]
    async fn shell_failure_returns_structured_error() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "shell_exec",
                &serde_json::json!({"command": "exit 1"}),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "shell_error");
    }

    #[tokio::test]
    async fn shell_command_not_found() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "shell_exec",
                &serde_json::json!({"command": "totally_nonexistent_cmd_xyz"}),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "shell_error");
        assert!(v["message"].as_str().unwrap().contains("127"));
    }

    #[tokio::test]
    async fn read_file_not_found() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "read_file",
                &serde_json::json!({"path": "nonexistent.txt"}),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "file_not_found");
    }

    #[tokio::test]
    async fn sandbox_violation() {
        let (_dir, exec) = setup();
        let out = exec
            .execute(
                "read_file",
                &serde_json::json!({"path": "../../etc/passwd"}),
            )
            .await;
        assert!(out.is_error);
        let v: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v["error"], "sandbox_violation");
    }

    #[tokio::test]
    async fn write_file_success() {
        let (dir, exec) = setup();
        let out = exec
            .execute(
                "write_file",
                &serde_json::json!({"path": "test.txt", "content": "hello world"}),
            )
            .await;
        assert!(!out.is_error);
        let content = fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert_eq!(content, "hello world");
    }
}
