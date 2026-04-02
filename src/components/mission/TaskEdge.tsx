import type { EdgeLayout } from "./dag-layout";

interface TaskEdgeProps {
  edge: EdgeLayout;
}

export function TaskEdge({ edge }: TaskEdgeProps) {
  const midX = (edge.x1 + edge.x2) / 2;

  return (
    <g>
      <path
        d={`M ${edge.x1} ${edge.y1} C ${midX} ${edge.y1}, ${midX} ${edge.y2}, ${edge.x2} ${edge.y2}`}
        fill="none"
        stroke="var(--color-border-strong)"
        strokeWidth={1.5}
        markerEnd="url(#arrowhead)"
      />
    </g>
  );
}
