import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * 包一层 listen：在 Tauri IPC 不可用（例如开发时直接用浏览器访问 vite dev
 * server，或者 webview bridge 还没完全注入）时，listen() 会同步抛
 * `Cannot read properties of undefined (reading 'transformCallback')`，
 * 进而把 React effect 整段炸掉，最终导致根组件 unmount 出现白屏。
 *
 * 这里把同步 throw 转成 rejected promise + 一个 noop unlisten，让 effect
 * 既可以走正常 cleanup 路径，又不会污染 React tree。
 */
function listen<T>(
  event: string,
  handler: Parameters<typeof tauriListen<T>>[1],
): Promise<UnlistenFn> {
  try {
    return tauriListen<T>(event, handler);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(`[ipc] listen("${event}") failed synchronously, IPC bridge unavailable`, err);
    return Promise.resolve(() => {});
  }
}

export interface AgentEventPayload {
  agent_id: string;
  step: number;
  kind: string;
  content: string;
  /// Single-Agent Uplift Phase 0.2: 结构化 payload，由后端 emit_event_with_meta 携带。
  /// 后端 #[serde(skip_serializing_if = "Option::is_none")]，所以裸字符串事件不带这个字段。
  meta?: unknown;
}

export interface AgentStreamPayload {
  agent_id: string;
  step: number;
  kind: string;
  content: string;
  meta?: unknown;
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

// FM-15 v2.2 Slice 1: Planner Agent Loop step events

export type PlannerStepKind =
  | "tool_call"
  | "tool_result"
  | "tool_result_error"
  | "text"
  | "thinking"
  | "error"
  | "status";

export interface PlannerStepPayload {
  session_id: string;
  step_no: number;
  kind: PlannerStepKind;
  tool_name?: string;
  tool_args?: string;
  tool_result?: string;
  text_content?: string;
  error?: string;
}

export function onPlannerStep(
  callback: (payload: PlannerStepPayload) => void,
): Promise<UnlistenFn> {
  return listen<PlannerStepPayload>("planner-step", (event) => {
    callback(event.payload);
  });
}

export interface PlannerSessionStatusPayload {
  session_id: string;
  status: "started" | "completed" | "failed";
}

export function onPlannerSessionStatus(
  callback: (payload: PlannerSessionStatusPayload) => void,
): Promise<UnlistenFn> {
  return listen<PlannerSessionStatusPayload>("planner-session-status", (event) => {
    callback(event.payload);
  });
}

// FM-15 v2.2 (S3-4): Planner fetch_url confirmation prompt
// 后端发出此事件后，前端需要弹窗让用户决定 allow_once / allow_session / deny，
// 然后调 commands.confirmPlannerFetch(request_id, decision) 回执。

export interface PlannerFetchConfirmationPayload {
  request_id: string;
  session_id: string;
  url: string;
  host: string;
  reason: string;
}

export function onPlannerFetchConfirmation(
  callback: (payload: PlannerFetchConfirmationPayload) => void,
): Promise<UnlistenFn> {
  return listen<PlannerFetchConfirmationPayload>("planner-fetch-confirmation", (event) => {
    callback(event.payload);
  });
}

// FM-15 Phase 2 (FR-08): Mission frontier-merge progress.
// 每合并一个 frontier task 就发一次 progress；全部完成（或失败）后再发一次 completed。
//
// status:
//   - "merged"   合并成功（可能含 LLM-resolved / theirs-fallback 内部状态）
//   - "conflict" 暂未使用（保留语义，未来用作"合并保留冲突待人工处理"）
//   - "skipped"  暂未使用
//   - "error"    git 操作直接失败

export interface MissionMergeProgressPayload {
  mission_id: string;
  branch: string;
  status: "merged" | "conflict" | "skipped" | "error";
}

export function onMissionMergeProgress(
  callback: (payload: MissionMergeProgressPayload) => void,
): Promise<UnlistenFn> {
  return listen<MissionMergeProgressPayload>("mission-merge-progress", (event) => {
    callback(event.payload);
  });
}

export interface MissionMergeCompletedPayload {
  mission_id: string;
  total_merged: number;
  errors: string[];
  /// 由 git 自动 / heuristic / theirs-fallback 解决的文件路径
  auto_resolved: string[];
  /// FM-15 FR-08.2 (3): LLM 解决的文件路径
  llm_resolved: string[];
}

export function onMissionMergeCompleted(
  callback: (payload: MissionMergeCompletedPayload) => void,
): Promise<UnlistenFn> {
  return listen<MissionMergeCompletedPayload>("mission-merge-completed", (event) => {
    callback(event.payload);
  });
}

// FM-15 Phase 2 (FR-07): Task base preparation event.
// scheduler 在 dispatch_task 前合并所有已完成的父任务到 task-base/<task_id> 分支，
// 然后从该分支派生 agent worktree。本事件汇报每次准备的结果，含冲突摘要。

export interface TaskBasePreparedPayload {
  missionId: string;
  taskId: string;
  baseBranch: string;
  parentCount: number;
  conflictCount: number;
  /// "auto" | "heuristic_theirs" | "llm_failed_fallback"
  layerSummary: string;
}

export function onTaskBasePrepared(
  callback: (payload: TaskBasePreparedPayload) => void,
): Promise<UnlistenFn> {
  return listen<TaskBasePreparedPayload>("task-base-prepared", (event) => {
    callback(event.payload);
  });
}

// FM-15 v2.2 P4-S4: mission-delivered 事件。所有 frontier 都成功合入 main 后发出，
// 前端用此 payload 渲染 MissionDeliveryPanel。

export interface MissionDeliveredArtifactSummary {
  taskId: string;
  taskTitle: string;
  localName: string;
  artifactType: string;
  filePaths: string[];
  summary?: string;
}

export interface MissionDeliveredPayload {
  missionId: string;
  repoPath: string;
  mainBranch: string;
  totalTasks: number;
  totalCommits: number;
  artifacts: MissionDeliveredArtifactSummary[];
  llmResolvedFiles: string[];
  autoResolvedFiles: string[];
}

export function onMissionDelivered(
  callback: (payload: MissionDeliveredPayload) => void,
): Promise<UnlistenFn> {
  return listen<MissionDeliveredPayload>("mission-delivered", (event) => {
    callback(event.payload);
  });
}

// FM-15 follow-up: shell_exec 流式输出。后端在每次 stdout/stderr 有增量字节时
// emit 一条 chunk；前端 Workspace 按 agent_id 拼接显示，让用户能实时看到子进程输出，
// 而不必等命令结束才在 tool_result 里看到一坨。
//
// stream:
//   - "stdout" / "stderr"：进程标准输出 / 错误流的字节增量
//   - "meta"            ：watchdog 元信息（命令开始 `$ ...`、watchdog kill、spawn 失败等）
// eof: 该 stream 已读到末尾（进程退出 / 被 kill）。

export interface AgentToolStreamPayload {
  agentId: string;
  tool: string;
  stream: "stdout" | "stderr" | "meta";
  chunk: string;
  eof: boolean;
}

export function onAgentToolStream(
  callback: (payload: AgentToolStreamPayload) => void,
): Promise<UnlistenFn> {
  return listen<{
    agent_id: string;
    tool: string;
    stream: "stdout" | "stderr" | "meta";
    chunk: string;
    eof: boolean;
  }>("agent-tool-stream", (event) => {
    callback({
      agentId: event.payload.agent_id,
      tool: event.payload.tool,
      stream: event.payload.stream,
      chunk: event.payload.chunk,
      eof: event.payload.eof,
    });
  });
}

// FM-15 v2.2 P4-S5: Chat 流式输出 + Follow-up 提议事件。

export interface ChatStreamPayload {
  missionId: string;
  messageId: string;
  kind: string;
  content: string;
}

export function onChatStream(
  callback: (payload: ChatStreamPayload) => void,
): Promise<UnlistenFn> {
  return listen<{
    mission_id: string;
    message_id: string;
    kind: string;
    content: string;
  }>("chat-stream", (event) => {
    callback({
      missionId: event.payload.mission_id,
      messageId: event.payload.message_id,
      kind: event.payload.kind,
      content: event.payload.content,
    });
  });
}

export interface FollowupProposedPayload {
  missionId: string;
  chatMessageId: string;
  title: string;
  rationale: string;
  estimatedTasks: number;
  requestSummary: string;
}

export function onFollowupProposed(
  callback: (payload: FollowupProposedPayload) => void,
): Promise<UnlistenFn> {
  return listen<{
    mission_id: string;
    chat_message_id: string;
    title: string;
    rationale: string;
    estimated_tasks: number;
    request_summary: string;
  }>("followup-proposed", (event) => {
    callback({
      missionId: event.payload.mission_id,
      chatMessageId: event.payload.chat_message_id,
      title: event.payload.title,
      rationale: event.payload.rationale,
      estimatedTasks: event.payload.estimated_tasks,
      requestSummary: event.payload.request_summary,
    });
  });
}

// FM-14: Approval Queue 事件。所有审批生命周期变化都会广播到这两个 channel，
// 前端 ApprovalQueue / Sidebar Badge 订阅即可保持视图同步而无需轮询。

export interface ApprovalRequestedPayload {
  request_id: string;
  mission_id: string;
  kind: "tool" | "fetch" | "escalation" | "budget" | "chat_commit";
  title: string;
}

export function onApprovalRequested(
  callback: (payload: ApprovalRequestedPayload) => void,
): Promise<UnlistenFn> {
  return listen<ApprovalRequestedPayload>("approval-requested", (event) => {
    callback(event.payload);
  });
}

export interface ApprovalResolvedPayload {
  request_id: string;
  /** approved / rejected / expired / cancelled */
  status: string;
  /** "user" / "auto_expire" 等；和后端 decided_by 列对齐 */
  decided_by?: string;
  note?: string | null;
}

export function onApprovalResolved(
  callback: (payload: ApprovalResolvedPayload) => void,
): Promise<UnlistenFn> {
  return listen<ApprovalResolvedPayload>("approval-resolved", (event) => {
    callback(event.payload);
  });
}
