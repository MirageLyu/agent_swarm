import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "../components/ui/Button";
import { Badge } from "../components/ui/Badge";
import {
  commands,
  onAgentEvent,
  onAgentStream,
  onAgentStarted,
  onTaskStatusChanged,
  onMissionStatusChanged,
} from "../ipc";
import type { AgentEventPayload, AgentStreamPayload } from "../ipc";
import type { TaskStatus, MissionStatus } from "../ipc/commands";
import { useTaskStore } from "../stores/task-store";
import styles from "./WorkspaceView.module.css";

type AgentActivityStatus = "running" | "completed" | "failed" | "cancelled";

interface AgentActivity {
  agentId: string;
  events: Array<{
    id: number;
    step: number;
    kind: string;
    content: string;
    timestamp: number;
  }>;
  streamBuffer: string;
  status: AgentActivityStatus;
}

function statusBadgeVariant(status: AgentActivityStatus) {
  switch (status) {
    case "completed":
      return "success" as const;
    case "failed":
      return "error" as const;
    case "cancelled":
      return "warning" as const;
    default:
      return "info" as const;
  }
}

export function WorkspaceView() {
  const [input, setInput] = useState("");
  const [agents, setAgents] = useState<Record<string, AgentActivity>>({});
  const [selectedAgent, setSelectedAgent] = useState<string | null>(null);
  const eventCountRef = useRef(0);
  const scrollRef = useRef<HTMLDivElement>(null);
  const { updateTaskLocal, updateMissionStatus } = useTaskStore();

  // Load historical agents from DB on mount
  useEffect(() => {
    commands.listAgents().then((list) => {
      if (list.length === 0) return;
      const restored: Record<string, AgentActivity> = {};
      for (const a of list) {
        restored[a.id] = {
          agentId: a.id,
          events: [],
          streamBuffer: "",
          status: (["running", "completed", "failed", "cancelled"].includes(a.status)
            ? a.status
            : "completed") as AgentActivityStatus,
        };
      }
      setAgents((prev) => ({ ...restored, ...prev }));
      setSelectedAgent((cur) => cur ?? list[0].id);
    }).catch(() => {});
  }, []);

  useEffect(() => {
    const unlistenEvent = onAgentEvent((payload: AgentEventPayload) => {
      setAgents((prev) => {
        const agent = prev[payload.agent_id] || {
          agentId: payload.agent_id,
          events: [],
          streamBuffer: "",
          status: "running" as const,
        };

        const newEvent = {
          id: eventCountRef.current++,
          step: payload.step,
          kind: payload.kind,
          content: payload.content,
          timestamp: Date.now(),
        };

        let status = agent.status;
        if (payload.kind === "status_change") {
          if (payload.content === "cancelled") status = "cancelled";
          else if (payload.content === "running") status = "running";
        } else if (payload.kind === "message" || payload.content.includes("Completed")) {
          status = "completed";
        } else if (payload.kind === "error" && payload.content.includes("Max steps")) {
          status = "failed";
        }

        return {
          ...prev,
          [payload.agent_id]: {
            ...agent,
            events: [...agent.events, newEvent],
            status,
          },
        };
      });

      if (!selectedAgent) {
        setSelectedAgent(payload.agent_id);
      }
    });

    const unlistenStream = onAgentStream((payload: AgentStreamPayload) => {
      setAgents((prev) => {
        const agent = prev[payload.agent_id];
        if (!agent) return prev;
        return {
          ...prev,
          [payload.agent_id]: {
            ...agent,
            streamBuffer: agent.streamBuffer + payload.content,
          },
        };
      });
    });

    const unlistenStarted = onAgentStarted((payload) => {
      setAgents((prev) => {
        if (prev[payload.agent_id]) return prev;
        return {
          ...prev,
          [payload.agent_id]: {
            agentId: payload.agent_id,
            events: [],
            streamBuffer: "",
            status: "running",
          },
        };
      });
      setSelectedAgent((cur) => cur ?? payload.agent_id);
    });

    const unlistenTask = onTaskStatusChanged((payload) => {
      updateTaskLocal(payload.task_id, { status: payload.to as TaskStatus });
    });

    const unlistenMission = onMissionStatusChanged((payload) => {
      updateMissionStatus(payload.mission_id, payload.to as MissionStatus);
    });

    return () => {
      unlistenEvent.then((fn) => fn());
      unlistenStream.then((fn) => fn());
      unlistenStarted.then((fn) => fn());
      unlistenTask.then((fn) => fn());
      unlistenMission.then((fn) => fn());
    };
  }, [selectedAgent, updateTaskLocal, updateMissionStatus]);

  // Auto-load events when selecting an agent with no events yet (historical)
  useEffect(() => {
    if (!selectedAgent) return;
    const agent = agents[selectedAgent];
    if (agent && agent.events.length === 0 && agent.status !== "running") {
      commands.getAgentEvents(selectedAgent).then((events) => {
        setAgents((prev) => {
          const cur = prev[selectedAgent];
          if (!cur || cur.events.length > 0) return prev;
          return {
            ...prev,
            [selectedAgent]: {
              ...cur,
              events: events.map((e, i) => ({
                id: i,
                step: e.step,
                kind: e.kind,
                content: e.content,
                timestamp: new Date(e.created_at).getTime(),
              })),
            },
          };
        });
      }).catch(() => {});
    }
  }, [selectedAgent, agents]);

  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [agents, selectedAgent]);

  const handleSubmit = useCallback(async () => {
    if (!input.trim()) return;
    const desc = input.trim();
    setInput("");
    try {
      const resp = await commands.runAgent({
        task_description: desc,
        workspace_path: "/tmp/miragenty-workspace",
      });
      setSelectedAgent(resp.agent_id);
    } catch (e) {
      alert(String(e));
    }
  }, [input]);

  const handleStop = useCallback(async () => {
    if (!selectedAgent) return;
    try {
      await commands.stopAgent(selectedAgent);
    } catch (e) {
      alert(String(e));
    }
  }, [selectedAgent]);

  const handleLoadHistory = useCallback(async () => {
    if (!selectedAgent) return;
    try {
      const events = await commands.getAgentEvents(selectedAgent);
      setAgents((prev) => ({
        ...prev,
        [selectedAgent]: {
          agentId: selectedAgent,
          events: events.map((e, i) => ({
            id: i,
            step: e.step,
            kind: e.kind,
            content: e.content,
            timestamp: new Date(e.created_at).getTime(),
          })),
          streamBuffer: "",
          status: (prev[selectedAgent]?.status ?? "completed") as AgentActivityStatus,
        },
      }));
    } catch (e) {
      alert(String(e));
    }
  }, [selectedAgent]);

  const activeAgent = selectedAgent ? agents[selectedAgent] : null;
  const agentIds = Object.keys(agents);
  const isRunning = activeAgent?.status === "running";

  return (
    <div className={styles.container}>
      <div className={styles.inputBar}>
        <input
          className={styles.taskInput}
          placeholder="Describe a task for the agent..."
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleSubmit()}
        />
        <Button variant="primary" size="md" onClick={handleSubmit} disabled={!input.trim()}>
          Run Agent
        </Button>
      </div>

      {agentIds.length === 0 ? (
        <div className={styles.empty}>
          <p>No agents running. Enter a task above to start.</p>
        </div>
      ) : (
        <div className={styles.workspace}>
          <div className={styles.agentTabs}>
            {agentIds.map((id) => (
              <button
                key={id}
                className={`${styles.tab} ${selectedAgent === id ? styles.tabActive : ""}`}
                onClick={() => setSelectedAgent(id)}
              >
                <span className={styles.tabDot} data-status={agents[id].status} />
                <span className={styles.tabLabel}>
                  Agent {id.substring(0, 6)}
                </span>
                <Badge variant={statusBadgeVariant(agents[id].status)}>
                  {agents[id].status}
                </Badge>
              </button>
            ))}
          </div>

          {activeAgent && (
            <div className={styles.toolbar}>
              {isRunning && (
                <Button variant="ghost" size="sm" onClick={handleStop}>
                  Stop Agent
                </Button>
              )}
              <Button variant="ghost" size="sm" onClick={handleLoadHistory}>
                Load History
              </Button>
            </div>
          )}

          <div className={styles.stream} ref={scrollRef}>
            {activeAgent ? (
              <>
                {activeAgent.events.map((evt) => (
                  <div key={evt.id} className={`${styles.event} ${styles[`event_${evt.kind}`]}`}>
                    <div className={styles.eventHeader}>
                      <Badge
                        variant={
                          evt.kind === "error"
                            ? "error"
                            : evt.kind === "checkpoint"
                              ? "warning"
                              : evt.kind === "tool_use"
                                ? "info"
                                : evt.kind === "status_change"
                                  ? "default"
                                  : "default"
                        }
                      >
                        Step {evt.step} · {evt.kind}
                      </Badge>
                    </div>
                    <pre className={styles.eventContent}>{evt.content}</pre>
                  </div>
                ))}
                {activeAgent.streamBuffer && (
                  <div className={styles.streamingBlock}>
                    <pre className={styles.streamText}>{activeAgent.streamBuffer}</pre>
                    <span className={styles.cursor} />
                  </div>
                )}
              </>
            ) : (
              <p className={styles.empty}>Select an agent to view activity</p>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
