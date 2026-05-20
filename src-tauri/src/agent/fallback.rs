//! Single-Agent Uplift P1-2：Cross-Model Fallback 的纯逻辑 helpers。
//!
//! ## 为什么单独建模块
//!
//! engine.rs 已经 3000+ 行，再塞 fallback 状态机进去会让主循环更难追。把
//! 「跨模型切换前必须做的事」抽出来：
//!   1. `strip_reasoning_blocks`：清掉 reasoning content（thinking 签名跨模型不通用）
//!   2. （未来扩展）pre-flight 校验：fallback 模型能不能装下当前 messages
//!   3. （未来扩展）cost 估算：切到 fallback 后预期支出
//!
//! 这层不感知 AgentEngine，只接受 `&mut Vec<Message>`，便于单测。
//!
//! ## 设计取舍
//!
//! 不在这里管「decide whether to switch」——那是 engine 主循环的事，因为决策
//! 依赖 step 内状态（switched_to_fallback_this_step、attempted_compact 等）。
//! 这里只做「**已经决定要切了**，把 messages 清理好交给下一轮 LLM 请求」。

use crate::llm::{ContentBlock, Message};

/// 跨模型 fallback 前调用：移除所有 `Reasoning` 块。
///
/// **为什么必须做**：reasoning_content（DeepSeek-R1 / DeepSeek-V4 thinking 模式 /
/// Anthropic extended thinking）携带 **model-specific** 的签名/格式：
///   - DeepSeek 的 reasoning_content 是纯文本，但 API 校验"必须由同一个 reasoning
///     model 在同一会话回传"
///   - Anthropic 的 thinking block 携带加密 signature，签名跨模型一律 400
///
/// 把上一个 model 的 thinking 喂给 fallback model 的结果：要么 400 silent fail
/// 要么 fallback 不识别字段静默丢——总之"切了 fallback 还是发不出去"。
///
/// **代价**：丢失 reasoning 上下文，fallback 看到的就是 user + assistant text/tool_use。
/// 这是 unavoidable cost：完成任务 > 保留 thinking。reasoning 内容仍在 events 表里
/// 完整保留，需要时可查。
///
/// **幂等**：多次调用安全（第二次没有 Reasoning 块可移除）。
pub fn strip_reasoning_blocks(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        msg.content
            .retain(|b| !matches!(b, ContentBlock::Reasoning { .. }));
    }
}

/// 一次 fallback 切换的事件 metadata。给 engine 主循环构造 system_hint 时使用。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FallbackSwitchMeta {
    pub from: String,
    pub to: String,
    /// 触发分类的 `as_str()`：`"overloaded"` / `"rate_limited"`
    pub trigger: &'static str,
    /// step 内累计切换次数（实际只会是 1，因为单 step 只切一次）
    pub switch_in_step: u32,
    /// agent 累计切换次数（跨 step）
    pub switch_total: u32,
}

impl FallbackSwitchMeta {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "model_fallback",
            "from": self.from,
            "to": self.to,
            "trigger": self.trigger,
            "switch_in_step": self.switch_in_step,
            "switch_total": self.switch_total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ContentBlock, Message, MessageRole};
    use serde_json::json;

    fn assistant(blocks: Vec<ContentBlock>) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: blocks,
            cache_control: None,
        }
    }

    fn user(blocks: Vec<ContentBlock>) -> Message {
        Message {
            role: MessageRole::User,
            content: blocks,
            cache_control: None,
        }
    }

    #[test]
    fn strip_removes_only_reasoning_keeps_others() {
        let mut msgs = vec![assistant(vec![
            ContentBlock::Reasoning {
                text: "thinking step 1...".into(),
            },
            ContentBlock::Text {
                text: "answer".into(),
            },
            ContentBlock::ToolUse {
                id: "tu_1".into(),
                name: "read_file".into(),
                input: json!({"path": "a.txt"}),
            },
            ContentBlock::Reasoning {
                text: "more thinking".into(),
            },
        ])];
        strip_reasoning_blocks(&mut msgs);
        assert_eq!(msgs[0].content.len(), 2);
        assert!(matches!(msgs[0].content[0], ContentBlock::Text { .. }));
        assert!(matches!(msgs[0].content[1], ContentBlock::ToolUse { .. }));
    }

    #[test]
    fn strip_is_idempotent() {
        let mut msgs = vec![assistant(vec![ContentBlock::Text {
            text: "no thinking here".into(),
        }])];
        let original_len = msgs[0].content.len();
        strip_reasoning_blocks(&mut msgs);
        strip_reasoning_blocks(&mut msgs);
        assert_eq!(msgs[0].content.len(), original_len);
    }

    #[test]
    fn strip_handles_empty_and_mixed_roles() {
        let mut msgs = vec![
            user(vec![ContentBlock::Text {
                text: "what's 2+2?".into(),
            }]),
            assistant(vec![
                ContentBlock::Reasoning {
                    text: "let me think".into(),
                },
                ContentBlock::Text {
                    text: "4".into(),
                },
            ]),
            user(vec![ContentBlock::ToolResult {
                tool_use_id: "tu_1".into(),
                content: "ok".into(),
                is_error: false,
            }]),
        ];
        strip_reasoning_blocks(&mut msgs);
        // User 消息原样保留；assistant Reasoning 被移除
        assert_eq!(msgs[0].content.len(), 1);
        assert_eq!(msgs[1].content.len(), 1);
        assert!(matches!(msgs[1].content[0], ContentBlock::Text { .. }));
        assert_eq!(msgs[2].content.len(), 1);
    }

    #[test]
    fn strip_assistant_with_only_reasoning_becomes_empty_message() {
        // 边界：assistant message 只有 Reasoning 块时，strip 后变成空内容
        // 调用方需要意识到这点——空 content 的 assistant message 在某些 API 会被拒
        // 但这是 caller 责任（pre-flight 时 filter 空 message），不是本函数责任
        let mut msgs = vec![assistant(vec![ContentBlock::Reasoning {
            text: "only thinking".into(),
        }])];
        strip_reasoning_blocks(&mut msgs);
        assert_eq!(msgs[0].content.len(), 0);
    }

    #[test]
    fn fallback_switch_meta_serializes_stable_keys() {
        let meta = FallbackSwitchMeta {
            from: "deepseek-v4".into(),
            to: "qwen3.5-plus".into(),
            trigger: "overloaded",
            switch_in_step: 1,
            switch_total: 3,
        };
        let json = meta.to_json();
        // 防御：key 名稳定（前端 + DB 都按这些 key 解析）
        assert_eq!(json["kind"], "model_fallback");
        assert_eq!(json["from"], "deepseek-v4");
        assert_eq!(json["to"], "qwen3.5-plus");
        assert_eq!(json["trigger"], "overloaded");
        assert_eq!(json["switch_in_step"], 1);
        assert_eq!(json["switch_total"], 3);
    }
}
