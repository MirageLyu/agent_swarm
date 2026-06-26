# Bundled ripgrep Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a pinned official `rg` binary with Miragenty and restore `grep` / `search_files` to real ripgrep CLI semantics instead of a partial Rust reimplementation.

**Architecture:** Add a small vendor-management layer for ripgrep: a manifest plus fetch script stages official release binaries under `src-tauri/vendor/rg/bin/<target>/`, Tauri bundles those staged files as resources, and Rust resolves the bundled/staged/PATH command path before spawning `rg`. Remove `grep_engine.rs` and its crate dependencies so there is only one grep implementation: the real `rg` executable.

**Tech Stack:** Tauri v2 resources, Rust `tokio::process::Command`, Node.js build script using built-in `https`, `crypto`, `fs`, and platform archive tools (`tar`), official ripgrep 14.1.1 release assets.

---

## File Structure

- Create: `scripts/fetch-rg.mjs`
  - Detects host target, downloads the pinned official ripgrep archive, verifies SHA-256, extracts only the `rg` executable, and stages it under `src-tauri/vendor/rg/bin/<target>/`.
- Create: `src-tauri/vendor/rg/manifest.json`
  - Pinned ripgrep version and per-target asset metadata. Initial entries: `aarch64-apple-darwin`, `x86_64-apple-darwin`.
- Create: `src-tauri/vendor/rg/README.md`
  - Explains why binaries are not committed and how to refresh the staged `rg` binary.
- Create: `src-tauri/src/tools/ripgrep.rs`
  - Owns target detection, candidate path resolution, executable checks, and structured dependency errors.
- Modify: `.gitignore`
  - Ignore staged vendor binaries and downloaded archives.
- Modify: `package.json`
  - Run `node scripts/fetch-rg.mjs` before `pnpm tauri build` in the bundle script.
- Modify: `src-tauri/tauri.conf.json`
  - Include `vendor/rg/bin/**/*` as bundled resources.
- Modify: `src-tauri/Cargo.toml`
  - Remove `grep`, `grep-regex`, `grep-searcher`, and `ignore` dependencies.
- Modify: `src-tauri/src/tools/mod.rs`
  - Replace `mod grep_engine;` with `mod ripgrep;`.
- Modify: `src-tauri/src/tools/executor.rs`
  - Restore the real `rg` subprocess implementation, using `ripgrep::resolve_rg_command()` instead of hard-coded `Command::new("rg")`.
  - Restore grep output truncation constants in this file.
  - Restore `capability_tool_error()`.
  - Remove `grep_engine` imports and test imports.
- Delete: `src-tauri/src/tools/grep_engine.rs`
  - The partial Rust grep engine is intentionally removed.

---

### Task 1: Add ripgrep vendor manifest and ignore rules

**Files:**
- Create: `src-tauri/vendor/rg/manifest.json`
- Create: `src-tauri/vendor/rg/README.md`
- Modify: `.gitignore`

- [ ] **Step 1: Write the manifest**

Create `src-tauri/vendor/rg/manifest.json` with this exact content:

```json
{
  "version": "14.1.1",
  "targets": {
    "aarch64-apple-darwin": {
      "archive": "ripgrep-14.1.1-aarch64-apple-darwin.tar.gz",
      "url": "https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-aarch64-apple-darwin.tar.gz",
      "sha256": "24ad76777745fbff131c8fbc466742b011f925bfa4fffa2ded6def23b5b937be",
      "archiveType": "tar.gz",
      "executablePath": "ripgrep-14.1.1-aarch64-apple-darwin/rg",
      "resourcePath": "vendor/rg/bin/aarch64-apple-darwin/rg"
    },
    "x86_64-apple-darwin": {
      "archive": "ripgrep-14.1.1-x86_64-apple-darwin.tar.gz",
      "url": "https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-x86_64-apple-darwin.tar.gz",
      "sha256": "fc87e78f7cb3fea12d69072e7ef3b21509754717b746368fd40d88963630e2b3",
      "archiveType": "tar.gz",
      "executablePath": "ripgrep-14.1.1-x86_64-apple-darwin/rg",
      "resourcePath": "vendor/rg/bin/x86_64-apple-darwin/rg"
    }
  }
}
```

