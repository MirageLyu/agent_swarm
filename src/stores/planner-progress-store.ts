/**
 * 全局 Planner 进度 store。
 *
 * 之前 `PlannerLoopPanel` 浮窗挂在 PreflightView 内：
 * 切走 view 浮窗就被 unmount，事件还在跑但用户看不到，回来又重新订阅、
 * 历史丢失，体验极差。
 *
 * 把"当前是否有正在跑的 Planner 流程"抽到全局 store，由 App 根挂载的
 * `<PlannerProgressOverlay>` 监听并 Portal 渲染。任意 view 切换都不会
 * 销毁浮窗本身，事件流持续可见。
 *
 * 设计要点：
 * - 只放"识别 + 元信息"，**不**放 steps 列表（步骤数据由 PlannerLoopPanel
 *   自己订阅 `planner-step` 事件维护，避免 store 变成事件转发中转）。
 * - `kind` 区分场景：`sign_contract` / `plan_mission` / 其他 future planner
 *   入口。完成时根据 kind 决定要不要展示"跳转 DAG"按钮。
 * - `completed` 是一个短暂态：成功后浮窗显示 ✓ 与跳转按钮，让用户主动跳；
 *   失败 / 取消时直接 clear，由 view 的 failure banner 接管错误展示。
 */
import { create } from "zustand";

export type PlannerProgressKind = "sign_contract" | "plan_mission";

export interface PlannerProgressActive {
  kind: PlannerProgressKind;
  /** 关联的 mission_id：完成后用它跳转 DAG / 选中 mission */
  missionId: string;
  /** 已知的 planner_session_id；从 sign_contract 启动时为空，浮窗会从首个 planner-step 事件自动发现 */
  sessionId?: string;
  /** 浮窗 header 标题，i18n 后字面量 */
  label: string;
  /** 启动毫秒戳，方便未来加"已运行 N 秒"显示 */
  startedAt: number;
}

interface PlannerProgressState {
  active: PlannerProgressActive | null;
  /**
   * 流程成功完成的 mission id；浮窗据此显示 ✓ 与"查看任务图"按钮。
   * 非 null 时 `active` 也仍然为 null —— 两个状态互斥：要么在跑，要么已完成。
   */
  completed: { missionId: string; kind: PlannerProgressKind } | null;

  setActive: (a: PlannerProgressActive) => void;
  /**
   * 流程成功完成：清掉 active，转入 completed 态。
   * 调用方在适当时机（用户点跳转 / overlay 自动消散）再调 clear()。
   */
  markCompleted: (params: { missionId: string; kind: PlannerProgressKind }) => void;
  /** 失败 / 取消 / 用户手动关：直接清空。 */
  clear: () => void;
}

export const usePlannerProgressStore = create<PlannerProgressState>((set) => ({
  active: null,
  completed: null,

  setActive: (active) => set({ active, completed: null }),
  markCompleted: ({ missionId, kind }) =>
    set({ active: null, completed: { missionId, kind } }),
  clear: () => set({ active: null, completed: null }),
}));
