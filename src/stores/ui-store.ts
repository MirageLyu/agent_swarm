import { create } from "zustand";

export type ViewId =
  | "missions"
  | "preflight"
  | "workspace"
  | "agents"
  | "review"
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

  setActiveView: (view: ViewId) => void;
  setTheme: (theme: Theme) => void;
  toggleSidebar: () => void;
  setWorkspaceMode: (mode: WorkspaceMode) => void;
  setCommandPaletteOpen: (open: boolean) => void;
  setDagSelectedTaskId: (id: string | null) => void;
  setActivePreflight: (missionId: string | null, sessionId: string | null) => void;
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

  setActiveView: (view) => set({ activeView: view }),
  setTheme: (theme) => set({ theme }),
  toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
  setWorkspaceMode: (mode) => set({ workspaceMode: mode }),
  setCommandPaletteOpen: (open) => set({ commandPaletteOpen: open }),
  setDagSelectedTaskId: (id) => set({ dagSelectedTaskId: id }),
  setActivePreflight: (missionId, sessionId) =>
    set({ activePreflightMissionId: missionId, activePreflightSessionId: sessionId }),
}));
