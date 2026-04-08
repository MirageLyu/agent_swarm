import { useEffect } from "react";
import { Sidebar } from "./components/Sidebar";
import { Titlebar } from "./components/Titlebar";
import { CommandPalette } from "./components/CommandPalette";
import { useUiStore } from "./stores/ui-store";
import { useTheme } from "./hooks/useTheme";
import { MissionsView } from "./views/MissionsView";
import { PreflightView } from "./views/PreflightView";
import { WorkspaceView } from "./views/WorkspaceView";
import { AgentsView } from "./views/AgentsView";
import { ReviewView } from "./views/ReviewView";
import { InsightsView } from "./views/InsightsView";
import { SettingsView } from "./views/SettingsView";
import styles from "./App.module.css";

function ActiveView() {
  const view = useUiStore((s) => s.activeView);
  switch (view) {
    case "missions":
      return <MissionsView />;
    case "preflight":
      return <PreflightView />;
    case "workspace":
      return <WorkspaceView />;
    case "agents":
      return <AgentsView />;
    case "review":
      return <ReviewView />;
    case "insights":
      return <InsightsView />;
    case "settings":
      return <SettingsView />;
  }
}

export default function App() {
  useTheme();

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        const store = useUiStore.getState();
        store.setCommandPaletteOpen(!store.commandPaletteOpen);
      }
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, []);

  return (
    <div className={styles.shell} data-component="Shell">
      <div className={styles.sidebarSlot}>
        <Sidebar />
      </div>
      <div className={styles.main} data-component="Main">
        <Titlebar />
        <div className={styles.content} data-component="Content">
          <ActiveView />
        </div>
      </div>
      <CommandPalette />
    </div>
  );
}
