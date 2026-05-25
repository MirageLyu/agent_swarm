//! Stream-level 诊断工具：当 LLM 流出现长沉默时，提供"四个假说判别证据"。
//!
//! 已知 stall 故障假说：
//!
//! | 假说 | 区分性证据 |
//! |---|---|
//! | A. 本地网络抖动     | stall 时同 host 独立连接探测**也慢/失败** |
//! | B. 多 stream 抢带宽 | 同时刻有多个 stream，全部 throughput 都低 |
//! | C. reseller 上游卡  | 探测连接快但主 stream 慢                  |
//! | D. 进程内 socket 调度 | 探测也慢但和 A 区分需外部 baseline      |
//!
//! 本模块提供：
//! 1. **`StreamRegistry`**：全局活跃 stream 表，stall 时一次性 dump 所有 stream
//!    的吞吐快照——直接证伪/证实假说 B
//! 2. **`StreamStats`**：单个 stream 的滚动计数（bytes / chunks / last_chunk_at），
//!    供 registry dump 和 in-process throughput 监控使用
//! 3. **`probe_endpoint`**：独立 HTTP 探测，stall 时打到同 host 测 RTT——
//!    直接证伪/证实假说 A/C
//!
//! 故意做得**纯诊断、零侧效**：所有调用都通过 `tracing` 输出，不影响主路径行为。
//! 触发频率受调用方控制（stream_guard 仅在 stall 检测点触发一次），不会轰日志。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 单个活跃 stream 的滚动计数。所有字段 `AtomicU64`，可在 forwarder 任务里更新，
/// 在 watchdog 任务里读快照——无锁，零阻塞。
#[derive(Debug)]
pub struct StreamStats {
    /// stream 启动时刻（用于计算 `running_for_ms`）
    pub started_at: Instant,
    /// 自 `started_at` 起，最后一次收到 SSE 字节的毫秒数。0 = 还没收到。
    pub last_byte_ms: AtomicU64,
    /// 累计 SSE 字节
    pub total_bytes: AtomicU64,
    /// 累计抵达的 chunk（已解析的 SSE event）
    pub total_chunks: AtomicU64,
    /// 累计 reasoning_content 字符
    pub reasoning_chars: AtomicU64,
    /// 累计 content 字符
    pub text_chars: AtomicU64,
    /// 仅用于日志可读性
    pub provider: String,
    pub model: String,
    pub endpoint: String,
}

impl StreamStats {
    pub fn new(provider: String, model: String, endpoint: String) -> Arc<Self> {
        Arc::new(Self {
            started_at: Instant::now(),
            last_byte_ms: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            total_chunks: AtomicU64::new(0),
            reasoning_chars: AtomicU64::new(0),
            text_chars: AtomicU64::new(0),
            provider,
            model,
            endpoint,
        })
    }

    /// 每收到一段 SSE 字节就调一次；`last_byte_ms` 用于 watchdog 判定 idle。
    pub fn record_bytes(&self, n: u64) {
        let elapsed = self.started_at.elapsed().as_millis() as u64;
        self.last_byte_ms.store(elapsed, Ordering::Relaxed);
        self.total_bytes.fetch_add(n, Ordering::Relaxed);
    }

    /// 每个 chunk 抵达时调用一次，**一站式**更新 chunks / bytes / chars 与
    /// `last_byte_ms`。
    ///
    /// 早先版本只更新 chunks 与 chars，没碰 `last_byte_ms` / `total_bytes`，
    /// 结果 stall_diagnostic dump 出来的 `idle_ms` 其实等于 `running_for_ms`
    /// （`last_byte_ms` 永远停在 0），`total_bytes` 永远 0——dd68c400 案例里
    /// 实际收了 600+KB 但诊断输出 `total_bytes=0 bytes_per_sec=0`，严重误导排查。
    ///
    /// 这里拿 chunk 解析后的 content 长度作为 byte 维度——和 raw SSE 字节略有差距
    /// （`data: ` 前缀、JSON 框架开销不计），但对"诊断 idle/吞吐"这个目的足够，
    /// 而且只在 forwarder 一处调用，比让 provider 层各自管理更不易漏。
    pub fn record_chunk(&self, kind_text_len: u64, kind_reasoning_len: u64) {
        let elapsed = self.started_at.elapsed().as_millis() as u64;
        self.last_byte_ms.store(elapsed, Ordering::Relaxed);
        self.total_chunks.fetch_add(1, Ordering::Relaxed);
        let content_len = kind_text_len + kind_reasoning_len;
        if content_len > 0 {
            self.total_bytes.fetch_add(content_len, Ordering::Relaxed);
        }
        if kind_text_len > 0 {
            self.text_chars.fetch_add(kind_text_len, Ordering::Relaxed);
        }
        if kind_reasoning_len > 0 {
            self.reasoning_chars
                .fetch_add(kind_reasoning_len, Ordering::Relaxed);
        }
    }

