//! Single-Agent Uplift P0-3：统一的"可恢复错误"日志器。
//!
//! # 解决的问题
//!
//! 旧实现下每个可恢复错误（idle timeout / max_tokens 撞顶 / prompt_too_long）都用
//! `system_hint` event 通知前端——但前端把 system_hint 当用户可见消息渲染，导致
//! 一个长 session 跑完用户会看到一堆"出错了但好了"的灰条，干扰真正需要关注的红色
//! error。这违背了"agent 自己救得回来的事，别打扰用户"的体验原则。
//!
//! 本模块引入两个新 event kind：
//! - `recovery_attempt`：开始一次恢复尝试（meta.silent=true → 前端默认隐藏）
//! - `recovery_succeeded`：恢复路径走通了（meta.silent=true）
//!
//! **恢复失败**仍走旧 `error` 路径（红色可见）——这是设计：让用户**只**关注
//! "真的救不回来"的事件。
//!
//! # 跟现有调用点的关系
//!
//! P0-3 不**新增**恢复机制——只统一 emit 通道。现有三条恢复路径分别由：
//! - **P0-1 Reactive Compact** (`engine.rs` Llm 错误分支) → 触发 `RecoveryTrigger::PromptTooLong`
//! - **idle retry** (`engine.rs` IdleTimeout 分支) → 触发 `RecoveryTrigger::IdleTimeout`
//! - **P1-3 max_output_tokens** (engine.rs stop_reason==length 分支) → 触发
//!   `RecoveryTrigger::MaxOutputTokens`（升档 / multi-turn 两种 strategy）
//!
//! 调用方负责自己判断恢复是否成功，分别调 `emit_recovery_attempt` / `emit_recovery_succeeded`。
//! `succeeded` 的判定语义：下一次 LLM 调用成功完成 = 上一次 attempt 成功。
//!
//! # 不在本模块的事
//!
//! - 决定恢复策略：由各分支自己实现
//! - 数据持久化：通过 caller 的 `emit_event_with_meta` 走 agent_events 表
//! - 前端是否渲染：前端按 `meta.silent` 决定（独立 PR 实现 toggle）

use serde::Serialize;

/// 可恢复错误的来源分类。**对应一种 retry 路径**——不同 trigger 走不同 recovery strategy。
#[derive(Debug, Clone, Copy)]
pub enum RecoveryTrigger {
    /// Context window 超长：触发 P0-1 reactive compact
    PromptTooLong,
    /// LLM 输出撞 max_tokens：触发 P1-3 三档恢复（escalate / multi-turn / surface）
    MaxOutputTokens,
    /// stream 长时间静默：触发 idle retry
    IdleTimeout,
    /// 上游模型暂时过载（P1-2）：触发 cross-model fallback
    Overloaded,
    /// Rate limit（P1-2）：触发 cross-model fallback
    RateLimited,
}

impl RecoveryTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PromptTooLong => "prompt_too_long",
            Self::MaxOutputTokens => "max_output_tokens",
            Self::IdleTimeout => "idle_timeout",
            Self::Overloaded => "overloaded",
            Self::RateLimited => "rate_limited",
        }
    }

    /// 从 `LlmErrorClass` 映射。仅对会触发 recovery 的 class 有意义；
    /// 不触发 recovery 的 class（Generic / PromptTooLong 由专门 trigger 表达）返回 None。
    pub fn from_error_class(class: crate::llm::LlmErrorClass) -> Option<Self> {
        match class {
            crate::llm::LlmErrorClass::PromptTooLong => Some(Self::PromptTooLong),
            crate::llm::LlmErrorClass::Overloaded => Some(Self::Overloaded),
            crate::llm::LlmErrorClass::RateLimited => Some(Self::RateLimited),
            crate::llm::LlmErrorClass::Generic => None,
        }
    }
}