- [ ] **Step 2: Add vendor README**

Create `src-tauri/vendor/rg/README.md` with this exact content:

```markdown
# Bundled ripgrep

Miragenty uses the official `rg` binary for `grep` / `search_files` so agent search behavior matches ripgrep CLI semantics.

The downloaded binaries are not committed to git. They are staged by:

```bash
node scripts/fetch-rg.mjs
```

The script reads `manifest.json`, downloads the pinned official ripgrep release for the current host target, verifies SHA-256, and writes the executable to `bin/<target>/rg`.

Tauri packages the staged `bin/**` files as app resources. Runtime lookup prefers the bundled or staged binary, then falls back to `rg` on PATH for development environments.
```

- [ ] **Step 3: Ignore generated vendor files**

Append these lines to `.gitignore`:

```gitignore

# Vendored tool binaries staged during build
src-tauri/vendor/rg/bin/
src-tauri/vendor/rg/downloads/
```

- [ ] **Step 4: Verify the manifest is valid JSON**

Run:

```bash
python3 -m json.tool src-tauri/vendor/rg/manifest.json >/tmp/miragenty-rg-manifest.json
```

Expected: exits with code 0 and no stderr.

- [ ] **Step 5: Commit**

```bash
git add .gitignore src-tauri/vendor/rg/manifest.json src-tauri/vendor/rg/README.md
git commit -m "chore: add ripgrep vendor manifest"
```

---

### Task 2: Add the ripgrep fetch script

**Files:**
- Create: `scripts/fetch-rg.mjs`
- Modify: `package.json`

- [ ] **Step 1: Create the fetch script**

Create `scripts/fetch-rg.mjs` with this exact content:

```javascript
import { createHash } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import https from 'node:https';
import { spawn } from 'node:child_process';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..');
const manifestPath = path.join(repoRoot, 'src-tauri', 'vendor', 'rg', 'manifest.json');

function hostTarget() {
  const archMap = {
    arm64: 'aarch64',
    x64: 'x86_64'
  };
  const platformMap = {
    darwin: 'apple-darwin',
    linux: 'unknown-linux-gnu',
    win32: 'pc-windows-msvc'
  };
  const arch = archMap[process.arch];
  const platform = platformMap[process.platform];
  if (!arch || !platform) {
    throw new Error(`Unsupported host for bundled ripgrep: ${process.platform}/${process.arch}`);
  }
  return `${arch}-${platform}`;
}

async function exists(filePath) {
  try {
    await stat(filePath);
    return true;
  } catch (error) {
    if (error && error.code === 'ENOENT') return false;
    throw error;
  }
}

function download(url, destination) {
  return new Promise((resolve, reject) => {
    const request = https.get(url, response => {
      if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
        response.resume();
        download(response.headers.location, destination).then(resolve, reject);
        return;
      }
      if (response.statusCode !== 200) {
        response.resume();
        reject(new Error(`Download failed with HTTP ${response.statusCode}: ${url}`));
        return;
      }
      const file = createWriteStream(destination);
      response.pipe(file);
      file.on('finish', () => file.close(resolve));
      file.on('error', reject);
    });
    request.on('error', reject);
  });
}

async function sha256(filePath) {
  const data = await readFile(filePath);
  return createHash('sha256').update(data).digest('hex');
}

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: 'inherit', ...options });
    child.on('error', reject);
    child.on('exit', code => {
      if (code === 0) resolve();
      else reject(new Error(`${command} ${args.join(' ')} exited with code ${code}`));
    });
  });
}

async function main() {
  const target = process.env.RG_TARGET || hostTarget();
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  const entry = manifest.targets[target];
  if (!entry) {
    throw new Error(`No ripgrep binary configured for target ${target}`);
  }

  const stagedPath = path.join(repoRoot, 'src-tauri', entry.resourcePath);
  if (await exists(stagedPath)) {
    console.log(`[fetch-rg] already staged: ${path.relative(repoRoot, stagedPath)}`);
    return;
  }

  const downloadsDir = path.join(repoRoot, 'src-tauri', 'vendor', 'rg', 'downloads');
  await mkdir(downloadsDir, { recursive: true });
  const archivePath = path.join(downloadsDir, entry.archive);

  if (!(await exists(archivePath))) {
    console.log(`[fetch-rg] downloading ${entry.url}`);
    await download(entry.url, archivePath);
  }

  const actualSha = await sha256(archivePath);
  if (actualSha !== entry.sha256) {
    await rm(archivePath, { force: true });
    throw new Error(`SHA-256 mismatch for ${entry.archive}: expected ${entry.sha256}, got ${actualSha}`);
  }

  const extractDir = await mkdtemp(path.join(tmpdir(), 'miragenty-rg-'));
  try {
    if (entry.archiveType !== 'tar.gz') {
      throw new Error(`Unsupported archive type: ${entry.archiveType}`);
    }
    await run('tar', ['-xzf', archivePath, '-C', extractDir]);
    const extractedBinary = path.join(extractDir, entry.executablePath);
    await mkdir(path.dirname(stagedPath), { recursive: true });
    await copyFile(extractedBinary, stagedPath);
    await chmod(stagedPath, 0o755);
    console.log(`[fetch-rg] staged ${path.relative(repoRoot, stagedPath)}`);
  } finally {
    await rm(extractDir, { recursive: true, force: true });
  }
}

main().catch(error => {
  console.error(`[fetch-rg] ${error.message}`);
  process.exit(1);
});
```

- [ ] **Step 2: Update the bundle script**

In `package.json`, change the `bundle` script from:

```json
"bundle": "pnpm tauri build && mkdir -p pkg && cp -f src-tauri/target/release/bundle/dmg/*.dmg pkg/ && ls -lh pkg/"
```

to:

```json
"bundle": "node scripts/fetch-rg.mjs && pnpm tauri build && mkdir -p pkg && cp -f src-tauri/target/release/bundle/dmg/*.dmg pkg/ && ls -lh pkg/"
```

- [ ] **Step 3: Run the fetch script**

Run:

```bash
node scripts/fetch-rg.mjs
```

Expected on Apple Silicon:

```text
[fetch-rg] downloading https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-aarch64-apple-darwin.tar.gz
[fetch-rg] staged src-tauri/vendor/rg/bin/aarch64-apple-darwin/rg
```

If the archive was already downloaded or binary already staged, the first line may differ; the command must exit 0.

- [ ] **Step 4: Verify staged rg runs**

Run:

```bash
src-tauri/vendor/rg/bin/$(rustc -vV | awk '/host:/ {print $2}')/rg --version
```

Expected first line contains:

```text
ripgrep 14.1.1
```

- [ ] **Step 5: Commit**

```bash
git add package.json scripts/fetch-rg.mjs
git commit -m "chore: fetch bundled ripgrep during packaging"
```

Do not add `src-tauri/vendor/rg/bin/` or `src-tauri/vendor/rg/downloads/`.

---

### Task 3: Configure Tauri resources and Rust ripgrep resolver

**Files:**
- Modify: `src-tauri/tauri.conf.json`
- Create: `src-tauri/src/tools/ripgrep.rs`
- Modify: `src-tauri/src/tools/mod.rs`

- [ ] **Step 1: Bundle staged ripgrep files as resources**

In `src-tauri/tauri.conf.json`, inside the existing `bundle` object, add a `resources` field after `licenseFile`:

```json
"licenseFile": "../LICENSE",
"resources": ["vendor/rg/bin/**/*"],
"icon": [
```

The surrounding bundle block should remain valid JSON.

- [ ] **Step 2: Add the Rust resolver module**

Create `src-tauri/src/tools/ripgrep.rs` with this exact content:

```rust
use std::path::PathBuf;

use super::executor::ToolOutput;

const RG_RESOURCE_ROOT: &str = "vendor/rg/bin";

pub(crate) const GREP_MAX_OUTPUT_CHARS: usize = 80 * 1024;
pub(crate) const GREP_MAX_LINE_CHARS: usize = 2 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct RgCommand {
    pub path: PathBuf,
    pub source: RgCommandSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RgCommandSource {
    BundledResource,
    DevVendor,
    Path,
}

pub(crate) fn host_target() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64")
    )))]
    {
        "unsupported"
    }
}

