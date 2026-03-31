import { useUiStore, type Theme } from "../stores/ui-store";
import styles from "./Titlebar.module.css";

const themeIcons: Record<Theme, React.ReactNode> = {
  light: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <circle cx="8" cy="8" r="3" stroke="currentColor" strokeWidth="1.5" />
      <path
        d="M8 1.5v1.5M8 13v1.5M1.5 8H3M13 8h1.5M3.3 3.3l1 1M11.7 11.7l1 1M3.3 12.7l1-1M11.7 4.3l1-1"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  ),
  dark: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <path
        d="M13.5 9.5a5.5 5.5 0 01-7-7A5.5 5.5 0 1013.5 9.5z"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  ),
  system: (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
      <rect
        x="2"
        y="3"
        width="12"
        height="9"
        rx="1.5"
        stroke="currentColor"
        strokeWidth="1.5"
      />
      <path d="M6 14h4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
    </svg>
  ),
};

export function Titlebar() {
  const activeView = useUiStore((s) => s.activeView);
  const theme = useUiStore((s) => s.theme);
  const setTheme = useUiStore((s) => s.setTheme);

  const viewTitles: Record<string, string> = {
    missions: "Mission Board",
    workspace: "Workspace",
    agents: "Agents",
    insights: "Insights",
    settings: "Settings",
  };

  const cycleTheme = () => {
    const order: Theme[] = ["system", "light", "dark"];
    const next = order[(order.indexOf(theme) + 1) % order.length];
    setTheme(next);
  };

  return (
    <div className={styles.titlebar} data-tauri-drag-region>
      <div className={styles.trafficLightSpacer} />
      <div className={styles.title}>{viewTitles[activeView] ?? ""}</div>
      <div className={styles.actions}>
        <button className={styles.actionBtn} onClick={cycleTheme} title={`Theme: ${theme}`}>
          {themeIcons[theme]}
        </button>
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
