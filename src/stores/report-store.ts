import { create } from "zustand";
import { commands, type MissionReportView } from "../ipc/commands";

/**
 * FM-12 Mission Report 状态。
 *
 * 与 ApprovalStore 不同，这个 store 是按 mission 维度懒加载，不订阅事件流：
 * - 用户进入 ReportView 时主动调用 load(missionId)
 * - 同一个 mission 短时间内重复 load 走缓存
 * - "重新生成"是显式动作（generate），失败/陈旧时手动触发
 *
 * 缓存策略：内存级 Map<missionId, MissionReportView>。
 * 退出 view 不清空（用户可能在 missions 列表来回切），但生成新报告会覆盖。
 */
interface ReportState {
  /** 当前正在加载的 missionId，避免并发重复请求 */
  loadingMissionId: string | null;
  /** 当前正在生成的 missionId */
  generatingMissionId: string | null;
  /** missionId → 报告视图的缓存 */
  reports: Map<string, MissionReportView>;
  /** missionId → 错误消息 */
  errors: Map<string, string>;

  load: (missionId: string, opts?: { force?: boolean }) => Promise<MissionReportView | null>;
  generate: (missionId: string) => Promise<MissionReportView | null>;
  /** 本地更新某个 decision 的投票（在 commands.voteDecision 成功后调用） */
  recordVote: (missionId: string, decisionId: string, vote: "agree" | "disagree") => void;
  clear: (missionId?: string) => void;
}

export const useReportStore = create<ReportState>((set, get) => ({
  loadingMissionId: null,
  generatingMissionId: null,
  reports: new Map(),
  errors: new Map(),

  load: async (missionId, opts) => {
    const force = opts?.force ?? false;
    const state = get();

    if (!force && state.reports.has(missionId)) {
      return state.reports.get(missionId)!;
    }
    if (state.loadingMissionId === missionId) {
      return null; // 已有同 mission 的请求在飞
    }

    set({ loadingMissionId: missionId });
    try {
      const view = await commands.getMissionReport(missionId);
      const reports = new Map(get().reports);
      const errors = new Map(get().errors);
      errors.delete(missionId);
      if (view) {
        reports.set(missionId, view);
      } else {
        reports.delete(missionId);
      }
      set({ reports, errors, loadingMissionId: null });
      return view;
    } catch (err) {
      const errors = new Map(get().errors);
      errors.set(missionId, err instanceof Error ? err.message : String(err));
      set({ errors, loadingMissionId: null });
      throw err;
    }
  },

  generate: async (missionId) => {
    const state = get();
    if (state.generatingMissionId === missionId) return null;

    set({ generatingMissionId: missionId });
    try {
      await commands.generateMissionReport(missionId);
      // 生成成功后立即拉一次最新数据
      set({ generatingMissionId: null });
      return await get().load(missionId, { force: true });
    } catch (err) {
      const errors = new Map(get().errors);
      errors.set(missionId, err instanceof Error ? err.message : String(err));
      set({ errors, generatingMissionId: null });
      throw err;
    }
  },

  recordVote: (missionId, decisionId, vote) => {
    const reports = new Map(get().reports);
    const view = reports.get(missionId);
    if (!view) return;
    const others = view.votes.filter((v) => v.decision_id !== decisionId);
    reports.set(missionId, {
      ...view,
      votes: [...others, { decision_id: decisionId, vote }],
    });
    set({ reports });
  },

  clear: (missionId) => {
    if (missionId === undefined) {
      set({ reports: new Map(), errors: new Map() });
      return;
    }
    const reports = new Map(get().reports);
    const errors = new Map(get().errors);
    reports.delete(missionId);
    errors.delete(missionId);
    set({ reports, errors });
  },
}));
