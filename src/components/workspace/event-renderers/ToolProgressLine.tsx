import { memo } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import styles from "./EventLine.module.css";

interface ToolProgressLineProps {
  event: AgentEvent;
}

interface LlmIdleMeta {
  kind?: string;
  idle_secs?: number;
  step?: number;
}

function asLlmIdle(meta: unknown): LlmIdleMeta | null {
  if (typeof meta !== "object" || meta === null) return null;
  return meta as LlmIdleMeta;
}

/// LLM idle heartbeat —— 用户看到这条意味着"agent 还活着，只是 LLM 在憋大招"。
/// 配脉冲点保持视觉活跃；不带 timeline 颜色，弱化避免污染信息流。
export const ToolProgressLine = memo(function ToolProgressLine({
  event,
}: ToolProgressLineProps) {
  const idle = asLlmIdle(event.meta);
  const text = idle?.idle_secs
    ? `LLM idle for ${idle.idle_secs}s — still waiting…`
    : event.content;
  return (
    <div className={styles.progress}>
      <span className={styles.progressDot} />
      <span>{text}</span>
    </div>
  );
});
