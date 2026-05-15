import { create } from "zustand";

export type AgentStatus =
  | "idle"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "waiting";

/// Single-Agent Uplift Phase 0.1: agent_events.kind 枚举与后端 migration 025 完全对齐。
/// 加新 kind 必须同时改：
///   1. src-tauri/src/db/migrations.rs 末尾追加 migration 扩 CHECK 约束
///   2. 这里
///   3. AgentTerminalPane / tool-renderers 里的分发逻辑
export type AgentEventKind =
  | "llm_call"
  | "tool_use"
  | "tool_result"
  | "checkpoint"
  | "error"
  | "message"
  | "status_change"
  | "review"
  | "system_hint"
  | "guardrail_pass"
  | "guardrail_fail"
  | "guardrail_summary"
  | "note_applied"
  | "tool_progress"
  | "tool_summary"
  | "compact"
  | "todo_update";

export interface AgentEvent {
  id: string;
  agentId: string;
  step: number;
  timestamp: string;
  kind: AgentEventKind;
  content: string;
  /// Single-Agent Uplift Phase 0.2: 结构化 payload。后端 emit_event_with_meta 写入。
  /// 前端按 kind 解析（每种 kind 有自己的 schema）：
  ///   - tool_use:     { tool, tool_use_id, input }
  ///   - tool_result:  { tool, tool_use_id, is_error, duration_ms, size_chars }
  ///   - guardrail_*:  GuardrailReport[]（直接 array，方便循环渲染）
  ///   - note_applied: { applied_count, note_ids, notes }
  ///   - tool_progress:{ kind: "llm_idle", idle_secs, step }
  /// 解析失败时 fallback 到 content 文本即可。
  meta?: unknown;
}

/// Single-Agent Uplift Phase 1.2: agent 自维护的 todo 项。
/// 一次 todo_write 调用全量替换；status 顺序约定 pending → in_progress → completed。
export interface AgentTodo {
  id: string;
  content: string;
  status: "pending" | "in_progress" | "completed" | "cancelled";
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
  /// Single-Agent Uplift Phase 1.2: agent 自维护的 todo 列表。
  /// 由后端 todo_update 事件 / list_agent_todos 命令同步刷新。
  /// 没用过 TodoWriteTool 的 agent 始终为 []，前端用空数组表示"不显示 panel"。
  todos: AgentTodo[];
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
  /// Single-Agent Uplift Phase 1.2: 全量替换某 agent 的 todo 清单。
  /// 调用方：① WorkspaceView 启动时 list_agent_todos hydrate；
  /// ② 实时收到 `todo_update` 事件后 setAgentTodos(meta.todos)。
  setAgentTodos: (agentId: string, todos: AgentTodo[]) => void;
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

  setAgentTodos: (agentId, todos) =>
    set((s) => {
      const agent = s.agents[agentId];
      if (!agent) return s;
      return {
        agents: {
          ...s.agents,
          [agentId]: { ...agent, todos },
        },
      };
    }),

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
            // todos 走 list_agent_todos / todo_update 单独 hydrate；这里保留 existing
            // 避免闪烁；新 agent 初始为空数组（来自上面构造）。
            todos: existing.todos,
          };
        } else {
          merged[a.id] = { ...a, todos: a.todos ?? [] };
        }
      }
      return { agents: merged };
    }),
}));
