import { useState, useRef, useCallback } from "react";
import { useUiStore, type ViewId } from "../stores/ui-store";
import { SidebarAgentList } from "./SidebarAgentList";
import styles from "./Sidebar.module.css";

interface NavItem {
  id: ViewId;
  label: string;
  icon: React.ReactNode;
}

const navItems: NavItem[] = [
  {
    id: "missions",
    label: "Missions",
    icon: (
      <svg width="18" height="18" viewBox="0 0 18 18" fill="none">
        <rect x="2" y="2" width="6" height="6" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
        <rect x="10" y="2" width="6" height="6" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
        <rect x="2" y="10" width="6" height="6" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
        <rect
          x="10"
          y="10"
          width="6"
          height="6"
          rx="1.5"
          stroke="currentColor"
          strokeWidth="1.5"
        />
      </svg>
    ),
  },
  {
    id: "workspace",
    label: "Workspace",
    icon: (
      <svg width="18" height="18" viewBox="0 0 18 18" fill="none">
        <path
          d="M3 5.5h12M3 9h12M3 12.5h8"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
        />
      </svg>
    ),
  },
  {
    id: "agents",
    label: "Agents",
    icon: (
      <svg width="18" height="18" viewBox="0 0 18 18" fill="none">
        <circle cx="9" cy="6" r="3" stroke="currentColor" strokeWidth="1.5" />
        <path
          d="M3 15c0-3.3 2.7-6 6-6s6 2.7 6 6"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
        />
      </svg>
    ),
  },
  {
    id: "review",
    label: "Review",
    icon: (
      <svg width="18" height="18" viewBox="0 0 18 18" fill="none">
        <path
          d="M3 9.5L7 13.5L15 4.5"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    ),
  },
  {
    id: "insights",
    label: "Insights",
    icon: (
      <svg width="18" height="18" viewBox="0 0 18 18" fill="none">
        <path
          d="M3 14l4-5 3 3 5-7"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    ),
  },
  {
    id: "settings",
    label: "Settings",
    icon: (
      <svg width="18" height="18" viewBox="0 0 18 18" fill="none">
        <circle cx="9" cy="9" r="2.5" stroke="currentColor" strokeWidth="1.5" />
        <path
          d="M9 2v2M9 14v2M2 9h2M14 9h2M4.2 4.2l1.4 1.4M12.4 12.4l1.4 1.4M4.2 13.8l1.4-1.4M12.4 5.6l1.4-1.4"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
        />
      </svg>
    ),
  },
];

const EXPAND_DELAY = 0;
const COLLAPSE_DELAY = 0;

export function Sidebar() {
  const { activeView, setActiveView } = useUiStore();
  const [expanded, setExpanded] = useState(false);
  const [tooltip, setTooltip] = useState<{ label: string; top: number } | null>(null);
  const expandTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const collapseTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const handleMouseEnter = useCallback(() => {
    if (collapseTimer.current) clearTimeout(collapseTimer.current);
    expandTimer.current = setTimeout(() => {
      setExpanded(true);
      setTooltip(null);
    }, EXPAND_DELAY);
  }, []);

  const handleMouseLeave = useCallback(() => {
    if (expandTimer.current) clearTimeout(expandTimer.current);
    setTooltip(null);
    collapseTimer.current = setTimeout(() => setExpanded(false), COLLAPSE_DELAY);
  }, []);

  const handleNavEnter = useCallback(
    (e: React.MouseEvent, label: string) => {
      if (expanded) return;
      const rect = e.currentTarget.getBoundingClientRect();
      setTooltip({ label, top: rect.top + rect.height / 2 });
    },
    [expanded],
  );

  const handleNavLeave = useCallback(() => {
    setTooltip(null);
  }, []);

  return (
    <>
      <aside
        className={`${styles.sidebar} ${expanded ? styles.expanded : ""}`}
        onMouseEnter={handleMouseEnter}
        onMouseLeave={handleMouseLeave}
      >
        <div className={styles.trafficLightSpacer} data-tauri-drag-region />
        <nav className={styles.nav}>
          {navItems.map((item) => (
            <button
              key={item.id}
              className={`${styles.navItem} ${activeView === item.id ? styles.active : ""}`}
              onClick={() => setActiveView(item.id)}
              onMouseEnter={(e) => handleNavEnter(e, item.label)}
              onMouseLeave={handleNavLeave}
            >
              <span className={styles.icon}>{item.icon}</span>
              {expanded && <span className={styles.label}>{item.label}</span>}
            </button>
          ))}
        </nav>
        {expanded && <SidebarAgentList />}
      </aside>
      {!expanded && tooltip && (
        <div className={styles.tooltip} style={{ top: tooltip.top }}>
          {tooltip.label}
        </div>
      )}
    </>
  );
}
