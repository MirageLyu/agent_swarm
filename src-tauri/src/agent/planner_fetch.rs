//! FM-15 FR-05.x: Planner `fetch_url` 工具的策略 + 用户确认协调器。
//!
//! 设计目标：
//! - **零默认网络访问**：除非命中 allowlist / session grant，否则 *每次* fetch
//!   都必须等用户在 UI 弹窗里确认；session 内同域可一次允许、多次复用。
//! - **明确 policy**：blocklist 永远拒绝（私有 IP / loopback / .local / .internal）；
//!   其余按 allowlist + grant + per-session 额度组合判定。
//! - **Coordinator** 用 oneshot channel 把 IPC 决策回送给阻塞的 tool call。
//!
//! 这里只负责"判定 + 执行 HTTP GET"；事件发送、IPC 在
//! `commands/planner.rs` 与 `agent/planner_engine.rs` 完成。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

/// 永远拒绝的主机模式（不区分大小写、点号边界匹配）。
/// 设计原则：阻断典型的 SSRF 与内网探测。
const HOST_BLOCKLIST_SUFFIXES: &[&str] =
    &[".local", ".internal", ".lan", ".intranet", ".corp", ".home"];

/// 单次 fetch 响应大小上限（字节），防止 LLM context 被巨型页面塞爆。
pub const MAX_FETCH_BYTES: usize = 256 * 1024;

/// HTTP 请求超时
pub const FETCH_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// 等用户在 UI 上点确认的最长时间——超时即视为 deny。
pub const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(120);

/// 用户对一次 fetch 请求的决定。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FetchDecision {
    /// 仅允许这一次，不写 grant
    AllowOnce,
    /// 同 session 内同 host 全部允许，写入 `planner_session_fetch_grants`
    AllowSession,
    /// 拒绝
    Deny,
}

#[derive(Debug, Clone)]
pub struct FetchPolicy {
    /// 主机后缀白名单（大小写无关），命中即直接允许。
    pub allowlist_hosts: Vec<String>,
    /// 单 session 的 fetch_url 调用次数上限（含被拒绝的、含 confirmation 失败的）。
    pub max_per_session: u32,
}

impl FetchPolicy {
    pub fn from_app_config(cfg: &crate::commands::AppConfig) -> Self {
        Self {
            allowlist_hosts: cfg
                .planner_fetch_allowlist
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
            max_per_session: cfg.planner_max_fetches_per_session,
        }
    }

    pub fn is_allowlisted(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        self.allowlist_hosts
            .iter()
            .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
    }
}

#[derive(thiserror::Error, Debug)]
pub enum FetchError {
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    #[error("scheme `{0}` is not allowed; only http(s) are accepted")]
    UnsupportedScheme(String),
    #[error("host `{0}` is on the planner fetch blocklist (private/internal name)")]
    HostBlocked(String),
    #[error("user denied fetching `{0}`")]
    UserDenied(String),
    #[error("user did not confirm fetching `{0}` within {timeout}s", timeout = CONFIRMATION_TIMEOUT.as_secs())]
    ConfirmationTimeout(String),
    #[error("fetch budget exceeded: {used}/{cap} fetches used in this planner session")]
    BudgetExceeded { used: u32, cap: u32 },
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("response too large (> {} bytes)", MAX_FETCH_BYTES)]
    ResponseTooLarge,
    #[error("unsupported content-type `{0}`; only text/* and application/json are accepted")]
    UnsupportedContentType(String),
    #[error("HTTP {status} from `{url}`")]
    BadStatus { status: u16, url: String },
    #[error("internal: {0}")]
    Internal(String),
}

/// 解析输入 URL 并做基础 sanity check。
/// 返回 `(host, normalized_url_string)`。
pub fn parse_and_check_host(raw: &str) -> Result<(String, String), FetchError> {
    let parsed = reqwest::Url::parse(raw).map_err(|e| FetchError::InvalidUrl(e.to_string()))?;
    let scheme = parsed.scheme().to_string();
    if scheme != "http" && scheme != "https" {
        return Err(FetchError::UnsupportedScheme(scheme));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| FetchError::InvalidUrl(format!("no host in `{raw}`")))?
        .to_lowercase();

    if is_blocklisted_host(&host) {
        return Err(FetchError::HostBlocked(host));
    }

    Ok((host, parsed.to_string()))
}

/// 判断主机名是否落在私有 / 内网阻断名单。包括：
/// - `localhost`、`127.0.0.0/8`、`::1`、私有 IPv4 段、IPv4 link-local
/// - 上面 `HOST_BLOCKLIST_SUFFIXES` 列举的 mDNS / 内网域后缀
fn is_blocklisted_host(host: &str) -> bool {
    let h = host.to_lowercase();
    if h == "localhost" || h == "ip6-localhost" {
        return true;
    }
    for suffix in HOST_BLOCKLIST_SUFFIXES {
        if h.ends_with(suffix) {
            return true;
        }
    }
    if let Ok(addr) = h.parse::<std::net::IpAddr>() {
        return is_private_or_loopback_ip(&addr);
    }
    false
}

