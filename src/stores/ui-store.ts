import { create } from "zustand";

export type ViewId =
  | "missions"
  | "workspace"
  | "agents"
  | "review"
  | "insights"
  | "settings";

export type Theme = "light" | "dark" | "system";

interface UiState {
  activeView: ViewId;
  theme: Theme;
  sidebarCollapsed: boolean;

  setActiveView: (view: ViewId) => void;
  setTheme: (theme: Theme) => void;
  toggleSidebar: () => void;
}

export const useUiStore = create<UiState>((set) => ({
  activeView: "missions",
  theme: "system",
  sidebarCollapsed: false,

  setActiveView: (view) => set({ activeView: view }),
  setTheme: (theme) => set({ theme }),
  toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
}));
