import type { MissionAgentInfo, ReviewAction } from "../../ipc";
import styles from "./AgentReviewTabs.module.css";

interface EvalBadge {
  score: number | null;
  evaluating: boolean;
}

interface AgentReviewTabsProps {
  agents: MissionAgentInfo[];
  selectedAgentId: string | null;
  reviewStatuses: Record<string, ReviewAction | null>;
  evalBadges?: Record<string, EvalBadge>;
  onSelect: (agentId: string) => void;
}

const statusLabel: Record<string, string> = {
  approved: "Approved",
  rejected: "Rejected",
  revision_requested: "Revision",
};

function scoreColorClass(score: number): string {
  if (score >= 8) return styles.scoreHigh;
  if (score >= 6) return styles.scoreMedium;
  return styles.scoreLow;
}

export function AgentReviewTabs({
  agents,
  selectedAgentId,
  reviewStatuses,
  evalBadges,
  onSelect,
}: AgentReviewTabsProps) {
  if (agents.length === 0) return null;

  return (
    <div className={styles.tabBar}>
      {agents.map((agent) => {
        const reviewStatus = reviewStatuses[agent.id];
        const statusClass =
          agent.status === "running" || agent.status === "completed" || agent.status === "failed"
            ? agent.status
            : "idle";
        const badge = evalBadges?.[agent.id];

        return (
          <button
            key={agent.id}
            className={`${styles.tab} ${selectedAgentId === agent.id ? styles.active : ""}`}
            onClick={() => onSelect(agent.id)}
          >
            <span className={`${styles.statusDot} ${styles[statusClass]}`} />
            <span>{agent.name}</span>
            {badge?.evaluating && (
              <span className={styles.evalSpinner} title="Evaluating…" />
            )}
            {badge?.score != null && !badge.evaluating && (
              <span
                className={`${styles.evalScore} ${scoreColorClass(badge.score)}`}
                title={`Evaluator score: ${badge.score}/10`}
              >
                {Math.round(badge.score * 10) / 10}
              </span>
            )}
            {reviewStatus && (
              <span className={`${styles.reviewBadge} ${styles[reviewStatus]}`}>
                {statusLabel[reviewStatus]}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}
