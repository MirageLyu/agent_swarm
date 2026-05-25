//! FM-15 FR-08.2 (3): 用 LLM Provider 实现 `LlmConflictResolver`。
//!
//! 把单个文件的 ours/theirs/base 三方原文喂给 LLM，要求其返回**纯文件内容**——
//! 不带 conflict markers、不带 markdown 围栏。
//!
//! 失败模式：
//! - LLM 返回包含明显 conflict markers (`<<<<<<<`) → 视为失败
//! - LLM 返回空内容 → 视为失败
//! - LLM 调用本身错误 → 失败
//!
//! 失败时上层（`git::llm_merge::merge_with_llm`）会回退到 theirs 兜底。

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use std::sync::Arc;

use crate::git::llm_merge::{ConflictBlob, LlmConflictResolver};
use crate::llm::{ContentBlock, LlmProvider, LlmRequest, Message, MessageRole};

/// 默认最大文件大小（字节）。超过则跳过 LLM（避免炸 context window）。
const MAX_BLOB_BYTES: usize = 64 * 1024;

/// 默认 max_tokens。设大一点以容纳完整文件。
const DEFAULT_MAX_TOKENS: u32 = 8192;

pub struct LlmProviderResolver {
    provider: Arc<dyn LlmProvider>,
    model: String,
    max_blob_bytes: usize,
    max_tokens: u32,
}

impl LlmProviderResolver {
    pub fn new(provider: Arc<dyn LlmProvider>, model: String) -> Self {
        Self {
            provider,
            model,
            max_blob_bytes: MAX_BLOB_BYTES,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    fn system_prompt() -> String {
        r#"You are a Git merge conflict resolver. You will receive three versions of one file:
- BASE: the common ancestor
- OURS: the target branch's version
- THEIRS: the source branch's version (incoming changes)

Your job: produce the correct merged content for that file.

STRICT OUTPUT RULES:
1. Return ONLY the full final file content. Nothing else.
2. NEVER include conflict markers (<<<<<<<, =======, >>>>>>>).
3. NEVER wrap your output in markdown code fences.
4. NEVER add commentary, headers, footers, or explanations.
5. Preserve the existing language / framework conventions visible in BASE/OURS/THEIRS.
6. When changes are non-overlapping (e.g., both sides add different functions), include BOTH.
7. When changes overlap, prefer the semantic intent of THEIRS unless OURS is clearly more correct.
8. Keep imports tidy and de-duplicated.
9. Preserve trailing newline if originals have one."#
            .to_string()
    }
}

#[async_trait]
impl LlmConflictResolver for LlmProviderResolver {
    async fn resolve(&self, conflict: &ConflictBlob) -> Result<String> {
        let ours = conflict.ours.as_deref().unwrap_or("");
        let theirs = conflict.theirs.as_deref().unwrap_or("");
        let base = conflict.base.as_deref().unwrap_or("");

        let total = ours.len() + theirs.len() + base.len();
        if total > self.max_blob_bytes {
            return Err(anyhow!(
                "conflict file `{}` too large ({} bytes > {} limit), skipping LLM",
                conflict.path,
                total,
                self.max_blob_bytes
            ));
        }

        let user_text = format!(
            "Path: {path}\n\n=== BASE ===\n{base}\n=== END BASE ===\n\n=== OURS ===\n{ours}\n=== END OURS ===\n\n=== THEIRS ===\n{theirs}\n=== END THEIRS ===\n\nReturn ONLY the merged file content (no markers, no fences).",
            path = conflict.path,
            base = base,
            ours = ours,
            theirs = theirs,
        );

        let req = LlmRequest {
            model: self.model.clone(),
            system: Some(Self::system_prompt()),
            messages: vec![Message {
                role: MessageRole::User,
                content: vec![ContentBlock::Text { text: user_text }],
                cache_control: None,
            }],
            tools: Vec::new(),
            max_tokens: self.max_tokens,
            provider_extras: None,
        };

        let resp = self
            .provider
            .chat(&req)
            .await
            .context("LLM provider chat() failed")?;

        // 取第一段 text content
        let text = resp
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        let merged = post_process(&text).ok_or_else(|| {
            anyhow!(
                "LLM returned empty / invalid content for `{}`",
                conflict.path
            )
        })?;

        if merged.contains("<<<<<<<") || merged.contains(">>>>>>>") {
            return Err(anyhow!(
                "LLM output for `{}` still contains conflict markers",
                conflict.path
            ));
        }

        Ok(merged)
    }
}

/// 清洗 LLM 输出：
/// - 去掉 markdown 围栏
/// - 修剪首尾空白行（但保留文件最后的换行）
fn post_process(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(|c: char| c == '\u{FEFF}'); // BOM
    let mut s = trimmed.trim().to_string();
    if s.is_empty() {
        return None;
    }

    // 去掉 ```lang ... ``` 围栏
    if s.starts_with("```") {
        // 跳过第一行
        if let Some(first_nl) = s.find('\n') {
            s = s[first_nl + 1..].to_string();
        }
        if let Some(last_fence) = s.rfind("```") {
            s.truncate(last_fence);
        }
        s = s.trim_end().to_string();
    }

    if s.is_empty() {
        return None;
    }

    // 保留末尾换行（很多源文件以 \n 结束，原始 ours/theirs 通常有）
    if !s.ends_with('\n') {
        s.push('\n');
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_process_strips_code_fence() {
        let raw = "```rust\nfn main() {}\n```";
        let p = post_process(raw).unwrap();
        assert_eq!(p, "fn main() {}\n");
    }

    #[test]
    fn post_process_keeps_plain_text_with_trailing_newline() {
        let raw = "hello world";
        let p = post_process(raw).unwrap();
        assert_eq!(p, "hello world\n");
    }

    #[test]
    fn post_process_returns_none_for_empty() {
        assert!(post_process("").is_none());
        assert!(post_process("   \n  \n").is_none());
    }

    #[test]
    fn post_process_preserves_existing_trailing_newline() {
        let raw = "first\nsecond\n";
        let p = post_process(raw).unwrap();
        assert_eq!(p, "first\nsecond\n");
    }

    #[test]
    fn post_process_strips_fence_with_no_lang() {
        let raw = "```\ncontent\n```";
        let p = post_process(raw).unwrap();
        assert_eq!(p, "content\n");
    }
}
