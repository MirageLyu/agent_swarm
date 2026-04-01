import { invoke } from "@tauri-apps/api/core";

export interface AppInfo {
  version: string;
  data_dir: string;
}

export interface MissionInfo {
  id: string;
  title: string;
  description: string;
  status: string;
  total_cost_usd: number;
  created_at: string;
}

export interface CreateMissionRequest {
  title: string;
  description: string;
}

export interface ConfigResponse {
  default_model: string;
  base_url: string;
  provider: string;
  max_concurrent_agents: number;
  has_api_key: boolean;
}

export interface SetApiKeyRequest {
  provider: string;
  key: string;
}

export interface UpdateConfigRequest {
  default_model?: string;
  base_url?: string;
  provider?: string;
  max_concurrent_agents?: number;
}

export interface RunAgentRequest {
  task_description: string;
  workspace_path: string;
}

export interface RunAgentResponse {
  agent_id: string;
  status: string;
}

export const commands = {
  getAppInfo: () => invoke<AppInfo>("get_app_info"),

  getDbStatus: () => invoke<string>("get_db_status"),

  createMission: (request: CreateMissionRequest) =>
    invoke<MissionInfo>("create_mission", { request }),

  listMissions: () => invoke<MissionInfo[]>("list_missions"),

  getConfig: () => invoke<ConfigResponse>("get_config"),

  setApiKey: (request: SetApiKeyRequest) => invoke<void>("set_api_key", { request }),

  updateConfig: (request: UpdateConfigRequest) => invoke<void>("update_config", { request }),

  runAgent: (request: RunAgentRequest) => invoke<RunAgentResponse>("run_agent", { request }),

  stopAgent: (agentId: string) => invoke<void>("stop_agent", { agentId }),
};
