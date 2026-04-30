import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../components/ui/Button";
import {
  commands,
  onAgentEvent,
  onAgentStream,
  onAgentStarted,
  onAgentToolStream,
  onTaskStatusChanged,
  onMissionStatusChanged,
} from "../ipc";
import type { AgentEventPayload, AgentStreamPayload } from "../ipc";
import type { TaskStatus, MissionStatus, MissionCostSummary } from "../ipc/commands";
import { useAgentStore } from "../stores/agent-store";
import type { Agent, AgentEvent, AgentStatus } from "../stores/agent-store";
import { useTaskStore } from "../stores/task-store";
import { useUiStore } from "../stores/ui-store";
import { AgentStreamList } from "../components/workspace/AgentStreamList";
import { AgentGridView } from "../components/workspace/AgentGridView";
import { AgentTerminalPane } from "../components/workspace/AgentTerminalPane";
import { AgentPaneMenu } from "../components/workspace/AgentPaneMenu";
import { CostSummaryBar } from "../components/workspace/CostSummaryBar";
import { MissionNoteBar } from "../components/workspace/MissionNoteBar";
import styles from "./WorkspaceView.module.css";

let eventCounter = 0;

function parseCheckpointCost(content: string): { inputTokens: number; outputTokens: number; cost: number } | null {
  const tokensMatch = content.match(/tokens:\s*(\d+)in\/(\d+)out/);
  const costMatch = content.match(/cost:\s*\$([0-9.]+)/);
  if (!tokensMatch) return null;
  return {
    inputTokens: parseInt(tokensMatch[1], 10),
    outputTokens: parseInt(tokensMatch[2], 10),
    cost: costMatch ? parseFloat(costMatch[1]) : 0,
  };
}

function toAgentStatus(raw: string): AgentStatus {
  const valid: AgentStatus[] = ["idle", "running", "completed", "failed", "cancelled", "waiting"];
  return valid.includes(raw as AgentStatus) ? (raw as AgentStatus) : "completed";
}

type ViewMode = "grid" | "list" | "focus";

