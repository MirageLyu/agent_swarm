import { memo } from "react";
import styles from "./CostSummaryBar.module.css";

interface CostSummaryBarProps {
  totalCost: number;
  totalInputTokens: number;
  totalOutputTokens: number;
  agentCount: number;
}

function formatTokens(tokens: number): string {
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}k`;
  return String(tokens);
}

const COST_WARNING_THRESHOLD = 5.0;

export const CostSummaryBar = memo(function CostSummaryBar({
  totalCost,
  totalInputTokens,
  totalOutputTokens,
  agentCount,
}: CostSummaryBarProps) {
  const isWarning = totalCost >= COST_WARNING_THRESHOLD;

  return (
    <div className={styles.bar}>
      <div className={styles.section}>
        <span className={styles.label}>Agents</span>
        <span className={styles.value}>{agentCount}</span>
      </div>
      <div className={styles.divider} />
      <div className={styles.section}>
        <span className={styles.label}>Input</span>
        <span className={styles.value}>{formatTokens(totalInputTokens)}</span>
      </div>
      <div className={styles.section}>
        <span className={styles.label}>Output</span>
        <span className={styles.value}>{formatTokens(totalOutputTokens)}</span>
      </div>
      <div className={styles.divider} />
      <div className={`${styles.section} ${isWarning ? styles.warning : ""}`}>
        <span className={styles.label}>Total Cost</span>
        <span className={styles.costValue}>${totalCost.toFixed(4)}</span>
      </div>
    </div>
  );
});
