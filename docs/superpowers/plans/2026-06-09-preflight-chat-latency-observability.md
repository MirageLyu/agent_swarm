# Pre-flight Chat Latency Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add lightweight Pre-flight chat latency diagnostics and waiting-state feedback so we can tell whether slow turns are caused by model/provider latency, backend orchestration, tool continuation, compaction, or frontend-visible delay.

**Architecture:** Add a small Pre-flight-specific timing module, extend `planner::preflight_chat` to report LLM request timing, aggregate turn-level timing in `commands/preflight.rs`, and include an optional `perf` object in the existing `done` stream payload. On the frontend, keep UI timing local to `PreflightView` and use a small pure helper for tested console summaries; no persistent telemetry table or debug timeline UI is added in this phase.

**Tech Stack:** Rust/Tauri, `tracing`, `serde`, existing `preflight-stream` IPC events, React/TypeScript, Vitest.

---

## File Structure

- Create: `src-tauri/src/agent/preflight_perf.rs`
  - Pre-flight-specific metric structs and turn timing collector.
  - Has unit tests for missing TTFT, continuation aggregation, token aggregation, and stable JSON shape.
- Modify: `src-tauri/src/agent/mod.rs`
  - Expose `preflight_perf` as an agent module.
- Modify: `src-tauri/src/agent/planner.rs`
  - Return `PreflightLlmTiming` from `preflight_chat`.
  - Measure first reasoning/text activity, TTFT, total LLM call latency.
- Modify: `src-tauri/src/commands/preflight.rs`
  - Aggregate turn-level timings in `preflight_with_continuation`.
  - Extend `done` payload with optional `perf`.
  - Emit clearer `status` messages for long phases.
  - Add unit tests for `done` payload perf compatibility.
- Modify: `src/ipc/commands.ts`
  - Add `PreflightPerfSummary` type for frontend parsing and future reuse.
- Create: `src/utils/preflight-perf.ts`
  - Pure frontend helpers for UI timing summaries and dev log formatting.
- Create: `src/utils/preflight-perf.test.ts`
  - Vitest coverage for first-visible and done/error summary behavior.
- Modify: `src/views/PreflightView.tsx`
  - Track UI timing refs for send, choice, retry, mode switch, and initial stream start.
  - Merge backend `done.perf` with UI timing in development console logs.

---

### Task 1: Add backend Pre-flight perf data model

**Files:**
- Create: `src-tauri/src/agent/preflight_perf.rs`
- Modify: `src-tauri/src/agent/mod.rs`

- [ ] **Step 1: Create the failing Rust unit tests and model skeleton**

Create `src-tauri/src/agent/preflight_perf.rs` with this complete initial content:

