import { memo } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import styles from "./EventLine.module.css";

interface RecoveryLineProps {
  event: AgentEvent;
}

interface RecoveryMeta {
  silent?: boolean;
  trigger?: string;
  strategy?: string;
  attempt?: number;
  details?: Record<string, unknown>;
  error_excerpt?: string;
}

/// Single-Agent Uplift P0-3：silent recovery 事件渲染。
///
/// 仅在 Settings → Developer 开启 `showSilentRecoveryEvents` 后才会被
/// `EventLine` 渲染。默认隐藏遵循"agent 自救成功的事件不打扰用户"原则。
///
/// 视觉：浅灰色 chip，比 SystemHintLine 弱，避免与真正需要用户关注的 hint 混淆。
export const RecoveryLine = memo(function RecoveryLine({ event }: RecoveryLineProps) {
  const meta = (event.meta as RecoveryMeta | null | undefined) ?? {};
  const isAttempt = event.kind === "recovery_attempt";

  // attempt vs succeeded 用不同图标 + label 区分
  const label = isAttempt ? "Recovering" : "Recovered";
  const icon = isAttempt ? "↻" : "✓";

  return (
    <div className={styles.systemHint} title={meta.error_excerpt ?? ""}>
      <span className={styles.systemHintLabel}>
        {icon} {label}
      </span>
      <span>
        {event.content}
        {meta.trigger ? (
          <span className={styles.params}>
            {" "}
            ({meta.trigger}
            {meta.strategy ? ` · ${meta.strategy}` : ""})
          </span>
        ) : null}
      </span>
    </div>
  );
});