/// 恢复策略的具体形态。每种 trigger 可能对应一种或多种 strategy。
///
/// 用 enum 而非 String 描述：编译期约束 strategy 与 trigger 的合法组合（在 caller
/// 端 match arm 自然守住），后续加新策略也只需扩 enum 不影响 caller。
#[derive(Debug, Clone)]
pub enum RecoveryStrategy {
    /// P0-1: 整体激进压缩重发同 step
    ReactiveCompact {
        dropped_msgs: usize,
        tokens_before: usize,
        tokens_after: usize,
    },
    /// idle retry: 注入 continue 提示
    IdleRetryContinue { retries_left: u32 },
    /// P1-3 ① 升档 max_output_tokens 重发同 step
    OutputTokensEscalate { old_cap: u32, new_cap: u32 },
    /// P1-3 ② multi-turn "resume directly" 让 LLM 接着写
    OutputTokensContinue {
        recovery_count: u32,
        recovery_limit: u32,
    },
    /// P1-2: 切到 fallback 模型重发同 step
    ModelFallback {
        from: String,
        to: String,
        /// agent 累计第几次切换（含本次）
        switch_total: u32,
    },
}

impl RecoveryStrategy {
    /// 给 meta.strategy 用的稳定标识。前端 / 日志 grep 用，**不能随便改**。
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::ReactiveCompact { .. } => "reactive_compact",
            Self::IdleRetryContinue { .. } => "idle_retry_continue",
            Self::OutputTokensEscalate { .. } => "output_tokens_escalate",
            Self::OutputTokensContinue { .. } => "output_tokens_continue",
            Self::ModelFallback { .. } => "model_fallback",
        }
    }

    /// 给 event content 用的人话标签（前端开发者模式下展示给用户看）。
    pub fn human_label(&self) -> String {
        match self {
            Self::ReactiveCompact {
                dropped_msgs,
                tokens_before,
                tokens_after,
            } => {
                format!(
                    "reactive compact (drop {dropped_msgs} msg, ~{}K → ~{}K tokens)",
                    tokens_before / 1000,
                    tokens_after / 1000,
                )
            }
            Self::IdleRetryContinue { retries_left } => {
                format!("idle retry continue ({retries_left} retries left)")
            }
            Self::OutputTokensEscalate { old_cap, new_cap } => {
                format!("escalate max_output_tokens {old_cap} → {new_cap}")
            }
            Self::OutputTokensContinue {
                recovery_count,
                recovery_limit,
            } => {
                format!("multi-turn resume ({recovery_count}/{recovery_limit})")
            }
            Self::ModelFallback {
                from,
                to,
                switch_total,
            } => {
                format!("model fallback {from} → {to} (#{switch_total})")
            }
        }
    }

    /// 给 meta.details 序列化的细节字段。前端 / 后续分析需要更细数据时从这里读。
    pub fn details_json(&self) -> serde_json::Value {
        match self {
            Self::ReactiveCompact {
                dropped_msgs,
                tokens_before,
                tokens_after,
            } => {
                serde_json::json!({
                    "dropped_msgs": dropped_msgs,
                    "tokens_before": tokens_before,
                    "tokens_after": tokens_after,
                })
            }
            Self::IdleRetryContinue { retries_left } => {
                serde_json::json!({ "retries_left": retries_left })
            }
            Self::OutputTokensEscalate { old_cap, new_cap } => {
                serde_json::json!({ "old_cap": old_cap, "new_cap": new_cap })
            }
            Self::OutputTokensContinue {
                recovery_count,
                recovery_limit,
            } => {
                serde_json::json!({
                    "recovery_count": recovery_count,
                    "recovery_limit": recovery_limit,
                })
            }
            Self::ModelFallback {
                from,
                to,
                switch_total,
            } => {
                serde_json::json!({
                    "from": from,
                    "to": to,
                    "switch_total": switch_total,
                })
            }
        }
    }
}

/// 构造 recovery_attempt event 的 meta JSON。
///
/// 抽成 free 函数而非 AgentEngine method，是为了：
/// 1. **可单测**：纯函数，不需要 mock AgentEngine / app handle
/// 2. **不与 engine.rs 耦合**：engine.rs 自己 emit_event_with_meta(kind, content, meta) 即可
///
/// `attempt` 计数语义：同一 step 内同一 trigger 的第 N 次尝试。一般 N=1（recovery
/// 路径不嵌套）；P1-3 的 multi-turn count 用 strategy 内字段表达更准确，attempt 字段
/// 给将来"同一 step 多次失败重试"留位（目前永远为 1）。
pub fn build_recovery_attempt_meta(
    trigger: RecoveryTrigger,
    strategy: &RecoveryStrategy,
    error_excerpt: &str,
    attempt: u32,
) -> serde_json::Value {
    serde_json::json!({
        "silent": true,
        "trigger": trigger.as_str(),
        "strategy": strategy.kind_label(),
        "error_excerpt": error_excerpt.chars().take(200).collect::<String>(),
        "attempt": attempt,
        "details": strategy.details_json(),
    })
}

