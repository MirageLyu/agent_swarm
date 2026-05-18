//! Single-Agent Uplift B2: tool_summary 用小模型把超大 tool_result 压缩。
//!
//! 设计动机：
//!   - LLM 一次跑 `cargo test` / `cargo build` / 大文件 read_file 经常吐出
//!     20-200KB 的 tool_result。
//!   - 主 agent 的 context window（即便 200K Claude/64K DeepSeek）也经不住几条这种结果
//!     堆叠 → 触发 microcompact → 早期上下文丢失 → agent 失忆退化。
//!   - 既然结果里"真正给 LLM 看的信息"通常只有 1-2KB（错误堆栈、关键 diff），
//!     用便宜模型把它压成结构化摘要 ≪ 把整段塞进上下文 ≪ 简单截尾。
//!
//! 阈值 8KB（可配）：低于此值不动，省一次 LLM 往返；高于此值才走摘要 → fallback truncate。
//!
//! 失败模式：摘要 LLM 超时 / 报错 / 返回空 → caller 退回到 truncate。**不抛错**，
//! 让主 agent 永远不会因为这层优化而停。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;

use crate::llm::{
    deepseek_adapter, ContentBlock, LlmProvider, LlmRequest, Message, MessageRole,
    OpenAICompatProvider,
};

/// `ToolSummarizer::health_check` 的返回类型。
///
/// 序列化为前端用的 tagged enum：`{ "kind": "ok", ... }` 等，方便 i18n
/// 字符串映射。详见各分支注释。
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HealthOutcome {
    /// 模型正常吐出 content text，配置可直接用作 tool_summary。
    Ok {
        sample: String,
        input_tokens: u64,
        output_tokens: u64,
        elapsed_ms: u64,
    },
    /// 请求成功但 content 为空，`output_tokens` 全是 reasoning_tokens。
    /// → 用户配的是 reasoning model（v4-flash / v4-pro / glm-5 等），
    /// max_tokens=600 永远不出 final content，tool_summary 场景**不能用**。
    /// 建议改 `deepseek-v3-2-251201` 等非 reasoning 小模型。
    ReasoningModelEatsAllTokens {
        input_tokens: u64,
        output_tokens: u64,
        elapsed_ms: u64,
    },
    /// 网络/认证/模型不存在等错误。原样透传给用户。
    Err(String),
}

/// 一次摘要请求的预算。短得离谱是有意的——LLM 应当返回 ≤500 字浓缩，
/// 上限让无状态的便宜模型不会跑跑停停半分钟。
const SUMMARIZE_MAX_TOKENS: u32 = 600;

/// 整个摘要调用的 wall-clock 上限。
/// 超过即超时（caller 走 truncate fallback）；选 12s 是因为 deepseek-v4-flash 的 P95
/// 在 8KB 输入时通常 < 5s，保留 2x 余量；超过了也不该让主 agent 干等。
const SUMMARIZE_TIMEOUT_SECS: u64 = 12;

/// 摘要时给小模型的 system prompt。强调输出格式，避免它写"作为 AI 助手……"开场白。
const SUMMARIZE_SYSTEM: &str = "You compress tool execution outputs for a coding agent. \
Output a concise structured summary in <=500 characters that preserves: \
(a) success/failure verdict; (b) key error messages or stack traces (verbatim, last 1-2); \
(c) key file paths / line numbers / counts; (d) actionable hints for what to check next. \
Drop boilerplate, ASCII tables, repeating progress lines, and anything purely cosmetic. \
DO NOT add commentary, prefaces, or explanations of what you did. Just the summary.";

#[derive(Clone)]
pub struct ToolSummarizer {
    provider: Arc<dyn LlmProvider>,
    model: String,
    /// **Fail-fast 锁**：一旦观察到不可恢复的配置类错误（401/403/无效 key），
    /// 永久关停本实例后续 `summarize`，让主循环立即走 truncate fallback，
    /// 不再为每条超阈值的 tool_result 浪费 LLM 往返（`****zvdf` reseller key
    /// 配 deepseek 官方 endpoint 的 401 案例：每条 long result 白白消耗 3s）。
    ///
    /// 范围：进程内全局共享（`Arc<AtomicBool>` + 共享所有 clone 的 ToolSummarizer
    /// 实例），所以无论 caller 把 summarizer clone 多少份给并发的 agent 用，
    /// 第一次 401 之后**所有后续调用立即 fail-fast**。
    disabled: Arc<AtomicBool>,
}

