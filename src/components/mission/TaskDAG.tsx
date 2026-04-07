import { useMemo, useState } from "react";
import type { TaskInfo, DependencyInfo } from "../../ipc/commands";
import { computeDagLayout } from "./dag-layout";
import { TaskNode } from "./TaskNode";
import { TaskEdge } from "./TaskEdge";
import { DAGViewport, type ViewportTransform } from "./DAGViewport";
import styles from "./TaskDAG.module.css";

interface TaskDAGProps {
  tasks: TaskInfo[];
  dependencies: DependencyInfo[];
  onEditTask: (task: TaskInfo) => void;
  onDeleteTask: (taskId: string) => void;
  onAddTask: () => void;
}

export function TaskDAG({
  tasks,
  dependencies,
  onEditTask,
  onDeleteTask,
  onAddTask,
}: TaskDAGProps) {
  const layout = useMemo(
    () => computeDagLayout(tasks, dependencies),
    [tasks, dependencies],
  );

  const [transform, setTransform] = useState<ViewportTransform>({
    scale: 1,
    translateX: 0,
    translateY: 0,
  });

  if (tasks.length === 0) {
    return (
      <div className={styles.empty}>
        <p className={styles.emptyText}>No tasks yet</p>
        <button className={styles.addBtn} onClick={onAddTask}>
          + Add Task
        </button>
      </div>
    );
  }

  const nodeMap = new Map(layout.nodes.map((n) => [n.id, n]));

  return (
    <div className={styles.container}>
      <div className={styles.toolbar}>
        <button className={styles.addBtn} onClick={onAddTask}>
          + Add Task
        </button>
      </div>
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
          </defs>
          {layout.edges.map((edge) => (
            <TaskEdge key={`${edge.from}-${edge.to}`} edge={edge} />
          ))}
          {tasks.map((task) => {
            const nl = nodeMap.get(task.id);
            if (!nl) return null;
            return (
              <TaskNode
                key={task.id}
                task={task}
                layout={nl}
                onEdit={onEditTask}
                onDelete={onDeleteTask}
              />
            );
          })}
        </svg>
      </DAGViewport>
    </div>
  );
}
