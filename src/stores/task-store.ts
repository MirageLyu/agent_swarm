import { create } from "zustand";

export type TaskStatus = "pending" | "queued" | "running" | "completed" | "failed" | "cancelled";

export interface Task {
  id: string;
  missionId: string;
  title: string;
  description: string;
  status: TaskStatus;
  assignedAgentId: string | null;
  dependencies: string[];
  createdAt: string;
  completedAt: string | null;
}

export interface Mission {
  id: string;
  title: string;
  description: string;
  status: "planning" | "executing" | "completed" | "failed";
  tasks: string[];
  createdAt: string;
  totalCostUsd: number;
}

interface TaskState {
  missions: Record<string, Mission>;
  tasks: Record<string, Task>;

  addMission: (mission: Mission) => void;
  updateMission: (id: string, updates: Partial<Mission>) => void;
  addTask: (task: Task) => void;
  updateTask: (id: string, updates: Partial<Task>) => void;
}

export const useTaskStore = create<TaskState>((set) => ({
  missions: {},
  tasks: {},

  addMission: (mission) =>
    set((s) => ({ missions: { ...s.missions, [mission.id]: mission } })),

  updateMission: (id, updates) =>
    set((s) => ({
      missions: {
        ...s.missions,
        [id]: s.missions[id] ? { ...s.missions[id], ...updates } : s.missions[id],
      },
    })),

  addTask: (task) => set((s) => ({ tasks: { ...s.tasks, [task.id]: task } })),

  updateTask: (id, updates) =>
    set((s) => ({
      tasks: {
        ...s.tasks,
        [id]: s.tasks[id] ? { ...s.tasks[id], ...updates } : s.tasks[id],
      },
    })),
}));
