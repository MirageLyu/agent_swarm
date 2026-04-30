import { useMemo, useState, useCallback, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import type { TaskInfo, DependencyInfo } from "../../ipc/commands";
import { useUiStore } from "../../stores/ui-store";
import { computeDagLayout, NODE_WIDTH, NODE_HEIGHT } from "./dag-layout";
import { TaskNode } from "./TaskNode";
import { TaskEdge } from "./TaskEdge";
import { ArtifactBadge } from "./ArtifactBadge";
import { parseArtifactRefs } from "./task-meta";
import { DagSummaryBar } from "./DagSummaryBar";
import { DAGViewport, type ViewportTransform } from "./DAGViewport";
import styles from "./TaskDAG.module.css";

interface TaskDAGProps {
  tasks: TaskInfo[];
  dependencies: DependencyInfo[];
  onEditTask: (task: TaskInfo) => void;
  onDeleteTask: (taskId: string) => void;
  onAddTask: () => void;
  focusNodeId?: string | null;
  onFocusHandled?: () => void;
}

const STATUS_MARKER_COLORS: Record<string, string> = {
  completed: "var(--color-success)",
  running: "var(--color-accent)",
  pending: "var(--color-text-tertiary)",
  ready: "var(--color-text-tertiary)",
  failed: "var(--color-error)",
  cancelled: "var(--color-text-tertiary)",
};

export function TaskDAG({
  tasks,
  dependencies,
  onEditTask,
  onDeleteTask,
  onAddTask,
  focusNodeId,
  onFocusHandled,
}: TaskDAGProps) {
  const { t } = useTranslation("mission");
  const dagSelectedTaskId = useUiStore((s) => s.dagSelectedTaskId);
  const setDagSelectedTaskId = useUiStore((s) => s.setDagSelectedTaskId);
  const [elevatedNodeId, setElevatedNodeId] = useState<string | null>(null);

  const handleElevate = useCallback((id: string | null) => {
    setElevatedNodeId(id);
  }, []);

  const layout = useMemo(
    () => computeDagLayout(tasks, dependencies),
    [tasks, dependencies],
  );

  const [transform, setTransform] = useState<ViewportTransform>({
    scale: 1,
    translateX: 0,
    translateY: 0,
  });

  const viewportRef = useRef<HTMLDivElement>(null);
  const [positionOverrides, setPositionOverrides] = useState<Record<string, { x: number; y: number }>>({});
  const animFrameRef = useRef(0);

  useEffect(() => {
    if (!focusNodeId) return;
    const node = layout.nodes.find((n) => n.id === focusNodeId);
    if (!node) return;
    const el = viewportRef.current;
    if (!el) return;

    const vw = el.clientWidth;
    const vh = el.clientHeight;
    const scale = transform.scale;
    const pos = positionOverrides[focusNodeId];
    const nx = (pos?.x ?? node.x) + NODE_WIDTH / 2;
    const ny = (pos?.y ?? node.y) + NODE_HEIGHT / 2;
    setTransform({
      scale,
      translateX: vw / (2 * scale) - nx,
      translateY: vh / (2 * scale) - ny,
    });
    onFocusHandled?.();
  }, [focusNodeId]); // eslint-disable-line react-hooks/exhaustive-deps

  const handleNodeDrag = useCallback((taskId: string, dx: number, dy: number) => {
    setPositionOverrides((prev) => {
      const current = prev[taskId];
      const node = layout.nodes.find((n) => n.id === taskId);
      const baseX = current?.x ?? node?.x ?? 0;
      const baseY = current?.y ?? node?.y ?? 0;
      return { ...prev, [taskId]: { x: baseX + dx, y: baseY + dy } };
    });
  }, [layout.nodes]);

  const handleAutoLayout = useCallback(() => {
    cancelAnimationFrame(animFrameRef.current);
    const nodeMap = new Map(layout.nodes.map((n) => [n.id, n]));
    const snapshot = { ...positionOverrides };
    const startTime = performance.now();
    const duration = 300;

    function tick() {
      const elapsed = performance.now() - startTime;
      const t = Math.min(elapsed / duration, 1);
      const ease = 1 - Math.pow(1 - t, 3);

      if (t >= 1) {
        setPositionOverrides({});
        return;
      }

      const interpolated: Record<string, { x: number; y: number }> = {};
      for (const [id, from] of Object.entries(snapshot)) {
        const target = nodeMap.get(id);
        if (!target) continue;
        interpolated[id] = {
          x: from.x + (target.x - from.x) * ease,
          y: from.y + (target.y - from.y) * ease,
        };
      }
      setPositionOverrides(interpolated);
      animFrameRef.current = requestAnimationFrame(tick);
    }

    animFrameRef.current = requestAnimationFrame(tick);
  }, [layout.nodes, positionOverrides]);

  const handleCanvasClick = useCallback(() => {
    setDagSelectedTaskId(null);
  }, [setDagSelectedTaskId]);

  // 所有 hooks 必须在 early return 之前调用，否则会触发 Rules of Hooks 违例。
  // FM-15 v2.2 (S4): dependencies key=`${task_id}->${depends_on}`，
  // 反向映射成 layout.edges 的 (from -> to)：layout.edges[i] 上 from 是 producer，
  // to 是 consumer，所以 edge.from === dep.depends_on，edge.to === dep.task_id。
  const artifactRefMap = useMemo(() => {
    const m = new Map<string, string[]>();
    for (const dep of dependencies) {
      const refs = parseArtifactRefs(dep);
      if (refs.length > 0) {
        m.set(`${dep.depends_on}->${dep.task_id}`, refs);
      }
    }
    return m;
  }, [dependencies]);
  const taskMap = useMemo(() => new Map(tasks.map((t) => [t.id, t])), [tasks]);
  const nodeMap = useMemo(
    () => new Map(layout.nodes.map((n) => [n.id, n])),
    [layout.nodes],
  );

  if (tasks.length === 0) {
    return (
      <div className={styles.empty}>
        <p className={styles.emptyText}>{t("dag.noTasks")}</p>
        <button className={styles.addBtn} onClick={onAddTask}>
          {t("dag.addTask")}
        </button>
      </div>
    );
  }

  const hasOverrides = Object.keys(positionOverrides).length > 0;

  return (
    <div className={styles.container}>
      <div className={styles.toolbar}>
        <button className={styles.addBtn} onClick={onAddTask}>
          {t("dag.addTask")}
        </button>
        {hasOverrides && (
          <button className={styles.addBtn} onClick={handleAutoLayout}>
            {t("dag.autoLayout")}
          </button>
        )}
      </div>
      <div ref={viewportRef} className={styles.viewport} onClick={handleCanvasClick}>
        <DAGViewport
          contentWidth={layout.width}
          contentHeight={layout.height}
          transform={transform}
          onTransformChange={setTransform}
        >
          <svg
            width={layout.width}
            height={layout.height}
            className={styles.svg}
            style={{ overflow: "visible" }}
          >
            <defs>
              <marker
                id="arrowhead"
                markerWidth="8"
                markerHeight="6"
                refX="8"
                refY="3"
                orient="auto"
              >
                <polygon
                  points="0 0, 8 3, 0 6"
                  fill="var(--color-border-strong)"
                />
              </marker>
              {Object.entries(STATUS_MARKER_COLORS).map(([status, color]) => (
                <marker
                  key={status}
                  id={`arrowhead-${status}`}
                  markerWidth="8"
                  markerHeight="6"
                  refX="8"
                  refY="3"
                  orient="auto"
                >
                  <polygon points="0 0, 8 3, 0 6" fill={color} />
                </marker>
              ))}
            </defs>
            {layout.edges.map((edge) => {
              const sourceTask = taskMap.get(edge.from);
              const fromOrig = nodeMap.get(edge.from);
              const toOrig = nodeMap.get(edge.to);
              const fromOver = positionOverrides[edge.from];
              const toOver = positionOverrides[edge.to];
              const dx1 = fromOver && fromOrig ? fromOver.x - fromOrig.x : 0;
              const dy1 = fromOver && fromOrig ? fromOver.y - fromOrig.y : 0;
              const dx2 = toOver && toOrig ? toOver.x - toOrig.x : 0;
              const dy2 = toOver && toOrig ? toOver.y - toOrig.y : 0;
              if (dx1 || dy1 || dx2 || dy2) {
                const ax1 = edge.x1 + dx1, ay1 = edge.y1 + dy1;
                const ax2 = edge.x2 + dx2, ay2 = edge.y2 + dy2;
                const cx = (ax1 + ax2) / 2;
                const adjustedEdge = {
                  ...edge,
                  x1: ax1, y1: ay1, x2: ax2, y2: ay2,
                  path: `M ${ax1} ${ay1} C ${cx} ${ay1}, ${cx} ${ay2}, ${ax2} ${ay2}`,
                };
                return (
                  <TaskEdge
                    key={`${edge.from}-${edge.to}`}
                    edge={adjustedEdge}
                    status={sourceTask?.status}
                  />
                );
              }
              return (
                <TaskEdge
                  key={`${edge.from}-${edge.to}`}
                  edge={edge}
                  status={sourceTask?.status}
                />
              );
            })}
            {/* FM-15 v2.2 (S4): edge 上的 ArtifactBadge——单独一层，便于 z-index 管理 */}
            {layout.edges.map((edge) => {
              const refs = artifactRefMap.get(`${edge.from}->${edge.to}`);
              if (!refs || refs.length === 0) return null;
              const fromOrig = nodeMap.get(edge.from);
              const toOrig = nodeMap.get(edge.to);
              const fromOver = positionOverrides[edge.from];
              const toOver = positionOverrides[edge.to];
              const dx1 = fromOver && fromOrig ? fromOver.x - fromOrig.x : 0;
              const dy1 = fromOver && fromOrig ? fromOver.y - fromOrig.y : 0;
              const dx2 = toOver && toOrig ? toOver.x - toOrig.x : 0;
              const dy2 = toOver && toOrig ? toOver.y - toOrig.y : 0;
              const x1 = edge.x1 + dx1;
              const y1 = edge.y1 + dy1;
              const x2 = edge.x2 + dx2;
              const y2 = edge.y2 + dy2;
              const mx = (x1 + x2) / 2;
              const my = (y1 + y2) / 2;
              return (
                <ArtifactBadge
                  key={`badge-${edge.from}-${edge.to}`}
                  artifactRefs={refs}
                  x={mx}
                  y={my}
                />
              );
            })}
            {/* 所有节点统一在一个 map 里渲染：把 elevated 节点排到数组末尾，
                让 SVG document order 把它绘制在最上层。
                关键：必须保持单一 children slot，否则 React 在多 JSX 分支之间
                移动节点会触发 unmount+remount，TaskNode 内部的 tooltipAnchor
                等 useState 会被重置为初始值，导致 hover tooltip 立即消失。 */}
            {(() => {
              const ordered = elevatedNodeId
                ? [
                    ...tasks.filter((t) => t.id !== elevatedNodeId),
                    ...tasks.filter((t) => t.id === elevatedNodeId),
                  ]
                : tasks;
              return ordered.map((task) => {
                const nl = nodeMap.get(task.id);
                if (!nl) return null;
                const override = positionOverrides[task.id];
                const effectiveLayout = override
                  ? { ...nl, x: override.x, y: override.y }
                  : nl;
                return (
                  <TaskNode
                    key={task.id}
                    task={task}
                    layout={effectiveLayout}
                    onEdit={onEditTask}
                    onDelete={onDeleteTask}
                    onSelect={setDagSelectedTaskId}
                    selected={dagSelectedTaskId === task.id}
                    onDrag={handleNodeDrag}
                    viewportScale={transform.scale}
                    onElevate={handleElevate}
                  />
                );
              });
            })()}
          </svg>
        </DAGViewport>
      </div>
      <DagSummaryBar tasks={tasks} />
    </div>
  );
}
