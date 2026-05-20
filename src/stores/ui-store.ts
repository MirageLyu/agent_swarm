import { create } from "zustand";

export type ViewId =
  | "missions"
  | "preflight"
  | "workspace"
  | "agents"
  | "review"
  | "report"
  | "insights"
  | "settings";

export type Theme = "light" | "dark" | "system";
export type WorkspaceMode = "grid" | "list" | "focus";

interface UiState {
  activeView: ViewId;
  theme: Theme;
  sidebarCollapsed: boolean;
  workspaceMode: WorkspaceMode;
  commandPaletteOpen: boolean;
  dagSelectedTaskId: string | null;
  /**
   * FM-15 v2.3：DAG 视图是否显示 reference（文档型）依赖边。
   * 默认 false：避免一份架构文档扇出 N 条边把图糊成蜘蛛网。
   * 用户可以通过 DAG toolbar 的 toggle 打开查看 artifact provenance 全貌。
   */
  dagShowReferenceEdges: boolean;
  activePreflightMissionId: string | null;
  activePreflightSessionId: string | null;
  /** FM-12: 当前正在查看的 Mission Report 的 mission_id */
  activeReportMissionId: string | null;
  /**
   * MVP onboarding：API key 是否已配置。
   * - null = 还没探测过（启动初始）
   * - false = 已探测，未配置 → MissionsView 顶部显示引导 banner
   * - true = 已探测，已配置 → 不显示
   * 由 App.tsx 启动时调用 commands.getConfig().has_api_key 写入；
   * Settings 页面保存 API key 后也应回写。
   */
  apiKeyConfigured: boolean | null;
  /**
   * Single-Agent Uplift P0-3 (前端 toggle)：是否在 timeline 显示 silent
   * recovery 事件（recovery_attempt / recovery_succeeded，且 meta.silent=true）。
   *
   * 默认 false：agent 自救成功的事件不打扰用户。Developer 想 debug 恢复路径时
   * 在 Settings → Developer 打开。
   *
   * 持久化到 localStorage（key: SILENT_RECOVERY_LOCAL_KEY），避免每次启动重置。
   */
  showSilentRecoveryEvents: boolean;

  setActiveView: (view: ViewId) => void;
  setTheme: (theme: Theme) => void;
  toggleSidebar: () => void;
  setWorkspaceMode: (mode: WorkspaceMode) => void;
  setCommandPaletteOpen: (open: boolean) => void;
  setDagSelectedTaskId: (id: string | null) => void;
  setDagShowReferenceEdges: (show: boolean) => void;
  setActivePreflight: (missionId: string | null, sessionId: string | null) => void;
  /** FM-12: 切到 ReportView 同时设置查看哪个 mission；传 null 关闭报告 */
  openMissionReport: (missionId: string | null) => void;
  /** MVP onboarding：写入 API key 配置状态 */
  setApiKeyConfigured: (configured: boolean) => void;
  /** P0-3: 切换 silent recovery 事件可见性 + 同步 localStorage */
  setShowSilentRecoveryEvents: (show: boolean) => void;
}

/// localStorage 持久化键。**改名会丢用户偏好**，需要做迁移；目前没必要。
const SILENT_RECOVERY_LOCAL_KEY = "miragenty.ui.showSilentRecoveryEvents";

function loadShowSilentRecoveryEvents(): boolean {
  if (typeof window === "undefined" || !window.localStorage) return false;
  try {
    return window.localStorage.getItem(SILENT_RECOVERY_LOCAL_KEY) === "true";
  } catch {
    // SSR / private 模式 storage 抛错时安全回退到默认值
    return false;
  }
}

function persistShowSilentRecoveryEvents(show: boolean): void {
  if (typeof window === "undefined" || !window.localStorage) return;
  try {
    window.localStorage.setItem(SILENT_RECOVERY_LOCAL_KEY, show ? "true" : "false");
  } catch {
    // 写失败不影响内存 state，下次启动会回退默认 false
  }
}

export const useUiStore = create<UiState>((set) => ({
  activeView: "missions",
  theme: "system",
  sidebarCollapsed: false,
  workspaceMode: "grid",
  commandPaletteOpen: false,
  dagSelectedTaskId: null,
  dagShowReferenceEdges: false,
  activePreflightMissionId: null,
  activePreflightSessionId: null,
  activeReportMissionId: null,
  apiKeyConfigured: null,
  showSilentRecoveryEvents: loadShowSilentRecoveryEvents(),

  setActiveView: (view) => set({ activeView: view }),
  setTheme: (theme) => set({ theme }),
  toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
  setWorkspaceMode: (mode) => set({ workspaceMode: mode }),
  setCommandPaletteOpen: (open) => set({ commandPaletteOpen: open }),
  setDagSelectedTaskId: (id) => set({ dagSelectedTaskId: id }),
  setDagShowReferenceEdges: (show) => set({ dagShowReferenceEdges: show }),
  setActivePreflight: (missionId, sessionId) =>
    set({ activePreflightMissionId: missionId, activePreflightSessionId: sessionId }),
  openMissionReport: (missionId) =>
    set({
      activeReportMissionId: missionId,
      activeView: missionId ? "report" : "missions",
    }),
  setApiKeyConfigured: (configured) => set({ apiKeyConfigured: configured }),
  setShowSilentRecoveryEvents: (show) => {
    persistShowSilentRecoveryEvents(show);
    set({ showSilentRecoveryEvents: show });
  },
}));
