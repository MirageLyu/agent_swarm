import { useMemo, useState, useRef, useEffect, useCallback } from "react";
import type { TaskInfo } from "../../ipc/commands";
import type { NodeLayout } from "./dag-layout";
import { NODE_WIDTH, NODE_HEIGHT } from "./dag-layout";
import { RoleBadge } from "./RoleBadge";
import { TaskNodeTooltip } from "./TaskNodeTooltip";
import {
  parseAdditionalSkills,
  parseConsumedArtifacts,
  parseProducedArtifacts,
} from "./task-meta";
import styles from "./TaskNode.module.css";

// mousemove hit-test 已经持续判定鼠标是否在 hover 范围内，grace 仅用于兜底
// （例如鼠标停在窗外不再 fire mousemove），所以可以短一点。
const TOOLTIP_CLOSE_GRACE_MS = 120;

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
  onElevate?: (taskId: string | null) => void;
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

export function TaskNode({ task, layout, onEdit, onDelete, onSelect, selected, onDrag, viewportScale, onElevate }: TaskNodeProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [dragging, setDragging] = useState(false);
  // tooltip anchor=null 表示关闭；非 null 时持有触发瞬间的 viewport DOMRect，
  // 配合 grace timer + portal 渲染让 tooltip 不被 SVG/容器 overflow 裁剪，
  // 且鼠标可移入 tooltip 滚动查看长内容。
  const [tooltipAnchor, setTooltipAnchor] = useState<DOMRect | null>(null);
  const tooltipCloseTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const nodeRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const dragStartRef = useRef<{ x: number; y: number } | null>(null);
  const didDragRef = useRef(false);

  // FM-15 v2.2 (S4): rich semantics——只在需要时 parse，避免每次 render 反复 JSON.parse。
  const skills = useMemo(() => parseAdditionalSkills(task), [task]);
  const produced = useMemo(() => parseProducedArtifacts(task), [task]);
  const consumed = useMemo(() => parseConsumedArtifacts(task), [task]);

  const cancelTooltipClose = useCallback(() => {
    if (tooltipCloseTimerRef.current) {
      clearTimeout(tooltipCloseTimerRef.current);
      tooltipCloseTimerRef.current = null;
    }
  }, []);

  const openTooltip = useCallback(() => {
    cancelTooltipClose();
    if (!nodeRef.current) return;
    setTooltipAnchor(nodeRef.current.getBoundingClientRect());
  }, [cancelTooltipClose]);

  const scheduleTooltipClose = useCallback(() => {
    cancelTooltipClose();
    tooltipCloseTimerRef.current = setTimeout(() => {
      setTooltipAnchor(null);
      tooltipCloseTimerRef.current = null;
    }, TOOLTIP_CLOSE_GRACE_MS);
  }, [cancelTooltipClose]);

  // 卸载时确保 timer 不泄漏。
  useEffect(() => {
    return () => {
      if (tooltipCloseTimerRef.current) clearTimeout(tooltipCloseTimerRef.current);
    };
  }, []);

  // hit-test 在 TaskNodeTooltip 内部进行（它对自己的 ref 100% 可靠），
  // 通过此回调实时上报鼠标是否仍在 hover 范围内（node ∪ tooltip 矩形）。
  // 不再依赖父级拿 portal 子节点 ref（forwardRef + useImperativeHandle 在
  // portal 跨边界场景下时机不可靠）。
  const handleHoverChange = useCallback(
    (inside: boolean) => {
      if (inside) {
        cancelTooltipClose();
      } else if (!tooltipCloseTimerRef.current) {
        scheduleTooltipClose();
      }
    },
    [cancelTooltipClose, scheduleTooltipClose],
  );

  useEffect(() => {
    if (!menuOpen) return;
    function handleClick(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
        onElevate?.(null);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [menuOpen, onElevate]);

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
        onMouseEnter={() => {
          if (menuOpen) return;
          openTooltip();
          onElevate?.(task.id);
        }}
        onMouseLeave={() => {
          // schedule close 交给全局 mousemove hit-test 处理；这里只重置 elevate。
          if (!menuOpen) onElevate?.(null);
        }}
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
              setMenuOpen((v) => {
                const next = !v;
                if (next) onElevate?.(task.id);
                else onElevate?.(null);
                return next;
              });
              cancelTooltipClose();
              setTooltipAnchor(null);
            }}
          >
            ⋯
          </button>
        </div>
        <div className={styles.meta}>
          <RoleBadge role={task.role} compact />
          <span
            className={styles.complexity}
            style={{ color: COMPLEXITY_COLORS[task.complexity] }}
          >
            {task.complexity}
          </span>
          {produced.length > 0 && (
            <span
              className={styles.artifactPill}
              title={produced
                .map((a) => `${a.local_name} · ${a.artifact_type}`)
                .join("\n")}
            >
              {"\u2728"} {produced.length}
            </span>
          )}
          {task.assigned_agent_id && (
            <span className={styles.agentTag}>
              {task.assigned_agent_id.substring(0, 6)}
            </span>
          )}
        </div>

        {menuOpen && (
          <div className={styles.menu} ref={menuRef}>
            <button
              className={styles.menuItem}
              onClick={(e) => {
                e.stopPropagation();
                setMenuOpen(false);
                cancelTooltipClose();
                setTooltipAnchor(null);
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
                cancelTooltipClose();
                setTooltipAnchor(null);
                onDelete(task.id);
              }}
            >
              Delete
            </button>
          </div>
        )}
      </div>

      {tooltipAnchor && !menuOpen && (
        <TaskNodeTooltip anchor={tooltipAnchor} onHoverChange={handleHoverChange}>
          <div className={styles.tooltipHeader}>
            <RoleBadge role={task.role} />
            <p className={styles.tooltipTitle}>{task.title}</p>
          </div>
          <p className={styles.tooltipDesc}>{task.description || "No description"}</p>
          {task.expected_output && (
            <p className={styles.tooltipExpected}>
              <span className={styles.tooltipLabel}>Expected:</span>{" "}
              {task.expected_output}
            </p>
          )}
          {(produced.length > 0 || consumed.length > 0 || skills.length > 0) && (
            <div className={styles.tooltipChips}>
              {produced.length > 0 && (
                <div className={styles.tooltipChipRow}>
                  <span className={styles.tooltipLabel}>Produces:</span>
                  {produced.map((a) => (
                    <span key={a.local_name} className={styles.chip}>
                      {a.local_name}
                      <span className={styles.chipMuted}>·{a.artifact_type}</span>
                    </span>
                  ))}
                </div>
              )}
              {consumed.length > 0 && (
                <div className={styles.tooltipChipRow}>
                  <span className={styles.tooltipLabel}>Consumes:</span>
                  {consumed.map((id) => (
                    <span key={id} className={styles.chip}>
                      {id.includes(".") ? id.slice(id.indexOf(".") + 1) : id}
                    </span>
                  ))}
                </div>
              )}
              {skills.length > 0 && (
                <div className={styles.tooltipChipRow}>
                  <span className={styles.tooltipLabel}>Skills:</span>
                  {skills.map((s) => (
                    <span key={s} className={styles.chip}>
                      {s}
                    </span>
                  ))}
                </div>
              )}
            </div>
          )}
          <p className={styles.tooltipMeta}>Status: {task.status}</p>
        </TaskNodeTooltip>
      )}
    </foreignObject>
  );
}