```rust
use serde::Serialize;
use std::collections::BTreeSet;
use std::time::Instant;

use crate::llm::TokenUsage;

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct PreflightLlmTiming {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_activity_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    pub total_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_chunk_kind: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct PreflightPerfSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_prepare_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_first_activity_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_ttft_ms: Option<u64>,
    pub llm_total_ms: u64,
    pub tool_processing_ms: u64,
    pub continuation_count: u32,
    pub turn_total_ms: u64,
    pub tool_names: Vec<String>,
    pub compaction_triggered: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

pub struct PreflightTurnTiming {
    started_at: Instant,
    backend_prepare_ms: Option<u64>,
    llm_first_activity_ms: Option<u64>,
    llm_ttft_ms: Option<u64>,
    llm_total_ms: u64,
    tool_processing_ms: u64,
    continuation_count: u32,
    compaction_triggered: bool,
    tool_names: BTreeSet<String>,
    usage: TokenUsage,
}

pub fn elapsed_ms_since(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl PreflightTurnTiming {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            backend_prepare_ms: None,
            llm_first_activity_ms: None,
            llm_ttft_ms: None,
            llm_total_ms: 0,
            tool_processing_ms: 0,
            continuation_count: 0,
            compaction_triggered: false,
            tool_names: BTreeSet::new(),
            usage: TokenUsage::default(),
        }
    }

    pub fn mark_llm_request_start(&mut self) {
        if self.backend_prepare_ms.is_none() {
            self.backend_prepare_ms = Some(elapsed_ms_since(self.started_at));
        }
    }

    pub fn mark_compaction_triggered(&mut self) {
        self.compaction_triggered = true;
    }

    pub fn record_llm_call(&mut self, timing: &PreflightLlmTiming, usage: &TokenUsage) {
        if self.llm_first_activity_ms.is_none() {
            self.llm_first_activity_ms = timing.first_activity_ms;
        }
        if self.llm_ttft_ms.is_none() {
            self.llm_ttft_ms = timing.ttft_ms;
        }
        self.llm_total_ms = self.llm_total_ms.saturating_add(timing.total_ms);
        self.usage.input_tokens = self.usage.input_tokens.saturating_add(usage.input_tokens);
        self.usage.output_tokens = self.usage.output_tokens.saturating_add(usage.output_tokens);
        self.usage.cache_read_input_tokens = self
            .usage
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
        self.usage.cache_creation_input_tokens = self
            .usage
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens);
    }

    pub fn record_tool_processing<I, S>(&mut self, duration_ms: u64, tool_names: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tool_processing_ms = self.tool_processing_ms.saturating_add(duration_ms);
        for name in tool_names {
            self.tool_names.insert(name.into());
        }
    }

    pub fn record_continuation(&mut self) {
        self.continuation_count = self.continuation_count.saturating_add(1);
    }

    pub fn summary(&self) -> PreflightPerfSummary {
        PreflightPerfSummary {
            backend_prepare_ms: self.backend_prepare_ms,
            llm_first_activity_ms: self.llm_first_activity_ms,
            llm_ttft_ms: self.llm_ttft_ms,
            llm_total_ms: self.llm_total_ms,
            tool_processing_ms: self.tool_processing_ms,
            continuation_count: self.continuation_count,
            turn_total_ms: elapsed_ms_since(self.started_at),
            tool_names: self.tool_names.iter().cloned().collect(),
            compaction_triggered: self.compaction_triggered,
            input_tokens: self.usage.input_tokens,
            output_tokens: self.usage.output_tokens,
            cache_read_input_tokens: self.usage.cache_read_input_tokens,
            cache_creation_input_tokens: self.usage.cache_creation_input_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_timing_serializes_missing_ttft_without_field() {
        let timing = PreflightLlmTiming {
            first_activity_ms: Some(120),
            ttft_ms: None,
            total_ms: 450,
            first_chunk_kind: Some("reasoning_delta".into()),
        };

        let value = serde_json::to_value(&timing).unwrap();
        assert_eq!(value["first_activity_ms"], 120);
        assert!(value.get("ttft_ms").is_none());
        assert_eq!(value["total_ms"], 450);
        assert_eq!(value["first_chunk_kind"], "reasoning_delta");
    }

    #[test]
    fn turn_summary_aggregates_tokens_tools_and_continuations() {
        let mut turn = PreflightTurnTiming::new();
        turn.mark_llm_request_start();
        turn.mark_compaction_triggered();
        turn.record_llm_call(
            &PreflightLlmTiming {
                first_activity_ms: Some(100),
                ttft_ms: None,
                total_ms: 600,
                first_chunk_kind: Some("reasoning_delta".into()),
            },
            &TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_input_tokens: 3,
                cache_creation_input_tokens: 4,
            },
        );
        turn.record_continuation();
        turn.record_llm_call(
            &PreflightLlmTiming {
                first_activity_ms: Some(80),
                ttft_ms: Some(140),
                total_ms: 500,
                first_chunk_kind: Some("text_delta".into()),
            },
            &TokenUsage {
                input_tokens: 30,
                output_tokens: 40,
                cache_read_input_tokens: 5,
                cache_creation_input_tokens: 6,
            },
        );
        turn.record_tool_processing(
            75,
            ["present_choices".to_string(), "add_contract_item".to_string()],
        );
        turn.record_tool_processing(25, ["present_choices".to_string()]);

        let summary = turn.summary();
        assert!(summary.backend_prepare_ms.is_some());
        assert_eq!(summary.llm_first_activity_ms, Some(100));
        assert_eq!(summary.llm_ttft_ms, Some(140));
        assert_eq!(summary.llm_total_ms, 1100);
        assert_eq!(summary.tool_processing_ms, 100);
        assert_eq!(summary.continuation_count, 1);
        assert_eq!(summary.tool_names, vec!["add_contract_item", "present_choices"]);
        assert!(summary.compaction_triggered);
        assert_eq!(summary.input_tokens, 40);
        assert_eq!(summary.output_tokens, 60);
        assert_eq!(summary.cache_read_input_tokens, 8);
        assert_eq!(summary.cache_creation_input_tokens, 10);
    }
}
```

Modify `src-tauri/src/agent/mod.rs` by adding the module declaration near the other `pub mod` declarations:

```rust
pub mod preflight_perf;
```

- [ ] **Step 2: Run the new backend tests and verify they pass**

Run:

```bash
cd /Volumes/T7/Miragenty/src-tauri && cargo test preflight_perf --lib
```

Expected: tests containing `llm_timing_serializes_missing_ttft_without_field` and `turn_summary_aggregates_tokens_tools_and_continuations` pass.

- [ ] **Step 3: Commit Task 1**

```bash
cd /Volumes/T7/Miragenty
git add src-tauri/src/agent/mod.rs src-tauri/src/agent/preflight_perf.rs
git commit -m "feat(preflight): add latency timing model"
```

---

### Task 2: Measure LLM-level timing in `planner::preflight_chat`

**Files:**
- Modify: `src-tauri/src/agent/planner.rs:11-15`
- Modify: `src-tauri/src/agent/planner.rs:1549-1781`

