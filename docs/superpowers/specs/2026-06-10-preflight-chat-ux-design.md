# Preflight Chat UX: Reasoning Panel, Table Rendering, Model Tiering

**Date**: 2026-06-10
**Status**: design-approved
**Scope**: 3 independent changes to preflight chat frontend + backend

## 1. Collapsible Reasoning Panel

### Problem
`reasoning_delta` events flow from backend → frontend but are silently dropped. Users see a blank "思考中..." typing indicator for 15-43s while DeepSeek-V4-Pro performs internal reasoning.

### Solution
Accumulate reasoning content during streaming and render it in a collapsible panel above the main response text inside the assistant bubble.

### Backend

**`done` payload** (`build_done_payload` in `commands/preflight.rs`):
- Add `reasoning: String` field — accumulated from all `reasoning_delta` chunks during the turn

**Persistence** (`stored_msgs`):
- Assistant messages stored via the preflight session save path gain a `reasoning` field

### Frontend

**State** (`PreflightView.tsx`):
- New `streamingReasoning: string` state
- `reasoning_delta` handler: `setStreamingReasoning(prev => prev + content)`
- `done` handler: attach reasoning to the saved assistant message, clear `streamingReasoning`
- Pass `streamingReasoning` to `PreflightChat`

**Type** (`commands.ts`):
- `PreflightMessageInfo` gains optional `reasoning?: string`

**Rendering** (`ChatMessage.tsx`):
- If message has `reasoning`, render expandable panel ABOVE the markdown body
- Header row: `▼ 深度思考中 · X.Xs` (chevron + label + elapsed timer)
- Body: gray italic text, `font-size: 0.85em`, `border-left: 2px solid #d1d5db`, `color: #6b7280`
- Uses native `<details>` + `<summary>` for collapse/expand
- Streaming: default open, timer ticks every 100ms until done
- Done: collapsed by default, header shows "已完成思考"

**Timer**: Simple `useEffect` with 100ms interval while `streaming && streamingReasoning` is non-empty. Display `(elapsed / 1000).toFixed(1) + "s"`.

### No streaming reasoning for flash model
When model tiering switches to flash (rounds 1-2), there are no `reasoning_delta` events, so no panel renders. This is correct — flash has no reasoning phase.

---

## 2. Markdown Table Rendering

### Problem
- `.markdown` container and `.streamingBubble` have zero CSS rules for `<table>`, `<th>`, `<td>`, `<thead>`, `<tbody>`
- Tables overflow the 386px bubble with no scroll
- No borders, no alternating row colors — raw browser defaults

### Solution
Create shared CSS module with table styles, import into both `ChatMessage.module.css` and `PreflightChat.module.css`.

### New file: `src/components/preflight/markdown-shared.module.css`

```css
.markdownTable {
  overflow-x: auto;
  -webkit-overflow-scrolling: touch;
}
.markdownTable table {
  border-collapse: collapse;
  width: max-content;
  min-width: 100%;
  font-size: 0.85em;
}
.markdownTable th {
  border: 1px solid #e5e7eb;
  padding: 5px 8px;
  background: #f9fafb;
  font-weight: 600;
  text-align: left;
}
.markdownTable td {
  border: 1px solid #f3f4f6;
  padding: 4px 8px;
}
.markdownTable tr:nth-child(even) td {
  background: #fafafa;
}
```

### Integration

**`ChatMessage.tsx`**: Wrap markdown in `<div className={sharedStyles.markdownTable}>` — the existing `.markdown` class is on the outer container, the table wrapper goes inside.

**`PreflightChat.tsx`**: Same wrapper around the streaming markdown content in `.streamingBubble`.

No changes to `react-markdown` — it already parses GFM tables. This is purely CSS.

---

## 3. Model Tiering (P3)

### Problem
DeepSeek-V4-Pro prompt caching returns `cache_read_input_tokens=0` — the reseller doesn't support it. Cannot be fixed client-side.

### Solution
Use `deepseek-v4-flash` for early rounds (1-2) where conversation is simple, switch to `deepseek-v4-pro` for round 3+ when complex reasoning is needed.

Flash has no reasoning phase → ~1.5s TTFT vs 15-43s for pro. First two rounds dominate the user's waiting experience.

### Implementation (`commands/preflight.rs`)

In the continuation loop (`preflight_with_continuation`), before constructing the LLM request:

```rust
let effective_model = if belief_state.round <= 2 {
    model.replace("-pro", "-flash")
} else {
    model.to_string()
};
```

Simple string substitution. If the configured model doesn't contain `-pro`, no replacement occurs (safe fallback).

### Constraints
- Flash max tokens may need adjustment (pro uses 4096, flash default should be fine)
- No reasoning_delta events for flash rounds → reasoning panel naturally absent
- Flash may produce slightly less nuanced responses for early exploration — acceptable tradeoff for 10-20x latency improvement
