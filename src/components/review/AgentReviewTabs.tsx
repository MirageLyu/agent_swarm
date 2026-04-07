import type { MissionAgentInfo, ReviewAction } from "../../ipc";
import styles from "./AgentReviewTabs.module.css";

interface AgentReviewTabsProps {
  agents: MissionAgentInfo[];
  selectedAgentId: string | null;
  reviewStatuses: Record<string, ReviewAction | null>;
  onSelect: (agentId: string) => void;
}

const statusLabel: Record<string, string> = {
  approved: "Approved",
  rejected: "Rejected",
  revision_requested: "Revision",
};

export function AgentReviewTabs({
  agents,
  selectedAgentId,
  reviewStatuses,
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

        return (
          <button
            key={agent.id}
            className={`${styles.tab} ${selectedAgentId === agent.id ? styles.active : ""}`}
            onClick={() => onSelect(agent.id)}
          >
            <span className={`${styles.statusDot} ${styles[statusClass]}`} />
            <span>{agent.name}</span>
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
