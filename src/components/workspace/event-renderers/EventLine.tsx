import { memo } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import styles from "./EventLine.module.css";
import { ToolUseLine } from "./ToolUseLine";
import { ToolResultLine } from "./ToolResultLine";
import { SystemHintLine } from "./SystemHintLine";
import { GuardrailLine } from "./GuardrailLine";
import { ToolProgressLine } from "./ToolProgressLine";
import { NoteAppliedLine } from "./NoteAppliedLine";

interface EventLineProps {
  event: AgentEvent;
  isLast: boolean;
  isRunning: boolean;
}

const PLAIN_KIND_CLASS: Partial<Record<AgentEvent["kind"], string>> = {
  llm_call: styles.plainLlmCall,
  checkpoint: styles.plainCheckpoint,
  status_change: styles.plainStatusChange,
  message: styles.plainMessage,
  error: styles.plainError,
};

/// Dispatcher: 按 event.kind 选 renderer。新加 kind 时记得：
///   ① 在 src/stores/agent-store.ts 的 AgentEventKind 加联合
///   ② 在 src-tauri/src/db/migrations.rs 末尾追加 migration 扩 CHECK
///   ③ 这里 switch 加分支
export const EventLine = memo(function EventLine({
  event,
  isLast,
  isRunning,
}: EventLineProps) {
  switch (event.kind) {
    case "tool_use":
      return <ToolUseLine event={event} />;
    case "tool_result":
      // 注意：error kind 走 plain 分支（保留原来的红色行为；error 含义比 tool_result 广，
      // 也包含 LLM 异常之类的非工具错误）。tool 失败仍会正常走 ToolResult，因为后端
      // 在 is_error=true 时 emit kind="error" + meta.is_error=true。改：让 error kind
      // 也命中 ToolResultLine 当 meta 里带 tool 字段时——更人类。
      return <ToolResultLine event={event} />;
    case "error":
      // meta.tool 在则视作工具错误，走 ToolResultLine；否则降级到 plain error 行
      if (
        event.meta &&
        typeof event.meta === "object" &&
        (event.meta as { tool?: string }).tool
      ) {
        return <ToolResultLine event={event} />;
      }
      break;
    case "system_hint":
      return <SystemHintLine event={event} />;
    case "guardrail_pass":
    case "guardrail_fail":
    case "guardrail_summary":
      return <GuardrailLine event={event} />;
    case "tool_progress":
      return <ToolProgressLine event={event} />;
    case "note_applied":
      return <NoteAppliedLine event={event} />;
    case "compact":
    case "tool_summary":
    case "todo_update":
    case "review":
      // Phase 1.2 / 2.x 接管。当前先走 plain 行展示，避免编译期 exhaustive 错。
      break;
    default:
      break;
  }

  const cls = PLAIN_KIND_CLASS[event.kind] ?? styles.plainMessage;
  const cursorCls = isLast && isRunning ? ` ${styles.cursor}` : "";
  return <div className={`${styles.plain} ${cls}${cursorCls}`}>{event.content}</div>;
});
