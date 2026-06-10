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
    }, 500);
    return () => clearInterval(interval);
  }, [isStreaming, streamingStartTime]);

  const label = isStreaming ? "深度思考中" : "已完成思考";
  const elapsedSec = (elapsed / 1000).toFixed(1);

  return (
    <details className={styles.panel} open={isStreaming ? true : undefined}>
      <summary className={styles.summary}>
        <span className={styles.chevron} aria-hidden="true">▶</span>
        <span className={styles.label}>{label}</span>
        {isStreaming && (
          <span className={styles.timer}>&middot; {elapsedSec}s</span>
        )}
      </summary>
      <div className={styles.body}>{reasoning}</div>
    </details>
  );
}
