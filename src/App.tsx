import { Sidebar } from "./components/Sidebar";
import { Titlebar } from "./components/Titlebar";
import { useUiStore } from "./stores/ui-store";
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
  return (
    <div className={styles.shell}>
      <Sidebar />
      <div className={styles.main}>
        <Titlebar />
        <div className={styles.content}>
          <ActiveView />
        </div>
      </div>
    </div>
  );
}