pub(crate) fn executable_name() -> &'static str {
    if cfg!(windows) {
        "rg.exe"
    } else {
        "rg"
    }
}

pub(crate) fn resource_relative_path() -> PathBuf {
    PathBuf::from(RG_RESOURCE_ROOT)
        .join(host_target())
        .join(executable_name())
}

pub(crate) fn dev_vendor_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(resource_relative_path())
}

pub(crate) fn resolve_rg_command(resource_dir: Option<PathBuf>) -> Result<RgCommand, ToolOutput> {
    if let Some(resource_dir) = resource_dir {
        let candidate = resource_dir.join(resource_relative_path());
        if is_executable_file(&candidate) {
            return Ok(RgCommand {
                path: candidate,
                source: RgCommandSource::BundledResource,
            });
        }
    }

    let dev_candidate = dev_vendor_path();
    if is_executable_file(&dev_candidate) {
        return Ok(RgCommand {
            path: dev_candidate,
            source: RgCommandSource::DevVendor,
        });
    }

    Ok(RgCommand {
        path: PathBuf::from("rg"),
        source: RgCommandSource::Path,
    })
}

pub(crate) fn dependency_missing_error(spawn_error: &std::io::Error) -> ToolOutput {
    ToolOutput::error(
        "dependency_missing",
        &format!(
            "grep requires bundled ripgrep or ripgrep (`rg`) on PATH; failed to launch rg: {spawn_error}. Run `node scripts/fetch-rg.mjs` before packaging or install rg for development."
        ),
    )
}

