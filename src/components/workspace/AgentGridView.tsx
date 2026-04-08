import { memo } from "react";
import type { Agent } from "../../stores/agent-store";
import { AgentTerminalPane } from "./AgentTerminalPane";
import { AgentPaneMenu } from "./AgentPaneMenu";
import styles from "./AgentGridView.module.css";

interface AgentGridViewProps {
  agents: Agent[];
  taskTitleMap?: Record<string, string>;
  onFocusAgent: (agentId: string) => void;
}

export const AgentGridView = memo(function AgentGridView({
  agents,
  taskTitleMap,
  onFocusAgent,
}: AgentGridViewProps) {
  const count = Math.min(agents.length, 20);

  return (
    <div
      className={styles.grid}
      data-count={String(count)}
    >
      {agents.map((agent) => (
        <div key={agent.id} className={styles.cell}>
          <AgentTerminalPane
            agent={agent}
            taskTitle={taskTitleMap?.[agent.taskId]}
            onFocus={onFocusAgent}
            menuSlot={<AgentPaneMenu agent={agent} />}
          />
        </div>
      ))}
    </div>
  );
});