/// 构造 recovery_succeeded event 的 meta JSON。
pub fn build_recovery_succeeded_meta(
    trigger: RecoveryTrigger,
    strategy: &RecoveryStrategy,
) -> serde_json::Value {
    serde_json::json!({
        "silent": true,
        "trigger": trigger.as_str(),
        "strategy": strategy.kind_label(),
        "details": strategy.details_json(),
    })
}

/// 给 event content 字段（content 是用户可读字符串）的格式化。
pub fn format_attempt_content(trigger: RecoveryTrigger, strategy: &RecoveryStrategy) -> String {
    format!(
        "Auto-recovery for {}: {}",
        trigger.as_str(),
        strategy.human_label()
    )
}

pub fn format_succeeded_content(trigger: RecoveryTrigger, strategy: &RecoveryStrategy) -> String {
    format!(
        "Recovered from {}: {}",
        trigger.as_str(),
        strategy.human_label()
    )
}

// Serialize impls 不是必须，但让 strategy 本身能 JSON 序列化方便测试。
#[derive(Serialize)]
struct _Unused;

#[cfg(test)]
mod tests {
    //! 守住的不变量：
    //!   ① meta 必含 `silent: true`（前端按此过滤）
    //!   ② meta.strategy 字串与 RecoveryStrategy 一一对应（前端按此分类渲染）
    //!   ③ trigger.as_str / strategy.kind_label 返回值稳定（落库 schema 一部分）
    //!   ④ details_json 字段名稳定

    use super::*;

    #[test]
    fn attempt_meta_marks_silent_true() {
        let meta = build_recovery_attempt_meta(
            RecoveryTrigger::PromptTooLong,
            &RecoveryStrategy::ReactiveCompact {
                dropped_msgs: 5,
                tokens_before: 60_000,
                tokens_after: 30_000,
            },
            "context_length_exceeded",
            1,
        );
        assert_eq!(
            meta["silent"],
            serde_json::Value::Bool(true),
            "recovery_attempt 必须 silent，否则前端会渲染成可见事件"
        );
    }

    #[test]
    fn succeeded_meta_marks_silent_true() {
        let meta = build_recovery_succeeded_meta(
            RecoveryTrigger::IdleTimeout,
            &RecoveryStrategy::IdleRetryContinue { retries_left: 1 },
        );
        assert_eq!(meta["silent"], serde_json::Value::Bool(true));
    }

    #[test]
    fn trigger_labels_are_stable() {
        // 这些字串会进 meta.trigger 落库；改了等于破坏前端解析约定 + 旧事件无法识别
        assert_eq!(RecoveryTrigger::PromptTooLong.as_str(), "prompt_too_long");
        assert_eq!(
            RecoveryTrigger::MaxOutputTokens.as_str(),
            "max_output_tokens"
        );
        assert_eq!(RecoveryTrigger::IdleTimeout.as_str(), "idle_timeout");
        assert_eq!(RecoveryTrigger::Overloaded.as_str(), "overloaded");
        assert_eq!(RecoveryTrigger::RateLimited.as_str(), "rate_limited");
    }

    #[test]
    fn from_error_class_only_maps_recoverable_classes() {
        use crate::llm::LlmErrorClass;
        assert!(matches!(
            RecoveryTrigger::from_error_class(LlmErrorClass::PromptTooLong),
            Some(RecoveryTrigger::PromptTooLong)
        ));
        assert!(matches!(
            RecoveryTrigger::from_error_class(LlmErrorClass::Overloaded),
            Some(RecoveryTrigger::Overloaded)
        ));
        assert!(matches!(
            RecoveryTrigger::from_error_class(LlmErrorClass::RateLimited),
            Some(RecoveryTrigger::RateLimited)
        ));
        // Generic 不映射 —— 没法 recover，应该直接 fail
        assert!(RecoveryTrigger::from_error_class(LlmErrorClass::Generic).is_none());
    }

