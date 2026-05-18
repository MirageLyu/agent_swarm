//! 复现 Tauri agent 用 reqwest 调用 Bitfun reseller 时 ~9s 后 RST 的问题。
//!
//! 与生产 `OpenAICompatProvider::stream_chat` 使用**相同的 reqwest Client 配置**，
//! 但只发一次请求、详细打印每个 SSE chunk 的抵达节奏，便于直接观察故障点。
//!
//! 用法：
//!     cd src-tauri && BITFUN_KEY=... cargo run --example probe_stream
//!
//! 可选环境变量：
//!     PROBE_HTTP1=1     强制 HTTP/1.1（禁掉 ALPN HTTP/2 协商）
//!     PROBE_NO_PROXY=1  绕过系统代理
//!     PROBE_PAYLOAD=tiny|step6_heavy  选预设请求体

use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use reqwest::Client;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("BITFUN_KEY").context("BITFUN_KEY env required")?;
    let payload_name = std::env::var("PROBE_PAYLOAD").unwrap_or_else(|_| "step6_heavy".to_string());
    let force_http1 = std::env::var("PROBE_HTTP1").is_ok();
    let no_proxy = std::env::var("PROBE_NO_PROXY").is_ok();

    let mut builder = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(1800));
    if force_http1 {
        builder = builder.http1_only();
    }
    if no_proxy {
        builder = builder.no_proxy();
    }
    let client = builder.build()?;

    let body = build_body(&payload_name)?;
    let body_str = serde_json::to_string(&body)?;

    println!(
        "[+0ms] config: force_http1={force_http1} no_proxy={no_proxy} body_bytes={} payload={payload_name}",
        body_str.len()
    );

    let started = Instant::now();
    let resp = client
        .post("https://api.openbitfun.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .body(body_str)
        .send()
        .await?;

    let connect_ms = started.elapsed().as_millis();
    let status = resp.status();
    let version = format!("{:?}", resp.version());
    println!("[+{connect_ms}ms] HTTP connected: status={status} version={version}");

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("HTTP error {status}: {}", &text[..text.len().min(500)]));
    }

    let mut stream = resp.bytes_stream();
    let mut buffer = Vec::<u8>::new();
    let mut total_bytes: u64 = 0;
    let mut events: u64 = 0;
    let mut last_byte_at: Option<Instant> = None;
    let mut first_byte_at: Option<Instant> = None;

    while let Some(item) = stream.next().await {
        let now = Instant::now();
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match item {
            Ok(chunk) => {
                if first_byte_at.is_none() {
                    first_byte_at = Some(now);
                    println!("[+{elapsed_ms}ms] first SSE byte ({} bytes)", chunk.len());
                }
                if let Some(prev) = last_byte_at {
                    let gap = now.duration_since(prev).as_millis();
                    if gap >= 5_000 {
                        println!(
                            "[+{elapsed_ms}ms] !!! BYTE GAP {gap}ms after {total_bytes}B"
                        );
                    }
                }
                last_byte_at = Some(now);
                total_bytes += chunk.len() as u64;
                buffer.extend_from_slice(&chunk);

                // 提取完整 SSE event 数（通过 \n\n 分隔）
                while let Some(pos) = buffer.windows(2).position(|w| w == b"\n\n") {
                    let line = String::from_utf8_lossy(&buffer[..pos]).to_string();
                    buffer.drain(..pos + 2);
                    if line.starts_with("data: ") {
                        events += 1;
                        if events <= 5 || events % 50 == 0 {
                            let preview: String = line.chars().take(120).collect();
                            println!("[+{elapsed_ms}ms] event#{events}: {}", preview);
                        }
                        if line.contains("[DONE]") {
                            println!("[+{elapsed_ms}ms] [DONE] received");
                        }
                    }
                }
            }
            Err(e) => {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                println!(
                    "[+{elapsed_ms}ms] !!! STREAM ERROR after {total_bytes}B / {events} events: {e}"
                );
                println!("    is_decode={} is_request={} is_timeout={} is_connect={} is_body={}",
                    e.is_decode(), e.is_request(), e.is_timeout(), e.is_connect(), e.is_body());
                if let Some(src) = std::error::Error::source(&e) {
                    println!("    source: {src}");
                    if let Some(src2) = std::error::Error::source(src) {
                        println!("    source^2: {src2}");
                    }
                }
                return Err(anyhow!("stream broke after {} bytes / {} events / {}ms", total_bytes, events, elapsed_ms));
            }
        }
    }

    let total_ms = started.elapsed().as_millis();
    println!("[+{total_ms}ms] stream finished cleanly: {total_bytes} bytes, {events} events");
    Ok(())
}

fn build_body(name: &str) -> Result<serde_json::Value> {
    let messages = match name {
        "tiny" => json!([{"role": "user", "content": "Say hello in 3 words."}]),
        "step6_heavy" => {
            // 模拟实际 step 5 之前的 8 messages context；故意填充让 input 接近 5K tokens
            let filler = "Section detail: cover architecture, components, state, data, UI, engine, tests, build. ";
            json!([
                {"role": "system", "content": format!("You are an autonomous coding agent. Use tools to complete tasks.\n{}", filler.repeat(60))},
                {"role": "user", "content": format!("Create docs/design/calculator-architecture.md with 8 sections, ~3000 words. {}", filler.repeat(20))},
                {"role": "assistant", "content": "Examining workspace and planning."},
                {"role": "user", "content": format!("[tool_result] list_files: {}", filler.repeat(5))},
                {"role": "assistant", "content": "Setting up plan and todos."},
                {"role": "user", "content": "[tool_result] enter_plan_mode ok\n[tool_result] todo_write ok"},
                {"role": "assistant", "content": "Now creating directory."},
                {"role": "user", "content": "[tool_result] mkdir docs/design ok\n\nContinue: now write the calculator-architecture.md file with all 8 sections, ~3000 words. Use write_file tool."},
            ])
        }
        other => return Err(anyhow!("unknown payload {other}")),
    };
    Ok(json!({
        "model": "deepseek-v4-pro",
        "messages": messages,
        "max_tokens": 16384,
        "stream": true,
        "stream_options": {"include_usage": true},
    }))
}
