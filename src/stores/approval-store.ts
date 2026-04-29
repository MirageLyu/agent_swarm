/**
 * FM-14: Approval Queue store.
 *
 * 单一全局队列；消费者：
 * - <ApprovalQueue/> 在 Sidebar / 顶栏显示
 * - <ApprovalBadge/> 仅显示数字
 * - Mission 详情页可以选择 filterByMission 来本地化过滤
 *
 * 订阅模式：
 * - 应用启动时 `init()` 拉取一次 + 订阅 `approval-requested` / `approval-resolved`
 * - 任意命令成功（resolve / resolveAll）后由调用方再 fetch 一次以获取最新 expires_at 等字段
 *   （事件流只够通知，不够保证字段完整一致）。
 */
import { create } from "zustand";
import { commands } from "../ipc/commands";
import type { ApprovalView } from "../ipc/commands";
import {
  onApprovalRequested,
  onApprovalResolved,
  type ApprovalRequestedPayload,
  type ApprovalResolvedPayload,
} from "../ipc/events";
import type { UnlistenFn } from "@tauri-apps/api/event";

interface ApprovalState {
  /** 全部 pending 审批，按 created_at 升序 */
  items: ApprovalView[];
  loading: boolean;
  /** 后端连接错误（白屏 fallback：让 UI 显示 "approval queue offline"） */
  error: string | null;
  /** 已经初始化，不需要再 init */
  initialized: boolean;
  unlistens: UnlistenFn[];
}

interface ApprovalActions {
  init(): Promise<void>;
  refresh(): Promise<void>;
  /** 当某条 approval 被本地直接 resolve 时同步移除（避免等事件 round-trip） */
  removeLocal(requestId: string): void;
  /** 仅供测试：清空状态 */
  reset(): void;
  /** 应用退出时调用 */
  dispose(): Promise<void>;
}

export const useApprovalStore = create<ApprovalState & ApprovalActions>((set, get) => ({
  items: [],
  loading: false,
  error: null,
  initialized: false,
  unlistens: [],

  async init() {
    if (get().initialized) return;
    set({ loading: true, error: null });
    try {
      const items = await commands.listPendingApprovals();
      set({ items, loading: false, initialized: true });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      set({ loading: false, error: msg, initialized: true });
    }
    const refresh = () => {
      void get().refresh();
    };
    const reqUnlisten = await onApprovalRequested((_p: ApprovalRequestedPayload) => {
      refresh();
    });
    const resUnlisten = await onApprovalResolved((p: ApprovalResolvedPayload) => {
      // 解决类事件总是把那条移出 pending 列表
      set((s) => ({ items: s.items.filter((it) => it.id !== p.request_id) }));
    });
    set((s) => ({ unlistens: [...s.unlistens, reqUnlisten, resUnlisten] }));
  },

  async refresh() {
    try {
      const items = await commands.listPendingApprovals();
      set({ items, error: null });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      set({ error: msg });
    }
  },

  removeLocal(requestId: string) {
    set((s) => ({ items: s.items.filter((it) => it.id !== requestId) }));
  },

  reset() {
    set({ items: [], loading: false, error: null, initialized: false });
  },

  async dispose() {
    const { unlistens } = get();
    await Promise.all(unlistens.map((u) => Promise.resolve(u()).catch(() => {})));
    set({ unlistens: [] });
  },
}));

/** 衍生 selector：按 mission 过滤 */
export function selectApprovalsByMission(missionId: string | null) {
  return (s: ApprovalState) =>
    missionId == null ? s.items : s.items.filter((it) => it.mission_id === missionId);
}
