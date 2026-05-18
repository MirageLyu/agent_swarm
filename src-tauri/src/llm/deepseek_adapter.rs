//! DeepSeek 系列模型的特定参数适配。
//!
//! # 为什么需要这一层
//!
//! DeepSeek-V4 系列（`deepseek-v4-flash` / `deepseek-v4-pro` / 后续版本）默认
//! 是 reasoning 模型——服务端会先用一段 reasoning_tokens "思考"，再产出 final
//! `content`。这个行为对**主 agent loop** 是好事：复杂任务靠 reasoning 才能
//! 拿出像样的方案。但对**辅助场景**（tool_summary、guardrail 判断、轻量分类
//! 等）就是灾难——
//!
//! - 我们给 tool_summary 配的 `max_tokens=600`：reasoning 一口气吃光，content
//!   永远是空字符串（实测 bitfun reseller 上 deepseek-v4-flash, max_tokens=200
//!   时 reasoning_tokens=200 / content_chars=0，finish_reason=length）。
//! - 给到 4096 tokens 后 content 终于出来，但 reasoning 占 ~7s+，对一个
//!   "压缩 8KB 文本"的任务严重不成比例。
//!
//! # bitfun reseller 提供的关闭参数
//!
//! 经 2026-05-18 dial-test 验证，bitfun 的 deepseek-v4-flash 接受
//! Anthropic 风格的请求体顶层字段：
//!
//! ```json
//! { "thinking": { "type": "disabled" } }
//! ```
//!
//! 加上后行为：
//! - reasoning_chars=0、reasoning_tokens=0
//! - finish_reason=stop（不再被 length 截断）
//! - 同样 8KB 压缩任务延迟从 ~7s 降到 **1.5s**
//! - content 质量比 v3-2-251201 略好（更详细，能抓到关键调用名、路由）
//!
//! 实测对照：
//! | 候选参数 | 是否生效 |
//! |---|---|
//! | `reasoning_effort: "none"` (OpenAI 风格) | ❌ 400 unknown variant |
//! | `reasoning_effort: "minimal"` | ❌ 400 unknown variant |
//! | `enable_thinking: false` (Qwen 风格) | ❌ 静默忽略，仍 reasoning |
//! | `chat_template_kwargs: {thinking: false}` (vLLM 风格) | ❌ 静默忽略 |
//! | `extra_body: {enable_thinking: false}` | ❌ 静默忽略 |
//! | `thinking: {type: disabled}` | ✅ **唯一可用** |
//!
//! # 适配范围
//!
//! 只对 **deepseek-v4 家族 + 辅助场景** 主动关 thinking。主 agent 流仍走
//! 默认 reasoning（FM-15 体系上层依赖 reasoning_content 续命）。本模块由
//! [`crate::agent::tool_summarizer`] 等"明确不需要思考"的调用方显式启用。
//!
//! 当 reseller 切换或模型更新时（例如未来 DeepSeek 官方支持 `reasoning_effort`），
//! 在这一处加一层匹配/特征探测即可，不需要散落到各 caller。

use serde_json::json;

use crate::llm::LlmRequest;

/// 判断给定 model 名是否属于"DeepSeek-V4 reasoning 家族"。
///
/// 命名规则：
/// - `deepseek-v4-flash` / `deepseek-v4-pro` 是当前命中的型号
/// - `deepseek-v4-*` 任何后续 V4 子系列默认认为是 reasoning
/// - `deepseek-v3-*` 不是 reasoning（V3 系列默认 chat completion）
/// - `deepseek-r1-*` / `deepseek-reasoner` 也是 reasoning，但官方/第三方 API
///   暴露的关法不一致，留作后续扩展时再加
///
/// 仅根据模型名做启发式判断；模型注册表（capability registry）目前不区分
/// 这种"是否 reasoning"，加进去要侵入 [`crate::llm::types::ModelCapability`]，
/// 收益小代价大，留作可见 bug 时再说。
pub fn is_deepseek_v4_reasoning(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.starts_with("deepseek-v4-") || lower == "deepseek-v4"
}

