import { memo } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import { useUiStore } from "../../../stores/ui-store";
import styles from "./EventLine.module.css";
import { ToolUseLine } from "./ToolUseLine";
import { ToolResultLine } from "./ToolResultLine";
import { SystemHintLine } from "./SystemHintLine";
import { GuardrailLine } from "./GuardrailLine";
import { ToolProgressLine } from "./ToolProgressLine";
import { NoteAppliedLine } from "./NoteAppliedLine";
import { AskUserQuestionLine } from "./AskUserQuestionLine";
import { ToolSummaryLine } from "./ToolSummaryLine";
import { RecoveryLine } from "./RecoveryLine";

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
  // P0-3: silent recovery 事件默认隐藏；showSilentRecoveryEvents toggle 打开时显示。
  // 不在 switch 里判定是因为 zustand selector 要放在组件顶层（hook 规则），
  // 而 switch 里 return null 已经足够——React 不会渲染。
  const showSilentRecoveryEvents = useUiStore((s) => s.showSilentRecoveryEvents);

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
    case "system_hint": {
      // B1: ask_user_question 卡片在 system_hint 之上分流。
      // resolved 事件不渲染卡片（仅作为 store 副作用更新），返回 null。
      const meta = event.meta as { kind?: string } | null | undefined;
      if (meta && meta.kind === "ask_user_question") {
        return <AskUserQuestionLine event={event} />;
      }
      if (meta && meta.kind === "ask_user_question_resolved") {
        return null;
      }
      return <SystemHintLine event={event} />;
    }
    case "guardrail_pass":
    case "guardrail_fail":
    case "guardrail_summary":
      return <GuardrailLine event={event} />;
    case "tool_progress":
      return <ToolProgressLine event={event} />;
    case "note_applied":
      return <NoteAppliedLine event={event} />;
    case "tool_summary":
      return <ToolSummaryLine event={event} />;
    case "compact":
    case "todo_update":
    case "review":
      // Phase 1.2 / 2.x 接管。当前先走 plain 行展示，避免编译期 exhaustive 错。
      break;
    case "recovery_attempt":
    case "recovery_succeeded": {
      // P0-3: meta.silent=true 时默认不渲染（agent 自救成功的事件不打扰用户）。
      // toggle 打开后用 RecoveryLine 展示为浅灰色 chip，给 developer debug 用。
      const silent = (event.meta as { silent?: boolean } | null | undefined)?.silent === true;
      if (silent && !showSilentRecoveryEvents) return null;
      return <RecoveryLine event={event} />;
    }
    default:
      break;
  }

  const cls = PLAIN_KIND_CLASS[event.kind] ?? styles.plainMessage;
  const cursorCls = isLast && isRunning ? ` ${styles.cursor}` : "";
  return <div className={`${styles.plain} ${cls}${cursorCls}`}>{event.content}</div>;
});
