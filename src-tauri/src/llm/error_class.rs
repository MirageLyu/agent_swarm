//! Single-Agent Uplift P0-1：LLM stream / chat 错误的轻量分类器。
//!
//! # 为什么需要
//!
//! Agent 主循环（`engine.rs`）在 `StreamGuardError::Llm(msg)` 分支里需要区分：
//! - **可恢复**的错误（context 超长 → reactive compact 救一次）
//! - **不可恢复**的错误（鉴权失败、模型不存在等 → 直接 fail）
//!
//! 现状是统一 `bail!`，所有可恢复故障都被当不可恢复处理。本模块把识别逻辑独立出来，
//! 让上层 caller 拿到分类后做差异化处理。
//!
//! # 设计原则
//!
//! 1. **只看错误消息文本，不依赖 HTTP status**：reseller（DeepSeek/通义/SiliconFlow）
//!    经常把 413 / 400 包成 200 + body 里写 `error.code`。看消息文本（混合了 status
//!    和 body 提取的错误描述）反而最稳。
//! 2. **关键词集合保守**：宁可漏判为 `Generic`（继续 bail），也不要误判把网络抖动当
//!    context 超长触发 compact——compact 自己也要消耗 tokens。
//! 3. **后续扩展低风险**：未来 P1-2 加 `Overloaded` / `RateLimited` 也只是加 enum 变体
//!    + 加一组关键词，不影响现有调用点。
//!
//! # 不在本模块的事
//!
//! - 选取恢复策略：分类器只回答"这是什么错"，决定怎么救由 caller 做
//! - 收集错误日志：上层 caller 自己 tracing；分类器是纯函数
//! - retry 计数 / state：和分类正交

/// LLM 错误的高层分类。**只用于决定恢复策略**，不用于用户文案。
///
/// 变体由各 P 引入：
/// - P0-1：`PromptTooLong` → reactive compact
/// - P1-2：`Overloaded` / `RateLimited` → cross-model fallback
/// - P1-3 之后还可能加 `MediaSize` 等
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmErrorClass {
    /// Context window 超长。典型错误样本：
    /// - Anthropic: `"prompt is too long: ... tokens > X maximum"`
    /// - OpenAI:    `"This model's maximum context length is X tokens..."`
    /// - DeepSeek:  body 含 `"context_length_exceeded"`
    /// - 通义:      `"Range of input length should be ... InputTokensLimit"`
    PromptTooLong,
    /// 上游模型暂时过载（HTTP 503/529 + body 写 overloaded）。
    /// 切 fallback 模型大概率立刻成功。
    Overloaded,
    /// Rate limit (HTTP 429 / quota_exceeded)。Account/key 维度限速，
    /// 切到不同账户的 fallback 模型可解。
    RateLimited,
    /// 其它（包括但不限于：auth / network / 未知 5xx）。
    Generic,
}

impl LlmErrorClass {
    /// 给日志 / event meta 用的短字符串标识。
    ///
    /// **稳定性契约**：这些字符串会进 event meta 持久化到 DB，前端按字符串匹配渲染。
    /// 改名 = 破坏所有历史 event 解析 + 前端 fallback 渲染 → 不允许变。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PromptTooLong => "prompt_too_long",
            Self::Overloaded => "overloaded",
            Self::RateLimited => "rate_limited",
            Self::Generic => "generic",
        }
    }

    /// 判定：这个分类是否应该触发 P1-2 cross-model fallback。
    pub fn is_fallback_trigger(&self) -> bool {
        matches!(self, Self::Overloaded | Self::RateLimited)
    }
}

/// 把一条 LLM 错误消息归类。
///
/// # 优先级
///
/// **PromptTooLong 优先级最高**：如果消息同时命中 context-length 和 overload 关键词
/// （罕见但可能 —— 某些 reseller 把多种错误塞在一条消息里），优先按 PromptTooLong
/// 处理。原因：reactive compact 是 step 内自救（成本低），fallback 是切 model（成本高+
/// 行为变更），保守路径优先。
///
/// 其它顺序：RateLimited > Overloaded > Generic。RateLimited 比 Overloaded 优先是
/// 因为 429 通常意味着"短时间内同样会被拒"，需要切账号/模型；529 则可能"等几秒就好"。
///
/// # 实现注意
///
/// - **大小写不敏感**：reseller 大小写不一致，统一 lowercase 后匹配
/// - **needle 集合需要单测覆盖各家真实错误样本**（见下方 mod tests）
pub fn classify_llm_error(msg: &str) -> LlmErrorClass {
    let lower = msg.to_lowercase();
    for needle in PROMPT_TOO_LONG_NEEDLES {
        if lower.contains(needle) {
            return LlmErrorClass::PromptTooLong;
        }
    }
    for needle in RATE_LIMIT_NEEDLES {
        if lower.contains(needle) {
            return LlmErrorClass::RateLimited;
        }
    }
    for needle in OVERLOAD_NEEDLES {
        if lower.contains(needle) {
            return LlmErrorClass::Overloaded;
        }
    }
    LlmErrorClass::Generic
}

