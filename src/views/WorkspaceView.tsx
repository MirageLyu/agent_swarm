import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "../components/ui/Button";
import { Badge } from "../components/ui/Badge";
import { commands, onAgentEvent, onAgentStream } from "../ipc";
import type { AgentEventPayload, AgentStreamPayload } from "../ipc";
import styles from "./WorkspaceView.module.css";

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
  status: "running" | "completed" | "failed";
}

export function WorkspaceView() {
  const [input, setInput] = useState("");
  const [agents, setAgents] = useState<Record<string, AgentActivity>>({});
  const [selectedAgent, setSelectedAgent] = useState<string | null>(null);
  const eventCountRef = useRef(0);
  const scrollRef = useRef<HTMLDivElement>(null);

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
        if (payload.kind === "message" || payload.content.includes("Completed")) {
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

    return () => {
      unlistenEvent.then((fn) => fn());
      unlistenStream.then((fn) => fn());
    };
  }, [selectedAgent]);

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

  const activeAgent = selectedAgent ? agents[selectedAgent] : null;
  const agentIds = Object.keys(agents);

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
                <Badge
                  variant={
                    agents[id].status === "completed"
                      ? "success"
                      : agents[id].status === "failed"
                        ? "error"
                        : "info"
                  }
                >
                  {agents[id].status}
                </Badge>
              </button>
            ))}
          </div>

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
