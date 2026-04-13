import { invoke } from "@tauri-apps/api/core";

export interface AppInfo {
  version: string;
  data_dir: string;
}

// ---------- Mission / Task types ----------

export type MissionStatus = "draft" | "preflight" | "planned" | "running" | "completed" | "failed";
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

export interface SetTaskDependenciesRequest {
  task_id: string;
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

// ---------- FM-04: Activity stream & cost tracking ----------

export interface ListAgentEventsRequest {
  mission_id?: string;
  agent_id?: string;
}

export interface MissionCostSummary {
  total_cost: number;
  total_input_tokens: number;
  total_output_tokens: number;
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

// ---------- FM-05: Code Review & Diff ----------

export type ReviewAction = "approved" | "rejected" | "revision_requested";

export interface DiffFile {
  path: string;
  status: "added" | "modified" | "deleted";
  old_content: string | null;
  new_content: string | null;
}

export interface AgentDiffResponse {
  agent_id: string;
  files: DiffFile[];
  review_status: ReviewAction | null;
}

export interface SubmitReviewActionRequest {
  agent_id: string;
  action: ReviewAction;
  comment?: string;
}

// ---------- FM-06: Runtime Intervention ----------

export type NoteStatus = "queued" | "applied" | "expired";

export interface AgentNoteRecord {
  id: string;
  agent_id: string;
  content: string;
  status: NoteStatus;
  created_at: string;
  applied_at: string | null;
  mission_id: string | null;
}

export interface InjectAgentNoteRequest {
  agent_id: string;
  note: string;
}

export interface InjectAgentNoteResponse {
  note_id: string;
}

export interface InjectMissionNoteRequest {
  mission_id: string;
  note: string;
}

export interface InjectMissionNoteResponse {
  note_ids: string[];
  agent_count: number;
}

// ---------- Mission Template Export / Import ----------

export interface ExportMissionTemplateRequest {
  mission_id: string;
  file_path: string;
}

// ---------- FM-10: Pre-flight & Contract ----------

export type PreflightMode = "scenario_walk" | "devils_advocate" | "risk_highlighter";
export type ContractSection = "scope" | "constraints" | "exclusions" | "assumptions";
export type ContractStatus = "drafting" | "signed";

export interface StartPreflightRequest {
  description: string;
}

export interface StartPreflightResponse {
  mission_id: string;
  session_id: string;
}

export interface SendPreflightMessageRequest {
  session_id: string;
  message: string;
  mode: PreflightMode;
}

export interface AddContractItemRequest {
  mission_id: string;
  section: ContractSection;
  text: string;
}

export interface RemoveContractItemRequest {
  mission_id: string;
  item_id: string;
}

export interface UpdateContractConfigRequest {
  mission_id: string;
  budget_usd?: number;
  quality_threshold?: number;
  max_duration_hours?: number;
}

export interface ContractImpact {
  section: ContractSection;
  text: string;
}

export interface PreflightChoice {
  id: string;
  label: string;
  contract_impact: ContractImpact | null;
}

export interface ContractItemInfo {
  id: string;
  section: ContractSection;
  text: string;
  source: "user" | "agent";
  created_at: string;
}

export interface ContractInfo {
  id: string;
  mission_id: string;
  status: ContractStatus;
  budget_usd: number | null;
  quality_threshold: number | null;
  max_duration_hours: number | null;
  signed_at: string | null;
  items: ContractItemInfo[];
}

export interface PreflightMessageInfo {
  role: "user" | "assistant";
  content: string;
  choices: PreflightChoice[];
  mode?: PreflightMode;
}

export interface PreflightSessionInfo {
  id: string;
  mode: PreflightMode;
  messages: PreflightMessageInfo[];
  convergence_score: number;
  phase: ConversationPhase;
}

export type ConversationPhase = "exploring" | "narrowing" | "confirming" | "ready_to_sign";

// ---------- FM-10.6: Decision Log ----------

export type DecisionType = "confirmed" | "rejected" | "inferred" | "revised" | "skipped";

export interface Alternative {
  label: string;
  reason_rejected: string;
}

export interface DecisionEntry {
  id: string;
  session_id: string;
  round: number;
  decision_type: DecisionType;
  description: string;
  rationale: string;
  alternatives: Alternative[];
  contract_item_id: string | null;
  created_at: string;
}

export interface GetDecisionLogRequest {
  session_id: string;
  decision_type?: DecisionType;
}

// ---------- FM-11: Evaluator Agent ----------

export type AnnotationType = "bug" | "style" | "performance" | "security" | "suggestion";
export type AnnotationSeverity = "error" | "warning" | "info";
export type AnnotationStatus = "open" | "auto_fixed" | "revision_requested" | "dismissed";

export interface TriggerEvaluationResponse {
  evaluator_agent_id: string;
}

export interface EvaluationResult {
  agent_id: string;
  overall_score: number;
  summary: string;
  contract_compliance: string | null;
  annotation_count: number;
  auto_fixed_count: number;
  needs_review_count: number;
  created_at: string;
}

export interface AnnotationInfo {
  id: string;
  review_id: string;
  agent_id: string;
  file_path: string;
  line_number: number;
  type: AnnotationType;
  severity: AnnotationSeverity;
  status: AnnotationStatus;
  message: string;
  suggestion: string | null;
  auto_fixable: boolean;
  original_code: string | null;
  fixed_code: string | null;
  created_at: string;
}

export interface GetAnnotationsRequest {
  agent_id: string;
  file_path?: string;
}

export interface UpdateAnnotationStatusRequest {
  annotation_id: string;
  status: AnnotationStatus;
}

// ---------- FM-08: Mission Lifecycle ----------

export interface DeleteMissionRequest {
  mission_id: string;
  clean_workspace: boolean;
}

export interface RestartMissionRequest {
  mission_id: string;
  mode: "full" | "failed_only";
}

export interface RestartResult {
  reset_count: number;
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

