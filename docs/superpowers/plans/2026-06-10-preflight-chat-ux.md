# Preflight Chat UX Improvements — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Three independent UX improvements: (1) collapsible reasoning panel showing model thinking during streaming, persisted to history; (2) markdown table CSS rendering in chat bubbles; (3) model tiering — flash for early rounds, pro for complex ones.

**Architecture:** Backend (Rust/Tauri) changes: accumulate reasoning in done payload + stored_msgs, tier model by round. Frontend (React/TypeScript) changes: new streamingReasoning state + reasoning panel component, shared markdown-table CSS module.

**Tech Stack:** Tauri IPC, React + TypeScript, CSS Modules, react-markdown v10, rusqlite

---

### Task 1: Add reasoning to done payload and stored_msgs persistence

**Files:**
- Modify: `src-tauri/src/commands/preflight.rs:535-604`

**Why:** The backend already emits `reasoning_delta` events and holds reasoning in `response.reasoning`. The missing piece: `reasoning` isn't included in the `done` payload sent to frontend, and `build_assistant_stored_msg` stores it as `reasoning_content` (for LLM round-trip) but not as a user-visible `reasoning` field.

- [ ] **Step 1: Add `reasoning` field to `build_done_payload`**

In `src-tauri/src/commands/preflight.rs`, modify `build_done_payload` to include response.reasoning:

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

    // Include reasoning so frontend can render the collapsible panel
    if !response.reasoning.is_empty() {
        done_payload["reasoning"] = json!(response.reasoning);
    }

    if let Some(perf) = perf {
        done_payload["perf"] = serde_json::to_value(perf).unwrap_or_else(|_| json!({}));
    }

    done_payload
}
```

- [ ] **Step 2: Add user-visible `reasoning` field to `build_assistant_stored_msg`**

In the same file, modify `build_assistant_stored_msg` to add a `reasoning` field (for frontend display) alongside the existing `reasoning_content` (for LLM round-trip):

```rust
fn build_assistant_stored_msg(
    response: &planner::PreflightResponse,
    mode: &str,
) -> serde_json::Value {
    let mut msg = json!({
        "role": "assistant",
        "content": response.text,
        "choices": response.choices,
        "mode": mode,
    });

    // reasoning_content: for LLM round-trip (OpenAI-compat protocol requirement)
    if !response.reasoning.is_empty() {
        msg["reasoning_content"] = json!(response.reasoning);
        // reasoning: for frontend display in collapsible panel
        msg["reasoning"] = json!(response.reasoning);
    }

    if !response.tool_calls.is_empty() {
        let tool_calls: Vec<serde_json::Value> = response
            .tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id,
                    "name": tc.name,
                    "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                })
            })
            .collect();
        msg["tool_calls"] = json!(tool_calls);
    }

    msg
}
```

- [ ] **Step 3: Add `reasoning` projection in `get_preflight_session`**

In `src-tauri/src/commands/preflight.rs`, modify the message projection at line ~1711 to include `reasoning`:

```rust
Some(PreflightMessageInfo {
    role: role.to_string(),
    content,
    choices,
    mode: m["mode"].as_str().map(|s| s.to_string()),
    failed: m["failed"].as_bool(),
    error: m["error"].as_str().map(|s| s.to_string()),
    reasoning: m["reasoning"].as_str().map(|s| s.to_string()),
})
```

- [ ] **Step 4: Build and verify compilation**

Run: `cd /Volumes/T7/Miragenty/src-tauri && cargo build 2>&1 | tail -5`
Expected: `Finished` without errors

- [ ] **Step 5: Commit**

```bash
cd /Volumes/T7/Miragenty && git add src-tauri/src/commands/preflight.rs && git commit -m "feat(preflight): add reasoning to done payload and stored_msgs persistence"
```

---

### Task 2: Add reasoning type and streaming state to frontend

**Files:**
- Modify: `src/ipc/commands.ts:515-525`
- Modify: `src/views/PreflightView.tsx`

**Why:** The frontend needs a TypeScript type for reasoning and state to accumulate streaming reasoning chunks.

- [ ] **Step 1: Add `reasoning` field to `PreflightMessageInfo`**

In `src/ipc/commands.ts`, add `reasoning` to the interface:

```typescript
export interface PreflightMessageInfo {
  role: "user" | "assistant";
  content: string;
  choices: PreflightChoice[];
  mode?: PreflightMode;
  failed?: boolean;
  error?: string;
  /** Reasoning / thinking content from the model (shown in collapsible panel). */
  reasoning?: string;
}
```

- [ ] **Step 2: Add `streamingReasoning` state and reasoning_delta handler in PreflightView**

In `src/views/PreflightView.tsx`:

Add state (after line 43, alongside existing streaming states):
```typescript
const [streamingReasoning, setStreamingReasoning] = useState("");
```

Add handler inside the `onPreflightStream` callback (after the `text_delta` handler, around line 112):
```typescript
} else if (kind === "reasoning_delta") {
  markFirstVisible();
  setStreamingReasoning((prev) => prev + content);
  setStatusText("");
  setInitialLoading(false);
}
```

In the `done` handler (line 133-167), attach reasoning to the saved message and clear streaming reasoning:
```typescript
} else if (kind === "done") {
  setStreaming(false);
  setStreamingText("");
  setStreamingReasoning("");
  setStatusText("");
  setInitialLoading(false);
  try {
    const parsed = JSON.parse(content);
    const backendPerf = (parsed.perf ?? null) as PreflightPerfSummary | null;
    finishUiTurn(backendPerf);
    setMessages((prev) => [
      ...prev,
      {
        role: "assistant",
        content: parsed.text,
        choices: parsed.choices ?? [],
        mode: parsed.mode ?? undefined,
        reasoning: parsed.reasoning ?? undefined,
      },
    ]);
    // ... rest of existing done handler
```

In the `error` handler, clear `streamingReasoning`:
```typescript
} else if (kind === "error") {
  setStreaming(false);
  setStreamingText("");
  setStreamingReasoning("");
  setInitialLoading(false);
  // ... rest of existing error handler
```

- [ ] **Step 3: Pass `streamingReasoning` to PreflightChat**

In the JSX (line ~424), add the prop:
```typescript
<PreflightChat
  messages={messages}
  mode={mode}
  streaming={streaming}
  streamingText={streamingText}
  streamingReasoning={streamingReasoning}
  statusText={statusText}
  initialLoading={initialLoading}
  onSend={handleSend}
  onModeChange={handleModeChange}
  onChoiceSelect={handleChoiceSelect}
  onRetry={handleRetry}
/>
```

- [ ] **Step 4: Add prop to PreflightChatProps interface**

In `src/components/preflight/PreflightChat.tsx`, add to the interface (line ~10):
```typescript
interface PreflightChatProps {
  messages: PreflightMessageInfo[];
  mode: PreflightMode;
  streaming: boolean;
  streamingText: string;
  streamingReasoning?: string;
  statusText?: string;
  initialLoading?: boolean;
  onSend: (text: string) => void;
  onModeChange: (mode: PreflightMode) => void;
  onChoiceSelect: (choice: PreflightChoice) => void;
  onRetry?: () => void;
}
```

Destructure in the component (line ~24):
```typescript
export function PreflightChat({
  messages,
  mode,
  streaming,
  streamingText,
  streamingReasoning,
  statusText,
  initialLoading,
  onSend,
  onModeChange,
  onChoiceSelect,
  onRetry,
}: PreflightChatProps) {
```

- [ ] **Step 5: Verify TypeScript compilation**

Run: `cd /Volumes/T7/Miragenty && npx tsc --noEmit 2>&1 | head -20`
Expected: No errors related to the changes above

- [ ] **Step 6: Commit**

```bash
cd /Volumes/T7/Miragenty && git add src/ipc/commands.ts src/views/PreflightView.tsx src/components/preflight/PreflightChat.tsx && git commit -m "feat(preflight): add reasoning streaming state to frontend"
```

---

### Task 3: Build collapsible reasoning panel component

**Files:**
- Create: `src/components/preflight/ReasoningPanel.tsx`
- Create: `src/components/preflight/ReasoningPanel.module.css`

**Why:** The reasoning content needs a dedicated collapsible panel rendered above the markdown body in both ChatMessage (historical messages) and PreflightChat (streaming bubble).

- [ ] **Step 1: Create ReasoningPanel.module.css**

Create `src/components/preflight/ReasoningPanel.module.css`:

```css
.panel {
  margin-bottom: 8px;
  border: 1px solid #e5e7eb;
  border-radius: 6px;
  overflow: hidden;
  font-size: 0.85em;
}

.summary {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 10px;
  background: #f0f4ff;
  cursor: pointer;
  user-select: none;
  font-size: 12px;
  font-weight: 600;
  color: #2563eb;
}

.summary::-webkit-details-marker {
  display: none;
}

.chevron {
  font-size: 10px;
  transition: transform 0.15s ease;
  flex-shrink: 0;
}

details[open] .chevron {
  transform: rotate(90deg);
}

.label {
  flex: 1;
}

.timer {
  font-weight: 400;
  color: #93c5fd;
  font-size: 11px;
}

.body {
  padding: 8px 10px;
  font-size: 0.9em;
  line-height: 1.55;
  color: #6b7280;
  border-left: 2px solid #d1d5db;
  margin-left: 10px;
  font-style: italic;
  white-space: pre-wrap;
  word-break: break-word;
}
```

- [ ] **Step 2: Create ReasoningPanel.tsx**

Create `src/components/preflight/ReasoningPanel.tsx`:

```typescript
import { useState, useEffect } from "react";
import styles from "./ReasoningPanel.module.css";

interface ReasoningPanelProps {
  reasoning: string;
  isStreaming?: boolean;
  streamingStartTime?: number;
}

export function ReasoningPanel({
  reasoning,
  isStreaming = false,
  streamingStartTime,
}: ReasoningPanelProps) {
  const [elapsed, setElapsed] = useState(0);

  useEffect(() => {
    if (!isStreaming || !streamingStartTime) return;
    const interval = setInterval(() => {
      setElapsed(Date.now() - streamingStartTime);
    }, 100);
    return () => clearInterval(interval);
  }, [isStreaming, streamingStartTime]);

  const label = isStreaming ? "深度思考中" : "已完成思考";
  const elapsedSec = (elapsed / 1000).toFixed(1);

  return (
    <details className={styles.panel} open={isStreaming}>
      <summary className={styles.summary}>
        <span className={styles.chevron}>▶</span>
        <span className={styles.label}>{label}</span>
        {isStreaming && (
          <span className={styles.timer}>&middot; {elapsedSec}s</span>
        )}
      </summary>
      <div className={styles.body}>{reasoning}</div>
    </details>
  );
}
```

- [ ] **Step 3: Verify TypeScript compilation**

Run: `cd /Volumes/T7/Miragenty && npx tsc --noEmit 2>&1 | head -20`
Expected: No errors

- [ ] **Step 4: Commit**

```bash
cd /Volumes/T7/Miragenty && git add src/components/preflight/ReasoningPanel.tsx src/components/preflight/ReasoningPanel.module.css && git commit -m "feat(preflight): add collapsible reasoning panel component"
```

---

### Task 4: Integrate reasoning panel into ChatMessage and PreflightChat

**Files:**
- Modify: `src/components/preflight/ChatMessage.tsx`
- Modify: `src/components/preflight/PreflightChat.tsx`

**Why:** ChatMessage renders historical assistant messages (with possible reasoning from stored_msgs). PreflightChat renders the streaming bubble (with live reasoning).

- [ ] **Step 1: Integrate reasoning panel into ChatMessage.tsx**

Modify the props and render in `src/components/preflight/ChatMessage.tsx`:

```typescript
import Markdown from "react-markdown";
import { useTranslation } from "react-i18next";
import type { PreflightMode } from "../../ipc/commands";
import { ReasoningPanel } from "./ReasoningPanel";
import styles from "./ChatMessage.module.css";

interface ChatMessageProps {
  role: "user" | "assistant";
  content: string;
  mode?: PreflightMode;
  reasoning?: string;
}

const MODE_STYLE: Record<string, string> = {
  scenario_walk: styles.modeScenario,
  devils_advocate: styles.modeDevil,
  risk_highlighter: styles.modeRisk,
};

export function ChatMessage({ role, content, mode, reasoning }: ChatMessageProps) {
  const { t } = useTranslation("preflight");
  const isUser = role === "user";
  const modeClass = !isUser && mode ? (MODE_STYLE[mode] ?? "") : "";
  const className = `${styles.message} ${isUser ? styles.user : styles.agent} ${modeClass}`;

  return (
    <div className={className}>
      <div className={styles.label}>
        {isUser ? t("userLabel") : t("agentLabel")}
        {!isUser && mode && mode !== "scenario_walk" && (
          <span className={styles.modeBadge}>{t(`modeLabel.${mode}`)}</span>
        )}
      </div>
      <div className={styles.bubble}>
        {isUser ? content : (
          <div className={styles.markdown}>
            {reasoning && <ReasoningPanel reasoning={reasoning} />}
            <Markdown>{content}</Markdown>
          </div>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Pass reasoning prop to ChatMessage in PreflightChat.tsx**

In the messages loop in `src/components/preflight/PreflightChat.tsx` (line ~101), pass `reasoning`:

```typescript
<ChatMessage
  role={msg.role as "user" | "assistant"}
  content={msg.content}
  mode={msg.mode}
  reasoning={msg.reasoning}
/>
```

- [ ] **Step 3: Add streaming reasoning panel to PreflightChat.tsx**

After the streaming label (line ~136), add reasoning panel before the streaming content. Add import at top of file:

```typescript
import { ReasoningPanel } from "./ReasoningPanel";
```

Add `streamingStartRef` to track when streaming reasoning starts:

```typescript
const streamingStartRef = useRef<number | null>(null);
```

Add effect to reset timer when streaming reasoning begins:
(Inside the component, after other refs)

```typescript
// Track when streaming reasoning starts for the timer
useEffect(() => {
  if (streamingReasoning && !streamingStartRef.current) {
    streamingStartRef.current = Date.now();
  }
  if (!streamingReasoning) {
    streamingStartRef.current = null;
  }
}, [streamingReasoning]);
```

Modify the streaming section (lines ~134-142) to show reasoning panel:

```typescript
{streaming && (streamingText || streamingReasoning) && (
  <div className={styles.streamingText}>
    <div className={styles.streamingLabel}>{t("agentLabel")}</div>
    <div className={styles.streamingBubble}>
      {streamingReasoning && (
        <ReasoningPanel
          reasoning={streamingReasoning}
          isStreaming
          streamingStartTime={streamingStartRef.current ?? undefined}
        />
      )}
      {streamingText && (
        <>
          <Markdown>{streamingText}</Markdown>
          <span className={styles.streamEllipsis} />
        </>
      )}
    </div>
  </div>
)}
```

- [ ] **Step 4: Update the showTypingIndicator condition**

The typing dots should show when streaming is active but neither reasoning nor text has arrived yet. Update line ~65:

```typescript
const showTypingIndicator = (streaming && !streamingText && !streamingReasoning) || initialLoading;
```

- [ ] **Step 5: Verify TypeScript compilation**

Run: `cd /Volumes/T7/Miragenty && npx tsc --noEmit 2>&1 | head -20`
Expected: No errors

- [ ] **Step 6: Commit**

```bash
cd /Volumes/T7/Miragenty && git add src/components/preflight/ChatMessage.tsx src/components/preflight/PreflightChat.tsx && git commit -m "feat(preflight): integrate reasoning panel into chat bubbles"
```

---

### Task 5: Add markdown table CSS (shared styles)

**Files:**
- Create: `src/components/preflight/markdown-shared.module.css`
- Modify: `src/components/preflight/ChatMessage.module.css`
- Modify: `src/components/preflight/PreflightChat.module.css`

**Why:** Tables overflow the 386px bubble and have no styling. Solution: shared table CSS composable into both modules.

- [ ] **Step 1: Create markdown-shared.module.css**

Create `src/components/preflight/markdown-shared.module.css`:

```css
.tableWrapper {
  overflow-x: auto;
  -webkit-overflow-scrolling: touch;
  margin: 6px 0;
}

.tableWrapper table {
  border-collapse: collapse;
  width: max-content;
  min-width: 100%;
  font-size: 0.85em;
}

.tableWrapper th {
  border: 1px solid #e5e7eb;
  padding: 5px 8px;
  background: #f9fafb;
  font-weight: 600;
  text-align: left;
  white-space: nowrap;
}

.tableWrapper td {
  border: 1px solid #f3f4f6;
  padding: 4px 8px;
}

.tableWrapper tr:nth-child(even) td {
  background: #fafafa;
}
```

- [ ] **Step 2: Add table styles to ChatMessage.module.css**

Append to `src/components/preflight/ChatMessage.module.css`:

```css
/* Shared table styles — composable with markdown-shared */
.tableWrapper {
  composes: tableWrapper from "./markdown-shared.module.css";
}
```

Note: CSS Modules `composes` requires the referenced file to be a CSS Module. If the build tool doesn't support cross-file `composes`, use an alternative approach — just add the styles directly. Let's use direct inclusion instead:

Actually, append these styles directly to `src/components/preflight/ChatMessage.module.css`:

```css
/* Markdown table rendering */
.tableWrapper {
  overflow-x: auto;
  -webkit-overflow-scrolling: touch;
  margin: 6px 0;
}

.tableWrapper table {
  border-collapse: collapse;
  width: max-content;
  min-width: 100%;
  font-size: 0.85em;
}

.tableWrapper th {
  border: 1px solid #e5e7eb;
  padding: 5px 8px;
  background: #f9fafb;
  font-weight: 600;
  text-align: left;
  white-space: nowrap;
}

.tableWrapper td {
  border: 1px solid #f3f4f6;
  padding: 4px 8px;
}

.tableWrapper tr:nth-child(even) td {
  background: #fafafa;
}
```

- [ ] **Step 3: Add same table styles to PreflightChat.module.css**

Append the identical block from Step 2 to `src/components/preflight/PreflightChat.module.css`.

The shared file `markdown-shared.module.css` will serve as documentation / the canonical source, even if build tool limitations prevent true CSS Module composition. Both ChatMessage.module.css and PreflightChat.module.css contain the same rules.

- [ ] **Step 4: Wrap markdown tables in ChatMessage.tsx**

In `src/components/preflight/ChatMessage.tsx`, for table rendering to work, react-markdown needs a custom `components` prop that wraps `<table>` elements:

```typescript
import Markdown from "react-markdown";
import type { Components } from "react-markdown";
// ... existing imports

const markdownComponents: Components = {
  table: ({ children, ...props }) => (
    <div className={styles.tableWrapper}>
      <table {...props}>{children}</table>
    </div>
  ),
};
```

Then update the Markdown usage:

```typescript
<Markdown components={markdownComponents}>{content}</Markdown>
```

- [ ] **Step 5: Wrap markdown tables in PreflightChat.tsx**

Import `markdownComponents` pattern in `src/components/preflight/PreflightChat.tsx`. Since we can't directly import the components object from ChatMessage (different CSS module class), define an equivalent in PreflightChat:

```typescript
import type { Components } from "react-markdown";
// ... existing imports

const streamingMarkdownComponents: Components = {
  table: ({ children, ...props }) => (
    <div className={styles.tableWrapper}>
      <table {...props}>{children}</table>
    </div>
  ),
};
```

Update the streaming `<Markdown>`:

```typescript
<Markdown components={streamingMarkdownComponents}>{streamingText}</Markdown>
```

- [ ] **Step 6: Verify TypeScript compilation**

Run: `cd /Volumes/T7/Miragenty && npx tsc --noEmit 2>&1 | head -20`
Expected: No errors

- [ ] **Step 7: Commit**

```bash
cd /Volumes/T7/Miragenty && git add src/components/preflight/markdown-shared.module.css src/components/preflight/ChatMessage.module.css src/components/preflight/PreflightChat.module.css src/components/preflight/ChatMessage.tsx src/components/preflight/PreflightChat.tsx && git commit -m "fix(preflight): add markdown table CSS with horizontal scroll"
```

---

### Task 6: Model tiering — flash for early rounds

**Files:**
- Modify: `src-tauri/src/commands/preflight.rs:615-690`

**Why:** DeepSeek-V4-Pro prompt caching yields 0 cache hits (provider limitation). Use fast flash model for rounds 1-2 where questions are simple, switch to pro for round 3+.

- [ ] **Step 1: Add model tiering logic in preflight_with_continuation**

In `src-tauri/src/commands/preflight.rs`, in `preflight_with_continuation`, add model tiering before the LLM call. After the `belief_state_snapshot` is loaded (around line 692), compute the effective model:

```rust
// Model tiering: use flash for early rounds (1-2) to avoid reasoning latency,
// switch to pro for round 3+ when complex reasoning is needed.
let effective_model = if belief_state_snapshot.round <= 2 {
    let flash_model = model.replace("-pro", "-flash");
    if flash_model != *model {
        tracing::info!(
            round = belief_state_snapshot.round,
            original_model = model,
            effective_model = %flash_model,
            "model tiering: using flash for early round"
        );
        flash_model
    } else {
        model.to_string()
    }
} else {
    model.to_string()
};
```

- [ ] **Step 2: Pass effective_model to preflight_chat**

Find the call to `planner::preflight_chat(...)` inside the loop (around line 840-860). Replace the `model` parameter with `&effective_model`:

In the existing call, change:
```rust
planner::preflight_chat(
    provider.clone(),
    model,                    // change this
```
to:
```rust
planner::preflight_chat(
    provider.clone(),
    &effective_model,         // to this
```

- [ ] **Step 3: Build and verify compilation**

Run: `cd /Volumes/T7/Miragenty/src-tauri && cargo build 2>&1 | tail -5`
Expected: `Finished` without errors

- [ ] **Step 4: Commit**

```bash
cd /Volumes/T7/Miragenty && git add src-tauri/src/commands/preflight.rs && git commit -m "feat(preflight): model tiering — flash for rounds 1-2, pro for 3+"
```

---

### Task 7: End-to-end verification

**Files:** None (verification only)

- [ ] **Step 1: Run Rust tests**

Run: `cd /Volumes/T7/Miragenty/src-tauri && cargo test --lib 2>&1 | tail -15`
Expected: All tests pass

- [ ] **Step 2: Run TypeScript type check**

Run: `cd /Volumes/T7/Miragenty && npx tsc --noEmit 2>&1 | head -20`
Expected: No errors (may have pre-existing errors unrelated to these changes)

- [ ] **Step 3: Run frontend tests**

Run: `cd /Volumes/T7/Miragenty && npx vitest run 2>&1 | tail -15`
Expected: All tests pass (specifically `preflight-perf.test.ts`)

- [ ] **Step 4: Manual test checklist**
  - Start a new preflight session, send a message
  - Verify: typing dots appear briefly, then "深度思考中" panel appears with reasoning text
  - Verify: reasoning panel shows elapsed time ticking
  - Verify: after done, panel collapses to "已完成思考"
  - Verify: refresh page → reasoning still visible in historical messages
  - Verify: send a message with a markdown table → table has borders, alternating rows, horizontal scroll
  - Verify: rounds 1-2 use flash (faster response, no reasoning panel)
  - Verify: round 3+ uses pro (reasoning panel appears)
  - Check dev console: `[preflight ui perf]` logs show flash latency ~1-2s for early rounds

- [ ] **Step 5: Commit (if any final fixes needed)**

```bash
cd /Volumes/T7/Miragenty && git add -A && git commit -m "chore(preflight): final verification fixes"
```
