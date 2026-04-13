import type { EdgeLayout } from "./dag-layout";
import type { TaskStatus } from "../../ipc/commands";

interface TaskEdgeProps {
  edge: EdgeLayout;
  status?: TaskStatus;
}

const STATUS_COLORS: Record<string, { stroke: string; dash?: string }> = {
  completed: { stroke: "var(--color-success)" },
  running: { stroke: "var(--color-accent)", dash: "6 3" },
  pending: { stroke: "var(--color-text-tertiary)", dash: "4 4" },
  ready: { stroke: "var(--color-text-tertiary)", dash: "4 4" },
  failed: { stroke: "var(--color-error)" },
  cancelled: { stroke: "var(--color-text-tertiary)", dash: "2 4" },
};

export function TaskEdge({ edge, status }: TaskEdgeProps) {
  const cfg = STATUS_COLORS[status ?? "pending"] ?? STATUS_COLORS.pending;
  const markerId = status ? `arrowhead-${status}` : "arrowhead";

  const d = edge.path
    ?? `M ${edge.x1} ${edge.y1} C ${(edge.x1 + edge.x2) / 2} ${edge.y1}, ${(edge.x1 + edge.x2) / 2} ${edge.y2}, ${edge.x2} ${edge.y2}`;

  return (
    <g>
      <path
        d={d}
        fill="none"
        stroke={cfg.stroke}
        strokeWidth={1.5}
        strokeDasharray={cfg.dash}
        markerEnd={`url(#${markerId})`}
      />
      {status === "running" && (
        <animate
          attributeName="stroke-dashoffset"
          from="18"
          to="0"
          dur="1s"
          repeatCount="indefinite"
        />
      )}
    </g>
  );
}