    /// 拍一份当前状态用于日志。
    pub fn snapshot(&self) -> StatsSnapshot {
        let running_for_ms = self.started_at.elapsed().as_millis() as u64;
        let last_byte_ms = self.last_byte_ms.load(Ordering::Relaxed);
        let idle_ms = if last_byte_ms == 0 {
            running_for_ms
        } else {
            running_for_ms.saturating_sub(last_byte_ms)
        };
        StatsSnapshot {
            provider: self.provider.clone(),
            model: self.model.clone(),
            endpoint: self.endpoint.clone(),
            running_for_ms,
            idle_ms,
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
            total_chunks: self.total_chunks.load(Ordering::Relaxed),
            reasoning_chars: self.reasoning_chars.load(Ordering::Relaxed),
            text_chars: self.text_chars.load(Ordering::Relaxed),
        }
    }
}

/// `StreamStats::snapshot()` 的瞬时拷贝，可安全跨线程传递并 `Debug` 打印。
#[derive(Debug, Clone)]
pub struct StatsSnapshot {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub running_for_ms: u64,
    pub idle_ms: u64,
    pub total_bytes: u64,
    pub total_chunks: u64,
    pub reasoning_chars: u64,
    pub text_chars: u64,
}

/// 全局活跃 stream 注册表。stall 触发时遍历所有条目 dump 状态——这是分辨"假说 B
/// 多 stream 抢带宽"vs"假说 C 单 stream 卡死"的关键证据。
pub struct StreamRegistry {
    inner: Mutex<HashMap<u64, Arc<StreamStats>>>,
    next_id: AtomicU64,
}

impl StreamRegistry {
    pub fn global() -> &'static StreamRegistry {
        static REG: OnceLock<StreamRegistry> = OnceLock::new();
        REG.get_or_init(|| StreamRegistry {
            inner: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
    }

    /// 注册一个 stream，返回 `RegistrationGuard`——drop 时自动注销。
    /// 用 RAII 而非 explicit unregister，确保任何路径退出（panic / abort / cancel）
    /// 都不会泄漏 ghost 条目。
    pub fn register(&self, stats: Arc<StreamStats>) -> RegistrationGuard {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.inner.lock().expect("StreamRegistry mutex poisoned");
        guard.insert(id, stats);
        RegistrationGuard { id }
    }

    /// 拍一份当前所有活跃 stream 的快照。
    pub fn snapshot_all(&self) -> Vec<(u64, StatsSnapshot)> {
        let guard = self.inner.lock().expect("StreamRegistry mutex poisoned");
        guard.iter().map(|(id, s)| (*id, s.snapshot())).collect()
    }

    pub fn active_count(&self) -> usize {
        self.inner
            .lock()
            .expect("StreamRegistry mutex poisoned")
            .len()
    }

    fn unregister(&self, id: u64) {
        let mut guard = self.inner.lock().expect("StreamRegistry mutex poisoned");
        guard.remove(&id);
    }
}

pub struct RegistrationGuard {
    id: u64,
}

impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        StreamRegistry::global().unregister(self.id);
    }
}

/// In-process probe 结果。
#[derive(Debug)]
pub struct ProbeOutcome {
    pub url: String,
    /// TCP + TLS 完成时间（reqwest 的 send + first byte）
    pub elapsed_ms: u64,
    /// HTTP 状态码（任何 < 500 都视为"网络层正常"——401/404 也算通）
    pub status: Option<u16>,
    /// 错误信息（连接失败/超时时填充）
    pub error: Option<String>,
}

impl ProbeOutcome {
    /// "网络层是否健康"。stall 时打这条 probe，若 healthy=true → 排除假说 A/C
    /// （网络通），故障在 stream 特定连接 → 假说 D/B；若 healthy=false → 假说 A/C
    /// （网络/上游问题），不是单 stream 特异的。
    pub fn is_network_healthy(&self) -> bool {
        match (self.status, &self.error) {
            (Some(s), _) if s < 500 => true,
            _ => false,
        }
    }
}

/// 对 `endpoint` 同 host 发一次轻量 GET（默认走根路径 `/`），用**全新**的 reqwest
/// Client 避免连接复用——这是验证"网络通不通"的关键，不能借用主 stream 的 client，
/// 否则同一 connection pool 卡住会导致探测也卡，无法区分。
///
/// 超时设 8s：网络正常时 RTT < 1s 就够，给 7s 余量；网络不通时 8s 内必失败。
pub async fn probe_endpoint(endpoint_url: &str) -> ProbeOutcome {
    // 构造同 host 探测 URL：取 endpoint 的 scheme://host[:port]/，避免触发实际接口逻辑
    let probe_url = match url_origin(endpoint_url) {
        Some(origin) => origin,
        None => endpoint_url.to_string(),
    };

    let start = Instant::now();
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        // 不要连接复用——每次探测都新建 socket，避免和主 stream 共用 keep-alive
        // 让结果失真。
        .pool_max_idle_per_host(0)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ProbeOutcome {
                url: probe_url,
                elapsed_ms: start.elapsed().as_millis() as u64,
                status: None,
                error: Some(format!("client build failed: {e}")),
            };
        }
    };

    match client.get(&probe_url).send().await {
        Ok(resp) => ProbeOutcome {
            url: probe_url,
            elapsed_ms: start.elapsed().as_millis() as u64,
            status: Some(resp.status().as_u16()),
            error: None,
        },
        Err(e) => ProbeOutcome {
            url: probe_url,
            elapsed_ms: start.elapsed().as_millis() as u64,
            status: None,
            error: Some(e.to_string()),
        },
    }
}

