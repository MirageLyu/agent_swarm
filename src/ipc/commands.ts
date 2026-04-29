import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/**
 * 包一层 invoke：当 Tauri IPC bridge 不可用（例如开发时直接用浏览器访问
 * vite dev server，或者 webview 还没注入 `window.__TAURI_INTERNALS__`）时，
 * `invoke()` 会**同步**抛出 `Cannot read properties of undefined (reading 'invoke')`。
 *
 * 该同步 throw 会把任何 useEffect 回调炸掉，React 在没有 ErrorBoundary
 * 兜底时会 unmount 整棵 tree，最终表现为白屏。
 *
 * 这里把同步 throw 转成 rejected promise，让所有 `.catch(...)` 路径正常
 * 工作。配合 `App.tsx` 里的 ErrorBoundary，至少 UI 不会再因 IPC 不可用而全白。
 */
function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return tauriInvoke<T>(cmd, args);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(`[ipc] invoke("${cmd}") threw synchronously, IPC bridge unavailable`, err);
    return Promise.reject(err instanceof Error ? err : new Error(String(err)));
  }
}

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
  /** FM-15 v2.2 (S4): 角色 / 富语义字段。后端始终下发；旧 mission 默认值为 implementer。 */
  role?: string;
  expected_output?: string | null;
  /** 列表型字段以原始 JSON 字符串透传（后端写入时即为 JSON），前端按需 parse。 */
  additional_skills_json?: string | null;
  produces_artifacts_json?: string | null;
  consumes_artifacts_json?: string | null;
  file_scope_hints_json?: string | null;
  /** 最近一次失败原因（带分类前缀：timeout: / max_steps: / guardrail: / cancelled: / agent_error: 等） */
  last_error?: string | null;
  /** 最近一次失败时间（UTC ISO8601） */
  last_failed_at?: string | null;
}

export interface DependencyInfo {
  task_id: string;
  depends_on: string;
  /** FM-15 v2.2 (S4): 该依赖边上承载的 artifact id 列表，JSON 字符串。 */
  artifact_refs_json?: string | null;
}

export interface MissionDetail {
  mission: MissionInfo;
  tasks: TaskInfo[];
  dependencies: DependencyInfo[];
}

export type RepoOrigin = "from_scratch" | "from_existing";

export interface CreateMissionRequest {
  title: string;
  description: string;
  /** FM-15 v2.2 / FR-18: 必填（S3 起前端全量传值）。 */
  repo_origin?: RepoOrigin;
  /** from_existing 必填；from_scratch 时由后端按 mission slug 生成。 */
  repo_path?: string;
}

export interface CreateMissionResponse extends MissionInfo {
  repo_path: string | null;
  repo_origin: RepoOrigin | null;
}

export interface PlanMissionRequest {
  /** FM-15 v2.2 (S2): mission-first。先 createMission 拿到 mission_id，再 plan。 */
  mission_id: string;
}

export interface PlanMissionResponse {
  mission_id: string;
  tasks: TaskInfo[];
  /** FM-15 v2.2: PlannerEngine 路径下产生的 session id（旧路径为 null/undefined） */
  planner_session_id?: string | null;
}

// FM-15 v2.2 Slice 1: Planner Loop session inspection

export interface PlannerSessionRow {
  id: string;
  mission_id: string | null;
  kind: string;
  contract_id: string | null;
  repo_path: string;
  description: string;
  status: "running" | "completed" | "failed" | "cancelled";
  total_steps: number;
  total_tokens: number;
  error_message: string | null;
  created_at: string;
  completed_at: string | null;
}

