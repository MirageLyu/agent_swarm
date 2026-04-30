import { memo } from "react";
import { useTranslation } from "react-i18next";
import { AgentStreamCard } from "./AgentStreamCard";
import type { Agent } from "../../stores/agent-store";
import styles from "./AgentStreamList.module.css";

interface AgentStreamListProps {
  agents: Agent[];
  taskTitleMap?: Record<string, string>;
  activeAgentId: string | null;
  onSelectAgent: (id: string) => void;
}

export const AgentStreamList = memo(function AgentStreamList({
  agents,
  taskTitleMap,
  activeAgentId,
  onSelectAgent,
}: AgentStreamListProps) {
  if (agents.length === 0) {
    return <EmptyState />;
  }

  return (
    <div className={styles.list}>
      {agents.map((agent) => (
        <AgentStreamCard
          key={agent.id}
          agent={agent}
          taskTitle={taskTitleMap?.[agent.taskId]}
          isActive={agent.id === activeAgentId}
          onClick={() => onSelectAgent(agent.id)}
        />
      ))}
    </div>
  );
});

function EmptyState() {
  const { t } = useTranslation("workspace");
  return (
    <div className={styles.empty}>
      <p>{t("streamList.emptyForMission")}</p>
    </div>
  );
}
