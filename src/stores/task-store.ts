import { create } from "zustand";
import type {
  MissionInfo,
  TaskInfo,
  DependencyInfo,
  MissionStatus,
} from "../ipc/commands";

/**
 * FM-15 v2.2: Planner 流式回显状态。
 *
 * 必须放在全局 store 里（而不是 MissionsView 的 useState），因为：
 * 用户在 plan 进行中切到其他 view，MissionsView 会 unmount，局部 useState
 * 全部丢失；切回来后只能看到"新生成的部分"，旧 thinking/agent loop 全没。
 *
 * 真正的 step 流可以靠 PlannerLoopPanel 重新挂载时调 listPlannerSteps()
 * 从 DB 拉回来；但 raw token 流没有 DB 持久化，至少要把 status / startTime /
 * 已经累积的 text 持久化，UI 才能"切回来还能继续看到"。
 *
 * startTime 持久化的目的：让计时器重新挂载时基于它计算 elapsedMs，
 * 而不是从 0 开始。
 */
export interface PlannerStreamSnapshot {
  visible: boolean;
  text: string;
  tokenCount: number;
  startTime: number | null;
  elapsedMs: number;
  status: "streaming" | "done" | "cancelled" | "error";
  errorMessage?: string;
  collapsed: boolean;
}

const EMPTY_PLANNER_STREAM: PlannerStreamSnapshot = {
  visible: false,
  text: "",
  tokenCount: 0,
  startTime: null,
  elapsedMs: 0,
  status: "streaming",
  collapsed: false,
};

export interface MissionState {
  missions: MissionInfo[];
  selectedMissionId: string | null;
  tasks: TaskInfo[];
  dependencies: DependencyInfo[];
  planning: boolean;
  error: string | null;

  // FM-15 v2.2: 跨 view 持久化的 planner 状态
  livePlannerSessionId: string | null;
  plannerStream: PlannerStreamSnapshot;

  setMissions: (missions: MissionInfo[]) => void;
  addMission: (mission: MissionInfo) => void;
  removeMission: (id: string) => void;
  updateMissionStatus: (id: string, status: MissionStatus) => void;
  selectMission: (id: string | null) => void;

  setDetail: (tasks: TaskInfo[], dependencies: DependencyInfo[]) => void;
  addTaskLocal: (task: TaskInfo, deps: DependencyInfo[]) => void;
  updateTaskLocal: (id: string, updates: Partial<TaskInfo>) => void;
  removeTaskLocal: (id: string) => void;

  setPlanning: (v: boolean) => void;
  setError: (err: string | null) => void;

  setLivePlannerSessionId: (id: string | null) => void;
  setPlannerStream: (
    next:
      | PlannerStreamSnapshot
      | ((prev: PlannerStreamSnapshot) => PlannerStreamSnapshot),
  ) => void;
  resetPlannerStream: () => void;
}

export const useTaskStore = create<MissionState>((set) => ({
  missions: [],
  selectedMissionId: null,
  tasks: [],
  dependencies: [],
  planning: false,
  error: null,
  livePlannerSessionId: null,
  plannerStream: EMPTY_PLANNER_STREAM,

  setMissions: (missions) => set({ missions }),

  addMission: (mission) =>
    set((s) => {
      // FM-15 v2.2 (S2-3): upsert——同 id 时用新数据替换旧条目，避免 Step 1 + Step 2 双 addMission 出现重复行。
      const existsIdx = s.missions.findIndex((m) => m.id === mission.id);
      if (existsIdx === -1) return { missions: [mission, ...s.missions] };
      const next = s.missions.slice();
      next[existsIdx] = { ...next[existsIdx], ...mission };
      return { missions: next };
    }),

  removeMission: (id) =>
    set((s) => ({
      missions: s.missions.filter((m) => m.id !== id),
      selectedMissionId: s.selectedMissionId === id ? null : s.selectedMissionId,
    })),

  updateMissionStatus: (id, status) =>
    set((s) => ({
      missions: s.missions.map((m) => (m.id === id ? { ...m, status } : m)),
    })),

  selectMission: (id) => set({ selectedMissionId: id }),

  setDetail: (tasks, dependencies) => set({ tasks, dependencies }),

  addTaskLocal: (task, deps) =>
    set((s) => ({
      tasks: [...s.tasks, task],
      dependencies: [...s.dependencies, ...deps],
      missions: s.missions.map((m) =>
        m.id === task.mission_id
          ? { ...m, task_count: m.task_count + 1 }
          : m,
      ),
    })),

  updateTaskLocal: (id, updates) =>
    set((s) => ({
      tasks: s.tasks.map((t) => (t.id === id ? { ...t, ...updates } : t)),
    })),

  removeTaskLocal: (id) =>
    set((s) => {
      const task = s.tasks.find((t) => t.id === id);
      return {
        tasks: s.tasks.filter((t) => t.id !== id),
        dependencies: s.dependencies.filter(
          (d) => d.task_id !== id && d.depends_on !== id,
        ),
        missions: task
          ? s.missions.map((m) =>
              m.id === task.mission_id
                ? { ...m, task_count: Math.max(0, m.task_count - 1) }
                : m,
            )
          : s.missions,
      };
    }),

  setPlanning: (v) => set({ planning: v }),
  setError: (err) => set({ error: err }),

  setLivePlannerSessionId: (id) => set({ livePlannerSessionId: id }),
  setPlannerStream: (next) =>
    set((s) => ({
      plannerStream:
        typeof next === "function"
          ? (next as (p: PlannerStreamSnapshot) => PlannerStreamSnapshot)(
              s.plannerStream,
            )
          : next,
    })),
  resetPlannerStream: () => set({ plannerStream: EMPTY_PLANNER_STREAM }),
}));
