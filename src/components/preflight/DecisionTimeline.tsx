import { useState, useEffect, useCallback } from "react";
import { commands } from "../../ipc/commands";
import type { DecisionEntry, DecisionType } from "../../ipc/commands";
import styles from "./DecisionTimeline.module.css";

interface DecisionTimelineProps {
  sessionId: string | null;
}

const TYPE_CONFIG: Record<DecisionType, { icon: string; label: string; style: string }> = {
  confirmed: { icon: "\u2713", label: "\u786E\u8BA4", style: styles.confirmed },
  rejected:  { icon: "\u2717", label: "\u5426\u51B3", style: styles.rejected },
  revised:   { icon: "\u21BB", label: "\u4FEE\u8BA2", style: styles.revised },
  inferred:  { icon: "\u2193", label: "\u63A8\u65AD", style: styles.inferred },
  skipped:   { icon: "\u2014", label: "\u8DF3\u8FC7", style: styles.skipped },
};

export function DecisionTimeline({ sessionId }: DecisionTimelineProps) {
  const [entries, setEntries] = useState<DecisionEntry[]>([]);
  const [expanded, setExpanded] = useState(false);

  const refresh = useCallback(() => {
    if (!sessionId) return;
    commands
      .getDecisionLog({ session_id: sessionId })
      .then(setEntries)
      .catch(console.error);
  }, [sessionId]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    if (!expanded) return;
    const timer = setInterval(refresh, 5000);
    return () => clearInterval(timer);
  }, [expanded, refresh]);

  if (!sessionId) return null;

  return (
    <div className={styles.wrapper}>
      <button
        className={styles.toggle}
        onClick={() => setExpanded((v) => !v)}
      >
        <span className={`${styles.chevron} ${expanded ? styles.chevronOpen : ""}`}>
          {"\u25B6"}
        </span>
        <span>{"\u51B3\u7B56\u65F6\u95F4\u7EBF"}</span>
        <span className={styles.count}>{entries.length}</span>
      </button>

      {expanded && (
        <div className={styles.timeline}>
          {entries.length === 0 ? (
            <div className={styles.empty}>{"\u5C1A\u65E0\u51B3\u7B56\u8BB0\u5F55"}</div>
          ) : (
            entries.map((entry) => {
              const cfg = TYPE_CONFIG[entry.decision_type] ?? TYPE_CONFIG.confirmed;
              return (
                <div key={entry.id} className={styles.entry}>
                  <div className={styles.icon}>{cfg.icon}</div>
                  <div className={styles.body}>
                    <div className={styles.description}>{entry.description}</div>
                    <div className={styles.meta}>
                      <span className={`${styles.typeBadge} ${cfg.style}`}>
                        {cfg.label}
                      </span>
                      <span>R{entry.round}</span>
                      {entry.rationale && <span>{entry.rationale}</span>}
                    </div>
                  </div>
                </div>
              );
            })
          )}
        </div>
      )}
    </div>
  );
}