/// 在 `request.provider_extras` 上**追加**关 thinking 的参数。
///
/// 行为契约：
/// - 命中 [`is_deepseek_v4_reasoning`] 时，往 extras 里塞
///   `"thinking": {"type": "disabled"}`，**保留**调用方已有的其他 extras key。
/// - 未命中时**不动** extras。
/// - extras 已存在 `thinking` key 时**不覆盖**——尊重 caller 显式意图。
///
/// 设计成 in-place mutate 而非返回新 LlmRequest：调用方通常已经构造好
/// 完整 LlmRequest，让它一行调用就能"打开 thinking-off 适配"。
pub fn apply_thinking_off_for_deepseek_v4(request: &mut LlmRequest) {
    if !is_deepseek_v4_reasoning(&request.model) {
        return;
    }

    let extras = request
        .provider_extras
        .get_or_insert_with(|| json!({}));

    let extras_obj = match extras.as_object_mut() {
        Some(o) => o,
        None => {
            tracing::warn!(
                model = %request.model,
                "provider_extras already set to a non-object value; \
                 skipping deepseek thinking-off adapter to avoid corrupting it"
            );
            return;
        }
    };

    if extras_obj.contains_key("thinking") {
        // caller 已显式指定，尊重它（可能是 enable + budget 之类 thinking 子结构）
        return;
    }

    extras_obj.insert("thinking".to_string(), json!({"type": "disabled"}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmRequest, Message, MessageRole};

    fn req(model: &str) -> LlmRequest {
        LlmRequest {
            model: model.to_string(),
            system: None,
            messages: vec![Message {
                role: MessageRole::User,
                content: vec![],
                cache_control: None,
            }],
            tools: vec![],
            max_tokens: 100,
            provider_extras: None,
        }
    }

    #[test]
    fn matches_v4_flash_and_pro() {
        assert!(is_deepseek_v4_reasoning("deepseek-v4-flash"));
        assert!(is_deepseek_v4_reasoning("deepseek-v4-pro"));
        assert!(is_deepseek_v4_reasoning("deepseek-v4-future-variant"));
        assert!(is_deepseek_v4_reasoning("deepseek-v4"));
        // 大小写不敏感（reseller 偶尔大写）
        assert!(is_deepseek_v4_reasoning("DeepSeek-V4-Flash"));
    }

    #[test]
    fn does_not_match_v3_or_other_families() {
        assert!(!is_deepseek_v4_reasoning("deepseek-v3-2-251201"));
        assert!(!is_deepseek_v4_reasoning("deepseek-chat"));
        assert!(!is_deepseek_v4_reasoning("deepseek-coder"));
        assert!(!is_deepseek_v4_reasoning("deepseek-r1"));
        assert!(!is_deepseek_v4_reasoning("gpt-5.5"));
        assert!(!is_deepseek_v4_reasoning("qwen3.6-flash"));
        assert!(!is_deepseek_v4_reasoning(""));
    }

    #[test]
    fn injects_thinking_disabled_for_v4_when_extras_empty() {
        let mut r = req("deepseek-v4-flash");
        apply_thinking_off_for_deepseek_v4(&mut r);
        let extras = r.provider_extras.expect("extras should be set");
        assert_eq!(extras["thinking"]["type"], "disabled");
    }

    #[test]
    fn preserves_existing_unrelated_extras() {
        let mut r = req("deepseek-v4-pro");
        r.provider_extras = Some(json!({"top_p": 0.5, "frequency_penalty": 0.1}));
        apply_thinking_off_for_deepseek_v4(&mut r);
        let extras = r.provider_extras.unwrap();
        assert_eq!(extras["top_p"], json!(0.5));
        assert_eq!(extras["frequency_penalty"], json!(0.1));
        assert_eq!(extras["thinking"]["type"], "disabled");
    }

    #[test]
    fn does_not_override_explicit_thinking_setting() {
        let mut r = req("deepseek-v4-flash");
        // caller 想要 thinking 开着 + budget=2000
        r.provider_extras = Some(json!({
            "thinking": {"type": "enabled", "budget_tokens": 2000}
        }));
        apply_thinking_off_for_deepseek_v4(&mut r);
        let extras = r.provider_extras.unwrap();
        assert_eq!(
            extras["thinking"]["type"], "enabled",
            "caller explicit thinking config must not be overwritten"
        );
        assert_eq!(extras["thinking"]["budget_tokens"], json!(2000));
    }

    #[test]
    fn no_op_for_non_deepseek_v4() {
        let mut r = req("deepseek-v3-2-251201");
        apply_thinking_off_for_deepseek_v4(&mut r);
        assert!(r.provider_extras.is_none());

        let mut r = req("gpt-5.5");
        apply_thinking_off_for_deepseek_v4(&mut r);
        assert!(r.provider_extras.is_none());
    }
}