pub(crate) fn capability_tool_error(class: &str, message: String) -> ToolOutput {
    let payload = serde_json::json!({
        "error": "tool_capability_error",
        "tool": "grep",
        "capability_error_class": class,
        "capability_feedback": true,
        "message": message,
        "hint": "The ripgrep binary is missing, not executable, or could not be launched. Use the bundled resource when packaged, or run `node scripts/fetch-rg.mjs` / install `rg` in development."
    });
    ToolOutput {
        content: payload.to_string(),
        is_error: true,
        meta: Some(serde_json::json!({
            "capability_feedback": true,
            "capability_error_class": class,
            "tool": "grep",
        })),
    }
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_relative_path_uses_host_target() {
        let path = resource_relative_path();
        assert!(path.starts_with(RG_RESOURCE_ROOT));
        assert!(path.to_string_lossy().contains(host_target()));
        assert_eq!(path.file_name().unwrap(), executable_name());
    }
}
```

- [ ] **Step 3: Wire the module**

In `src-tauri/src/tools/mod.rs`, change:

```rust
mod grep_engine;
```

to:

```rust
mod ripgrep;
```

- [ ] **Step 4: Run resolver tests**

Run:

```bash
cd src-tauri && cargo test tools::ripgrep::tests::resource_relative_path_uses_host_target
```

Expected:

```text
test tools::ripgrep::tests::resource_relative_path_uses_host_target ... ok
```

- [ ] **Step 5: Commit**

```bash
git add src-tauri/tauri.conf.json src-tauri/src/tools/mod.rs src-tauri/src/tools/ripgrep.rs
git commit -m "feat: resolve bundled ripgrep binary"
```

---

### Task 4: Restore grep execution to real rg CLI

**Files:**
- Modify: `src-tauri/src/tools/executor.rs`
- Delete: `src-tauri/src/tools/grep_engine.rs`
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Replace imports and constants**

In `src-tauri/src/tools/executor.rs`, replace the top import block:

```rust
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::grep_engine::{grep_lib_search, GrepParams};
```

with:

```rust
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use super::ripgrep::{
    capability_tool_error, dependency_missing_error, resolve_rg_command, GREP_MAX_LINE_CHARS,
    GREP_MAX_OUTPUT_CHARS,
};
```

- [ ] **Step 2: Restore the grep implementation**

In `src-tauri/src/tools/executor.rs`, replace the body of `async fn grep(&self, input: &serde_json::Value) -> ToolOutput` from after `let output_mode = ...;` through `grep_lib_search(params)` with this code:

```rust
        let mut args: Vec<String> = Vec::new();
        match output_mode {
            "files_with_matches" => {
                args.push("--files-with-matches".into());
            }
            "count" => {
                args.push("--count-matches".into());
            }
            "content" => {
                args.push("--line-number".into());
                if let Some(c) = context {
                    args.push(format!("-C{c}"));
                } else {
                    if let Some(b) = context_before {
                        args.push(format!("-B{b}"));
                    }
                    if let Some(a) = context_after {
                        args.push(format!("-A{a}"));
                    }
                }
            }
            other => {
                return ToolOutput::error(
                    "parameter_error",
                    &format!(
                        "Unknown output_mode `{other}`. Use one of: content, files_with_matches, count."
                    ),
                );
            }
        }
        if case_insensitive {
            args.push("-i".into());
        }
        if multiline {
            args.push("--multiline".into());
            args.push("--multiline-dotall".into());
        }
        if let Some(g) = glob_pat {
            args.push("--glob".into());
            args.push(g.into());
        }
        if let Some(t) = type_filter {
            args.push("--type".into());
            args.push(t.into());
        }
        if !explicit_path && !matches!(self.path_scope(&search_path), PathScope::Evidence) {
            args.push("--glob".into());
            args.push("!.miragenty-evidence/**".into());
            args.push("--glob".into());
            args.push("!.miragenty/tool-results/**".into());
            if let Some(workspace_name) = self
                .workspace_root
                .file_name()
                .and_then(|name| name.to_str())
            {
                args.push("--glob".into());
                args.push(format!("!assets/{workspace_name}/**"));
            }
        }
        args.push("--color=never".into());
        args.push("-e".into());
        args.push(pattern.into());

        let rg_command = match resolve_rg_command(None) {
            Ok(command) => command,
            Err(output) => return output,
        };
        tracing::debug!(
            tool = "grep",
            rg_path = %rg_command.path.display(),
            rg_source = ?rg_command.source,
            workspace = %search_path.display(),
            "spawning ripgrep"
        );

        let mut child = match Command::new(&rg_command.path)
            .args(&args)
            .current_dir(&search_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return dependency_missing_error(&e);
            }
            Err(e) => {
                return capability_tool_error(
                    "dependency_spawn_failure",
                    format!("failed to spawn rg at {}: {e}", rg_command.path.display()),
                )
            }
        };
        let Some(stdout) = child.stdout.take() else {
            return ToolOutput::error("rg_error", "failed to capture rg stdout");
        };
        let Some(mut stderr) = child.stderr.take() else {
            return ToolOutput::error("rg_error", "failed to capture rg stderr");
        };
        let stdout_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            let mut lines = Vec::new();
            let mut truncated = false;
            while let Some(line) = reader.next_line().await? {
                if lines.len() < head_limit {
                    lines.push(line);
                } else {
                    truncated = true;
                    break;
                }
            }
            Ok::<_, std::io::Error>((lines, truncated))
        });
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            stderr.read_to_string(&mut buf).await?;
            Ok::<_, std::io::Error>(buf)
        });

        let (lines, truncated) = match stdout_task.await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                return ToolOutput::error("rg_error", &format!("failed reading rg stdout: {e}"))
            }
            Err(e) => return ToolOutput::error("rg_error", &format!("rg stdout task failed: {e}")),
        };
        if truncated {
            let _ = child.kill().await;
        }
        let status = match child.wait().await {
            Ok(status) => status,
            Err(e) => return ToolOutput::error("rg_error", &format!("failed waiting for rg: {e}")),
        };
        let stderr = match stderr_task.await {
            Ok(Ok(stderr)) => stderr,
            Ok(Err(e)) => {
                return ToolOutput::error("rg_error", &format!("failed reading rg stderr: {e}"))
            }
            Err(e) => return ToolOutput::error("rg_error", &format!("rg stderr task failed: {e}")),
        };
        let exit_code = status.code().unwrap_or(-1);

        if !truncated && exit_code == 1 {
            return ToolOutput::ok(format!(
                "No matches for pattern `{pattern}`{}.",
                glob_pat
                    .map(|g| format!(" (glob `{g}`)"))
                    .unwrap_or_default()
            ));
        }
        if !truncated && exit_code >= 2 {
            return ToolOutput::error(
                "rg_error",
                &format!("ripgrep exited with code {exit_code}: {}", stderr.trim()),
            );
        }

        let mut body = lines
            .iter()
            .map(|line| truncate_middle_chars(line, GREP_MAX_LINE_CHARS))
            .collect::<Vec<_>>()
            .join("\n");
        if truncated {
            body.push_str(&format!(
                "\n... [truncated after {head_limit} lines; pass head_limit higher or narrow with glob/type]"
            ));
        }
        if body.is_empty() {
            body = format!("(rg returned no output for `{pattern}`)");
        }
        body = cap_text_with_notice(
            body,
            GREP_MAX_OUTPUT_CHARS,
            "use a narrower pattern, glob/type filter, output_mode=count/files_with_matches, or lower head_limit",
        );
        ToolOutput::ok(body)