    #[test]
    fn model_fallback_strategy_serializes_correctly() {
        let s = RecoveryStrategy::ModelFallback {
            from: "deepseek-v4".into(),
            to: "qwen3.5-plus".into(),
            switch_total: 2,
        };
        assert_eq!(s.kind_label(), "model_fallback");
        let d = s.details_json();
        assert_eq!(d["from"], "deepseek-v4");
        assert_eq!(d["to"], "qwen3.5-plus");
        assert_eq!(d["switch_total"], 2);
        let h = s.human_label();
        assert!(h.contains("deepseek-v4"));
        assert!(h.contains("qwen3.5-plus"));
        assert!(h.contains("#2"));
    }

    #[test]
    fn strategy_labels_are_stable() {
        // 同上，meta.strategy 是前端按 strategy 分类的依据
        assert_eq!(
            RecoveryStrategy::ReactiveCompact {
                dropped_msgs: 1,
                tokens_before: 0,
                tokens_after: 0
            }
            .kind_label(),
            "reactive_compact"
        );
        assert_eq!(
            RecoveryStrategy::IdleRetryContinue { retries_left: 0 }.kind_label(),
            "idle_retry_continue"
        );
        assert_eq!(
            RecoveryStrategy::OutputTokensEscalate {
                old_cap: 0,
                new_cap: 0
            }
            .kind_label(),
            "output_tokens_escalate"
        );
        assert_eq!(
            RecoveryStrategy::OutputTokensContinue {
                recovery_count: 0,
                recovery_limit: 0
            }
            .kind_label(),
            "output_tokens_continue"
        );
        assert_eq!(
            RecoveryStrategy::ModelFallback {
                from: "a".into(),
                to: "b".into(),
                switch_total: 1
            }
            .kind_label(),
            "model_fallback"
        );
    }

    #[test]
    fn details_json_carries_strategy_specific_fields() {
        let d = RecoveryStrategy::ReactiveCompact {
            dropped_msgs: 7,
            tokens_before: 50_000,
            tokens_after: 25_000,
        }
        .details_json();
        assert_eq!(d["dropped_msgs"], 7);
        assert_eq!(d["tokens_before"], 50_000);
        assert_eq!(d["tokens_after"], 25_000);

        let d = RecoveryStrategy::OutputTokensEscalate {
            old_cap: 16_384,
            new_cap: 65_536,
        }
        .details_json();
        assert_eq!(d["old_cap"], 16_384);
        assert_eq!(d["new_cap"], 65_536);
    }

    #[test]
    fn error_excerpt_is_truncated_to_200_chars() {
        let long_err = "X".repeat(1000);
        let meta = build_recovery_attempt_meta(
            RecoveryTrigger::PromptTooLong,
            &RecoveryStrategy::ReactiveCompact {
                dropped_msgs: 1,
                tokens_before: 0,
                tokens_after: 0,
            },
            &long_err,
            1,
        );
        let excerpt = meta["error_excerpt"].as_str().unwrap();
        assert_eq!(
            excerpt.chars().count(),
            200,
            "error_excerpt 必须 cap 在 200 字符防 meta 爆炸"
        );
    }

    #[test]
    fn human_label_contains_strategy_specific_info() {
        let h = RecoveryStrategy::ReactiveCompact {
            dropped_msgs: 12,
            tokens_before: 80_000,
            tokens_after: 40_000,
        }
        .human_label();
        assert!(h.contains("12"));
        assert!(h.contains("80"));
        assert!(h.contains("40"));
        assert!(h.contains("reactive"));
    }

    #[test]
    fn format_attempt_content_mentions_trigger_and_strategy() {
        let s = format_attempt_content(
            RecoveryTrigger::PromptTooLong,
            &RecoveryStrategy::ReactiveCompact {
                dropped_msgs: 5,
                tokens_before: 60_000,
                tokens_after: 30_000,
            },
        );
        assert!(s.starts_with("Auto-recovery for prompt_too_long"));
        assert!(s.contains("reactive compact"));
    }
}
