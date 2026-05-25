//! Stable, i18n-friendly error codes returned by Tauri commands.
//!
//! # Why
//!
//! Pre-i18n, commands returned `Result<_, String>` where the `String` was a
//! human-readable English (or Chinese) message. The frontend just rendered it.
//! That couples backend to UI language and prevents the user picking a UI
//! language different from the one the message was hard-coded in.
//!
//! # Design
//!
//! - This module defines stable string codes, e.g. `"error.no_api_key"`.
//! - `IpcError::to_string()` returns a JSON envelope:
//!   `{"code":"error.no_api_key","params":{"provider":"openai"},"detail":"..."}`
//!   (always parseable on the frontend).
//! - The frontend has a thin `formatBackendError(e)` helper that:
//!     1. tries `JSON.parse(e)`. If it has a `code`, look it up in the
//!        `errors` namespace via `t(code, params)`.
//!     2. otherwise, falls back to the raw string (legacy commands still
//!        returning bare strings continue to work).
//!
//! This is **opt-in per command** — existing `Result<T, String>` returns keep
//! working unchanged. New / migrated commands should return
//! `Result<T, IpcError>`. This avoids a big-bang change.
//!
//! # Adding a new error code
//!
//! 1. Add a constant + builder function below
//! 2. Add the same key under the `errors` namespace in
//!    `src/i18n/locales/en-US.json` and `zh-CN.json`
//! 3. Use `IpcError::no_api_key("openai")` from the command, e.g.:
//!    `return Err(IpcError::no_api_key("openai").into());`

use serde::Serialize;
use serde_json::{json, Value};

/// Wire format passed back to the frontend in `Err`.
///
/// The Display impl produces a one-line JSON string, so commands that already
/// `?`-bubble into `String` with `.map_err(|e| e.to_string())` keep working
/// transparently — the JSON envelope just rides along the existing `String`
/// channel without needing a new IPC contract.
#[derive(Debug, Clone, Serialize)]
pub struct IpcError {
    pub code: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
    /// Optional raw detail string (debug log, stack trace, original error).
    /// Kept for support purposes; the UI may show it in a "details" disclosure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl IpcError {
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            params: Value::Null,
            detail: None,
        }
    }

    pub fn with_param(mut self, key: &str, value: impl Into<Value>) -> Self {
        if !self.params.is_object() {
            self.params = json!({});
        }
        if let Some(map) = self.params.as_object_mut() {
            map.insert(key.to_string(), value.into());
        }
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    // --- High-frequency builders ---
    // Prefer these over `IpcError::new(...)` so codes stay grep-able.

    pub fn no_api_key(provider: impl Into<Value>) -> Self {
        Self::new("error.no_api_key").with_param("provider", provider.into())
    }

    pub fn mission_not_found(id: impl Into<Value>) -> Self {
        Self::new("error.mission_not_found").with_param("id", id.into())
    }

    pub fn agent_not_found(id: impl Into<Value>) -> Self {
        Self::new("error.agent_not_found").with_param("id", id.into())
    }

    pub fn task_not_found(id: impl Into<Value>) -> Self {
        Self::new("error.task_not_found").with_param("id", id.into())
    }

    pub fn approval_expired() -> Self {
        Self::new("error.approval_expired")
    }

    pub fn approval_already_resolved() -> Self {
        Self::new("error.approval_already_resolved")
    }

    pub fn workspace_invalid(reason: impl Into<Value>) -> Self {
        Self::new("error.workspace_invalid").with_param("reason", reason.into())
    }

    pub fn provider_unavailable(reason: impl Into<Value>) -> Self {
        Self::new("error.provider_unavailable").with_param("reason", reason.into())
    }

    /// Catch-all wrapper for `anyhow::Error` and friends. Frontend will fall
    /// back to `errors.generic` with the message interpolated.
    pub fn generic(reason: impl Into<String>) -> Self {
        Self::new("error.generic").with_detail(reason.into())
    }

    /// Issue 2: Pre-flight 对话超过兜底硬限（>80 round）。仅作为防死循环兜底。
    /// 日常 round-pressure 引导走 `planner.rs::render_round_pressure_directive`，
    /// 不靠这条错误推动用户。
    pub fn preflight_too_long(messages: impl Into<Value>, hard_limit: impl Into<Value>) -> Self {
        Self::new("error.preflight_too_long")
            .with_param("messages", messages.into())
            .with_param("hard_limit", hard_limit.into())
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Single-line JSON; frontend tries JSON.parse and falls back to raw.
        match serde_json::to_string(self) {
            Ok(s) => f.write_str(&s),
            // Should never happen for our shape, but if it does at least the UI
            // sees the code text.
            Err(_) => f.write_str(&self.code),
        }
    }
}

impl From<anyhow::Error> for IpcError {
    fn from(err: anyhow::Error) -> Self {
        IpcError::generic(err.to_string())
    }
}

impl From<IpcError> for String {
    fn from(err: IpcError) -> Self {
        err.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_envelope_roundtrips() {
        let err = IpcError::no_api_key("openai");
        let s = err.to_string();
        let v: Value = serde_json::from_str(&s).expect("parse json");
        assert_eq!(v["code"], "error.no_api_key");
        assert_eq!(v["params"]["provider"], "openai");
    }

    #[test]
    fn detail_is_preserved() {
        let err = IpcError::workspace_invalid("not a directory").with_detail("path: /tmp/x");
        let s = err.to_string();
        assert!(s.contains("\"detail\":\"path: /tmp/x\""));
    }

    #[test]
    fn generic_wraps_anyhow() {
        let any = anyhow::anyhow!("boom");
        let err: IpcError = any.into();
        assert_eq!(err.code, "error.generic");
        assert_eq!(err.detail.as_deref(), Some("boom"));
    }
}
