import { memo } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import styles from "./EventLine.module.css";

interface ToolSummaryMeta {
  tool?: string;
  from_chars?: number;
  to_chars?: number;
  duration_ms?: number;
  model?: string;
}

function isMeta(value: unknown): value is ToolSummaryMeta {
  return value !== null && typeof value === "object";
}

/**
 * Single-Agent Uplift B2: 渲染 tool_summary 事件。
 *
 * 这条信息对用户的价值是"看到优化在生效"——压了多少、用了哪个模型、花了多久。
 * 视觉上靠近 tool_progress（次要信息，不该抢戏），用蓝色 hint 块。
 */
export const ToolSummaryLine = memo(function ToolSummaryLine({
  event,
}: {
  event: AgentEvent;
}) {
  const meta = isMeta(event.meta) ? event.meta : {};
  const from = meta.from_chars;
  const to = meta.to_chars;
  const ratio =
    typeof from === "number" && typeof to === "number" && from > 0
      ? Math.round((to / from) * 100)
      : null;

  return (
    <div className={styles.progress}>
      <span>📦</span>
      <span>
        compressed tool output
        {typeof from === "number" && typeof to === "number" ? (
          <>
            {" "}
            <strong>{(from / 1024).toFixed(1)}KB → {to} chars</strong>
            {ratio !== null && <> ({ratio}%)</>}
          </>
        ) : null}
        {typeof meta.duration_ms === "number" && (
          <span className={styles.duration}>{meta.duration_ms} ms</span>
        )}
        {meta.model && (
          <span className={styles.duration} style={{ marginLeft: 8 }}>
            via {meta.model}
          </span>
        )}
      </span>
    </div>
  );
});