/// "Context 超长" 关键词集合。**新增 needle 必须配对加单测**（确保不会误伤其它错误）。
///
/// 每个 needle 后的注释标注出处：哪家 provider 在什么场景下吐这条。这样后续删/改时
/// 能 trace 回原始 case，避免"看着像没用的就删了"。
const PROMPT_TOO_LONG_NEEDLES: &[&str] = &[
    // OpenAI 标准 error.code，被 OpenAI-compat reseller 普遍透传
    "context_length_exceeded",
    // Anthropic 标准错误描述（HTTP 400 body）
    "prompt is too long",
    // OpenAI 标准错误描述（"This model's maximum context length is X..."）
    "maximum context length",
    // 通义 / DashScope 的错误结构
    "input tokens limit",
    "inputtokenslimit",
    // 通用兜底——很多 reseller 用简短形式
    "prompt too long",
    "context length",
    // 某些代理转译后的字段
    "max_input_tokens",
];

/// "模型过载" 关键词集合（P1-2 cross-model fallback 触发器）。
///
/// 错误特征：上游服务能接到请求但暂时无法处理（5xx 系列）。切到 fallback 模型
/// 通常立刻成功，因为 fallback 走另一条计算路径/集群。
const OVERLOAD_NEEDLES: &[&str] = &[
    // Anthropic 标准 error.type（HTTP 529 body）
    "overloaded_error",
    // 通用形式
    "overloaded",
    // OpenAI 标准 503
    "service_unavailable",
    "service unavailable",
    // 某些 reseller 用的描述
    "model_overloaded",
    // 容量不足 —— 一些 GPU reseller 在算力紧张时返回
    "insufficient_capacity",
    // 通用兜底，与 rate-limit 有重叠（先匹 RATE_LIMIT_NEEDLES 防误判）
    "try again later",
];

/// "Rate limit" 关键词集合（P1-2 cross-model fallback 触发器）。
///
/// 错误特征：触发账号/key/模型维度的限速。切到不同账号或不同模型的 fallback 通常可解。
const RATE_LIMIT_NEEDLES: &[&str] = &[
    // OpenAI 标准 error.code
    "rate_limit_exceeded",
    // Anthropic / 通用 HTTP 429 描述
    "too many requests",
    // 一些 reseller 用的字段
    "request_limit",
    "requests per minute",
    // 配额耗尽——视同 rate limit 处理，fallback 可能用不同 key 解
    "quota_exceeded",
    "insufficient_quota",
    // 兜底
    "rate limit",
];

#[cfg(test)]
mod tests {
    //! 覆盖每家 provider 真实抓到的错误消息样本（出处见 needle 注释）+ 关键反例：
    //! 不能把 rate limit / auth / 网络错误误判成 PromptTooLong。

    use super::*;

