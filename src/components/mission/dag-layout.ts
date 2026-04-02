import type { TaskInfo, DependencyInfo } from "../../ipc/commands";

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
}

export interface DagLayout {
  nodes: NodeLayout[];
  edges: EdgeLayout[];
  width: number;
  height: number;
}

export const NODE_WIDTH = 200;
export const NODE_HEIGHT = 72;
const LAYER_GAP = 80;
const NODE_GAP = 24;
const PADDING = 40;
const PORT_MARGIN = 12;

export function computeDagLayout(
  tasks: TaskInfo[],
  dependencies: DependencyInfo[],
): DagLayout {
  if (tasks.length === 0) {
    return { nodes: [], edges: [], width: 0, height: 0 };
  }

  // Deduplicate dependencies
  const depSet = new Set<string>();
  const uniqueDeps: DependencyInfo[] = [];
  for (const dep of dependencies) {
    const key = `${dep.task_id}::${dep.depends_on}`;
    if (!depSet.has(key)) {
      depSet.add(key);
      uniqueDeps.push(dep);
    }
  }

  const depMap = new Map<string, Set<string>>();
  for (const dep of uniqueDeps) {
    if (!depMap.has(dep.task_id)) depMap.set(dep.task_id, new Set());
    depMap.get(dep.task_id)!.add(dep.depends_on);
  }

  // Assign layers via longest-path (max dependency depth + 1)
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

  const taskIndex = new Map(tasks.map((t, i) => [t.id, i]));
  for (const [, ids] of layers) {
    ids.sort((a, b) => (taskIndex.get(a) ?? 0) - (taskIndex.get(b) ?? 0));
  }

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

  // Build port maps: for each node, collect outgoing and incoming edges,
  // then assign vertically distributed y-offsets along the node's side.
  const outgoing = new Map<string, string[]>(); // sourceId → [targetIds]
  const incoming = new Map<string, string[]>(); // targetId → [sourceIds]

  for (const dep of uniqueDeps) {
    if (!nodeMap.has(dep.depends_on) || !nodeMap.has(dep.task_id)) continue;
    if (!outgoing.has(dep.depends_on)) outgoing.set(dep.depends_on, []);
    outgoing.get(dep.depends_on)!.push(dep.task_id);
    if (!incoming.has(dep.task_id)) incoming.set(dep.task_id, []);
    incoming.get(dep.task_id)!.push(dep.depends_on);
  }

  // Sort ports by the y-position of the connected node to minimize crossings
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

  const edges: EdgeLayout[] = [];
  for (const dep of uniqueDeps) {
    const from = nodeMap.get(dep.depends_on);
    const to = nodeMap.get(dep.task_id);
    if (!from || !to) continue;

    edges.push({
      from: dep.depends_on,
      to: dep.task_id,
      x1: from.x + NODE_WIDTH,
      y1: portY(dep.depends_on, dep.task_id, outgoing),
      x2: to.x,
      y2: portY(dep.task_id, dep.depends_on, incoming),
    });
  }

  const totalWidth =
    PADDING * 2 + numLayers * NODE_WIDTH + (numLayers - 1) * LAYER_GAP;
  const totalHeight =
    PADDING * 2 + maxNodesInLayer * (NODE_HEIGHT + NODE_GAP) - NODE_GAP;

  return {
    nodes: tasks.map((t) => nodeMap.get(t.id)!).filter(Boolean),
    edges,
    width: Math.max(totalWidth, NODE_WIDTH + PADDING * 2),
    height: Math.max(totalHeight, NODE_HEIGHT + PADDING * 2),
  };
}
