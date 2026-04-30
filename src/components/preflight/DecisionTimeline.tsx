import { useState, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "../../ipc/commands";
import type { DecisionEntry, DecisionType } from "../../ipc/commands";
import styles from "./DecisionTimeline.module.css";

interface DecisionTimelineProps {
  sessionId: string | null;
}

const TYPE_VISUAL: Record<DecisionType, { icon: string; style: string }> = {
  confirmed: { icon: "\u2713", style: styles.confirmed },
  rejected:  { icon: "\u2717", style: styles.rejected },
  revised:   { icon: "\u21BB", style: styles.revised },
  inferred:  { icon: "\u2193", style: styles.inferred },
  skipped:   { icon: "\u2014", style: styles.skipped },
};

export function DecisionTimeline({ sessionId }: DecisionTimelineProps) {
  const { t } = useTranslation("preflight");
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
        <span>{t("decisionTimeline.title")}</span>
        <span className={styles.count}>{entries.length}</span>
      </button>

      {expanded && (
        <div className={styles.timeline}>
          {entries.length === 0 ? (
            <div className={styles.empty}>{t("decisionTimeline.empty")}</div>
          ) : (
            entries.map((entry) => {
              const cfg = TYPE_VISUAL[entry.decision_type] ?? TYPE_VISUAL.confirmed;
              return (
                <div key={entry.id} className={styles.entry}>
                  <div className={styles.icon}>{cfg.icon}</div>
                  <div className={styles.body}>
                    <div className={styles.description}>{entry.description}</div>
                    <div className={styles.meta}>
                      <span className={`${styles.typeBadge} ${cfg.style}`}>
                        {t(`decisionTimeline.type.${entry.decision_type}`)}
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
