use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const PROFILE_MAX_CHARS: usize = 2400;
const PATH_ENTRY_LIMIT: usize = 8;

const COMMON_COMMANDS: &[&str] = &[
    "rg", "grep", "find", "python3", "python", "node", "npm", "cargo", "git", "curl", "tar",
    "unzip",
];

const PROXY_ENV_KEYS: &[&str] = &[
    "ALL_PROXY",
    "all_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
];

#[derive(Debug, Clone)]
pub struct RuntimeEnvironmentProfile {
    os: &'static str,
    arch: &'static str,
    workspace: String,
    path_entry_count: usize,
    path_samples: Vec<String>,
    commands: BTreeMap<&'static str, bool>,
    proxy_env_present: Vec<&'static str>,
}

impl RuntimeEnvironmentProfile {
    pub fn collect(workspace: &Path) -> Self {
        let path_entries = std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default();
        let path_samples = path_entries
            .iter()
            .take(PATH_ENTRY_LIMIT)
            .map(|p| sanitize_path_sample(p))
            .collect::<Vec<_>>();
        let commands = COMMON_COMMANDS
            .iter()
            .map(|cmd| (*cmd, command_exists(cmd, &path_entries)))
            .collect::<BTreeMap<_, _>>();
        let proxy_env_present = PROXY_ENV_KEYS
            .iter()
            .copied()
            .filter(|key| std::env::var_os(key).is_some())
            .collect::<Vec<_>>();

        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            workspace: workspace.display().to_string(),
            path_entry_count: path_entries.len(),
            path_samples,
            commands,
            proxy_env_present,
        }
    }

    pub fn render_system_block(&self) -> String {
        let commands = self
            .commands
            .iter()
            .map(|(cmd, present)| format!("{cmd}={}", if *present { "yes" } else { "no" }))
            .collect::<Vec<_>>()
            .join(", ");
        let proxy = if self.proxy_env_present.is_empty() {
            "none".to_string()
        } else {
            self.proxy_env_present.join(", ")
        };
        let path_samples = if self.path_samples.is_empty() {
            "none".to_string()
        } else {
            let suffix = if self.path_entry_count > self.path_samples.len() {
                format!(
                    " ... (+{} more)",
                    self.path_entry_count - self.path_samples.len()
                )
            } else {
                String::new()
            };
            format!("{}{}", self.path_samples.join(", "), suffix)
        };

        let shell_note = if self.os == "windows" {
            "shell_exec runs commands in the workspace using the Windows system shell; it inherits the agent process environment and is not an interactive login shell"
        } else {
            "shell_exec runs commands in the workspace using the system shell (`sh -c` on Unix-like platforms); it inherits the agent process environment and is not an interactive login shell"
        };
        let block = format!(
            "\n\n## Runtime Environment\n\
             - Platform: {} / {}.\n\
             - Workspace: {}.\n\
             - {}.\n\
             - PATH entries: {} (sample: {}).\n\
             - Detected commands from PATH scan: {}.\n\
             - Proxy environment variables present: {}. Values are intentionally redacted.\n\
             - Adapt shell commands to these observed capabilities. If a tool result reports capability feedback, use the exit code/stderr facts to choose a portable alternative instead of repeating the same failing command.",
            self.os,
            self.arch,
            self.workspace,
            shell_note,
            self.path_entry_count,
            path_samples,
            commands,
            proxy,
        );
        truncate_chars(&block, PROFILE_MAX_CHARS)
    }
}

pub fn build_profile(workspace: &Path) -> RuntimeEnvironmentProfile {
    RuntimeEnvironmentProfile::collect(workspace)
}

fn command_exists(command: &str, path_entries: &[PathBuf]) -> bool {
    path_entries.iter().any(|dir| {
        command_candidates(command).into_iter().any(|name| {
            let candidate = dir.join(name);
            executable_file(&candidate)
        })
    })
}

fn command_candidates(command: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        let pathext =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        let mut names = vec![command.to_string()];
        for ext in pathext.split(';').filter(|ext| !ext.trim().is_empty()) {
            names.push(format!("{}{}", command, ext.to_ascii_lowercase()));
            names.push(format!("{}{}", command, ext.to_ascii_uppercase()));
        }
        names
    }
    #[cfg(not(windows))]
    {
        vec![command.to_string()]
    }
}

fn executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn sanitize_path_sample(path: &Path) -> String {
    let s = path.display().to_string();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home).display().to_string();
        if !home.is_empty() && s.starts_with(&home) {
            return format!("~{}", &s[home.len()..]);
        }
    }
    s
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = s
        .chars()
        .take(max_chars.saturating_sub(32))
        .collect::<String>();
    out.push_str("\n[Runtime Environment truncated]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn runtime_profile_redacts_proxy_values_and_renders_capabilities() {
        let _guard = env_lock().lock().unwrap();
        let previous = std::env::var_os("ALL_PROXY");
        std::env::set_var("ALL_PROXY", "http://secret-proxy.example");
        let profile = RuntimeEnvironmentProfile::collect(Path::new("/tmp/workspace"));
        let block = profile.render_system_block();
        assert!(block.contains("Runtime Environment"));
        assert!(block.contains("sh -c"));
        assert!(block.contains("ALL_PROXY"));
        assert!(!block.contains("secret-proxy"));
        assert!(block.chars().count() <= PROFILE_MAX_CHARS);
        if let Some(previous) = previous {
            std::env::set_var("ALL_PROXY", previous);
        } else {
            std::env::remove_var("ALL_PROXY");
        }
    }
}
