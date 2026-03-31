use anyhow::{bail, Result};
use std::path::PathBuf;
use tokio::process::Command;

pub struct ToolExecutor {
    workspace_root: PathBuf,
}

impl ToolExecutor {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    pub async fn execute(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Result<String> {
        match tool_name {
            "read_file" => self.read_file(input).await,
            "write_file" => self.write_file(input).await,
            "search_files" => self.search_files(input).await,
            "shell_exec" => self.shell_exec(input).await,
            "list_files" => self.list_files(input).await,
            _ => bail!("Unknown tool: {tool_name}"),
        }
    }

    async fn read_file(&self, input: &serde_json::Value) -> Result<String> {
        let rel_path = input["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;
        let full_path = self.resolve_path(rel_path)?;
        let content = tokio::fs::read_to_string(&full_path).await?;
        Ok(content)
    }

    async fn write_file(&self, input: &serde_json::Value) -> Result<String> {
        let rel_path = input["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;
        let content = input["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;
        let full_path = self.resolve_path(rel_path)?;

        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&full_path, content).await?;
        Ok(format!("Written {} bytes to {rel_path}", content.len()))
    }

    async fn search_files(&self, input: &serde_json::Value) -> Result<String> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' parameter"))?;
        let search_path = input["path"]
            .as_str()
            .map(|p| self.resolve_path(p))
            .transpose()?
            .unwrap_or_else(|| self.workspace_root.clone());

        let output = Command::new("rg")
            .args(["--max-count", "50", "--line-number", pattern])
            .current_dir(&search_path)
            .output()
            .await?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    async fn shell_exec(&self, input: &serde_json::Value) -> Result<String> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;

        let output = Command::new("sh")
            .args(["-c", command])
            .current_dir(&self.workspace_root)
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("[stderr] ");
            result.push_str(&stderr);
        }
        if !output.status.success() {
            result.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
        }
        Ok(result)
    }

    async fn list_files(&self, input: &serde_json::Value) -> Result<String> {
        let rel_path = input["path"].as_str().unwrap_or(".");
        let full_path = self.resolve_path(rel_path)?;

        let mut entries = Vec::new();
        let mut dir = tokio::fs::read_dir(&full_path).await?;
        while let Some(entry) = dir.next_entry().await? {
            let file_type = entry.file_type().await?;
            let name = entry.file_name().to_string_lossy().to_string();
            let suffix = if file_type.is_dir() { "/" } else { "" };
            entries.push(format!("{name}{suffix}"));
        }
        entries.sort();
        Ok(entries.join("\n"))
    }

    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf> {
        let full = self.workspace_root.join(rel_path);
        let canonical = full
            .canonicalize()
            .unwrap_or_else(|_| full.clone());

        if !canonical.starts_with(&self.workspace_root) {
            bail!(
                "Path escapes workspace: {} is outside {}",
                canonical.display(),
                self.workspace_root.display()
            );
        }
        Ok(full)
    }
}
