import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface AgentEventPayload {
  agent_id: string;
  step: number;
  kind: string;
  content: string;
}

export interface AgentStreamPayload {
  agent_id: string;
  step: number;
  kind: string;
  content: string;
}

export interface AgentStartedPayload {
  agent_id: string;
  task_id: string;
  worktree_path: string;
}

export interface TaskStatusChangedPayload {
  task_id: string;
  from: string;
  to: string;
}

export interface MissionStatusChangedPayload {
  mission_id: string;
  from: string;
  to: string;
}

export function onAgentEvent(callback: (payload: AgentEventPayload) => void): Promise<UnlistenFn> {
  return listen<AgentEventPayload>("agent-event", (event) => {
    callback(event.payload);
  });
}

export function onAgentStream(
  callback: (payload: AgentStreamPayload) => void,
): Promise<UnlistenFn> {
  return listen<AgentStreamPayload>("agent-stream", (event) => {
    callback(event.payload);
  });
}

export function onAgentStarted(
  callback: (payload: AgentStartedPayload) => void,
): Promise<UnlistenFn> {
  return listen<AgentStartedPayload>("agent-started", (event) => {
    callback(event.payload);
  });
}

export function onTaskStatusChanged(
  callback: (payload: TaskStatusChangedPayload) => void,
): Promise<UnlistenFn> {
  return listen<TaskStatusChangedPayload>("task-status-changed", (event) => {
    callback(event.payload);
  });
}

export function onMissionStatusChanged(
  callback: (payload: MissionStatusChangedPayload) => void,
): Promise<UnlistenFn> {
  return listen<MissionStatusChangedPayload>("mission-status-changed", (event) => {
    callback(event.payload);
  });
}

// FM-10: Pre-flight stream events

export interface PreflightStreamPayload {
  session_id: string;
  chunk: {
    kind:
      | "start"
      | "text_delta"
      | "reasoning_delta"
      | "done"
      | "error"
      | "status"
      | "choices"
      | "contract_item_added"
      | "suggest_sign"
      | "mode_switched";
    content: string;
  };
}

export function onPreflightStream(
  callback: (payload: PreflightStreamPayload) => void,
): Promise<UnlistenFn> {
  return listen<PreflightStreamPayload>("preflight-stream", (event) => {
    callback(event.payload);
  });
}

// FM-11: Evaluator events

export interface EvaluationCompletePayload {
  agent_id: string;
  overall_score: number;
  annotation_count: number;
}

export function onEvaluationComplete(
  callback: (payload: EvaluationCompletePayload) => void,
): Promise<UnlistenFn> {
  return listen<EvaluationCompletePayload>("evaluation-complete", (event) => {
    callback(event.payload);
  });
}

// FM-07: Planner stream events

export interface PlannerStreamPayload {
  kind: "text_delta" | "reasoning_delta" | "done" | "error";
  content: string;
}

export function onPlannerStream(
  callback: (payload: PlannerStreamPayload) => void,
): Promise<UnlistenFn> {
  return listen<PlannerStreamPayload>("planner-stream", (event) => {
    callback(event.payload);
  });
}
