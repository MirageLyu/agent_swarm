# Pre-flight Chat Latency Observability and UX Feedback Design

Date: 2026-06-09

## Context

The current pain point is experiential: during Pre-flight scenario-walk chat, responses sometimes feel slow, and it is unclear whether the delay comes from the model/provider, backend orchestration, tool-call continuation, compaction, repository exploration, or frontend rendering.

This design intentionally does **not** introduce a broad telemetry platform. The immediate goal is to make Pre-flight chat latency diagnosable and to improve the waiting experience. The design keeps metric names and boundaries stable so that benchmark or persistent telemetry can be added later without rewriting the first iteration.

## Goals

1. Diagnose where Pre-flight chat latency is spent.
2. Improve user feedback while a turn is in progress.
3. Reuse existing logging, stream events, and benchmark conventions instead of adding an isolated telemetry stack.
4. Preserve a clean path toward later benchmark aggregation or persistent metrics.

## Non-goals for the first implementation

- No new product-wide telemetry service.
- No new persistent metrics table in Phase 1.
- No full debug timeline UI in Phase 1.
- No changes to provider-level streaming guard behavior unless later evidence shows it is necessary.
- No use of `agent_events` for Pre-flight chat timing; it is agent-scoped and does not naturally map to `preflight_sessions`.

## Existing mechanisms to reuse

### Backend tracing

The provider and stream layers already emit useful low-level logs:

- `llm/anthropic.rs`: connection, first byte, total stream timing.
- `llm/openai_compat.rs`: connection timing, raw byte gaps, stream throughput, first SSE bytes.
- `llm/stream_guard.rs`: first parsed chunk, chunk gaps, idle-stall diagnostics.
- `agent/planner.rs`: Pre-flight cache metrics and stream completion logs.
- `commands/preflight.rs`: compaction already measures elapsed wall-clock time with `Instant`.

These logs should remain the low-level diagnostics. Pre-flight should add higher-level turn and user-visible timing around them.

### Pre-flight stream events

Pre-flight already emits `preflight-stream` events with `kind` and `content`. The frontend already consumes `start`, `text_delta`, `reasoning_delta`, `status`, `done`, and `error`. Phase 1 should reuse this channel rather than creating a new event bus.

### Benchmark metrics

The benchmark system already has `BenchmarkMetrics`, metric snapshots, and report export. Those should be extended only in a later phase if Pre-flight latency becomes a benchmark dimension.

## Metric vocabulary

The following names should be used consistently across backend logs, frontend console output, and future benchmark fields.

| Metric | Definition | Primary use |
|---|---|---|
| `frontend_wait_ms` | User action to backend `start` event received by frontend | IPC / command scheduling perception |
| `backend_prepare_ms` | Backend command/turn start to first LLM request start | DB, history reconstruction, prompt setup, compaction |
| `llm_first_activity_ms` | LLM request start to first `reasoning_delta` or `text_delta` | Model/provider activity started |
| `llm_ttft_ms` | LLM request start to first `text_delta` | First visible model text |
| `first_visible_ms` | User action to first visible feedback in UI | User-perceived first feedback |
| `llm_total_ms` | LLM request start to stream completion for one model call | Single LLM call duration |
| `tool_processing_ms` | Time spent processing tool calls and side effects | DB/tool/repo explorer overhead |
| `continuation_count` | Extra LLM continuation iterations in one user turn | Tool-call orchestration cost |
| `turn_total_ms` | User/backend turn start to final `done`/`error` | Whole turn completion time |

Important distinction: `first_byte_ms` and stream guard `first_chunk_ms` are useful lower-level signals, but they are not Pre-flight UX TTFT. Pre-flight UX needs `llm_ttft_ms` and `first_visible_ms`.

## Phased design

### Phase 1: Diagnostic logs and lightweight frontend summaries

Phase 1 answers: “Where is the current delay?”

Backend changes:

- Add Pre-flight turn-level timing around `preflight_with_continuation`.
- Add LLM call-level timing around `planner::preflight_chat`.
- Emit one structured summary log per Pre-flight turn.
- Add an optional `perf` object to the existing `done` payload.
- Emit clearer `status` messages at key slow points.