- [ ] **Step 1: Update imports and function signature**

In `src-tauri/src/agent/planner.rs`, add this import near the existing belief-state import:

```rust
use crate::agent::preflight_perf::{elapsed_ms_since, PreflightLlmTiming};
```

Change the `preflight_chat` return type from:

```rust
) -> Result<(PreflightResponse, TokenUsage), PlannerError> {
```

to:

```rust
) -> Result<(PreflightResponse, TokenUsage, PreflightLlmTiming), PlannerError> {
```

- [ ] **Step 2: Measure first activity, TTFT, and total LLM latency**

In `preflight_chat`, immediately before the `mpsc::channel` setup currently near `let (tx, mut rx) = ...`, add shared timing state:

```rust
    let llm_started_at = std::time::Instant::now();
    let first_activity_ms: Arc<tokio::sync::Mutex<Option<u64>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let ttft_ms: Arc<tokio::sync::Mutex<Option<u64>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let first_chunk_kind: Arc<tokio::sync::Mutex<Option<String>>> =
        Arc::new(tokio::sync::Mutex::new(None));
```

Before spawning the forwarder, clone those values:

```rust
    let first_activity_for_fwd = first_activity_ms.clone();
    let ttft_for_fwd = ttft_ms.clone();
    let first_chunk_kind_for_fwd = first_chunk_kind.clone();
```

Replace the `StreamChunkKind::TextDelta` and `StreamChunkKind::ReasoningDelta` arms in the forwarder with:

```rust
                StreamChunkKind::TextDelta => {
                    let elapsed = elapsed_ms_since(llm_started_at);
                    {
                        let mut first_activity = first_activity_for_fwd.lock().await;
                        if first_activity.is_none() {
                            *first_activity = Some(elapsed);
                        }
                    }
                    {
                        let mut first_text = ttft_for_fwd.lock().await;
                        if first_text.is_none() {
                            *first_text = Some(elapsed);
                        }
                    }
                    {
                        let mut kind = first_chunk_kind_for_fwd.lock().await;
                        if kind.is_none() {
                            *kind = Some("text_delta".to_string());
                        }
                    }
                    full_text_for_fwd.lock().await.push_str(&chunk.content);
                    emit_preflight_event(&app_clone, &sid, "text_delta", &chunk.content);
                }
                StreamChunkKind::ReasoningDelta => {
                    let elapsed = elapsed_ms_since(llm_started_at);
                    {
                        let mut first_activity = first_activity_for_fwd.lock().await;
                        if first_activity.is_none() {
                            *first_activity = Some(elapsed);
                        }
                    }
                    {
                        let mut kind = first_chunk_kind_for_fwd.lock().await;
                        if kind.is_none() {
                            *kind = Some("reasoning_delta".to_string());
                        }
                    }
                    emit_preflight_event(&app_clone, &sid, "reasoning_delta", &chunk.content);
                }
```

After `let _ = forwarder.await;`, create the final timing:

```rust
    let llm_timing = PreflightLlmTiming {
        first_activity_ms: *first_activity_ms.lock().await,
        ttft_ms: *ttft_ms.lock().await,
        total_ms: elapsed_ms_since(llm_started_at),
        first_chunk_kind: first_chunk_kind.lock().await.clone(),
    };
```

At the end of the function, change:

```rust
    Ok((result, response.usage))
```

to:

```rust
    Ok((result, response.usage, llm_timing))
```

- [ ] **Step 3: Extend the existing stream-complete tracing log**

In the `tracing::info!` call near the end of `preflight_chat`, add these fields:

```rust
        llm_first_activity_ms = ?llm_timing.first_activity_ms,
        llm_ttft_ms = ?llm_timing.ttft_ms,
        llm_total_ms = llm_timing.total_ms,
        first_chunk_kind = ?llm_timing.first_chunk_kind,
```

The final log block should still include existing `choices_count`, `tool_calls_count`, `fallback_used`, `input_tokens`, and `output_tokens`.

- [ ] **Step 4: Update the one call site to compile against the new return type**

In `src-tauri/src/commands/preflight.rs`, find the match around the `planner::preflight_chat(...).await` call. Change:

```rust
        let (response, usage) = match planner::preflight_chat(
```

to:

```rust
        let (response, usage, llm_timing) = match planner::preflight_chat(
```

This variable is used in Task 3.

- [ ] **Step 5: Run backend compile/test check**

Run:

```bash
cd /Volumes/T7/Miragenty/src-tauri && cargo test preflight_perf --lib
```

Expected: the `preflight_perf` tests still pass and the crate compiles with the new `preflight_chat` return type.

- [ ] **Step 6: Commit Task 2**

```bash
cd /Volumes/T7/Miragenty
git add src-tauri/src/agent/planner.rs src-tauri/src/commands/preflight.rs
git commit -m "feat(preflight): measure llm response timing"
```

---

### Task 3: Aggregate turn timing and include optional `done.perf`

