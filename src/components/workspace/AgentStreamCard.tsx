import { memo } from "react";
import { Badge } from "../ui/Badge";
import type { Agent } from "../../stores/agent-store";
import styles from "./AgentStreamCard.module.css";

interface AgentStreamCardProps {
  agent: Agent;
  isActive: boolean;
  onClick: () => void;
}

const STATUS_BADGE_VARIANT = {
  running: "info",
  completed: "success",
  failed: "error",
  cancelled: "warning",
  idle: "default",
  waiting: "default",
} as const;

const COST_WARNING_THRESHOLD = 1.0;

function formatCost(cost: number): string {
  return `$${cost.toFixed(4)}`;
}

function formatTokens(tokens: number): string {
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(1)}M`;
  if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}k`;
  return String(tokens);
}

function getLatestEvent(agent: Agent) {
  const events = agent.events;
  if (events.length === 0) return null;
  return events[events.length - 1];
}

export const AgentStreamCard = memo(function AgentStreamCard({
  agent,
  isActive,
  onClick,
}: AgentStreamCardProps) {
  const latestEvent = getLatestEvent(agent);
  const isWarning = agent.costUsd >= COST_WARNING_THRESHOLD;

  return (
    <button
      className={`${styles.card} ${isActive ? styles.cardActive : ""}`}
      onClick={onClick}
      type="button"
    >
      <div className={styles.header}>
        <div className={styles.agentInfo}>
          <span className={styles.statusDot} data-status={agent.status} />
          <span className={styles.name}>{agent.name}</span>
        </div>
        <Badge variant={STATUS_BADGE_VARIANT[agent.status] ?? "default"}>
          {agent.status}
        </Badge>
      </div>

      <div className={styles.metrics}>
        <div className={styles.metric}>
          <span className={styles.metricLabel}>Step</span>
          <span className={styles.metricValue}>
            {agent.currentStep}
            {agent.totalSteps != null ? `/${agent.totalSteps}` : ""}
          </span>
        </div>
        <div className={styles.metric}>
          <span className={styles.metricLabel}>Tokens</span>
          <span className={styles.metricValue}>{formatTokens(agent.tokensUsed)}</span>
        </div>
        <div className={`${styles.metric} ${isWarning ? styles.metricWarning : ""}`}>
          <span className={styles.metricLabel}>Cost</span>
          <span className={styles.metricValue}>{formatCost(agent.costUsd)}</span>
        </div>
      </div>

      {latestEvent && (
        <div className={styles.preview}>
          <Badge
            variant={
              latestEvent.kind === "error"
                ? "error"
                : latestEvent.kind === "checkpoint"
                  ? "warning"
                  : "default"
            }
          >
            {latestEvent.kind}
          </Badge>
          <span className={styles.previewText}>
            {latestEvent.content.length > 80
              ? latestEvent.content.slice(0, 80) + "…"
              : latestEvent.content}
          </span>
        </div>
      )}

      {agent.streamBuffer && (
        <div className={styles.streaming}>
          <span className={styles.streamIndicator} />
          <span className={styles.streamPreview}>
            {agent.streamBuffer.slice(-60)}
          </span>
        </div>
      )}
    </button>
  );
});