/// 抽 `https://api.openbitfun.com/v1/chat/completions` 的 origin 部分
/// → `https://api.openbitfun.com/`。用最朴素的 split 而非引 url crate
/// 是为了避免新增依赖；本函数仅诊断用，对极端 URL 失败时调用方会 fallback 到原 URL。
fn url_origin(full: &str) -> Option<String> {
    let scheme_end = full.find("://")?;
    let after_scheme = &full[scheme_end + 3..];
    let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    Some(format!(
        "{}://{}/",
        &full[..scheme_end],
        &after_scheme[..host_end]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_origin_basic() {
        assert_eq!(
            url_origin("https://api.openbitfun.com/v1/chat/completions"),
            Some("https://api.openbitfun.com/".to_string())
        );
        assert_eq!(
            url_origin("http://localhost:8080/api/v1/foo"),
            Some("http://localhost:8080/".to_string())
        );
        assert_eq!(
            url_origin("https://example.com"),
            Some("https://example.com/".to_string())
        );
        assert_eq!(url_origin("not a url"), None);
    }

    #[test]
    fn registry_register_and_unregister() {
        let reg = StreamRegistry::global();
        let baseline = reg.active_count();
        let stats = StreamStats::new("p".into(), "m".into(), "https://e.com/x".into());
        {
            let _g = reg.register(stats);
            assert_eq!(reg.active_count(), baseline + 1);
        }
        assert_eq!(reg.active_count(), baseline, "guard drop 应自动注销");
    }

    #[test]
    fn stats_snapshot_tracks_bytes_and_idle() {
        let s = StreamStats::new("p".into(), "m".into(), "https://e.com/x".into());
        s.record_bytes(100);
        s.record_chunk(0, 30);
        let snap = s.snapshot();
        // 100 from record_bytes + 30 from record_chunk(0, 30) (reasoning bytes)
        assert_eq!(snap.total_bytes, 130);
        assert_eq!(snap.total_chunks, 1);
        assert_eq!(snap.reasoning_chars, 30);
        assert!(snap.idle_ms < 50, "刚 record 完，idle 应近 0");
    }

    /// **回归测试**（针对 dd68c400 诊断输出 `total_bytes=0 idle_ms≈running_for_ms`）：
    /// `record_chunk` 必须同时更新 `last_byte_ms` 与 `total_bytes`，
    /// 否则 stall 诊断会显示"全零"，严重误导排查方向（看起来像网络挂了，
    /// 实际 reseller 流早就吐了 600KB+）。
    #[test]
    fn record_chunk_alone_updates_last_byte_and_total_bytes() {
        let s = StreamStats::new("p".into(), "m".into(), "https://e.com/x".into());
        // 故意只调 record_chunk，不调 record_bytes —— 模拟 forwarder 路径
        for _ in 0..5 {
            s.record_chunk(0, 50);
        }
        let snap = s.snapshot();
        assert_eq!(snap.total_chunks, 5);
        assert_eq!(snap.reasoning_chars, 250);
        assert_eq!(
            snap.total_bytes, 250,
            "record_chunk 必须把 chars 累计到 total_bytes，否则 stall_diagnostic 永远 0"
        );
        assert!(
            snap.idle_ms < 50,
            "record_chunk 必须更新 last_byte_ms，否则 idle_ms 等于 running_for_ms 误报 stall"
        );
    }

    #[test]
    fn probe_outcome_network_health_classification() {
        let healthy = ProbeOutcome {
            url: "x".into(),
            elapsed_ms: 100,
            status: Some(401),
            error: None,
        };
        assert!(healthy.is_network_healthy(), "401 也算网络通");

        let down = ProbeOutcome {
            url: "x".into(),
            elapsed_ms: 8000,
            status: None,
            error: Some("timeout".into()),
        };
        assert!(!down.is_network_healthy(), "无响应必判不健康");

        let server_err = ProbeOutcome {
            url: "x".into(),
            elapsed_ms: 100,
            status: Some(503),
            error: None,
        };
        assert!(!server_err.is_network_healthy(), "5xx 算上游有问题");
    }
}