**Files:**
- Modify: `src-tauri/src/commands/preflight.rs:1-10`
- Modify: `src-tauri/src/commands/preflight.rs:572-587`
- Modify: `src-tauri/src/commands/preflight.rs:598-1120`
- Test: `src-tauri/src/commands/preflight.rs` test module

- [ ] **Step 1: Import the perf model and add a done-payload helper**

In `src-tauri/src/commands/preflight.rs`, add this import near the existing `use crate::agent::planner...` line:

```rust
use crate::agent::preflight_perf::{elapsed_ms_since, PreflightPerfSummary, PreflightTurnTiming};
```

Replace `emit_done_with_belief_state` with these two functions:

```rust
fn build_done_payload(
    response: &planner::PreflightResponse,
    belief_state: &PreflightBeliefState,
    mode: &str,
    perf: Option<&PreflightPerfSummary>,
) -> serde_json::Value {
    let mut done_payload = json!({
        "text": response.text,
        "choices": response.choices,
        "convergence_score": belief_state.convergence_score,
        "phase": belief_state.phase.label(),
        "mode": mode,
    });

    if let Some(perf) = perf {
        done_payload["perf"] = serde_json::to_value(perf).unwrap_or_else(|_| json!({}));
    }

    done_payload
}

fn emit_done_with_belief_state(
    app: &tauri::AppHandle,
    session_id: &str,
    response: &planner::PreflightResponse,
    belief_state: &PreflightBeliefState,
    mode: &str,
    perf: Option<&PreflightPerfSummary>,
) {
    let done_payload = build_done_payload(response, belief_state, mode, perf);
    planner::emit_preflight_event_pub(app, session_id, "done", &done_payload.to_string());
}
```

- [ ] **Step 2: Write the done payload compatibility tests**

Add this test module before the existing `preflight_session_projection_tests` module in `src-tauri/src/commands/preflight.rs`:

```rust
#[cfg(test)]
mod preflight_perf_payload_tests {
    use super::*;
    use crate::agent::belief_state::PreflightBeliefState;

    fn sample_response() -> planner::PreflightResponse {
        planner::PreflightResponse {
            text: "下一步请选择默认登录方式。".into(),
            choices: vec![],
            tool_calls: vec![],
            fallback_used: "none".into(),
            reasoning: String::new(),
        }
    }

    #[test]
    fn done_payload_omits_perf_when_unavailable() {
        let belief_state = PreflightBeliefState::new();
        let payload = build_done_payload(&sample_response(), &belief_state, "scenario_walk", None);

        assert_eq!(payload["text"], "下一步请选择默认登录方式。");
        assert_eq!(payload["mode"], "scenario_walk");
        assert!(payload.get("perf").is_none());
    }

    #[test]
    fn done_payload_includes_perf_when_available() {
        let belief_state = PreflightBeliefState::new();
        let perf = PreflightPerfSummary {
            backend_prepare_ms: Some(12),
            llm_first_activity_ms: Some(100),
            llm_ttft_ms: Some(150),
            llm_total_ms: 900,
            tool_processing_ms: 33,
            continuation_count: 1,
            turn_total_ms: 1200,
            tool_names: vec!["add_contract_item".into(), "present_choices".into()],
            compaction_triggered: false,
            input_tokens: 500,
            output_tokens: 80,
            cache_read_input_tokens: 200,
            cache_creation_input_tokens: 0,
        };

        let payload = build_done_payload(
            &sample_response(),
            &belief_state,
            "scenario_walk",
            Some(&perf),
        );

        assert_eq!(payload["perf"]["backend_prepare_ms"], 12);
        assert_eq!(payload["perf"]["llm_ttft_ms"], 150);
        assert_eq!(payload["perf"]["continuation_count"], 1);
        assert_eq!(payload["perf"]["tool_names"][0], "add_contract_item");
        assert_eq!(payload["perf"]["input_tokens"], 500);
    }
}
```

- [ ] **Step 3: Run the payload tests and verify they pass**

Run:

```bash
cd /Volumes/T7/Miragenty/src-tauri && cargo test preflight_perf_payload_tests --lib
```

Expected: both `done_payload_omits_perf_when_unavailable` and `done_payload_includes_perf_when_available` pass.

- [ ] **Step 4: Start turn timing inside `preflight_with_continuation`**

At the top of `preflight_with_continuation`, after `let mut combined_text = String::new();`, add:

```rust
    let mut turn_timing = PreflightTurnTiming::new();
```

When compaction is triggered inside `if needs_compact {`, add this before the existing `tracing::info!` call:

```rust
                turn_timing.mark_compaction_triggered();
```

Immediately before calling `planner::preflight_chat`, emit a useful status and mark backend preparation:

```rust
        planner::emit_preflight_event_pub(app, session_id, "status", "正在等待模型响应…");
        turn_timing.mark_llm_request_start();
```

After a successful `planner::preflight_chat` result, record timing and token usage:

```rust
        turn_timing.record_llm_call(&llm_timing, &usage);
```

- [ ] **Step 5: Measure tool processing and continuation overhead**

Before the explorer tool loop begins, add:

```rust
        let tool_processing_started = std::time::Instant::now();
        let mut executed_tool_names: Vec<String> = response
            .tool_calls
            .iter()
            .map(|tc| tc.name.clone())
            .collect();
```

Inside the explorer branch, before executing `ex.execute(...)`, add a user-facing status:

```rust
                    planner::emit_preflight_event_pub(app, session_id, "status", "正在读取仓库信息…");
```

Before `process_tool_actions(...)`, if `actions` is not empty, emit:

```rust
            if !actions.is_empty() {
                planner::emit_preflight_event_pub(app, session_id, "status", "正在整理刚刚确认的内容…");
            }
```

After `let (tool_result_msgs, belief_state) = match process_result { ... };`, record the processing time:

```rust
        executed_tool_names.sort();
        executed_tool_names.dedup();
        turn_timing.record_tool_processing(
            elapsed_ms_since(tool_processing_started),
            executed_tool_names,
        );
```

Inside the existing `if needs_continuation {` branch, before emitting `正在准备下一个问题…`, add:

```rust
            turn_timing.record_continuation();
```

- [ ] **Step 6: Include perf in successful `done` and summary logs**

Before constructing `final_response` in the normal stop path, create the summary:

```rust
        let perf_summary = turn_timing.summary();
```

Change the `emit_done_with_belief_state` call from:

```rust
        emit_done_with_belief_state(app, session_id, &final_response, &belief_state, mode);
```

to:

```rust
        emit_done_with_belief_state(
            app,
            session_id,
            &final_response,
            &belief_state,
            mode,
            Some(&perf_summary),
        );
```

Add these fields to the existing `tracing::info!` block for `"preflight round completed"`:

```rust
            backend_prepare_ms = ?perf_summary.backend_prepare_ms,
            llm_first_activity_ms = ?perf_summary.llm_first_activity_ms,
            llm_ttft_ms = ?perf_summary.llm_ttft_ms,
            llm_total_ms = perf_summary.llm_total_ms,
            tool_processing_ms = perf_summary.tool_processing_ms,
            continuation_count = perf_summary.continuation_count,
            turn_total_ms = perf_summary.turn_total_ms,
            input_tokens_total = perf_summary.input_tokens,
            output_tokens_total = perf_summary.output_tokens,
            cache_read_input_tokens_total = perf_summary.cache_read_input_tokens,
            cache_creation_input_tokens_total = perf_summary.cache_creation_input_tokens,
            compaction_triggered = perf_summary.compaction_triggered,
```

- [ ] **Step 7: Include partial perf in the max-continuation fallback `done`**

In the max-iterations-exhausted path near the end of `preflight_with_continuation`, before `emit_done_with_belief_state`, add:

```rust
    let perf_summary = turn_timing.summary();
```

Change the fallback emit call to:

```rust
    emit_done_with_belief_state(
        app,
        session_id,
        &final_response,
        &belief_state,
        mode,
        Some(&perf_summary),
    );
```

- [ ] **Step 8: Include partial perf in error logs without changing retry behavior**

In the LLM error branch, before `planner::emit_preflight_event_pub(app, session_id, "error", &user_msg);`, add:

```rust
                let perf_summary = turn_timing.summary();
                tracing::info!(
                    session_id = %session_id,
                    mission_id = %mission_id,
                    mode = %mode,
                    iteration,
                    backend_prepare_ms = ?perf_summary.backend_prepare_ms,
                    llm_first_activity_ms = ?perf_summary.llm_first_activity_ms,
                    llm_ttft_ms = ?perf_summary.llm_ttft_ms,
                    llm_total_ms = perf_summary.llm_total_ms,
                    tool_processing_ms = perf_summary.tool_processing_ms,
                    continuation_count = perf_summary.continuation_count,
                    turn_total_ms = perf_summary.turn_total_ms,
                    compaction_triggered = perf_summary.compaction_triggered,
                    status = "error",
                    "preflight round perf"
                );
```

Do not add perf to the existing `error` event payload in Phase 1; keep frontend error handling unchanged.

- [ ] **Step 9: Update all remaining `emit_done_with_belief_state` call sites**

Search:

```bash
cd /Volumes/T7/Miragenty && rg -n "emit_done_with_belief_state" src-tauri/src/commands/preflight.rs
```

Every call must now pass `Some(&perf_summary)` or `None`. The function definition itself is one match; all call sites should compile.

- [ ] **Step 10: Run backend tests**

Run:

```bash
cd /Volumes/T7/Miragenty/src-tauri && cargo test preflight_perf --lib && cargo test preflight_perf_payload_tests --lib
```

Expected: all perf model and payload tests pass.

- [ ] **Step 11: Commit Task 3**