fn is_private_or_loopback_ip(addr: &std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_documentation()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // unique-local fc00::/7 + link-local fe80::/10 — checked manually since stdlib
                // accessors are still unstable on Rust 1.78
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// 待用户确认的 fetch 请求。Coordinator 在 IPC `confirm_planner_fetch` 触发时
/// 用 request_id 找回 oneshot::Sender。
#[derive(Debug, Clone, Serialize)]
pub struct PendingFetchRequest {
    pub request_id: String,
    pub session_id: String,
    pub url: String,
    pub host: String,
}

/// 全局 coordinator——`tauri::manage` 注册为 State。每个 PlannerEngine
/// run 都通过它注册 / 解析 confirmation 请求。
#[derive(Default)]
pub struct PlannerFetchCoordinator {
    pending: Mutex<HashMap<String, oneshot::Sender<FetchDecision>>>,
}

impl PlannerFetchCoordinator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Tool 侧：注册一个等待槽，返回 `(request_id, receiver)`。
    pub async fn register(&self) -> (String, oneshot::Receiver<FetchDecision>) {
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id.clone(), tx);
        (request_id, rx)
    }

    /// IPC 侧：把用户决定送回。返回 false 表示 request_id 不存在 / 已超时。
    pub async fn resolve(&self, request_id: &str, decision: FetchDecision) -> bool {
        let tx = self.pending.lock().await.remove(request_id);
        match tx {
            Some(sender) => sender.send(decision).is_ok(),
            None => false,
        }
    }

    /// 清理：超时或 PlannerEngine 异常退出时。
    pub async fn forget(&self, request_id: &str) {
        self.pending.lock().await.remove(request_id);
    }
}

/// 真正去 HTTP GET，做 size + content-type 守卫。
pub async fn http_fetch(url: &str) -> Result<String, FetchError> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_HTTP_TIMEOUT)
        .user_agent(concat!("Miragenty-Planner/", env!("CARGO_PKG_VERSION")))
        // 主动禁用 cookies / redirect-to-private:
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|e| FetchError::Internal(e.to_string()))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(FetchError::BadStatus {
            status: status.as_u16(),
            url: url.to_string(),
        });
    }

    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let allowed = ct.starts_with("text/")
        || ct.starts_with("application/json")
        || ct.starts_with("application/xml")
        || ct.starts_with("application/xhtml")
        || ct.starts_with("application/yaml")
        || ct.starts_with("application/x-yaml");
    if !allowed && !ct.is_empty() {
        return Err(FetchError::UnsupportedContentType(ct));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    if bytes.len() > MAX_FETCH_BYTES {
        return Err(FetchError::ResponseTooLarge);
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| FetchError::Internal("non-utf8 body".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_exact_and_subdomain() {
        let p = FetchPolicy {
            allowlist_hosts: vec!["example.com".into(), "rust-lang.org".into()],
            max_per_session: 5,
        };
        assert!(p.is_allowlisted("example.com"));
        assert!(p.is_allowlisted("docs.example.com"));
        assert!(p.is_allowlisted("Docs.Example.com"));
        assert!(p.is_allowlisted("doc.rust-lang.org"));
        assert!(!p.is_allowlisted("evil-example.com")); // not subdomain
        assert!(!p.is_allowlisted("notexample.com"));
        assert!(!p.is_allowlisted("google.com"));
    }

    #[test]
    fn blocklist_localhost_and_private_ip() {
        for bad in [
            "localhost",
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.1.1",
            "fe80::1",
            "fc00::1",
            "::1",
            "0.0.0.0",
            "router.lan",
            "service.internal",
            "host.local",
            "x.corp",
        ] {
            assert!(is_blocklisted_host(bad), "should block `{bad}`");
        }
    }

    #[test]
    fn blocklist_allows_public() {
        for ok in [
            "example.com",
            "8.8.8.8",
            "github.com",
            "raw.githubusercontent.com",
            "1.1.1.1",
        ] {
            assert!(!is_blocklisted_host(ok), "should allow `{ok}`");
        }
    }

    #[test]
    fn parse_rejects_non_http_schemes() {
        let cases = [
            ("ftp://example.com/x", "scheme"),
            ("file:///etc/passwd", "scheme"),
            ("javascript:alert(1)", "scheme"),
        ];
        for (url, kind) in cases {
            let err = parse_and_check_host(url).unwrap_err();
            match kind {
                "scheme" => assert!(
                    matches!(err, FetchError::UnsupportedScheme(_)),
                    "{url}: {err}"
                ),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn parse_blocks_localhost_url() {
        let err = parse_and_check_host("http://localhost:8080/x").unwrap_err();
        assert!(matches!(err, FetchError::HostBlocked(_)), "{err}");
    }

    #[test]
    fn parse_invalid_url() {
        let err = parse_and_check_host("not a url").unwrap_err();
        assert!(matches!(err, FetchError::InvalidUrl(_)), "{err}");
    }

    #[tokio::test]
    async fn coordinator_resolves_pending_request() {
        let coord = PlannerFetchCoordinator::new();
        let coord2 = coord.clone();
        let (rid, rx) = coord.register().await;
        let rid2 = rid.clone();
        let resolver = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            coord2.resolve(&rid2, FetchDecision::AllowSession).await
        });
        let decision = rx.await.unwrap();
        assert_eq!(decision, FetchDecision::AllowSession);
        assert!(resolver.await.unwrap());

        // resolve unknown → false
        assert!(!coord.resolve("nonexistent", FetchDecision::Deny).await);
    }

    #[tokio::test]
    async fn coordinator_forget_drops_sender() {
        let coord = PlannerFetchCoordinator::new();
        let (rid, rx) = coord.register().await;
        coord.forget(&rid).await;
        // After forget, awaiting receiver yields RecvError
        assert!(rx.await.is_err());
    }
}