```

- [ ] **Step 3: Remove the old Rust grep engine file**

Delete:

```bash
rm src-tauri/src/tools/grep_engine.rs
```

- [ ] **Step 4: Remove custom grep crate dependencies**

In `src-tauri/Cargo.toml`, remove these dependency lines:

```toml
grep = "0.3"
grep-regex = "0.1"
grep-searcher = "0.1"
ignore = "0.4"
```

- [ ] **Step 5: Update Cargo.lock**

Run:

```bash
cd src-tauri && cargo check
```

Expected: exits 0. `Cargo.lock` should drop the now-unused grep-engine dependencies if nothing else uses them.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/tools/executor.rs src-tauri/src/tools/mod.rs
git rm --ignore-unmatch src-tauri/src/tools/grep_engine.rs
git commit -m "feat: use bundled ripgrep for grep tool"
```

---

### Task 5: Update and run grep tests against real rg

**Files:**
- Modify: `src-tauri/src/tools/executor.rs`
- Modify: `src-tauri/src/tools/ripgrep.rs`

- [ ] **Step 1: Remove obsolete grep-engine test import**

In the test module of `src-tauri/src/tools/executor.rs`, remove this line:

```rust
use crate::tools::grep_engine::GREP_MAX_LINE_CHARS;
```

The production import from `super::ripgrep` already brings `GREP_MAX_LINE_CHARS` into the parent module, and `use super::*;` makes it available to tests.