```bash
cd /Volumes/T7/Miragenty
git add src-tauri/src/commands/preflight.rs src-tauri/src/agent/preflight_perf.rs
git commit -m "feat(preflight): report turn latency summary"
```

---

### Task 4: Add frontend perf types and pure UI timing helper

**Files:**
- Modify: `src/ipc/commands.ts:515-535`
- Create: `src/utils/preflight-perf.ts`
- Create: `src/utils/preflight-perf.test.ts`

- [ ] **Step 1: Add the frontend perf summary type**

In `src/ipc/commands.ts`, after `PreflightMessageInfo` and before `PreflightSessionInfo`, add:

```ts
export interface PreflightPerfSummary {
  backend_prepare_ms?: number;
  llm_first_activity_ms?: number;
  llm_ttft_ms?: number;
  llm_total_ms: number;
  tool_processing_ms: number;
  continuation_count: number;
  turn_total_ms: number;
  tool_names: string[];
  compaction_triggered: boolean;
  input_tokens: number;
  output_tokens: number;
  cache_read_input_tokens: number;
  cache_creation_input_tokens: number;
}
```

- [ ] **Step 2: Create the frontend helper**

Create `src/utils/preflight-perf.ts` with this complete content:

```ts
import type { PreflightPerfSummary } from "../ipc/commands";

export type PreflightTurnSource = "initial" | "free_input" | "choice" | "mode_switch" | "retry";

export interface PreflightUiTurnState {
  source: PreflightTurnSource;
  startedAt: number;
  firstVisibleAt: number | null;
}

export interface PreflightUiPerfSummary {
  source: PreflightTurnSource;
  first_visible_ms: number | null;
  done_ms: number;
  backend_perf: PreflightPerfSummary | null;
}

export function startPreflightUiTurn(
  source: PreflightTurnSource,
  now: number = Date.now(),
): PreflightUiTurnState {
  return {
    source,
    startedAt: now,
    firstVisibleAt: null,
  };
}

export function markPreflightFirstVisible(
  state: PreflightUiTurnState,
  now: number = Date.now(),
): PreflightUiTurnState {
  if (state.firstVisibleAt !== null) return state;
  return { ...state, firstVisibleAt: now };
}

export function finishPreflightUiTurn(
  state: PreflightUiTurnState,
  backendPerf: PreflightPerfSummary | null,
  now: number = Date.now(),
): PreflightUiPerfSummary {
  const firstVisibleAt = state.firstVisibleAt ?? now;
  return {
    source: state.source,
    first_visible_ms: firstVisibleAt - state.startedAt,
    done_ms: now - state.startedAt,
    backend_perf: backendPerf,
  };
}

export function formatPreflightPerfLog(summary: PreflightUiPerfSummary): Record<string, unknown> {
  const backend = summary.backend_perf;
  return {
    source: summary.source,
    first_visible_ms: summary.first_visible_ms,
    done_ms: summary.done_ms,
    backend_prepare_ms: backend?.backend_prepare_ms ?? null,
    llm_first_activity_ms: backend?.llm_first_activity_ms ?? null,
    llm_ttft_ms: backend?.llm_ttft_ms ?? null,
    llm_total_ms: backend?.llm_total_ms ?? null,
    tool_processing_ms: backend?.tool_processing_ms ?? null,
    continuation_count: backend?.continuation_count ?? null,
    turn_total_ms: backend?.turn_total_ms ?? null,
    tool_names: backend?.tool_names ?? [],
    compaction_triggered: backend?.compaction_triggered ?? false,
    input_tokens: backend?.input_tokens ?? null,
    output_tokens: backend?.output_tokens ?? null,
    cache_read_input_tokens: backend?.cache_read_input_tokens ?? null,
    cache_creation_input_tokens: backend?.cache_creation_input_tokens ?? null,
  };
}
```

- [ ] **Step 3: Add Vitest coverage for the helper**

Create `src/utils/preflight-perf.test.ts` with this complete content:

