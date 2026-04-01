import { Sidebar } from "./components/Sidebar";
import { Titlebar } from "./components/Titlebar";
import { useUiStore } from "./stores/ui-store";
import { useTheme } from "./hooks/useTheme";
import { MissionsView } from "./views/MissionsView";
import { WorkspaceView } from "./views/WorkspaceView";
import { AgentsView } from "./views/AgentsView";
import { InsightsView } from "./views/InsightsView";
import { SettingsView } from "./views/SettingsView";
import styles from "./App.module.css";

function ActiveView() {
  const view = useUiStore((s) => s.activeView);
  switch (view) {
    case "missions":
      return <MissionsView />;
    case "workspace":
      return <WorkspaceView />;
    case "agents":
      return <AgentsView />;
    case "insights":
      return <InsightsView />;
    case "settings":
      return <SettingsView />;
  }
}

export default function App() {
  useTheme();

  return (
    <div className={styles.shell} data-component="Shell">
      <Sidebar />
      <div className={styles.main} data-component="Main">
        <Titlebar />
        <div className={styles.content} data-component="Content">
          <ActiveView />
        </div>
      </div>
    </div>
  );
}
