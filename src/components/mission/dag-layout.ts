import type { TaskInfo, DependencyInfo, DependencyKind } from "../../ipc/commands";

export interface NodeLayout {
  id: string;
  x: number;
  y: number;
  layer: number;
}

export interface EdgeLayout {
  from: string;
  to: string;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  /** SVG path "d" attribute for the edge curve */
  path: string;
  /** FM-15 v2.3：边语义分类（producer / reference）。旧 mission 默认 producer。 */
  kind: DependencyKind;
}

export interface DagLayout {
  nodes: NodeLayout[];
  edges: EdgeLayout[];
  width: number;
  height: number;
}

export const NODE_WIDTH = 200;
export const NODE_HEIGHT = 72;
const LAYER_GAP = 120;
const NODE_GAP = 32;
const PADDING = 48;
const PORT_MARGIN = 12;
const BARYCENTRIC_ITERATIONS = 6;

export function computeDagLayout(
  tasks: TaskInfo[],
  dependencies: DependencyInfo[],
): DagLayout {
  if (tasks.length === 0) {
    return { nodes: [], edges: [], width: 0, height: 0 };
  }

  // --- Deduplicate dependencies ---
  const depSet = new Set<string>();
  const uniqueDeps: DependencyInfo[] = [];
  for (const dep of dependencies) {
    const key = `${dep.task_id}::${dep.depends_on}`;
    if (!depSet.has(key)) {
      depSet.add(key);
      uniqueDeps.push(dep);
    }
  }

  // depMap: task_id → set of upstream ids (task depends on these)
  const depMap = new Map<string, Set<string>>();
  // successors: source_id → set of downstream ids (these depend on source)
  const successors = new Map<string, Set<string>>();
  for (const dep of uniqueDeps) {
    if (!depMap.has(dep.task_id)) depMap.set(dep.task_id, new Set());
    depMap.get(dep.task_id)!.add(dep.depends_on);
    if (!successors.has(dep.depends_on)) successors.set(dep.depends_on, new Set());
    successors.get(dep.depends_on)!.add(dep.task_id);
  }

  // --- Layer assignment (longest-path) ---
  const layerOf = new Map<string, number>();

  function getLayer(id: string, visited: Set<string>): number {
    if (layerOf.has(id)) return layerOf.get(id)!;
    if (visited.has(id)) return 0;
    visited.add(id);

    const deps = depMap.get(id);
    if (!deps || deps.size === 0) {
      layerOf.set(id, 0);
      return 0;
    }

    let maxDep = 0;
    for (const depId of deps) {
      maxDep = Math.max(maxDep, getLayer(depId, visited) + 1);
    }
    layerOf.set(id, maxDep);
    return maxDep;
  }

  for (const task of tasks) {
    getLayer(task.id, new Set());
  }

  const layers = new Map<number, string[]>();
  for (const task of tasks) {
    const l = layerOf.get(task.id) ?? 0;
    if (!layers.has(l)) layers.set(l, []);
    layers.get(l)!.push(task.id);
  }

  const numLayers = Math.max(...layers.keys()) + 1;

  // --- Barycentric ordering (Sugiyama-style crossing reduction) ---
  // Initial order: by original task array index
  const taskIndex = new Map(tasks.map((t, i) => [t.id, i]));
  for (const [, ids] of layers) {
    ids.sort((a, b) => (taskIndex.get(a) ?? 0) - (taskIndex.get(b) ?? 0));
  }

  // positionInLayer: nodeId → index within its layer (updated each iteration)
  const positionInLayer = new Map<string, number>();
  function refreshPositions() {
    for (const [, ids] of layers) {
      for (let i = 0; i < ids.length; i++) {
        positionInLayer.set(ids[i], i);
      }
    }
  }
  refreshPositions();

  function neighbors(id: string, direction: "up" | "down"): string[] {
    if (direction === "up") {
      return [...(depMap.get(id) ?? [])];
    }
    return [...(successors.get(id) ?? [])];
  }

  function barycenter(id: string, direction: "up" | "down"): number | null {
    const nbrs = neighbors(id, direction);
    if (nbrs.length === 0) return null;
    let sum = 0;
    for (const n of nbrs) {
      sum += positionInLayer.get(n) ?? 0;
    }
    return sum / nbrs.length;
  }

  for (let iter = 0; iter < BARYCENTRIC_ITERATIONS; iter++) {
    const direction: "up" | "down" = iter % 2 === 0 ? "down" : "up";
    const layerOrder = direction === "down"
      ? Array.from({ length: numLayers }, (_, i) => i)
      : Array.from({ length: numLayers }, (_, i) => numLayers - 1 - i);

    for (const l of layerOrder) {
      const ids = layers.get(l);
      if (!ids || ids.length <= 1) continue;

      const baryValues = new Map<string, number>();
      for (const id of ids) {
        const bc = barycenter(id, direction === "down" ? "up" : "down");
        baryValues.set(id, bc ?? positionInLayer.get(id)!);
      }

      ids.sort((a, b) => baryValues.get(a)! - baryValues.get(b)!);
    }
    refreshPositions();
  }

  // --- Coordinate assignment ---
  const nodeMap = new Map<string, NodeLayout>();
  let maxNodesInLayer = 0;
  for (const [, ids] of layers) {
    maxNodesInLayer = Math.max(maxNodesInLayer, ids.length);
  }

  for (let l = 0; l < numLayers; l++) {
    const ids = layers.get(l) ?? [];
    const layerHeight = ids.length * (NODE_HEIGHT + NODE_GAP) - NODE_GAP;
    const startY =
      PADDING +
      (maxNodesInLayer * (NODE_HEIGHT + NODE_GAP) - NODE_GAP - layerHeight) / 2;

    for (let i = 0; i < ids.length; i++) {
      const x = PADDING + l * (NODE_WIDTH + LAYER_GAP);
      const y = startY + i * (NODE_HEIGHT + NODE_GAP);
      nodeMap.set(ids[i], { id: ids[i], x, y, layer: l });
    }
  }

  // --- Port assignment ---
  const outgoing = new Map<string, string[]>();
  const incoming = new Map<string, string[]>();

  for (const dep of uniqueDeps) {
    if (!nodeMap.has(dep.depends_on) || !nodeMap.has(dep.task_id)) continue;
    if (!outgoing.has(dep.depends_on)) outgoing.set(dep.depends_on, []);
    outgoing.get(dep.depends_on)!.push(dep.task_id);
    if (!incoming.has(dep.task_id)) incoming.set(dep.task_id, []);
    incoming.get(dep.task_id)!.push(dep.depends_on);
  }

  for (const [, targets] of outgoing) {
    targets.sort(
      (a, b) => (nodeMap.get(a)?.y ?? 0) - (nodeMap.get(b)?.y ?? 0),
    );
  }
  for (const [, sources] of incoming) {
    sources.sort(
      (a, b) => (nodeMap.get(a)?.y ?? 0) - (nodeMap.get(b)?.y ?? 0),
    );
  }

  function portY(
    nodeId: string,
    connectedId: string,
    portList: Map<string, string[]>,
  ): number {
    const node = nodeMap.get(nodeId)!;
    const list = portList.get(nodeId) ?? [connectedId];
    const count = list.length;
    const idx = list.indexOf(connectedId);
    if (count <= 1) return node.y + NODE_HEIGHT / 2;

    const usable = NODE_HEIGHT - PORT_MARGIN * 2;
    const step = usable / (count - 1);
    return node.y + PORT_MARGIN + idx * step;
  }

  // --- Edge routing ---
  // Collect occupied Y ranges per layer for avoidance
  const layerNodeRanges = new Map<number, Array<{ top: number; bottom: number }>>();
  for (let l = 0; l < numLayers; l++) {
    const ids = layers.get(l) ?? [];
    const ranges = ids.map((id) => {
      const n = nodeMap.get(id)!;
      return { top: n.y - 4, bottom: n.y + NODE_HEIGHT + 4 };
    });
    layerNodeRanges.set(l, ranges);
  }

  function buildEdgePath(
    x1: number, y1: number, x2: number, y2: number,
    fromLayer: number, toLayer: number,
  ): string {
    const layerSpan = toLayer - fromLayer;

    if (layerSpan <= 1) {
      const cx = (x1 + x2) / 2;
      return `M ${x1} ${y1} C ${cx} ${y1}, ${cx} ${y2}, ${x2} ${y2}`;
    }

    // For multi-layer edges, route through the gaps between layers
    // to avoid crossing over intermediate nodes
    const points: Array<{ x: number; y: number }> = [{ x: x1, y: y1 }];

    for (let l = fromLayer + 1; l < toLayer; l++) {
      const gapX = PADDING + l * (NODE_WIDTH + LAYER_GAP) - LAYER_GAP / 2;
      const progress = (l - fromLayer) / layerSpan;
      let idealY = y1 + (y2 - y1) * progress;

      // Check if idealY passes through a node in this layer and nudge
      const ranges = layerNodeRanges.get(l) ?? [];
      for (const r of ranges) {
        if (idealY >= r.top && idealY <= r.bottom) {
          const distToTop = idealY - r.top;
          const distToBottom = r.bottom - idealY;
          idealY = distToTop < distToBottom ? r.top - 8 : r.bottom + 8;
          break;
        }
      }

      points.push({ x: gapX, y: idealY });
    }

    points.push({ x: x2, y: y2 });

    // Build smooth cubic bezier spline through waypoints
    let d = `M ${points[0].x} ${points[0].y}`;
    for (let i = 0; i < points.length - 1; i++) {
      const p0 = points[i];
      const p1 = points[i + 1];
      const tension = 0.4;
      const dx = (p1.x - p0.x) * tension;
      d += ` C ${p0.x + dx} ${p0.y}, ${p1.x - dx} ${p1.y}, ${p1.x} ${p1.y}`;
    }
    return d;
  }

  const edges: EdgeLayout[] = [];
  for (const dep of uniqueDeps) {
    const from = nodeMap.get(dep.depends_on);
    const to = nodeMap.get(dep.task_id);
    if (!from || !to) continue;

    const x1 = from.x + NODE_WIDTH;
    const y1v = portY(dep.depends_on, dep.task_id, outgoing);
    const x2 = to.x;
    const y2v = portY(dep.task_id, dep.depends_on, incoming);

    edges.push({
      from: dep.depends_on,
      to: dep.task_id,
      x1, y1: y1v, x2, y2: y2v,
      path: buildEdgePath(x1, y1v, x2, y2v, from.layer, to.layer),
      kind: dep.kind ?? "producer",
    });
  }

  const MENU_CLEARANCE = 100;
  const totalWidth =
    PADDING * 2 + numLayers * NODE_WIDTH + (numLayers - 1) * LAYER_GAP;
  const totalHeight =
    PADDING * 2 +
    maxNodesInLayer * (NODE_HEIGHT + NODE_GAP) -
    NODE_GAP +
    MENU_CLEARANCE;

  return {
    nodes: tasks.map((t) => nodeMap.get(t.id)!).filter(Boolean),
    edges,
    width: Math.max(totalWidth, NODE_WIDTH + PADDING * 2),
    height: Math.max(totalHeight, NODE_HEIGHT + PADDING * 2 + MENU_CLEARANCE),
  };
}