```ts
import { describe, expect, it } from "vitest";
import {
  finishPreflightUiTurn,
  formatPreflightPerfLog,
  markPreflightFirstVisible,
  startPreflightUiTurn,
} from "./preflight-perf";
import type { PreflightPerfSummary } from "../ipc/commands";

const backendPerf: PreflightPerfSummary = {
  backend_prepare_ms: 12,
  llm_first_activity_ms: 90,
  llm_ttft_ms: 140,
  llm_total_ms: 900,
  tool_processing_ms: 33,
  continuation_count: 1,
  turn_total_ms: 1200,
  tool_names: ["add_contract_item", "present_choices"],
  compaction_triggered: false,
  input_tokens: 500,
  output_tokens: 80,
  cache_read_input_tokens: 200,
  cache_creation_input_tokens: 0,
};

describe("preflight perf helpers", () => {
  it("records first visible only once", () => {
    const started = startPreflightUiTurn("choice", 1000);
    const first = markPreflightFirstVisible(started, 1250);
    const second = markPreflightFirstVisible(first, 1500);

    expect(second.firstVisibleAt).toBe(1250);
  });

  it("uses done time as first visible when no earlier feedback exists", () => {
    const started = startPreflightUiTurn("free_input", 1000);
    const summary = finishPreflightUiTurn(started, null, 1800);

    expect(summary.source).toBe("free_input");
    expect(summary.first_visible_ms).toBe(800);
    expect(summary.done_ms).toBe(800);
    expect(summary.backend_perf).toBeNull();
  });

  it("merges backend perf into formatted dev log", () => {
    const started = startPreflightUiTurn("choice", 1000);
    const visible = markPreflightFirstVisible(started, 1300);
    const summary = finishPreflightUiTurn(visible, backendPerf, 2200);
    const log = formatPreflightPerfLog(summary);

    expect(log).toMatchObject({
      source: "choice",
      first_visible_ms: 300,
      done_ms: 1200,
      backend_prepare_ms: 12,
      llm_ttft_ms: 140,
      continuation_count: 1,
      tool_names: ["add_contract_item", "present_choices"],
      input_tokens: 500,
    });
  });
});
```

- [ ] **Step 4: Run frontend helper tests**

Run:

```bash
cd /Volumes/T7/Miragenty && pnpm test -- src/utils/preflight-perf.test.ts
```

Expected: all three `preflight perf helpers` tests pass.

- [ ] **Step 5: Commit Task 4**

```bash
cd /Volumes/T7/Miragenty
git add src/ipc/commands.ts src/utils/preflight-perf.ts src/utils/preflight-perf.test.ts
git commit -m "feat(preflight): add frontend latency helpers"
```

---

### Task 5: Wire frontend UI timing into `PreflightView`

**Files:**
- Modify: `src/views/PreflightView.tsx:1-20`
- Modify: `src/views/PreflightView.tsx:41-43`
- Modify: `src/views/PreflightView.tsx:65-146`
- Modify: `src/views/PreflightView.tsx:148-271`

- [ ] **Step 1: Add imports and timing refs**

In `src/views/PreflightView.tsx`, extend the type import from `../ipc/commands` to include `PreflightPerfSummary`:

```ts
  PreflightPerfSummary,
```

Add this import near the existing hook imports:

```ts
import {
  finishPreflightUiTurn,
  formatPreflightPerfLog,
  markPreflightFirstVisible,
  startPreflightUiTurn,
  type PreflightTurnSource,
  type PreflightUiTurnState,
} from "../utils/preflight-perf";
```

After `const sessionIdRef = useRef(sessionId);`, add:

```ts
  const uiTurnRef = useRef<PreflightUiTurnState | null>(null);
```

Add these helper callbacks before the stream subscription effect:

```ts
  const startUiTurn = useCallback((source: PreflightTurnSource) => {
    uiTurnRef.current = startPreflightUiTurn(source);
  }, []);

  const markFirstVisible = useCallback(() => {
    if (!uiTurnRef.current) return;
    uiTurnRef.current = markPreflightFirstVisible(uiTurnRef.current);
  }, []);

  const finishUiTurn = useCallback((backendPerf: PreflightPerfSummary | null) => {
    if (!uiTurnRef.current) return;
    const summary = finishPreflightUiTurn(uiTurnRef.current, backendPerf);
    uiTurnRef.current = null;
    if (import.meta.env.DEV) {
      console.info("[preflight ui perf]", formatPreflightPerfLog(summary));
    }
  }, []);
```

- [ ] **Step 2: Update stream event handling**

In the `kind === "start"` branch, after setting loading state, add:

```ts
        if (!uiTurnRef.current) {
          startUiTurn("initial");
        }
```

In the `kind === "text_delta"` branch, before appending text or immediately after appending text, add:

```ts
        markFirstVisible();
```

In the `kind === "status"` branch, after `setStatusText(content);`, add:

```ts
        if (content.trim()) {
          markFirstVisible();
        }
```

In the `kind === "done"` branch, after `const parsed = JSON.parse(content);`, add:

```ts
          const backendPerf = (parsed.perf ?? null) as PreflightPerfSummary | null;
          finishUiTurn(backendPerf);
```

In the `kind === "error"` branch, after `setError(content);`, add:

```ts
        finishUiTurn(null);
```

Update the stream subscription effect dependency array from:

```ts
  }, [missionId]);
```

to:

```ts
  }, [missionId, startUiTurn, markFirstVisible, finishUiTurn]);
```

- [ ] **Step 3: Start UI timing from each user action source**

Change the `handleSend` signature from:

```ts
    (text: string) => {
```

to:

```ts
    (text: string, source: PreflightTurnSource = "free_input") => {
```

Inside `handleSend`, before `setError(null);`, add:

```ts
      startUiTurn(source);
```

Update the `handleSend` dependency array to include `startUiTurn`.

Inside `handleRetry`, before `setError(null);`, add:

```ts
    startUiTurn("retry");
```

