import { invoke } from "@tauri-apps/api/core";

export interface AppInfo {
  version: string;
  data_dir: string;
}

// ---------- Mission / Task types ----------

export type MissionStatus = "draft" | "planned" | "running" | "completed" | "failed";
export type TaskStatus = "pending" | "ready" | "running" | "completed" | "failed" | "cancelled";
export type Complexity = "low" | "medium" | "high";

export interface MissionInfo {
  id: string;
  title: string;
  description: string;
  status: MissionStatus;
  total_cost_usd: number;
  created_at: string;
  task_count: number;
  completed_count: number;
}

export interface TaskInfo {
  id: string;
  mission_id: string;
  title: string;
  description: string;
  status: TaskStatus;
  complexity: Complexity;
  assigned_agent_id: string | null;
  created_at: string;
  completed_at: string | null;
}

export interface DependencyInfo {
  task_id: string;
  depends_on: string;
}

export interface MissionDetail {
  mission: MissionInfo;
  tasks: TaskInfo[];
  dependencies: DependencyInfo[];
}

export interface CreateMissionRequest {
  title: string;
  description: string;
}

export interface PlanMissionRequest {
  description: string;
}

export interface PlanMissionResponse {
  mission_id: string;
  tasks: TaskInfo[];
}

export interface UpdateTaskRequest {
  task_id: string;
  title?: string;
  description?: string;
  status?: string;
}

export interface AddTaskRequest {
  mission_id: string;
  title: string;
  description: string;
  complexity: Complexity;
  depends_on: string[];
}

// ---------- Config ----------

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

// ---------- Agent ----------

export interface RunAgentRequest {
  task_description: string;
  workspace_path: string;
}

export interface RunAgentResponse {
  agent_id: string;
  status: string;
}

export interface AgentEventRecord {
  id: string;
  agent_id: string;
  step: number;
  kind: string;
  content: string;
  created_at: string;
}

export interface AgentDetail {
  id: string;
  name: string;
  status: string;
  current_step: number;
  tokens_used: number;
  cost_usd: number;
  created_at: string;
  updated_at: string;
}

// ---------- FM-02: Scheduler ----------

export interface StartMissionRequest {
  mission_id: string;
  repo_path: string;
}

export interface SchedulerStatus {
  active_agents: number;
  ready_tasks: number;
  blocked_tasks: number;
}

export interface DefaultWorkspacePath {
  path: string;
}

export interface MissionAgentInfo {
  id: string;
  name: string;
  task_id: string | null;
  status: string;
  worktree_path: string | null;
  current_step: number;
  tokens_used: number;
  cost_usd: number;
  created_at: string;
  updated_at: string;
}

// ---------- commands ----------

export const commands = {
  getAppInfo: () => invoke<AppInfo>("get_app_info"),

  getDbStatus: () => invoke<string>("get_db_status"),

  // Mission CRUD
  createMission: (request: CreateMissionRequest) =>
    invoke<MissionInfo>("create_mission", { request }),

  listMissions: () => invoke<MissionInfo[]>("list_missions"),

  planMission: (request: PlanMissionRequest) =>
    invoke<PlanMissionResponse>("plan_mission", { request }),

  getMissionDetail: (missionId: string) =>
    invoke<MissionDetail>("get_mission_detail", { missionId }),

  confirmMission: (missionId: string) => invoke<void>("confirm_mission", { missionId }),

  deleteMission: (missionId: string) => invoke<void>("delete_mission", { missionId }),

  // Task CRUD
  updateTask: (request: UpdateTaskRequest) => invoke<void>("update_task", { request }),

  deleteTask: (taskId: string) => invoke<void>("delete_task", { taskId }),

  addTask: (request: AddTaskRequest) => invoke<TaskInfo>("add_task", { request }),

  // Config
  getConfig: () => invoke<ConfigResponse>("get_config"),

  setApiKey: (request: SetApiKeyRequest) => invoke<void>("set_api_key", { request }),

  updateConfig: (request: UpdateConfigRequest) => invoke<void>("update_config", { request }),

  // Agent
  runAgent: (request: RunAgentRequest) => invoke<RunAgentResponse>("run_agent", { request }),

  stopAgent: (agentId: string) => invoke<void>("stop_agent", { agentId }),

  getAgentEvents: (agentId: string) =>
    invoke<AgentEventRecord[]>("get_agent_events", { agentId }),

  getAgentDetail: (agentId: string) =>
    invoke<AgentDetail>("get_agent_detail", { agentId }),

  listAgents: () => invoke<AgentDetail[]>("list_agents"),

  // Scheduler (FM-02)
  startMissionExecution: (request: StartMissionRequest) =>
    invoke<void>("start_mission_execution", { request }),

  getSchedulerStatus: () => invoke<SchedulerStatus>("get_scheduler_status"),

  listAgentsByMission: (missionId: string) =>
    invoke<MissionAgentInfo[]>("list_agents_by_mission", { missionId }),

  getDefaultWorkspacePath: (missionId: string) =>
    invoke<DefaultWorkspacePath>("get_default_workspace_path", { missionId }),
};