/// Bail 的错误信息字符串 → 是否属于"不可恢复的配置类错误"。
///
/// 凭文本匹配看似脆弱，但 OpenAI-compat 各家 reseller 的 401/403 payload
/// 都包含 `Unauthorized` / `authentication` / `invalid api key` 字样，
/// 把这些当 disable 信号比解析 reqwest 错误链更稳。
fn is_unrecoverable_auth_error(err_str: &str) -> bool {
    let lower = err_str.to_lowercase();
    lower.contains("401 unauthorized")
        || lower.contains("403 forbidden")
        || lower.contains("authentication fails")
        || lower.contains("authentication_error")
        || lower.contains("invalid api key")
        || lower.contains("invalid_request_error") && lower.contains("api key")
}

impl ToolSummarizer {
    /// 构造一个 OpenAI-compat backed summarizer（DeepSeek 等都走这条路径）。
    /// `model` 为空时返回 None —— 调用方据此关闭摘要，等价于 disabled。
    pub fn try_openai_compat(api_key: String, base_url: String, model: String) -> Option<Self> {
        if model.trim().is_empty() || api_key.trim().is_empty() {
            return None;
        }
        let provider: Arc<dyn LlmProvider> =
            Arc::new(OpenAICompatProvider::new(api_key, base_url));
        Some(Self {
            provider,
            model,
            disabled: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// **配置 dial-test**：用一段最小的 summarize 负载试探当前
    /// (api_key, base_url, model) 三件套是否真的可用。
    ///
    /// 返回的 [`HealthOutcome`] 区分三种结局——前端可据此给出针对性提示：
    ///
    /// - `Ok`: 模型正常吐 final content，配置可用。
    /// - `ReasoningModelEatsAllTokens`: 请求成功但 content 为空，
    ///   reasoning_tokens 占满 max_tokens —— 是 reasoning model 且
    ///   **未被 [`crate::llm::deepseek_adapter`] 自动适配**。
    ///   - DeepSeek-V4 家族（`deepseek-v4-flash` / `deepseek-v4-pro`）
    ///     已被适配器自动注入 `thinking: {"type": "disabled"}`，不会
    ///     落到这一分支。
    ///   - 别家 reasoning model（Qwen QwQ、GLM-5、未知 reseller 私有模型等）
    ///     若没相应适配器，仍会落到这里。建议改成
    ///     `deepseek-v4-flash`（首选）或 `deepseek-v3-2-251201` 等非 reasoning
    ///     小模型。
    /// - `Err`: 网络/认证/模型不存在等错误，原样透传给用户。
    pub async fn health_check(&self) -> HealthOutcome {
        let mut req = LlmRequest {
            model: self.model.clone(),
            system: Some(SUMMARIZE_SYSTEM.to_string()),
            messages: vec![Message {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "Tool: `read_file`\n\nOriginal output (compress this):\n```\nHello world from a 12-byte tool result.\n```"
                        .to_string(),
                }],
                cache_control: None,
            }],
            tools: vec![],
            max_tokens: SUMMARIZE_MAX_TOKENS,
            provider_extras: None,
        };
        // DeepSeek-V4 系列默认 reasoning，max_tokens=600 在 tool_summary 场景
        // 会被吃光 → content 永远空。dial-test 验证 `thinking: disabled` 后
        // 延迟从 ~7s 降到 1.5s 且 content 完整。
        deepseek_adapter::apply_thinking_off_for_deepseek_v4(&mut req);

        let started = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(SUMMARIZE_TIMEOUT_SECS);
        let resp_res = match tokio::time::timeout(timeout, self.provider.chat(&req)).await {
            Ok(r) => r,
            Err(_) => {
                return HealthOutcome::Err(format!(
                    "tool_summary dial-test timed out after {SUMMARIZE_TIMEOUT_SECS}s"
                ));
            }
        };

        let elapsed_ms = started.elapsed().as_millis() as u64;

        let resp = match resp_res {
            Ok(r) => r,
            Err(e) => return HealthOutcome::Err(format!("{e:#}")),
        };

        let content_text = resp
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        if content_text.is_empty() {
            // 这一类配置在生产上会"每次 401 似的浪费 LLM 往返但 content 永远空"，
            // 必须让用户在配置阶段就发现。
            HealthOutcome::ReasoningModelEatsAllTokens {
                input_tokens: resp.usage.input_tokens,
                output_tokens: resp.usage.output_tokens,
                elapsed_ms,
            }
        } else {
            HealthOutcome::Ok {
                sample: if content_text.chars().count() > 200 {
                    content_text.chars().take(200).collect::<String>() + "…"
                } else {
                    content_text
                },
                input_tokens: resp.usage.input_tokens,
                output_tokens: resp.usage.output_tokens,
                elapsed_ms,
            }
        }
    }

    /// 把 tool_result 的内容压成 ≤500 字摘要。
    ///
    /// 上下文：tool_name 让模型知道是哪种工具——shell_exec 输出 vs read_file 输出
    /// 的关键信息密度完全不同，告诉模型有助于它选对压缩策略。
    ///
    /// **Fail-fast**：之前任何一次调用因为认证错误（401/403）失败就永久 disable
    /// 本实例，再次调用直接返回 disabled error 让 caller 走 truncate。
    pub async fn summarize(&self, tool_name: &str, content: &str) -> Result<String> {
        if self.disabled.load(Ordering::Relaxed) {
            anyhow::bail!(
                "tool_summary disabled after earlier auth failure (saving wasted LLM round-trips)"
            );
        }

        let user_text = format!(
            "Tool: `{tool_name}`\n\nOriginal output (compress this):\n```\n{content}\n```"
        );
        let mut req = LlmRequest {
            model: self.model.clone(),
            system: Some(SUMMARIZE_SYSTEM.to_string()),
            messages: vec![Message {
                role: MessageRole::User,
                content: vec![ContentBlock::Text { text: user_text }],
                cache_control: None,
            }],
            tools: vec![],
            max_tokens: SUMMARIZE_MAX_TOKENS,
            provider_extras: None,
        };
        // 与 health_check 一致：v4 系列必须关 thinking，否则 content 会空。
        deepseek_adapter::apply_thinking_off_for_deepseek_v4(&mut req);

        let timeout = std::time::Duration::from_secs(SUMMARIZE_TIMEOUT_SECS);
        let resp_res = tokio::time::timeout(timeout, self.provider.chat(&req))
            .await
            .map_err(|_| {
                anyhow::anyhow!("tool_summary timed out after {}s", SUMMARIZE_TIMEOUT_SECS)
            })?;

        let resp = match resp_res {
            Ok(r) => r,
            Err(e) => {
                // 错误分类：永久型（key 错 / 模型不存在）→ 一次性 disable；
                // 临时型（网络抖动 / reseller 5xx）→ 透传，下次还能再试。
                let s = format!("{e:#}");
                // swap 返回前一次值；只在第一次置位时打 warn，避免日志刷屏
                if is_unrecoverable_auth_error(&s)
                    && !self.disabled.swap(true, Ordering::Relaxed)
                {
                    tracing::warn!(
                        model = %self.model,
                        error = %s,
                        "tool_summary disabled for the rest of this session due to \
                         unrecoverable auth/config error; falling back to truncate"
                    );
                }
                return Err(e);
            }
        };

        // 取第一段 text；text 之外的 content（思考链 / tool_use）忽略。
        let summary = resp
            .content
            .iter()
            .find_map(|b| match b {
                ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("tool_summary returned no text content"))?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::is_unrecoverable_auth_error;

    #[test]
    fn classifies_401_and_403_as_unrecoverable() {
        assert!(is_unrecoverable_auth_error(
            "OpenAI compat API error 401 Unauthorized: ..."
        ));
        assert!(is_unrecoverable_auth_error("403 Forbidden — please check key"));
    }

    #[test]
    fn classifies_authentication_payload_as_unrecoverable() {
        assert!(is_unrecoverable_auth_error(
            r#"{"error":{"message":"Authentication Fails, Your api key: ****zvdf is invalid","type":"authentication_error"}}"#
        ));
    }

    #[test]
    fn does_not_misclassify_transient_errors() {
        assert!(!is_unrecoverable_auth_error("502 Bad Gateway"));
        assert!(!is_unrecoverable_auth_error("connection reset by peer"));
        assert!(!is_unrecoverable_auth_error("error decoding response body"));
        assert!(!is_unrecoverable_auth_error(
            "tool_summary timed out after 12s"
        ));
    }
}
