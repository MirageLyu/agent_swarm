import { memo } from "react";
import { AgentStreamCard } from "./AgentStreamCard";
import type { Agent } from "../../stores/agent-store";
import styles from "./AgentStreamList.module.css";

interface AgentStreamListProps {
  agents: Agent[];
  activeAgentId: string | null;
  onSelectAgent: (id: string) => void;
}

export const AgentStreamList = memo(function AgentStreamList({
  agents,
  activeAgentId,
  onSelectAgent,
}: AgentStreamListProps) {
  if (agents.length === 0) {
    return (
      <div className={styles.empty}>
        <p>No agents for this mission.</p>
      </div>
    );
  }

  return (
    <div className={styles.list}>
      {agents.map((agent) => (
        <AgentStreamCard
          key={agent.id}
          agent={agent}
          isActive={agent.id === activeAgentId}
          onClick={() => onSelectAgent(agent.id)}
        />
      ))}
    </div>
  );
});
