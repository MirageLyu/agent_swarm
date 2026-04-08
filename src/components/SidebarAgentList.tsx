import { useEffect, useCallback } from "react";
import { commands, onAgentStarted, onTaskStatusChanged, onMissionStatusChanged } from "../ipc";
import { useAgentStore } from "../stores/agent-store";
import type { SidebarAgent } from "../stores/agent-store";
import { useUiStore } from "../stores/ui-store";
import { useTaskStore } from "../stores/task-store";
import styles from "./SidebarAgentList.module.css";

export function SidebarAgentList() {
  const sidebarAgents = useAgentStore((s) => s.sidebarAgents);
  const setSidebarAgents = useAgentStore((s) => s.setSidebarAgents);
  const setActiveAgent = useAgentStore((s) => s.setActiveAgent);
  const setViewMode = useAgentStore((s) => s.setViewMode);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const missions = useTaskStore((s) => s.missions);
  const activeMission = missions.find((m) => m.status === "running");

  const loadAgents = useCallback(async () => {
    if (!activeMission) {
      setSidebarAgents([]);
      return;
    }
    try {
      const list = await commands.listAgentsByMission(activeMission.id);
      const agents: SidebarAgent[] = list.map((a) => ({
        id: a.id,
        name: a.name,
        status: a.status as SidebarAgent["status"],
        taskTitle: a.task_id ? `Task: ${a.task_id.substring(0, 8)}` : "idle",
      }));
      setSidebarAgents(agents);
    } catch {}
  }, [activeMission, setSidebarAgents]);

  useEffect(() => {
    loadAgents();
  }, [loadAgents]);

  useEffect(() => {
    const cleanups = [
      onAgentStarted(() => loadAgents()),
      onTaskStatusChanged(() => loadAgents()),
      onMissionStatusChanged(() => loadAgents()),
    ];
    return () => { cleanups.forEach((p) => p.then((fn) => fn())); };
  }, [loadAgents]);

  const handleClick = useCallback(
    (agentId: string) => {
      setActiveAgent(agentId);
      setViewMode("focus");
      setActiveView("workspace");
    },
    [setActiveAgent, setViewMode, setActiveView],
  );

  if (sidebarAgents.length === 0 && !activeMission) return null;

  return (
    <div className={styles.section}>
      <div className={styles.divider} />
      <div className={styles.label}>Agents</div>
      {sidebarAgents.length === 0 ? (
        <div className={styles.empty}>No agents</div>
      ) : (
        sidebarAgents.map((agent) => (
          <div
            key={agent.id}
            className={styles.agentRow}
            onClick={() => handleClick(agent.id)}
          >
            <div className={styles.statusDot} data-status={agent.status} />
            <span className={styles.agentName}>{agent.name}</span>
            <span className={styles.taskTitle}>{agent.taskTitle}</span>
          </div>
        ))
      )}
    </div>
  );
}
