import { useUiStore } from "../stores/ui-store";
import styles from "./Titlebar.module.css";

export function Titlebar() {
  const activeView = useUiStore((s) => s.activeView);

  const viewTitles: Record<string, string> = {
    missions: "Mission Board",
    workspace: "Workspace",
    agents: "Agents",
    insights: "Insights",
    settings: "Settings",
  };

  return (
    <div className={styles.titlebar} data-tauri-drag-region>
      <div className={styles.trafficLightSpacer} />
      <div className={styles.title}>{viewTitles[activeView] ?? ""}</div>
      <div className={styles.actions}>
        <button className={styles.actionBtn} title="Command Palette (⌘K)">
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
            <path
              d="M6.5 11.5a5 5 0 100-10 5 5 0 000 10zM14 14l-3.5-3.5"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
            />
          </svg>
        </button>
      </div>
    </div>
  );
}