export function WorkspaceView() {
  const { t } = useTranslation("workspace");
  const {
    agents,
    activeAgentId,
    filterMissionId,
    setActiveAgent,
    setFilterMissionId,
    addAgent,
    updateAgent,
    appendEvent,
    appendStream,
    appendShell,
    hydrateAgents,
    hydrateEvents,
  } = useAgentStore();

  const workspaceMode = useUiStore((s) => s.workspaceMode);
  const setWorkspaceMode = useUiStore((s) => s.setWorkspaceMode);

  const { missions, updateTaskLocal, updateMissionStatus } = useTaskStore();
  const [costSummary, setCostSummary] = useState<MissionCostSummary>({
    total_cost: 0,
    total_input_tokens: 0,
    total_output_tokens: 0,
  });
  const [taskTitleMap, setTaskTitleMap] = useState<Record<string, string>>({});

  useEffect(() => {
    commands.listMissions().then((list) => {
      useTaskStore.getState().setMissions(list);
    }).catch(() => {});
  }, []);

  useEffect(() => {
    const load = async () => {
      try {
        let agentList;
        if (filterMissionId) {
          const [agents, detail] = await Promise.all([
            commands.listAgentsByMission(filterMissionId),
            commands.getMissionDetail(filterMissionId),
          ]);
          agentList = agents;
          const map: Record<string, string> = {};
          for (const t of detail.tasks) {
            map[t.id] = t.title;
          }
          setTaskTitleMap(map);
        } else {
          agentList = await commands.listAgents();
          setTaskTitleMap({});
        }
        const hydrated: Agent[] = agentList.map((a) => ({
          id: a.id,
          name: a.name,
          taskId: ("task_id" in a && a.task_id) ? String(a.task_id) : "",
          missionId: filterMissionId,
          status: toAgentStatus(a.status),
          worktreePath: ("worktree_path" in a ? (a as { worktree_path?: string | null }).worktree_path : null) ?? null,
          currentStep: a.current_step,
          totalSteps: null,
          tokensUsed: a.tokens_used,
          costUsd: a.cost_usd,
          events: [],
          streamBuffer: "",
          shellBuffer: "",
        }));
        hydrateAgents(hydrated);
      } catch {}
    };
    load();
  }, [filterMissionId, hydrateAgents]);

  useEffect(() => {
    if (!filterMissionId) {
      setCostSummary({ total_cost: 0, total_input_tokens: 0, total_output_tokens: 0 });
      return;
    }
    commands.getMissionCostSummary(filterMissionId).then(setCostSummary).catch(() => {});
  }, [filterMissionId]);

  // Load events for the focused agent (focus/list mode)
  useEffect(() => {
    if (!activeAgentId) return;
    const agent = agents[activeAgentId];
    if (agent && agent.events.length === 0 && agent.status !== "running") {
      commands.getAgentEvents(activeAgentId).then((events) => {
        const mapped: AgentEvent[] = events.map((e) => ({
          id: e.id,
          agentId: e.agent_id,
          step: e.step,
          kind: e.kind as AgentEvent["kind"],
          content: e.content,
          timestamp: e.created_at,
        }));
        hydrateEvents(activeAgentId, mapped);
      }).catch(() => {});
    }
  }, [activeAgentId, agents, hydrateEvents]);

  // Auto-load events for all non-running agents so grid cards aren't empty
  const hydratedAgentIdsRef = useRef(new Set<string>());
  useEffect(() => {
    const agentList = Object.values(agents);
    for (const agent of agentList) {
      if (
        agent.events.length === 0 &&
        agent.status !== "running" &&
        !hydratedAgentIdsRef.current.has(agent.id)
      ) {
        hydratedAgentIdsRef.current.add(agent.id);
        commands.getAgentEvents(agent.id).then((events) => {
          const mapped: AgentEvent[] = events.map((e) => ({
            id: e.id,
            agentId: e.agent_id,
            step: e.step,
            kind: e.kind as AgentEvent["kind"],
            content: e.content,
            timestamp: e.created_at,
          }));
          hydrateEvents(agent.id, mapped);
        }).catch(() => {});
      }
    }
  }, [agents, hydrateEvents]);

  useEffect(() => {
    const unlistenEvent = onAgentEvent((payload: AgentEventPayload) => {
      const agentId = payload.agent_id;
      const store = useAgentStore.getState();

      if (!store.agents[agentId]) {
        addAgent({
          id: agentId,
          name: `Agent ${agentId.substring(0, 8)}`,
          taskId: "",
          missionId: null,
          status: "running",
          worktreePath: null,
          currentStep: payload.step,
          totalSteps: null,
          tokensUsed: 0,
          costUsd: 0,
          events: [],
          streamBuffer: "",
          shellBuffer: "",
        });
      }

      const evt: AgentEvent = {
        id: `rt-${eventCounter++}`,
        agentId,
        step: payload.step,
        kind: payload.kind as AgentEvent["kind"],
        content: payload.content,
        timestamp: new Date().toISOString(),
      };
      appendEvent(agentId, evt);

      const currentAgent = useAgentStore.getState().agents[agentId];
      const updates: Partial<Agent> = { currentStep: payload.step };

      if (payload.kind === "status_change") {
        updates.status = toAgentStatus(payload.content);
      } else if (payload.kind === "message") {
        updates.status = "completed";
      } else if (payload.kind === "error" && payload.content.includes("Max steps")) {
        updates.status = "failed";
      }

      if (payload.kind === "checkpoint" && currentAgent) {
        const parsed = parseCheckpointCost(payload.content);
        if (parsed) {
          updates.tokensUsed = (currentAgent.tokensUsed || 0) + parsed.inputTokens + parsed.outputTokens;
          updates.costUsd = (currentAgent.costUsd || 0) + parsed.cost;
        }
      }

      const terminalStatuses: AgentStatus[] = ["completed", "failed", "cancelled"];
      if (updates.status && terminalStatuses.includes(updates.status)) {
        updates.streamBuffer = "";
      }

      updateAgent(agentId, updates);

      if (!store.activeAgentId) {
        setActiveAgent(agentId);
      }
    });

    const unlistenStream = onAgentStream((payload: AgentStreamPayload) => {
      appendStream(payload.agent_id, payload.content);
    });

    // FM-15 follow-up：把 shell_exec 的 stdout/stderr/meta 拼到 shellBuffer，
    // 让 AgentTerminalPane 实时渲染当前命令的输出，而不必等 tool_result 回来。
    const unlistenToolStream = onAgentToolStream((payload) => {
      // meta（命令开始 / watchdog kill 等）加 [meta] 前缀更醒目；
      // stderr 加 [stderr] 前缀以便阅读时能区分；stdout 直接拼。
      let chunk = payload.chunk;
      if (payload.stream === "meta" && chunk) {
        chunk = chunk.startsWith("[") ? chunk : `[meta] ${chunk}`;
      } else if (payload.stream === "stderr" && chunk) {
        if (!chunk.startsWith("[stderr]")) {
          chunk = `[stderr] ${chunk}`;
        }
      }
      if (chunk) {
        appendShell(payload.agentId, chunk);
      }
    });

    const unlistenStarted = onAgentStarted((payload) => {
      const store = useAgentStore.getState();
      if (!store.agents[payload.agent_id]) {
        addAgent({
          id: payload.agent_id,
          name: `Agent ${payload.agent_id.substring(0, 8)}`,
          taskId: payload.task_id,
          missionId: null,
          status: "running",
          worktreePath: payload.worktree_path,
          currentStep: 0,
          totalSteps: null,
          tokensUsed: 0,
          costUsd: 0,
          events: [],
          streamBuffer: "",
          shellBuffer: "",
        });
      }
      if (!store.activeAgentId) {
        setActiveAgent(payload.agent_id);
      }
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
      unlistenToolStream.then((fn) => fn());
      unlistenStarted.then((fn) => fn());
      unlistenTask.then((fn) => fn());
      unlistenMission.then((fn) => fn());
    };
  }, [addAgent, updateAgent, appendEvent, appendStream, appendShell, setActiveAgent, updateTaskLocal, updateMissionStatus]);

  useEffect(() => {
    if (!filterMissionId) return;
    const interval = setInterval(() => {
      commands.getMissionCostSummary(filterMissionId).then(setCostSummary).catch(() => {});
    }, 5000);
    return () => clearInterval(interval);
  }, [filterMissionId]);

  const filteredAgents = useMemo(() => {
    const all = Object.values(agents);
    if (!filterMissionId) return all;
    return all.filter((a) => a.missionId === filterMissionId || a.taskId !== "");
  }, [agents, filterMissionId]);

  const activeAgent = activeAgentId ? agents[activeAgentId] : null;

  const hasRunningAgents = useMemo(
    () => filteredAgents.some((a) => a.status === "running"),
    [filteredAgents],
  );

  const handleSelectAgent = useCallback(
    (id: string) => {
      setActiveAgent(id);
      setWorkspaceMode("focus");
    },
    [setActiveAgent, setWorkspaceMode],
  );

  const handleBackToGrid = useCallback(() => {
    setWorkspaceMode("grid");
  }, [setWorkspaceMode]);

  const handleLoadHistory = useCallback(async () => {
    if (!filterMissionId) return;
    try {
      const events = await commands.listAgentEvents({ mission_id: filterMissionId });
      const byAgent = new Map<string, AgentEvent[]>();
      for (const e of events) {
        const mapped: AgentEvent = {
          id: e.id,
          agentId: e.agent_id,
          step: e.step,
          kind: e.kind as AgentEvent["kind"],
          content: e.content,
          timestamp: e.created_at,
        };
        const list = byAgent.get(e.agent_id) || [];
        list.push(mapped);
        byAgent.set(e.agent_id, list);
      }
      for (const [agentId, evts] of byAgent) {
        hydrateEvents(agentId, evts);
      }
    } catch {}
  }, [filterMissionId, hydrateEvents]);

  const viewModes: { mode: ViewMode; label: string }[] = useMemo(
    () => [
      { mode: "grid", label: t("viewMode.grid") },
      { mode: "list", label: t("viewMode.list") },
      { mode: "focus", label: t("viewMode.focus") },
    ],
    [t],
  );

  const [filterOpen, setFilterOpen] = useState(false);
  const filterRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!filterOpen) return;
    const handleClick = (e: MouseEvent) => {
      if (filterRef.current && !filterRef.current.contains(e.target as Node)) {
        setFilterOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [filterOpen]);

  const selectedMissionLabel = filterMissionId
    ? missions.find((m) => m.id === filterMissionId)?.title ?? "Mission"
    : t("filterAllAgents");

  return (
    <div className={styles.container}>
      <div className={styles.toolbar}>
        <div className={styles.toolbarLeft}>
          <div className={styles.filterWrap} ref={filterRef}>
            <button
              className={`${styles.filterTrigger} ${filterOpen ? styles.filterTriggerOpen : ""}`}
              onClick={() => setFilterOpen((v) => !v)}
              type="button"
            >
              <span className={styles.filterLabel}>{selectedMissionLabel}</span>
              <svg className={styles.filterChevron} width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                <polyline points="2.5,3.5 5,6.5 7.5,3.5" />
              </svg>
            </button>
            {filterOpen && (
              <div className={styles.filterDropdown}>
                <button
                  className={`${styles.filterOption} ${!filterMissionId ? styles.filterOptionActive : ""}`}
                  onClick={() => { setFilterMissionId(null); setFilterOpen(false); }}
                  type="button"
                >
                  {t("filterAllAgents")}
                </button>
                {missions.map((m) => (
                  <button
                    key={m.id}
                    className={`${styles.filterOption} ${filterMissionId === m.id ? styles.filterOptionActive : ""}`}
                    onClick={() => { setFilterMissionId(m.id); setFilterOpen(false); }}
                    type="button"
                  >
                    <span className={styles.filterOptionTitle}>{m.title}</span>
                    <span className={styles.filterOptionStatus}>{m.status}</span>
                  </button>
                ))}
              </div>
            )}
          </div>

          <div className={styles.segmentedControl}>
            {viewModes.map((vm) => (
              <button
                key={vm.mode}
                className={`${styles.segBtn} ${workspaceMode === vm.mode ? styles.segBtnActive : ""}`}
                onClick={() => setWorkspaceMode(vm.mode)}
              >
                {vm.label}
              </button>
            ))}
          </div>

          <span className={styles.viewLabel}>
            {t("agentCount", { count: filteredAgents.length })}
          </span>
        </div>

        <div className={styles.toolbarRight}>
          {filterMissionId && (
            <Button variant="ghost" size="sm" onClick={handleLoadHistory}>
              {t("loadHistory")}
            </Button>
          )}
        </div>
      </div>

      {filterMissionId && (
        <CostSummaryBar
          totalCost={costSummary.total_cost}
          totalInputTokens={costSummary.total_input_tokens}
          totalOutputTokens={costSummary.total_output_tokens}
          agentCount={filteredAgents.length}
        />
      )}

      {filterMissionId && (
        <MissionNoteBar
          missionId={filterMissionId}
          hasRunningAgents={hasRunningAgents}
        />
      )}

      {filteredAgents.length === 0 ? (
        <div className={styles.empty}>
          <p>
            {filterMissionId ? t("emptyForMission") : t("emptyAll")}
          </p>
        </div>
      ) : workspaceMode === "focus" && activeAgent ? (
        <div className={styles.focusContainer}>
          <button className={styles.backBtn} onClick={handleBackToGrid}>
            &larr; {t("back")}
          </button>
          <div className={styles.focusPane}>
            <AgentTerminalPane
              agent={activeAgent}
              taskTitle={taskTitleMap[activeAgent.taskId]}
              menuSlot={
                <AgentPaneMenu agent={activeAgent} />
              }
            />
          </div>
        </div>
      ) : workspaceMode === "grid" ? (
        <AgentGridView
          agents={filteredAgents}
          taskTitleMap={taskTitleMap}
          onFocusAgent={handleSelectAgent}
        />
      ) : (
        <AgentStreamList
          agents={filteredAgents}
          taskTitleMap={taskTitleMap}
          activeAgentId={activeAgentId}
          onSelectAgent={handleSelectAgent}
        />
      )}
    </div>
  );
}