- [ ] **Step 2: Add a PATH fallback test for the resolver**

In `src-tauri/src/tools/ripgrep.rs`, add this test inside the existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn resolve_rg_command_falls_back_to_path_when_no_resource_dir() {
        let command = resolve_rg_command(Some(PathBuf::from("/definitely/missing/resource/dir")))
            .expect("resolver should return PATH fallback instead of an error");
        if dev_vendor_path().exists() {
            assert_eq!(command.source, RgCommandSource::DevVendor);
        } else {
            assert_eq!(command.source, RgCommandSource::Path);
            assert_eq!(command.path, PathBuf::from("rg"));
        }
    }
```

- [ ] **Step 3: Run the focused resolver tests**

Run:

```bash
cd src-tauri && cargo test tools::ripgrep::tests -- --nocapture
```

Expected: both ripgrep resolver tests pass.

- [ ] **Step 4: Run the grep/search_files tests**

Run:

```bash
cd src-tauri && cargo test tools::executor::tests::search_files_files_with_matches_mode tools::executor::tests::search_files_glob_filters tools::executor::tests::search_files_no_match_returns_friendly_message tools::executor::tests::grep_content_mode_returns_line_numbers tools::executor::tests::grep_count_mode_aggregates_per_file tools::executor::tests::grep_truncates_long_matching_lines tools::executor::tests::grep_head_limit_stops_after_requested_lines tools::executor::tests::grep_and_search_files_alias_produce_identical_output -- --nocapture
```

Expected: all listed tests pass. If `rg` is not available on PATH and `src-tauri/vendor/rg/bin/<host>/rg` is missing, run `node ../scripts/fetch-rg.mjs` from `src-tauri`'s parent directory and retry.

- [ ] **Step 5: Run the broader tool tests that cover evidence exclusions**

Run:

```bash
cd src-tauri && cargo test tools::executor::tests::default_search_glob_and_list_skip_internal_evidence_dirs tools::executor::tests::default_discovery_skips_mirrored_benchmark_assets tools::executor::tests::explicit_evidence_path_can_be_read_grepped_and_listed -- --nocapture
```

Expected: all listed tests pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/tools/executor.rs src-tauri/src/tools/ripgrep.rs
git commit -m "test: cover real ripgrep resolver and grep behavior"
```

