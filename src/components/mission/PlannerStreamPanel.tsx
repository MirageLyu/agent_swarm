import { useRef, useEffect } from "react";
import styles from "./PlannerStreamPanel.module.css";

export interface PlannerStreamState {
  visible: boolean;
  text: string;
  tokenCount: number;
  elapsedMs: number;
  status: "streaming" | "done" | "error" | "cancelled";
  collapsed: boolean;
  errorMessage?: string;
}

interface PlannerStreamPanelProps {
  state: PlannerStreamState;
  onToggleCollapse: () => void;
  fullHeight?: boolean;
}

export function PlannerStreamPanel({
  state,
  onToggleCollapse,
  fullHeight,
}: PlannerStreamPanelProps) {
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!state.collapsed && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [state.text, state.collapsed]);

  if (!state.visible) return null;

  const elapsed = (state.elapsedMs / 1000).toFixed(0);

  return (
    <div className={`${styles.container} ${fullHeight ? styles.containerFull : ""}`}>
      <button className={styles.header} onClick={onToggleCollapse}>
        <span className={styles.headerLeft}>
          {state.status === "streaming" && (
            <span className={styles.dot} />
          )}
          <span className={styles.label}>
            {state.status === "streaming"
              ? "Thinking…"
              : state.status === "done"
                ? "Planning complete"
                : state.status === "error"
                  ? "Error"
                  : "Cancelled"}
          </span>
        </span>
        <span className={styles.stats}>
          {state.tokenCount > 0 && (
            <span className={styles.stat}>{state.tokenCount} tokens</span>
          )}
          <span className={styles.stat}>{elapsed}s</span>
          <span className={styles.chevron}>
            {state.collapsed ? "▸" : "▾"}
          </span>
        </span>
      </button>

      {!state.collapsed && (
        <div
          ref={scrollRef}
          className={`${styles.body} ${fullHeight ? styles.bodyFull : ""} ${state.status === "error" ? styles.bodyError : ""}`}
        >
          <pre className={styles.text}>
            {state.text || (state.errorMessage ?? "")}
          </pre>
          {state.status === "streaming" && state.elapsedMs > 30000 && !state.text && (
            <p className={styles.slowWarning}>Connection may be slow…</p>
          )}
        </div>
      )}
    </div>
  );
}