    #[test]
    fn classifies_openai_context_too_long() {
        // 来源：OpenAI Chat Completions API 标准 400 body
        let msg = "OpenAI compat API error 400: This model's maximum context length is 128000 tokens. However, you requested 195000 tokens (180000 in the messages, 15000 in the completion).";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::PromptTooLong);
    }

    #[test]
    fn classifies_anthropic_prompt_too_long() {
        // 来源：Anthropic Messages API 400 body
        let msg = "Anthropic API error 400: prompt is too long: 234234 tokens > 200000 maximum";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::PromptTooLong);
    }

    #[test]
    fn classifies_deepseek_context_length_exceeded() {
        // 来源：DeepSeek OpenAI-compat 端点 400 body（透传 OpenAI error.code）
        let msg = r#"OpenAI compat API error 400: {"error":{"message":"Range of input length is exceeded","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
        assert_eq!(classify_llm_error(msg), LlmErrorClass::PromptTooLong);
    }

    #[test]
    fn classifies_dashscope_input_tokens_limit() {
        // 来源：通义千问 DashScope OpenAI-compat 端点
        let msg = "OpenAI compat API error 400: Range of input length should be [1, 30720]. InputTokensLimit: 30720";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::PromptTooLong);
    }

    #[test]
    fn classifies_short_form_prompt_too_long() {
        // 来源：自建 vLLM / llama.cpp 等代理常见简短形式
        let msg = "Backend error: prompt too long";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::PromptTooLong);
    }

    #[test]
    fn case_insensitive_matching() {
        // 防御：大小写不应该影响分类
        let msg = "CONTEXT_LENGTH_EXCEEDED";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::PromptTooLong);
        let msg2 = "Context Length Exceeded";
        assert_eq!(classify_llm_error(msg2), LlmErrorClass::PromptTooLong);
    }

    #[test]
    fn network_error_is_generic_not_prompt_too_long() {
        // 关键反例：网络断开不能被当成 context 超长触发 compact
        let msg = "网络连接中断，请检查网络后重试 (stream error: connection reset)";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::Generic);
    }

    #[test]
    fn auth_error_is_generic() {
        // 鉴权失败：必须让用户看到，不能默默 compact
        let msg = "OpenAI compat API error 401: invalid api key";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::Generic);
    }

    #[test]
    fn classifies_rate_limit_openai_429() {
        // P1-2: OpenAI 标准 429 → RateLimited（不再是 Generic）
        let msg = "OpenAI compat API error 429: rate_limit_exceeded";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::RateLimited);
    }

    #[test]
    fn classifies_rate_limit_too_many_requests() {
        // Anthropic / generic HTTP 429
        let msg = "API error 429: too many requests";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::RateLimited);
    }

    #[test]
    fn classifies_quota_exceeded_as_rate_limit() {
        // 配额耗尽视同 RateLimited
        let msg = r#"OpenAI compat API error 429: {"error":{"code":"insufficient_quota"}}"#;
        assert_eq!(classify_llm_error(msg), LlmErrorClass::RateLimited);
    }

    #[test]
    fn classifies_overloaded_anthropic_529() {
        // P1-2: Anthropic 标准 529 → Overloaded（不再是 Generic）
        let msg = r#"Anthropic API error 529: {"error":{"type":"overloaded_error"}}"#;
        assert_eq!(classify_llm_error(msg), LlmErrorClass::Overloaded);
    }

    #[test]
    fn classifies_overloaded_openai_503() {
        // OpenAI 标准 503 service unavailable
        let msg = "OpenAI compat API error 503: service_unavailable";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::Overloaded);
    }

    #[test]
    fn classifies_overloaded_short_form() {
        let msg = "Backend: model overloaded, please retry";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::Overloaded);
    }

    #[test]
    fn rate_limit_takes_precedence_over_overload() {
        // 协议契约：当消息同时含 rate_limit 和 overload 关键词时，按 RateLimited
        // 处理（更准确的根因）。如果实践中 reseller 都把两个塞一起会引发问题再调
        let msg = "rate_limit_exceeded; service is overloaded";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::RateLimited);
    }

    #[test]
    fn prompt_too_long_takes_precedence_over_overload() {
        // 协议契约：context-length 优先于 overload。理由见 fn 注释
        let msg = "context_length_exceeded (also: service overloaded)";
        assert_eq!(classify_llm_error(msg), LlmErrorClass::PromptTooLong);
    }

    #[test]
    fn is_fallback_trigger_only_for_overload_and_rate_limit() {
        // 防御：fallback 触发集合不能漂移
        assert!(LlmErrorClass::Overloaded.is_fallback_trigger());
        assert!(LlmErrorClass::RateLimited.is_fallback_trigger());
        assert!(!LlmErrorClass::PromptTooLong.is_fallback_trigger());
        assert!(!LlmErrorClass::Generic.is_fallback_trigger());
    }

    #[test]
    fn empty_message_is_generic() {
        // 边界：空字符串不应该 panic 也不应该误判
        assert_eq!(classify_llm_error(""), LlmErrorClass::Generic);
    }

    #[test]
    fn as_str_returns_stable_label() {
        // 防御：as_str 输出会进 event meta 落库，改字符串等于破坏前端解析约定
        assert_eq!(LlmErrorClass::PromptTooLong.as_str(), "prompt_too_long");
        assert_eq!(LlmErrorClass::Overloaded.as_str(), "overloaded");
        assert_eq!(LlmErrorClass::RateLimited.as_str(), "rate_limited");
        assert_eq!(LlmErrorClass::Generic.as_str(), "generic");
    }
}
