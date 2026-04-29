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

  setActiveView: (view: ViewId) => void;
  setTheme: (theme: Theme) => void;
  toggleSidebar: () => void;
  setWorkspaceMode: (mode: WorkspaceMode) => void;
  setCommandPaletteOpen: (open: boolean) => void;
  setDagSelectedTaskId: (id: string | null) => void;
  setActivePreflight: (missionId: string | null, sessionId: string | null) => void;
  /** FM-12: 切到 ReportView 同时设置查看哪个 mission；传 null 关闭报告 */
  openMissionReport: (missionId: string | null) => void;
  /** MVP onboarding：写入 API key 配置状态 */
  setApiKeyConfigured: (configured: boolean) => void;
}

export const useUiStore = create<UiState>((set) => ({
  activeView: "missions",
  theme: "system",
  sidebarCollapsed: false,
  workspaceMode: "grid",
  commandPaletteOpen: false,
  dagSelectedTaskId: null,
  activePreflightMissionId: null,
  activePreflightSessionId: null,
  activeReportMissionId: null,
  apiKeyConfigured: null,

  setActiveView: (view) => set({ activeView: view }),
  setTheme: (theme) => set({ theme }),
  toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
  setWorkspaceMode: (mode) => set({ workspaceMode: mode }),
  setCommandPaletteOpen: (open) => set({ commandPaletteOpen: open }),
  setDagSelectedTaskId: (id) => set({ dagSelectedTaskId: id }),
  setActivePreflight: (missionId, sessionId) =>
    set({ activePreflightMissionId: missionId, activePreflightSessionId: sessionId }),
  openMissionReport: (missionId) =>
    set({
      activeReportMissionId: missionId,
      activeView: missionId ? "report" : "missions",
    }),
  setApiKeyConfigured: (configured) => set({ apiKeyConfigured: configured }),
}));