Frontend changes:

- Track user-action start time in `PreflightView` for send, choice, retry, mode switch, and initial flow where possible.
- Compute first visible feedback and total UI latency from existing stream events.
- Log a compact console summary in development/debug usage.
- Continue rendering existing chat UI; no new panel is required.

Phase 1 should be enough to classify slowness into:

- provider/model TTFT,
- model generation length,
- backend preparation,
- compaction,
- tool continuation,
- repository exploration,
- frontend/user-visible delay.

### Phase 2: User-facing waiting-state improvements

Phase 2 improves perceived responsiveness without requiring persistent telemetry.

Reuse the existing `status` stream event and `PreflightChat.statusText` rendering. Emit more specific statuses such as:

- `正在读取对话上下文…`
- `正在等待模型响应…`
- `正在记录 Contract 条目…`
- `正在读取仓库文件…`
- `正在准备下一个问题…`
- `对话较长，正在压缩上下文…`

These should be short, user-oriented, and tied to real backend phases. They should not reveal noisy internal implementation details unless in debug mode.

### Phase 3: Benchmark and persistence evolution

Only after Phase 1 data shows that longitudinal comparison is useful, extend benchmark metrics.

Possible benchmark aggregate fields:

- `preflight_turn_count`
- `preflight_avg_ttft_ms`
- `preflight_p95_ttft_ms`
- `preflight_avg_turn_total_ms`
- `preflight_avg_continuation_count`
- `preflight_tool_only_turn_count`
- `preflight_compaction_count`

If product-runtime historical analysis becomes necessary, add a dedicated Pre-flight metrics persistence model later. Do not overload `agent_events` for this.

## Backend design

### Turn-level collector

Add a small Pre-flight-specific timing collector near `commands/preflight.rs::preflight_with_continuation`. It can start as a local struct rather than a global telemetry abstraction.

Conceptual fields:

```rust
struct PreflightTurnTiming {
    session_id: String,
    mission_id: String,
    mode: String,
    round: u32,
    started_at: Instant,
    backend_prepare_ms: Option<u128>,
    tool_processing_ms: u128,
    continuation_count: u32,
    compaction_triggered: bool,
    tool_names: Vec<String>,
}
```

The collector should produce a serializable summary for logs and the `done` payload.

### LLM call timing

Extend `planner::preflight_chat` to return LLM timing alongside the response and token usage. Prefer a separate type instead of putting timing directly into `PreflightResponse`.

Conceptual return:

```rust
pub struct PreflightLlmTiming {
    pub first_activity_ms: Option<u128>,
    pub ttft_ms: Option<u128>,
    pub total_ms: u128,
    pub first_chunk_kind: Option<String>,
}
```

`first_activity_ms` should be set on the first `ReasoningDelta` or `TextDelta`. `ttft_ms` should be set only on the first `TextDelta`.

For tool-call-only responses, `ttft_ms` may be `None`; this is important diagnostic information.

### Done payload

The existing `done` payload should be extended compatibly:

```json
{
  "text": "...",
  "choices": [],
  "convergence_score": 0.42,
  "phase": "narrowing",
  "mode": "scenario_walk",
  "perf": {
    "backend_prepare_ms": 120,
    "llm_first_activity_ms": 900,
    "llm_ttft_ms": 1470,
    "llm_total_ms": 3900,
    "tool_processing_ms": 82,
    "continuation_count": 1,
    "turn_total_ms": 6120,
    "tool_names": ["add_contract_item", "present_choices"],
    "compaction_triggered": false
  }
}
```

Frontend code already tolerates extra JSON fields when parsing `done`, so this is a low-risk protocol extension.

### Structured log example

Each completed turn should produce one compact log:

```text
preflight turn perf: session_id=... mission_id=... mode=scenario_walk round=4 \
backend_prepare_ms=120 llm_first_activity_ms=900 llm_ttft_ms=1470 \
llm_total_ms=3900 tool_processing_ms=82 continuation_count=1 \
turn_total_ms=6120 tools=add_contract_item,present_choices \
input_tokens=5230 output_tokens=820 cache_read_tokens=3072 compaction_triggered=false
```

Errors should also log a summary with `status="error"` and whatever timings were available.

## Frontend design

### Local timing refs

`PreflightView` should own UI timing calculation because it already coordinates send, retry, mode switch, and stream events.

Suggested refs:

```ts
const turnStartedAtRef = useRef<number | null>(null);
const firstVisibleAtRef = useRef<number | null>(null);
const turnSourceRef = useRef<"initial" | "free_input" | "choice" | "mode_switch" | "retry" | null>(null);
```

Set these refs when the user initiates a turn:

- `handleSend` for free input,
- `handleChoiceSelect` before calling `handleSend`,
- `handleRetry`,
- `handleModeChange`,
- initial load can use backend `start` if no user-click timestamp exists.

### First visible feedback

For frontend UX measurement:

- first `text_delta` is visible feedback,
- first non-empty `status` can also count as visible feedback,
- initial typing indicator is already optimistic and should not be counted as model feedback,
- `reasoning_delta` should be logged separately or counted as activity only if the UI actually renders it.

### Console summary

On `done` or `error`, print a compact development log:

```text
[preflight ui perf] source=choice first_visible_ms=1650 done_ms=6900 backend_llm_ttft_ms=1470 continuation_count=1
```

If `done.perf` exists, merge backend and frontend data into one object for easier inspection.

## UX status design

Use existing `status` events for real progress states. The goal is not to show a detailed profiler to normal users; it is to avoid the feeling that the app is frozen.

Recommended status sequence examples:

- Before compaction: `对话较长，正在压缩上下文…`
- Before LLM call: `正在等待模型响应…`
- During tool-only continuation: `正在整理刚刚确认的内容…`
- During repository explorer calls: `正在读取仓库信息…`
- Before continuation question: `正在准备下一个问题…`

Status strings should clear on `text_delta` and `done`, matching current frontend behavior.

## Error handling

- If the LLM call fails before first text, log available timings and emit existing `error` behavior.
- Preserve existing retry semantics: retry reuses the last failed user/system_seed message and should start a new timing sample.
- If timing collection fails or has missing fields, never block the chat flow; missing metrics should serialize as `null` or be omitted.
- Tool-call-only turns should not fake `ttft_ms`; absence of text is meaningful.

## Testing strategy

### Backend unit tests

- Verify timing summary serialization handles missing `ttft_ms`.
- Verify continuation count increments only for extra iterations.
- Verify `done` payload remains backward compatible and includes `perf` when available.

### Frontend unit tests

- Verify first visible feedback is computed on first `text_delta`.
- Verify `status` can count as first visible feedback only when non-empty.
- Verify `done` with unknown extra fields still appends assistant messages and choices.
- Verify retry resets timing refs.

### Manual verification

Run one Pre-flight session and inspect logs for:

- one backend summary per turn,
- frontend console summary per user-visible turn,
- status text appearing during long phases,
- no duplicate user message on retry,
- no UI regression in choice rendering.

## Evolution guardrails

To keep Phase 1 from becoming a premature telemetry platform:

1. Keep the first collector local to Pre-flight.
2. Keep Phase 1 output in tracing logs and `done.perf` only.
3. Do not persist product metrics until there is a clear query/reporting need.
4. Use stable metric names now so benchmark integration can reuse them later.
5. Treat provider-level first-byte timings as correlation data, not user-facing TTFT.
6. Add a debug timeline only after logs show recurring patterns worth visualizing.

## Open implementation choices

The following choices should be made during implementation planning, not in this design:

1. Whether `done.perf` should include every LLM continuation timing or only aggregate values.
2. Whether frontend console perf logs are always enabled in development or gated behind a config flag.
3. Whether initial `start_preflight` should use a synthetic `source="initial"` UI timing sample when the user click timestamp is unavailable.

The design is intentionally compatible with either choice.
