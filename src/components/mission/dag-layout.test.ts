import { describe, it, expect } from "vitest";
import { computeDagLayout, NODE_WIDTH, NODE_HEIGHT } from "./dag-layout";
import type { TaskInfo, DependencyInfo } from "../../ipc/commands";

function makeTask(id: string, title = `Task ${id}`): TaskInfo {
  return {
    id,
    mission_id: "m1",
    title,
    description: "",
    status: "pending",
    complexity: "medium",
    assigned_agent_id: null,
    created_at: "",
    completed_at: null,
  };
}

function makeDep(from: string, to: string): DependencyInfo {
  return { task_id: to, depends_on: from };
}

describe("DAG Layout (UT-02)", () => {
  it("UT-02.1: linear dependency T1→T2→T3 → 3 layers, 1 node each", () => {
    const tasks = [makeTask("T1"), makeTask("T2"), makeTask("T3")];
    const deps = [makeDep("T1", "T2"), makeDep("T2", "T3")];
    const layout = computeDagLayout(tasks, deps);

    expect(layout.nodes.length).toBe(3);
    const layers = new Set(layout.nodes.map((n) => n.layer));
    expect(layers.size).toBe(3);

    const t1 = layout.nodes.find((n) => n.id === "T1")!;
    const t2 = layout.nodes.find((n) => n.id === "T2")!;
    const t3 = layout.nodes.find((n) => n.id === "T3")!;
    expect(t1.layer).toBe(0);
    expect(t2.layer).toBe(1);
    expect(t3.layer).toBe(2);
  });

  it("UT-02.2: parallel T1,T2 → T3 depends on both → 2 layers", () => {
    const tasks = [makeTask("T1"), makeTask("T2"), makeTask("T3")];
    const deps = [makeDep("T1", "T3"), makeDep("T2", "T3")];
    const layout = computeDagLayout(tasks, deps);

    const t1 = layout.nodes.find((n) => n.id === "T1")!;
    const t2 = layout.nodes.find((n) => n.id === "T2")!;
    const t3 = layout.nodes.find((n) => n.id === "T3")!;
    expect(t1.layer).toBe(0);
    expect(t2.layer).toBe(0);
    expect(t3.layer).toBe(1);
  });

  it("UT-02.3: diamond T1→T2,T3; T2→T4; T3→T4 → 3 layers", () => {
    const tasks = [makeTask("T1"), makeTask("T2"), makeTask("T3"), makeTask("T4")];
    const deps = [
      makeDep("T1", "T2"),
      makeDep("T1", "T3"),
      makeDep("T2", "T4"),
      makeDep("T3", "T4"),
    ];
    const layout = computeDagLayout(tasks, deps);

    const layers = layout.nodes.reduce(
      (acc, n) => {
        acc[n.layer] = acc[n.layer] || [];
        acc[n.layer].push(n.id);
        return acc;
      },
      {} as Record<number, string[]>,
    );

    expect(Object.keys(layers).length).toBe(3);
    expect(layers[0]).toEqual(["T1"]);
    expect(layers[1].sort()).toEqual(["T2", "T3"]);
    expect(layers[2]).toEqual(["T4"]);
  });

  it("UT-02.4: single node → 1 layer, centered", () => {
    const tasks = [makeTask("T1")];
    const layout = computeDagLayout(tasks, []);

    expect(layout.nodes.length).toBe(1);
    expect(layout.nodes[0].layer).toBe(0);
    expect(layout.width).toBeGreaterThan(0);
    expect(layout.height).toBeGreaterThan(0);
  });

  it("UT-02.5: 10 parallel tasks → 1 layer, y-coords distributed", () => {
    const tasks = Array.from({ length: 10 }, (_, i) => makeTask(`T${i + 1}`));
    const layout = computeDagLayout(tasks, []);

    const allLayer0 = layout.nodes.every((n) => n.layer === 0);
    expect(allLayer0).toBe(true);

    const ys = layout.nodes.map((n) => n.y).sort((a, b) => a - b);
    for (let i = 1; i < ys.length; i++) {
      const gap = ys[i] - ys[i - 1];
      expect(gap).toBe(NODE_HEIGHT + 24);
    }
  });

  it("UT-02.6: deep serial T1→T2→...→T10 → 10 layers", () => {
    const tasks = Array.from({ length: 10 }, (_, i) => makeTask(`T${i + 1}`));
    const deps = Array.from({ length: 9 }, (_, i) =>
      makeDep(`T${i + 1}`, `T${i + 2}`),
    );
    const layout = computeDagLayout(tasks, deps);

    const layers = new Set(layout.nodes.map((n) => n.layer));
    expect(layers.size).toBe(10);
  });

  it("UT-02.7: cross-layer dep T1→T3 (skip T2 layer) → T3 at layer 2", () => {
    const tasks = [makeTask("T1"), makeTask("T2"), makeTask("T3")];
    const deps = [makeDep("T1", "T2"), makeDep("T1", "T3"), makeDep("T2", "T3")];
    const layout = computeDagLayout(tasks, deps);

    const t3 = layout.nodes.find((n) => n.id === "T3")!;
    expect(t3.layer).toBe(2);
  });

  it("empty tasks → empty layout", () => {
    const layout = computeDagLayout([], []);
    expect(layout.nodes.length).toBe(0);
    expect(layout.edges.length).toBe(0);
  });

  it("single edge connects at node center", () => {
    const tasks = [makeTask("T1"), makeTask("T2")];
    const deps = [makeDep("T1", "T2")];
    const layout = computeDagLayout(tasks, deps);

    expect(layout.edges.length).toBe(1);
    const edge = layout.edges[0];
    const t1 = layout.nodes.find((n) => n.id === "T1")!;
    const t2 = layout.nodes.find((n) => n.id === "T2")!;
    expect(edge.x1).toBe(t1.x + NODE_WIDTH);
    expect(edge.y1).toBe(t1.y + NODE_HEIGHT / 2);
    expect(edge.x2).toBe(t2.x);
    expect(edge.y2).toBe(t2.y + NODE_HEIGHT / 2);
  });

  it("multiple edges from same node have distinct y-coordinates (port spread)", () => {
    const tasks = [makeTask("T1"), makeTask("T2"), makeTask("T3"), makeTask("T4")];
    const deps = [
      makeDep("T1", "T2"),
      makeDep("T1", "T3"),
      makeDep("T1", "T4"),
    ];
    const layout = computeDagLayout(tasks, deps);

    expect(layout.edges.length).toBe(3);
    const outY = layout.edges.map((e) => e.y1);
    const uniqueY = new Set(outY);
    expect(uniqueY.size).toBe(3);
  });

  it("multiple edges into same node have distinct y-coordinates (port spread)", () => {
    const tasks = [makeTask("T1"), makeTask("T2"), makeTask("T3")];
    const deps = [makeDep("T1", "T3"), makeDep("T2", "T3")];
    const layout = computeDagLayout(tasks, deps);

    expect(layout.edges.length).toBe(2);
    const inY = layout.edges.map((e) => e.y2);
    const uniqueY = new Set(inY);
    expect(uniqueY.size).toBe(2);
  });

  it("deduplicates identical dependencies", () => {
    const tasks = [makeTask("T1"), makeTask("T2")];
    const deps = [makeDep("T1", "T2"), makeDep("T1", "T2"), makeDep("T1", "T2")];
    const layout = computeDagLayout(tasks, deps);

    expect(layout.edges.length).toBe(1);
  });
});
