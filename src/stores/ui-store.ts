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

  setActiveView: (view: ViewId) => void;
  setTheme: (theme: Theme) => void;
  toggleSidebar: () => void;
  setWorkspaceMode: (mode: WorkspaceMode) => void;
  setCommandPaletteOpen: (open: boolean) => void;
  setDagSelectedTaskId: (id: string | null) => void;
  setActivePreflight: (missionId: string | null, sessionId: string | null) => void;
  /** FM-12: 切到 ReportView 同时设置查看哪个 mission；传 null 关闭报告 */
  openMissionReport: (missionId: string | null) => void;
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
}));