---

### Task 6: Verify packaging resource inclusion

**Files:**
- Modify only if verification exposes a Tauri resource path issue:
  - `src-tauri/tauri.conf.json`
  - `src-tauri/src/tools/ripgrep.rs`

- [ ] **Step 1: Confirm staged rg exists**

Run:

```bash
node scripts/fetch-rg.mjs
```

Expected: exits 0 and prints either `already staged` or `staged`.

- [ ] **Step 2: Validate Tauri config**

Run:

```bash
pnpm tauri build --debug
```

Expected: build exits 0. If Tauri rejects the `resources` syntax, change `src-tauri/tauri.conf.json` from:

```json
"resources": ["vendor/rg/bin/**/*"]
```

to explicit paths for the currently supported macOS resources:

```json
"resources": [
  "vendor/rg/bin/aarch64-apple-darwin/rg",
  "vendor/rg/bin/x86_64-apple-darwin/rg"
]
```

Then rerun `pnpm tauri build --debug`.

- [ ] **Step 3: Inspect the debug app bundle for rg**

Run:

```bash
find src-tauri/target/debug/bundle -name rg -type f -print
```

Expected: at least one result under the app bundle resources directory, ending with:

```text
vendor/rg/bin/aarch64-apple-darwin/rg
```

or, on Intel macOS:

```text
vendor/rg/bin/x86_64-apple-darwin/rg
```

- [ ] **Step 4: Run final Rust checks**

Run:

```bash
cd src-tauri && cargo test tools::ripgrep::tests tools::executor::tests::grep_and_search_files_alias_produce_identical_output
```

Expected: exits 0.

- [ ] **Step 5: Commit any resource syntax fix**

If Task 6 required a `tauri.conf.json` or resolver change, commit it:

```bash
git add src-tauri/tauri.conf.json src-tauri/src/tools/ripgrep.rs
git commit -m "fix: include bundled ripgrep resource in app bundle"
```

If no files changed, skip this commit.

---

### Task 7: Final cleanup and verification

**Files:**
- Review all changed files.

- [ ] **Step 1: Confirm the partial Rust grep engine is gone**

Run:

```bash
test ! -e src-tauri/src/tools/grep_engine.rs
grep -R "grep_engine\|grep_lib_search\|GrepParams" -n src-tauri/src src-tauri/Cargo.toml || true
```

Expected: first command exits 0. Second command prints no matches.

- [ ] **Step 2: Confirm no generated binaries are staged in git**

Run:

```bash
git status --short src-tauri/vendor/rg
```

Expected: only `manifest.json` and `README.md` are tracked/modified; no files under `bin/` or `downloads/` appear.

- [ ] **Step 3: Run frontend build if package script changed**

Run:

```bash
pnpm build
```

Expected: exits 0.

- [ ] **Step 4: Run final cargo check**

Run:

```bash
cd src-tauri && cargo check
```

Expected: exits 0.

- [ ] **Step 5: Inspect final diff**

Run:

```bash
git diff --stat
git diff -- src-tauri/src/tools/executor.rs src-tauri/src/tools/ripgrep.rs src-tauri/tauri.conf.json package.json scripts/fetch-rg.mjs
```

Expected: diff shows real `rg` subprocess execution, new resolver, fetch script, Tauri resource config, and no partial Rust grep engine.

- [ ] **Step 6: Commit final cleanup if needed**

If Task 7 produced any cleanup changes, commit them:

```bash
git add .
git commit -m "chore: finalize bundled ripgrep integration"
```

Do not commit staged binaries or downloads.

---

## Self-Review

- Spec coverage: covered manifest/fetch script, no committed binaries, Tauri resource bundling, bundled-first runtime lookup with dev/PATH fallback, removing the partial Rust grep engine, and tests for real `rg` behavior.
- Placeholder scan: no TBD/TODO/fill-in placeholders. The only conditional step is the explicit Tauri resource syntax fallback with exact replacement JSON.
- Type consistency: `RgCommand`, `RgCommandSource`, `resolve_rg_command`, `resource_relative_path`, `dev_vendor_path`, `GREP_MAX_OUTPUT_CHARS`, and `GREP_MAX_LINE_CHARS` are defined before use and referenced consistently.
