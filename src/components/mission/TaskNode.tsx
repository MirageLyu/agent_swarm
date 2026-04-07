import { useState, useRef, useEffect } from "react";
import type { TaskInfo } from "../../ipc/commands";
import type { NodeLayout } from "./dag-layout";
import { NODE_WIDTH, NODE_HEIGHT } from "./dag-layout";
import styles from "./TaskNode.module.css";

interface TaskNodeProps {
  task: TaskInfo;
  layout: NodeLayout;
  onEdit: (task: TaskInfo) => void;
  onDelete: (taskId: string) => void;
  onAddDependency?: (taskId: string) => void;
}

const COMPLEXITY_COLORS: Record<string, string> = {
  low: "var(--color-success)",
  medium: "var(--color-warning)",
  high: "var(--color-error)",
};

const STATUS_ICONS: Record<string, string> = {
  pending: "\u25CB",
  ready: "\u25CE",
  running: "\u25D4",
  completed: "\u25CF",
  failed: "\u2716",
  cancelled: "\u2014",
};

export function TaskNode({ task, layout, onEdit, onDelete }: TaskNodeProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [tooltip, setTooltip] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menuOpen) return;
    function handleClick(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [menuOpen]);

  return (
    <foreignObject
      x={layout.x}
      y={layout.y}
      width={NODE_WIDTH}
      height={NODE_HEIGHT}
      overflow="visible"
    >
      <div
        className={styles.node}
        data-status={task.status}
        onClick={() => setMenuOpen((v) => !v)}
        onMouseEnter={() => setTooltip(true)}
        onMouseLeave={() => setTooltip(false)}
      >
        <div className={styles.header}>
          <span className={styles.status}>{STATUS_ICONS[task.status] ?? "\u25CB"}</span>
          <span className={styles.title}>{task.title}</span>
        </div>
        <div className={styles.meta}>
          <span
            className={styles.complexity}
            style={{ color: COMPLEXITY_COLORS[task.complexity] }}
          >
            {task.complexity}
          </span>
        </div>

        {tooltip && !menuOpen && (
          <div className={styles.tooltip}>
            <p className={styles.tooltipTitle}>{task.title}</p>
            <p className={styles.tooltipDesc}>{task.description || "No description"}</p>
            <p className={styles.tooltipMeta}>Status: {task.status}</p>
          </div>
        )}

        {menuOpen && (
          <div className={styles.menu} ref={menuRef}>
            <button
              className={styles.menuItem}
              onClick={(e) => {
                e.stopPropagation();
                setMenuOpen(false);
                onEdit(task);
              }}
            >
              Edit
            </button>
            <button
              className={`${styles.menuItem} ${styles.menuDanger}`}
              onClick={(e) => {
                e.stopPropagation();
                setMenuOpen(false);
                onDelete(task.id);
              }}
            >
              Delete
            </button>
          </div>
        )}
      </div>
    </foreignObject>
  );
}
