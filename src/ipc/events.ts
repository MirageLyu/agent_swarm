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
