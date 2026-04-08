import { useState, useRef, useEffect, useCallback } from "react";
import type { TaskInfo } from "../../ipc/commands";
import type { NodeLayout } from "./dag-layout";
import { NODE_WIDTH, NODE_HEIGHT } from "./dag-layout";
import styles from "./TaskNode.module.css";

interface TaskNodeProps {
  task: TaskInfo;
  layout: NodeLayout;
  onEdit: (task: TaskInfo) => void;
  onDelete: (taskId: string) => void;
  onSelect?: (taskId: string) => void;
  onAddDependency?: (taskId: string) => void;
  selected?: boolean;
  onDrag?: (taskId: string, dx: number, dy: number) => void;
  viewportScale?: number;
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

export function TaskNode({ task, layout, onEdit, onDelete, onSelect, selected, onDrag, viewportScale }: TaskNodeProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [tooltip, setTooltip] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const dragStartRef = useRef<{ x: number; y: number } | null>(null);
  const didDragRef = useRef(false);

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

  const [dragging, setDragging] = useState(false);
  const nodeRef = useRef<HTMLDivElement>(null);

  const handlePointerDown = useCallback(
    (e: React.PointerEvent) => {
      if ((e.target as HTMLElement).closest(`.${styles.menuTrigger}`)) return;
      e.stopPropagation();
      dragStartRef.current = { x: e.clientX, y: e.clientY };
      didDragRef.current = false;
    },
    [],
  );

  const handlePointerMove = useCallback(
    (e: React.PointerEvent) => {
      if (!dragStartRef.current) return;
      const dx = e.clientX - dragStartRef.current.x;
      const dy = e.clientY - dragStartRef.current.y;
      if (!didDragRef.current && Math.abs(dx) + Math.abs(dy) > 3) {
        didDragRef.current = true;
        setDragging(true);
        nodeRef.current?.setPointerCapture(e.pointerId);
      }
      if (didDragRef.current && onDrag) {
        const scale = viewportScale ?? 1;
        const sdx = (e.clientX - dragStartRef.current.x) / scale;
        const sdy = (e.clientY - dragStartRef.current.y) / scale;
        dragStartRef.current = { x: e.clientX, y: e.clientY };
        onDrag(task.id, sdx, sdy);
      }
    },
    [onDrag, task.id, viewportScale],
  );

  const handlePointerUp = useCallback(
    (_e: React.PointerEvent) => {
      const wasDrag = didDragRef.current;
      dragStartRef.current = null;
      didDragRef.current = false;
      setDragging(false);
      if (!wasDrag) {
        onSelect?.(task.id);
      }
    },
    [onSelect, task.id],
  );

  const handleClick = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
  }, []);

  return (
    <foreignObject
      x={layout.x}
      y={layout.y}
      width={NODE_WIDTH}
      height={NODE_HEIGHT}
      overflow="visible"
      data-dag-node
    >
      <div
        ref={nodeRef}
        className={`${styles.node} ${selected ? styles.selected : ""}`}
        data-status={task.status}
        style={{ cursor: dragging ? "grabbing" : "default" }}
        onMouseEnter={() => setTooltip(true)}
        onMouseLeave={() => { setTooltip(false); }}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onClick={handleClick}
      >
        <div className={styles.header}>
          <span className={styles.status}>{STATUS_ICONS[task.status] ?? "\u25CB"}</span>
          <span className={styles.title}>{task.title}</span>
          <button
            className={styles.menuTrigger}
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setMenuOpen((v) => !v);
              setTooltip(false);
            }}
          >
            ⋯
          </button>
        </div>
        <div className={styles.meta}>
          <span
            className={styles.complexity}
            style={{ color: COMPLEXITY_COLORS[task.complexity] }}
          >
            {task.complexity}
          </span>
          {task.assigned_agent_id && (
            <span className={styles.agentTag}>
              {task.assigned_agent_id.substring(0, 6)}
            </span>
          )}
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
                setTooltip(false);
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
                setTooltip(false);
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