export interface PlannerStepRow {
  id: string;
  session_id: string;
  step_no: number;
  kind: string;
  tool_name: string | null;
  tool_args: string | null;
  tool_result: string | null;
  text_content: string | null;
  tokens_used: number;
  created_at: string;
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
  /** Coding Agent 单任务最大步数（FR-11 硬上限） */
  max_agent_steps: number;
  /** Coding Agent wall-clock 超时（秒），整个任务从开始到结束 */
  agent_timeout_seconds: number;
  /** LLM 流式相邻 chunk 的静默上限（秒），0 表示关闭 idle 检测 */
  agent_step_idle_seconds: number;
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
  max_agent_steps?: number;
  agent_timeout_seconds?: number;
  agent_step_idle_seconds?: number;
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
  /** FM-15 v2.2 (S2): mission-first。先 createMission 拿到 mission_id，再 startPreflight。 */
  mission_id: string;
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

// ---------- FM-15 FR-02: Skills ----------

export type SkillSource = "builtin" | "user" | "project";

export interface SkillMeta {
  id: string;
  description: string;
  /** 可选 tool 白名单（YAML frontmatter 里 `tools:`），SKILL.md 没声明就是 null */
  tools?: string[] | null;
  /** 可选兼容角色（YAML frontmatter 里 `compatible_roles:`） */
  compatible_roles?: string[] | null;
  source: SkillSource;
  source_path: string;
}

export interface ListSkillsResponse {
  skills: SkillMeta[];
}

// ---------- FM-15 FR-05.x: Planner fetch_url confirmation ----------

export type FetchDecision = "allow_once" | "allow_session" | "deny";

export interface PlannerFetchConfirmationEvent {
  request_id: string;
  session_id: string;
  url: string;
  host: string;
  reason: string;
}

export interface ConfirmPlannerFetchRequest {
  request_id: string;
  decision: FetchDecision;
}

export interface ConfirmPlannerFetchResponse {
  /** false = request_id 已经过期/超时/被其他人 resolve */
  delivered: boolean;
}

// ---------- FM-15 FR-03: Artifacts ----------

export type ArtifactType =
  | "design_doc"
  | "api_spec"
  | "schema"
  | "code_module"
  | "test_module"
  | "config"
  | "docs"
  | "report";

export interface ArtifactInfo {
  id: string;
  mission_id: string;
  producer_task_id: string;
  type: ArtifactType;
  local_name: string;
  summary: string;
  file_paths: string[];
  /** false = Planner 声明，但 Coding Agent 还没产出；true = 已发布 */
  published: boolean;
  created_at: string;
}

export interface ListArtifactsResponse {
  artifacts: ArtifactInfo[];
}

// ---------- FM-15 v2.2 P4-S5: Follow-up Chat ----------

export type ChatMessageRole = "user" | "assistant" | "system";

export interface ChatMessageInfo {
  id: string;
  mission_id: string;
  role: ChatMessageRole;
  content: string;
  tool_calls?: string | null;
  artifact_refs?: string | null;
  proposed_followup_mission_id?: string | null;
  created_at: string;
}

export interface SendChatMessageRequest {
  mission_id: string;
  content: string;
  force_direct?: boolean;
}

export interface FollowupProposedSummary {
  mission_id: string;
  chat_message_id: string;
  title: string;
  rationale: string;
  estimated_tasks: number;
  request_summary: string;
}

export type ChatTurnStatus =
  | "committed"
  | "answered"
  | "proposed"
  | "rejected_oversize"
  | "commit_failed"
  | "failed"
  | "timeout";

export interface ChatTurnSummary {
  mission_id: string;
  user_message_id: string;
  assistant_message_id: string;
  status: ChatTurnStatus;
  commit_hash?: string | null;
  files_changed?: number | null;
  lines_changed?: number | null;
  error?: string | null;
  proposed_followup?: FollowupProposedSummary | null;
}

export interface ConfirmFollowupRequest {
  parent_mission_id: string;
  title: string;
  request_summary: string;
  repo_path_override?: string;
}

export interface ConfirmFollowupResponse {
  child_mission_id: string;
  repo_path: string;
}

// ---------- FM-08: Mission Lifecycle ----------

// ---------- FM-14: Approval Queue ----------

export type ApprovalKind = "tool" | "fetch" | "escalation" | "budget" | "chat_commit";
export type ApprovalStatus =
  | "pending"
  | "approved"
  | "rejected"
  | "expired"
  | "cancelled";

/** 数据库行的前端镜像；payload/reason/context_summary 是字符串以保持透传。 */
export interface ApprovalView {
  id: string;
  mission_id: string;
  kind: ApprovalKind;
  agent_id: string | null;
  planner_session_id: string | null;
  chat_message_id: string | null;
  title: string;
  /** JSON 字符串；前端按 kind 解析（fetch -> {url,host}, tool -> {tool_name,input,...}）。 */
  payload: string;
  reason: string;
  context_summary: string;
  status: ApprovalStatus;
  decision_note: string | null;
  decided_by: string | null;
  resolved_at: string | null;
  expires_at: string;
  created_at: string;
}

export interface ResolveApprovalRequest {
  request_id: string;
  /** "approved" | "rejected" — 仅人为决定可经此命令传入 */
  decision: "approved" | "rejected";
  /** 可选备注（reject 必填，approve 可选；fetch 类用 "once" / "session" 区分） */
  note?: string | null;
}

export interface ResolveApprovalResponse {
  delivered: boolean;
  final_status: ApprovalStatus;
}

export interface ResolveAllApprovalsRequest {
  mission_id: string;
  decision: "approved" | "rejected";
  note?: string | null;
}

export interface ResolveAllApprovalsResponse {
  resolved_count: number;
}

export interface ApprovalPolicy {
  timeout_seconds: number;
  protected_paths: string[];
  destructive_commands: string[];
  budget_warn_ratio: number;
  chat_commit_soft_lines: number;
}

export interface UpdateApprovalPolicyRequest {
  timeout_seconds?: number | null;
  protected_paths?: string[] | null;
  destructive_commands?: string[] | null;
  budget_warn_ratio?: number | null;
  chat_commit_soft_lines?: number | null;
}

export interface DeleteMissionRequest {
  mission_id: string;
  clean_workspace: boolean;
}

export interface RestartMissionRequest {
  mission_id: string;
  mode: "full" | "failed_only";
  /** 复用上次 repo_path 直接拉起 scheduler，跳过工作区选择对话框 */
  auto_start?: boolean;
}

export interface RestartResult {
  reset_count: number;
  /** 实际是否已经被一键重跑拉起；为 false 时前端需 fallback 到工作区选择 */
  auto_started: boolean;
  /** 复用的 repo_path（若有） */
  repo_path: string | null;
}

// ---------- FM-12: Mission Report ----------

export interface MissionReportMission {
  id: string;
  title: string;
  description: string;
  status: string;
  started_at: string;
  completed_at: string | null;
  duration_seconds: number;
  total_cost_usd: number;
  main_branch: string | null;
}

export interface MissionReportMetrics {
  tasks_total: number;
  tasks_completed: number;
  tasks_failed: number;
  duration_seconds: number;
  total_cost_usd: number;
  avg_quality_score: number | null;
  auto_fixes: number;
  review_reduction_rate: number | null;
}

export interface MissionReportSummary {
  executive: string;
  metrics: MissionReportMetrics;
}

export interface MissionReportDecision {
  id: string;
  title: string;
  rationale: string;
  trade_off: string;
  risk: string;
}

export interface MissionReportEvaluatorRound {
  agent_id: string;
  agent_name: string;
  task_title: string;
  score: number;
  issues: number;
  auto_fixed: number;
  summary: string;
  created_at: string;
}

export interface MissionReportEvaluatorReview {
  rounds: MissionReportEvaluatorRound[];
  total_issues: number;
  auto_fixed: number;
}

export interface MissionReportTaskRow {
  task_id: string;
  title: string;
  agent_id: string | null;
  agent_name: string | null;
  score: number | null;
  cost_usd: number;
  duration_seconds: number | null;
  status: string;
}

export interface MissionReportCostModel {
  model: string;
  input_tokens: number;
  output_tokens: number;
  cost_usd: number;
}

export interface MissionReportCostTask {
  task_id: string;
  title: string;
  cost_usd: number;
}

export interface MissionReportCostAgent {
  agent_id: string;
  agent_name: string;
  cost_usd: number;
}

export interface MissionReportCostBreakdown {
  total_usd: number;
  total_input_tokens: number;
  total_output_tokens: number;
  by_model: MissionReportCostModel[];
  by_task: MissionReportCostTask[];
  by_agent: MissionReportCostAgent[];
  budget_usd: number | null;
  budget_used_ratio: number | null;
}

export interface MissionReportContractItem {
  section: string;
  text: string;
  achieved: boolean;
}

export interface MissionReportContract {
  status: string;
  items: MissionReportContractItem[];
  budget_usd: number | null;
  quality_threshold: number | null;
  max_duration_hours: number | null;
}

export interface MissionReportArtifact {
  artifact_type: string;
  local_name: string;
  summary: string;
  file_paths: string[];
}

export interface MissionReportLearningFlywheel {
  past_decision_patterns: string[];
  insight: string;
}

export interface MissionReportData {
  schema_version: number;
  mission: MissionReportMission;
  summary: MissionReportSummary;
  decisions: MissionReportDecision[];
  evaluator_review: MissionReportEvaluatorReview;
  task_matrix: MissionReportTaskRow[];
  cost_breakdown: MissionReportCostBreakdown;
  limitations: string[];
  contract: MissionReportContract | null;
  artifacts: MissionReportArtifact[];
  learning_flywheel: MissionReportLearningFlywheel;
}

export interface DecisionVoteView {
  decision_id: string;
  vote: "agree" | "disagree";
}

export interface MissionReportView {
  report_id: string;
  mission_id: string;
  generated_at: string;
  schema_version: number;
  report: MissionReportData;
  votes: DecisionVoteView[];
}

export interface GenerateMissionReportResponse {
  report_id: string;
  generated_at: string;
}

export interface VoteDecisionRequest {
  report_id: string;
  decision_id: string;
  vote: "agree" | "disagree";
}

export interface VoteDecisionResponse {
  report_id: string;
  decision_id: string;
  vote: "agree" | "disagree";
}

export interface ExportReportMarkdownRequest {
  mission_id: string;
  output_path: string;
}

export interface ExportReportMarkdownResponse {
  bytes_written: number;
  output_path: string;
}

// ---------- commands ----------

export const commands = {
  getAppInfo: () => invoke<AppInfo>("get_app_info"),

  getDbStatus: () => invoke<string>("get_db_status"),

  // Mission CRUD
  createMission: (request: CreateMissionRequest) =>
    invoke<CreateMissionResponse>("create_mission", { request }),

  listMissions: () => invoke<MissionInfo[]>("list_missions"),

  planMission: (request: PlanMissionRequest) =>
    invoke<PlanMissionResponse>("plan_mission", { request }),

  getPlannerSession: (sessionId: string) =>
    invoke<PlannerSessionRow | null>("get_planner_session", { sessionId }),

  listPlannerSteps: (sessionId: string) =>
    invoke<PlannerStepRow[]>("list_planner_steps", { sessionId }),

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

  // FM-15 FR-02: Skills
  listSkills: () => invoke<ListSkillsResponse>("list_skills"),

  // FM-15 FR-03: Artifacts
  listMissionArtifacts: (missionId: string) =>
    invoke<ListArtifactsResponse>("list_mission_artifacts", { missionId }),

  listTaskArtifacts: (taskId: string) =>
    invoke<ListArtifactsResponse>("list_task_artifacts", { taskId }),

  // FM-15 FR-05.x: Planner fetch_url confirmation
  confirmPlannerFetch: (request: ConfirmPlannerFetchRequest) =>
    invoke<ConfirmPlannerFetchResponse>("confirm_planner_fetch", { request }),

  // FM-15 v2.2 P4-S4: Mission delivery one-click open
  openInEditor: (path: string, editor?: string) =>
    invoke<void>("open_in_editor", { path, editor: editor ?? null }),

  openInTerminal: (path: string) =>
    invoke<void>("open_in_terminal", { path }),

  openInFinder: (path: string) =>
    invoke<void>("open_in_finder", { path }),

  // FM-15 v2.2 P4-S5: Follow-up Chat
  listChatMessages: (missionId: string) =>
    invoke<ChatMessageInfo[]>("list_chat_messages", { missionId }),

  sendChatMessage: (request: SendChatMessageRequest) =>
    invoke<ChatTurnSummary>("send_chat_message", { request }),

  confirmFollowupProposal: (request: ConfirmFollowupRequest) =>
    invoke<ConfirmFollowupResponse>("confirm_followup_proposal", { request }),

  rejectFollowupProposal: (missionId: string) =>
    invoke<void>("reject_followup_proposal", { request: { mission_id: missionId } }),

  // FM-14: Approval Queue
  listPendingApprovals: (missionId?: string) =>
    invoke<ApprovalView[]>("list_pending_approvals", {
      missionId: missionId ?? null,
    }),

  getApproval: (requestId: string) =>
    invoke<ApprovalView | null>("get_approval", { requestId }),

  resolveApproval: (request: ResolveApprovalRequest) =>
    invoke<ResolveApprovalResponse>("resolve_approval", { request }),

  resolveAllApprovals: (request: ResolveAllApprovalsRequest) =>
    invoke<ResolveAllApprovalsResponse>("resolve_all_approvals", { request }),

  getApprovalPolicy: () => invoke<ApprovalPolicy>("get_approval_policy"),

  updateApprovalPolicy: (request: UpdateApprovalPolicyRequest) =>
    invoke<ApprovalPolicy>("update_approval_policy", { request }),

  // FM-12: Mission Report
  generateMissionReport: (missionId: string) =>
    invoke<GenerateMissionReportResponse>("generate_mission_report", { missionId }),

  getMissionReport: (missionId: string) =>
    invoke<MissionReportView | null>("get_mission_report", { missionId }),

  voteDecision: (request: VoteDecisionRequest) =>
    invoke<VoteDecisionResponse>("vote_decision", { request }),

  exportReportMarkdown: (request: ExportReportMarkdownRequest) =>
    invoke<ExportReportMarkdownResponse>("export_report_markdown", { request }),

  // MVP polish: diagnostic export
  exportDiagnostics: (request: ExportDiagnosticsRequest) =>
    invoke<ExportDiagnosticsResponse>("export_diagnostics", { request }),
};

export interface ExportDiagnosticsRequest {
  output_path: string;
  log_tail_lines?: number;
}

export interface ExportDiagnosticsResponse {
  bytes_written: number;
  output_path: string;
  log_files_included: number;
}
