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
  const isReference = edge.kind === "reference";

  const d = edge.path
    ?? `M ${edge.x1} ${edge.y1} C ${(edge.x1 + edge.x2) / 2} ${edge.y1}, ${(edge.x1 + edge.x2) / 2} ${edge.y2}, ${edge.x2} ${edge.y2}`;

  // reference 边：弱化为半透明灰色 + 粗虚线 + 无箭头，
  // 让 producer 边在视觉上明显主导（解决"全连接图"的噪音问题）。
  if (isReference) {
    return (
      <g>
        <path
          d={d}
          fill="none"
          stroke="var(--color-text-tertiary)"
          strokeWidth={1}
          strokeDasharray="2 5"
          strokeOpacity={0.45}
        />
      </g>
    );
  }

  const markerId = status ? `arrowhead-${status}` : "arrowhead";
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
