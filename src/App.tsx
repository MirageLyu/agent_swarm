import { useEffect } from "react";
import { Sidebar } from "./components/Sidebar";
import { Titlebar } from "./components/Titlebar";
import { CommandPalette } from "./components/CommandPalette";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { useUiStore } from "./stores/ui-store";
import { useTheme } from "./hooks/useTheme";
import { usePlannerEventSync } from "./hooks/usePlannerEventSync";
import { MissionsView } from "./views/MissionsView";
import { PreflightView } from "./views/PreflightView";
import { WorkspaceView } from "./views/WorkspaceView";
import { AgentsView } from "./views/AgentsView";
import { ReportView } from "./views/ReportView";
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
    case "report":
      return <ReportView />;
    case "insights":
      return <InsightsView />;
    case "settings":
      return <SettingsView />;
    default:
      // 兜底：activeView 一旦出现非法值不至于回退到 undefined 让根组件返回空。
      return <MissionsView />;
  }
}

export default function App() {
  useTheme();
  // 全局订阅 planner 事件（必须在 App 根，不能在 view 内）。
  usePlannerEventSync();

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

  // 切 view 时 boundary 自动 reset，避免上一个 view 的错误把新 view 也卡死。
  const activeView = useUiStore((s) => s.activeView);

  return (
    <div className={styles.shell} data-component="Shell">
      <div className={styles.sidebarSlot}>
        <ErrorBoundary scope="Sidebar">
          <Sidebar />
        </ErrorBoundary>
      </div>
      <div className={styles.main} data-component="Main">
        <ErrorBoundary scope="Titlebar">
          <Titlebar />
        </ErrorBoundary>
        <div className={styles.content} data-component="Content">
          <ErrorBoundary key={activeView} scope={`view:${activeView}`}>
            <ActiveView />
          </ErrorBoundary>
        </div>
      </div>
      <ErrorBoundary scope="CommandPalette">
        <CommandPalette />
      </ErrorBoundary>
    </div>
  );
}