  deleteMission: (request: DeleteMissionRequest) =>
    invoke<void>("delete_mission", { request }),

  // Task CRUD
  updateTask: (request: UpdateTaskRequest) => invoke<void>("update_task", { request }),

  deleteTask: (taskId: string) => invoke<void>("delete_task", { taskId }),

  addTask: (request: AddTaskRequest) => invoke<TaskInfo>("add_task", { request }),

  setTaskDependencies: (request: SetTaskDependenciesRequest) =>
    invoke<void>("set_task_dependencies", { request }),

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

  // FM-04: Activity stream & cost tracking
  listAgentEvents: (request: ListAgentEventsRequest) =>
    invoke<AgentEventRecord[]>("list_agent_events", { request }),

  getMissionCostSummary: (missionId: string) =>
    invoke<MissionCostSummary>("get_mission_cost_summary", { missionId }),

  // FM-05: Code Review & Diff
  getAgentDiff: (agentId: string) =>
    invoke<AgentDiffResponse>("get_agent_diff", { agentId }),

  submitReviewAction: (request: SubmitReviewActionRequest) =>
    invoke<void>("submit_review_action", { request }),

  // FM-06: Runtime Intervention
  injectAgentNote: (request: InjectAgentNoteRequest) =>
    invoke<InjectAgentNoteResponse>("inject_agent_note", { request }),

  listAgentNotes: (agentId: string) =>
    invoke<AgentNoteRecord[]>("list_agent_notes", { agentId }),

  injectMissionNote: (request: InjectMissionNoteRequest) =>
    invoke<InjectMissionNoteResponse>("inject_mission_note", { request }),

  listMissionNotes: (missionId: string) =>
    invoke<AgentNoteRecord[]>("list_mission_notes", { missionId }),

  // FM-08: Mission Lifecycle
  stopMissionExecution: (missionId: string) =>
    invoke<void>("stop_mission_execution", { missionId }),

  restartMission: (request: RestartMissionRequest) =>
    invoke<RestartResult>("restart_mission", { request }),

  // Mission Template Export / Import
  exportMissionTemplate: (request: ExportMissionTemplateRequest) =>
    invoke<void>("export_mission_template", { request }),

  importMissionTemplate: (filePath: string) =>
    invoke<MissionInfo>("import_mission_template", { filePath }),

  // FM-10: Pre-flight & Contract
  startPreflight: (request: StartPreflightRequest) =>
    invoke<StartPreflightResponse>("start_preflight", { request }),

  sendPreflightMessage: (request: SendPreflightMessageRequest) =>
    invoke<void>("send_preflight_message", { request }),

  addContractItem: (request: AddContractItemRequest) =>
    invoke<ContractItemInfo>("add_contract_item", { request }),

  removeContractItem: (request: RemoveContractItemRequest) =>
    invoke<void>("remove_contract_item", { request }),

  updateContractConfig: (request: UpdateContractConfigRequest) =>
    invoke<void>("update_contract_config", { request }),

  getContract: (missionId: string) =>
    invoke<ContractInfo>("get_contract", { missionId }),

  getPreflightSession: (missionId: string) =>
    invoke<PreflightSessionInfo | null>("get_preflight_session", { missionId }),

  signContract: (missionId: string) =>
    invoke<PlanMissionResponse>("sign_contract", { missionId }),

  getDecisionLog: (request: GetDecisionLogRequest) =>
    invoke<DecisionEntry[]>("get_decision_log", { request }),

  // FM-11: Evaluator Agent
  triggerEvaluation: (agentId: string) =>
    invoke<TriggerEvaluationResponse>("trigger_evaluation", { agentId }),

  getEvaluationResult: (agentId: string) =>
    invoke<EvaluationResult | null>("get_evaluation_result", { agentId }),

  getAnnotations: (request: GetAnnotationsRequest) =>
    invoke<AnnotationInfo[]>("get_annotations", { request }),

  updateAnnotationStatus: (request: UpdateAnnotationStatusRequest) =>
    invoke<void>("update_annotation_status", { request }),
};
