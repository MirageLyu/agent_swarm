import { create } from "zustand";
import type {
  MissionInfo,
  TaskInfo,
  DependencyInfo,
  MissionStatus,
} from "../ipc/commands";

export interface MissionState {
  missions: MissionInfo[];
  selectedMissionId: string | null;
  tasks: TaskInfo[];
  dependencies: DependencyInfo[];
  planning: boolean;
  error: string | null;

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
}

export const useTaskStore = create<MissionState>((set) => ({
  missions: [],
  selectedMissionId: null,
  tasks: [],
  dependencies: [],
  planning: false,
  error: null,

  setMissions: (missions) => set({ missions }),

  addMission: (mission) =>
    set((s) => ({ missions: [mission, ...s.missions] })),

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
}));
