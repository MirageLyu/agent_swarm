import { create } from "zustand";

export type AgentStatus =
  | "idle"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "waiting";

export interface AgentEvent {
  id: string;
  agentId: string;
  step: number;
  timestamp: string;
  kind: "llm_call" | "tool_use" | "tool_result" | "checkpoint" | "error" | "message" | "status_change";
  content: string;
}

export interface Agent {
  id: string;
  name: string;
  taskId: string;
  missionId: string | null;
  status: AgentStatus;
  worktreePath: string | null;
  currentStep: number;
  totalSteps: number | null;
  tokensUsed: number;
  costUsd: number;
  events: AgentEvent[];
  streamBuffer: string;
  /// FM-15 follow-up: shell_exec 实时输出，独立于 LLM streamBuffer。
  /// 由 `agent-tool-stream` 事件驱动，进入终态时清空，避免下次重启混淆。
  shellBuffer: string;
}

export type WorkspaceViewMode = "list" | "focus" | "grid";

export interface SidebarAgent {
  id: string;
  name: string;
  status: AgentStatus;
  taskTitle: string;
}

interface AgentState {
  agents: Record<string, Agent>;
  activeAgentId: string | null;
  viewMode: WorkspaceViewMode;
  filterMissionId: string | null;
  sidebarAgents: SidebarAgent[];

  addAgent: (agent: Agent) => void;
  updateAgent: (id: string, updates: Partial<Agent>) => void;
  removeAgent: (id: string) => void;
  appendEvent: (agentId: string, event: AgentEvent) => void;
  appendStream: (agentId: string, content: string) => void;
  clearStream: (agentId: string) => void;
  appendShell: (agentId: string, content: string) => void;
  clearShell: (agentId: string) => void;
  setActiveAgent: (id: string | null) => void;
  setViewMode: (mode: WorkspaceViewMode) => void;
  setFilterMissionId: (missionId: string | null) => void;
  hydrateEvents: (agentId: string, events: AgentEvent[]) => void;
  hydrateAgents: (agents: Agent[]) => void;
  setSidebarAgents: (agents: SidebarAgent[]) => void;
}

export const useAgentStore = create<AgentState>((set) => ({
  agents: {},
  activeAgentId: null,
  viewMode: "list",
  filterMissionId: null,
  sidebarAgents: [],

  addAgent: (agent) =>
    set((s) => ({ agents: { ...s.agents, [agent.id]: agent } })),

  updateAgent: (id, updates) =>
    set((s) => ({
      agents: {
        ...s.agents,
        [id]: s.agents[id] ? { ...s.agents[id], ...updates } : s.agents[id],
      },
    })),

  removeAgent: (id) =>
    set((s) => {
      const { [id]: _, ...rest } = s.agents;
      return { agents: rest };
    }),

  appendEvent: (agentId, event) =>
    set((s) => {
      const agent = s.agents[agentId];
      if (!agent) return s;
      return {
        agents: {
          ...s.agents,
          [agentId]: { ...agent, events: [...agent.events, event] },
        },
      };
    }),

  appendStream: (agentId, content) =>
    set((s) => {
      const agent = s.agents[agentId];
      if (!agent) return s;
      return {
        agents: {
          ...s.agents,
          [agentId]: { ...agent, streamBuffer: agent.streamBuffer + content },
        },
      };
    }),

  clearStream: (agentId) =>
    set((s) => {
      const agent = s.agents[agentId];
      if (!agent) return s;
      return {
        agents: {
          ...s.agents,
          [agentId]: { ...agent, streamBuffer: "" },
        },
      };
    }),

  appendShell: (agentId, content) =>
    set((s) => {
      const agent = s.agents[agentId];
      if (!agent) return s;
      // 控制 shellBuffer 上限：保留尾部 ~24KB 字符，避免长跑命令把内存吃爆。
      const next = agent.shellBuffer + content;
      const SHELL_BUF_MAX = 24 * 1024;
      const trimmed =
        next.length > SHELL_BUF_MAX ? next.slice(next.length - SHELL_BUF_MAX) : next;
      return {
        agents: {
          ...s.agents,
          [agentId]: { ...agent, shellBuffer: trimmed },
        },
      };
    }),

  clearShell: (agentId) =>
    set((s) => {
      const agent = s.agents[agentId];
      if (!agent) return s;
      return {
        agents: {
          ...s.agents,
          [agentId]: { ...agent, shellBuffer: "" },
        },
      };
    }),

  setActiveAgent: (id) =>
    set({ activeAgentId: id }),

  setViewMode: (mode) =>
    set({ viewMode: mode }),

  setFilterMissionId: (missionId) =>
    set({ filterMissionId: missionId }),

  setSidebarAgents: (agents) => set({ sidebarAgents: agents }),

  hydrateEvents: (agentId, events) =>
    set((s) => {
      const agent = s.agents[agentId];
      if (!agent) return s;
      const sorted = [...events].sort(
        (a, b) => new Date(a.timestamp).getTime() - new Date(b.timestamp).getTime(),
      );
      return {
        agents: {
          ...s.agents,
          [agentId]: { ...agent, events: sorted },
        },
      };
    }),

  hydrateAgents: (agents) =>
    set((s) => {
      const merged: Record<string, Agent> = { ...s.agents };
      for (const a of agents) {
        const existing = merged[a.id];
        if (existing) {
          const isTerminal = ["completed", "failed", "cancelled"].includes(a.status);
          merged[a.id] = {
            ...existing,
            name: a.name,
            taskId: a.taskId || existing.taskId,
            missionId: a.missionId ?? existing.missionId,
            status: a.status,
            worktreePath: a.worktreePath ?? existing.worktreePath,
            currentStep: Math.max(a.currentStep, existing.currentStep),
            tokensUsed: Math.max(a.tokensUsed, existing.tokensUsed),
            costUsd: Math.max(a.costUsd, existing.costUsd),
            streamBuffer: isTerminal ? "" : existing.streamBuffer,
            shellBuffer: isTerminal ? "" : existing.shellBuffer,
          };
        } else {
          merged[a.id] = a;
        }
      }
      return { agents: merged };
    }),
}));
