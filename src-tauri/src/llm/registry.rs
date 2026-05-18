use super::types::ModelCapabilities;

/// Built-in model capability configurations.
/// Unknown models get a safe default (all capabilities false except streaming).
pub fn get_capabilities(provider: &str, model: &str) -> ModelCapabilities {
    let lower_model = model.to_lowercase();

    match provider {
        "dashscope" => {
            if lower_model.contains("qwen3.5") || lower_model.contains("qwen-plus") {
                ModelCapabilities {
                    supports_thinking: false,
                    supports_tool_use: true,
                    supports_prompt_caching: true,
                    supports_prefill: false,
                    supports_streaming: true,
                    supports_parallel_tools: true,
                    supports_logprobs: false,
                    thinking_api_param: None,
                    cache_control_syntax: Some("anthropic".into()),
                    context_window: 131072,
                }
            } else if lower_model.contains("qwen3") {
                ModelCapabilities {
                    supports_thinking: true,
                    supports_tool_use: true,
                    supports_prompt_caching: true,
                    supports_prefill: false,
                    supports_streaming: true,
                    supports_parallel_tools: true,
                    supports_logprobs: false,
                    thinking_api_param: Some("enable_thinking".into()),
                    cache_control_syntax: Some("anthropic".into()),
                    context_window: 131072,
                }
            } else {
                dashscope_default()
            }
        }
        "anthropic" => {
            if lower_model.contains("claude") && lower_model.contains("sonnet") {
                ModelCapabilities {
                    supports_thinking: true,
                    supports_tool_use: true,
                    supports_prompt_caching: true,
                    supports_prefill: true,
                    supports_streaming: true,
                    supports_parallel_tools: true,
                    supports_logprobs: false,
                    thinking_api_param: Some("thinking.type".into()),
                    cache_control_syntax: Some("anthropic".into()),
                    context_window: 200000,
                }
            } else {
                ModelCapabilities {
                    supports_thinking: false,
                    supports_tool_use: true,
                    supports_prompt_caching: true,
                    supports_prefill: true,
                    supports_streaming: true,
                    supports_parallel_tools: true,
                    supports_logprobs: false,
                    thinking_api_param: None,
                    cache_control_syntax: Some("anthropic".into()),
                    context_window: 200000,
                }
            }
        }
        "openai" => ModelCapabilities {
            supports_thinking: false,
            supports_tool_use: true,
            supports_prompt_caching: true,
            supports_prefill: false,
            supports_streaming: true,
            supports_parallel_tools: true,
            supports_logprobs: true,
            thinking_api_param: None,
            cache_control_syntax: Some("auto".into()),
            context_window: 128000,
        },
        "deepseek" => {
            if lower_model.contains("r1") {
                ModelCapabilities {
                    supports_thinking: true,
                    supports_tool_use: true,
                    supports_prompt_caching: true,
                    supports_prefill: false,
                    supports_streaming: true,
                    supports_parallel_tools: false,
                    supports_logprobs: false,
                    thinking_api_param: Some("enable_thinking".into()),
                    cache_control_syntax: Some("auto".into()),
                    context_window: 65536,
                }
            } else if lower_model.contains("v4") {
                // 2026-04 发布的 DeepSeek V4 系列：1M context；hybrid attention；
                // OpenAI-compat surface；支持 thinking/non-thinking 模式 + 并行工具调用。
                // flash 与 pro 共享相同 capability set，按 model name 区分模型大小不影响 surface。
                ModelCapabilities {
                    supports_thinking: true,
                    supports_tool_use: true,
                    supports_prompt_caching: true,
                    supports_prefill: false,
                    supports_streaming: true,
                    supports_parallel_tools: true,
                    supports_logprobs: false,
                    thinking_api_param: Some("enable_thinking".into()),
                    cache_control_syntax: Some("auto".into()),
                    context_window: 1_000_000,
                }
            } else {
                ModelCapabilities {
                    supports_thinking: false,
                    supports_tool_use: true,
                    supports_prompt_caching: true,
                    supports_prefill: false,
                    supports_streaming: true,
                    supports_parallel_tools: true,
                    supports_logprobs: false,
                    thinking_api_param: None,
                    cache_control_syntax: Some("auto".into()),
                    context_window: 65536,
                }
            }
        }
        _ => ModelCapabilities::default(),
    }
}

fn dashscope_default() -> ModelCapabilities {
    ModelCapabilities {
        supports_thinking: false,
        supports_tool_use: true,
        supports_prompt_caching: true,
        supports_prefill: false,
        supports_streaming: true,
        supports_parallel_tools: false,
        supports_logprobs: false,
        thinking_api_param: None,
        cache_control_syntax: Some("anthropic".into()),
        context_window: 32768,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ut_10_3_7a_qwen35_plus() {
        let caps = get_capabilities("dashscope", "qwen3.5-plus");
        assert!(caps.supports_tool_use);
        assert!(!caps.supports_thinking);
        assert!(caps.supports_prompt_caching);
    }

    #[test]
    fn ut_10_3_7b_qwen3_thinking() {
        let caps = get_capabilities("dashscope", "qwen3");
        assert!(caps.supports_thinking);
        assert_eq!(caps.thinking_api_param, Some("enable_thinking".into()));
    }

    #[test]
    fn ut_10_3_7c_unknown_model() {
        let caps = get_capabilities("unknown", "unknown-model");
        assert!(!caps.supports_tool_use);
        assert!(!caps.supports_thinking);
        assert!(!caps.supports_prompt_caching);
    }

    #[test]
    fn claude_sonnet() {
        let caps = get_capabilities("anthropic", "claude-4-sonnet");
        assert!(caps.supports_thinking);
        assert!(caps.supports_prefill);
        assert!(caps.supports_prompt_caching);
    }

    #[test]
    fn deepseek_r1() {
        let caps = get_capabilities("deepseek", "deepseek-r1");
        assert!(caps.supports_thinking);
        assert!(!caps.supports_prefill);
    }
}