Update the `handleRetry` dependency array to include `startUiTurn`.

Inside `handleModeChange`, before `setMode(newMode);`, add:

```ts
    startUiTurn("mode_switch");
```

Update the `handleModeChange` dependency array to include `startUiTurn`.

In `handleChoiceSelect`, change:

```ts
      handleSend(choice.label);
```

to:

```ts
      handleSend(choice.label, "choice");
```

- [ ] **Step 4: Run frontend tests and type check**

Run:

```bash
cd /Volumes/T7/Miragenty && pnpm test -- src/utils/preflight-perf.test.ts && pnpm build
```

Expected:

- Vitest helper tests pass.
- `pnpm build` succeeds without TypeScript errors.

- [ ] **Step 5: Commit Task 5**

```bash
cd /Volumes/T7/Miragenty
git add src/views/PreflightView.tsx
git commit -m "feat(preflight): log user-visible latency"
```

---

### Task 6: End-to-end verification and cleanup

**Files:**
- Verify: `docs/superpowers/specs/2026-06-09-preflight-chat-latency-observability-design.md`
- Verify: all files changed in Tasks 1-5

- [ ] **Step 1: Run focused backend tests**

```bash
cd /Volumes/T7/Miragenty/src-tauri && cargo test preflight_perf --lib && cargo test preflight_perf_payload_tests --lib
```

Expected: all focused backend tests pass.

- [ ] **Step 2: Run focused frontend tests**

```bash
cd /Volumes/T7/Miragenty && pnpm test -- src/utils/preflight-perf.test.ts
```

Expected: all frontend helper tests pass.

- [ ] **Step 3: Run broader build checks**

```bash
cd /Volumes/T7/Miragenty && pnpm build
```

Expected: TypeScript and Vite build complete successfully.

Then run:

```bash
cd /Volumes/T7/Miragenty/src-tauri && cargo test --lib
```

Expected: Rust library tests pass. If unrelated pre-existing tests fail, capture the failing test names and output in the task notes instead of hiding the failure.

- [ ] **Step 4: Manual Pre-flight verification**

Run the app in development mode:

```bash
cd /Volumes/T7/Miragenty && pnpm tauri dev
```

Manual checks:

1. Create or open a mission and start Pre-flight.
2. Send a free-input message.
3. Click a choice button in a later turn.
4. Trigger a mode switch.
5. Confirm the chat still renders assistant text and choices normally.
6. Confirm the typing/status row shows meaningful statuses such as `正在等待模型响应…` or `正在准备下一个问题…` during slow phases.
7. Open the frontend dev console and confirm `[preflight ui perf]` appears on done/error with `first_visible_ms` and backend perf fields when available.
8. Inspect backend logs and confirm one `preflight round completed` log per completed turn includes `llm_ttft_ms`, `turn_total_ms`, `continuation_count`, token totals, and `compaction_triggered`.

- [ ] **Step 5: Verify no telemetry overreach was introduced**

Run:

```bash
cd /Volumes/T7/Miragenty && rg -n "preflight_perf|PreflightPerf|agent_events|benchmark_metric_snapshots|CREATE TABLE.*preflight" src-tauri/src src
```

Expected:

- `preflight_perf` / `PreflightPerf` appear only in the new helper/model and Pre-flight integration files.
- No new `agent_events` writes were added for Pre-flight timing.
- No new benchmark metric snapshot schema change was added.
- No new Pre-flight metrics DB table was added.

- [ ] **Step 6: Commit verification notes if code changed during cleanup**

If Task 6 required code fixes, commit them:

```bash
cd /Volumes/T7/Miragenty
git add <fixed-files>
git commit -m "fix(preflight): stabilize latency instrumentation"
```

If no code changed, do not create an empty commit.

---

## Self-Review Checklist

- Spec coverage:
  - Phase 1 backend structured logs: Tasks 1-3.
  - Optional `done.perf`: Task 3.
  - Frontend console summary: Tasks 4-5.
  - UX status improvements: Task 3 and Task 6 manual verification.
  - No DB / no telemetry platform / no `agent_events`: Task 6 guardrail check.
  - Benchmark evolution left for later: no implementation task included by design.
- Placeholder scan:
  - No `TBD`, `TODO`, `FIXME`, or unspecified implementation steps are present.
- Type consistency:
  - Rust names: `PreflightLlmTiming`, `PreflightPerfSummary`, `PreflightTurnTiming`.
  - TypeScript names: `PreflightPerfSummary`, `PreflightTurnSource`, `PreflightUiTurnState`.
  - JSON field names use snake_case consistently across Rust and TypeScript.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-09-preflight-chat-latency-observability.md`. Two execution options:

1. **Subagent-Driven (recommended)** - dispatch a fresh subagent per task, review between tasks, fast iteration.

2. **Inline Execution** - execute tasks in this session using `superpowers:executing-plans`, batch execution with checkpoints.

Choose one before implementation begins.
