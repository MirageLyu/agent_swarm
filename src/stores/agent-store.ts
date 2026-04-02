import { create } from "zustand";

export type AgentStatus =
  | "idle"
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export interface AgentEvent {
  id: string;
  agentId: string;
  timestamp: string;
  kind: "llm_call" | "tool_use" | "tool_result" | "checkpoint" | "error" | "message";
  content: string;
}

export interface Agent {
  id: string;
  name: string;
  taskId: string;
  status: AgentStatus;
  worktreePath: string | null;
  currentStep: number;
  totalSteps: number | null;
  tokensUsed: number;
  costUsd: number;
  events: AgentEvent[];
}

interface AgentState {
  agents: Record<string, Agent>;

  addAgent: (agent: Agent) => void;
  updateAgent: (id: string, updates: Partial<Agent>) => void;
  removeAgent: (id: string) => void;
  appendEvent: (agentId: string, event: AgentEvent) => void;
}

export const useAgentStore = create<AgentState>((set) => ({
  agents: {},

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
}));
