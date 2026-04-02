import { useEffect, useState, useCallback } from "react";
import { commands } from "../ipc/commands";
import type { TaskInfo, Complexity } from "../ipc/commands";
import { useTaskStore } from "../stores/task-store";
import { useUiStore } from "../stores/ui-store";
import {
  PlanInput,
  TaskDAG,
  MissionList,
  TaskEditDialog,
  AddTaskDialog,
  StartMissionDialog,
} from "../components/mission";
import { Button } from "../components/ui";
import styles from "./MissionsView.module.css";

export function MissionsView() {
  const {
    missions,
    selectedMissionId,
    tasks,
    dependencies,
    planning,
    error,
    setMissions,
    addMission,
    removeMission,
    updateMissionStatus,
    selectMission,
    setDetail,
    addTaskLocal,
    updateTaskLocal,
    removeTaskLocal,
    setPlanning,
    setError,
  } = useTaskStore();

  const setActiveView = useUiStore((s) => s.setActiveView);

  const [editingTask, setEditingTask] = useState<TaskInfo | null>(null);
  const [addDialogOpen, setAddDialogOpen] = useState(false);
  const [startDialogOpen, setStartDialogOpen] = useState(false);

  const selectedMission = missions.find((m) => m.id === selectedMissionId);

  // Load missions on mount
  useEffect(() => {
    commands.listMissions().then(setMissions).catch(console.error);
  }, [setMissions]);

  // Load mission detail when selection changes
  useEffect(() => {
    if (!selectedMissionId) {
      setDetail([], []);
      return;
    }
    commands
      .getMissionDetail(selectedMissionId)
      .then((detail) => {
        setDetail(detail.tasks, detail.dependencies);
      })
      .catch(console.error);
  }, [selectedMissionId, setDetail]);

  // Auto-select first mission
  useEffect(() => {
    if (!selectedMissionId && missions.length > 0) {
      selectMission(missions[0].id);
    }
  }, [missions, selectedMissionId, selectMission]);

  const handlePlan = useCallback(
    async (description: string) => {
      setPlanning(true);
      setError(null);
      try {
        const result = await commands.planMission({ description });
        const detail = await commands.getMissionDetail(result.mission_id);
        addMission(detail.mission);
        selectMission(result.mission_id);
        setDetail(detail.tasks, detail.dependencies);
      } catch (e) {
        setError(String(e));
      } finally {
        setPlanning(false);
      }
    },
    [addMission, selectMission, setDetail, setPlanning, setError],
  );

  const handleEditSave = useCallback(
    async (taskId: string, title: string, description: string) => {
      updateTaskLocal(taskId, { title, description });
      try {
        await commands.updateTask({ task_id: taskId, title, description });
      } catch {
        if (selectedMissionId) {
          const detail = await commands.getMissionDetail(selectedMissionId);
          setDetail(detail.tasks, detail.dependencies);
        }
      }
    },
    [updateTaskLocal, selectedMissionId, setDetail],
  );

  const handleDeleteTask = useCallback(
    async (taskId: string) => {
      removeTaskLocal(taskId);
      try {
        await commands.deleteTask(taskId);
      } catch {
        if (selectedMissionId) {
          const detail = await commands.getMissionDetail(selectedMissionId);
          setDetail(detail.tasks, detail.dependencies);
        }
      }
    },
    [removeTaskLocal, selectedMissionId, setDetail],
  );

  const handleAddTask = useCallback(
    async (
      title: string,
      description: string,
      complexity: Complexity,
      dependsOn: string[],
    ) => {
      if (!selectedMissionId) return;
      try {
        const task = await commands.addTask({
          mission_id: selectedMissionId,
          title,
          description,
          complexity,
          depends_on: dependsOn,
        });
        const newDeps = dependsOn.map((d) => ({
          task_id: task.id,
          depends_on: d,
        }));
        addTaskLocal(task, newDeps);
      } catch (e) {
        setError(String(e));
      }
    },
    [selectedMissionId, addTaskLocal, setError],
  );

  const handleConfirmAndStart = useCallback(async () => {
    if (!selectedMissionId) return;
    try {
      const currentStatus = selectedMission?.status;

      if (currentStatus === "draft") {
        await commands.confirmMission(selectedMissionId);
        updateMissionStatus(selectedMissionId, "planned");
      }

      setStartDialogOpen(true);
    } catch (e) {
      setError(String(e));
    }
  }, [selectedMissionId, selectedMission?.status, updateMissionStatus, setError]);

  const handleStartMission = useCallback(
    async (repoPath: string) => {
      if (!selectedMissionId) return;
      try {
        await commands.startMissionExecution({
          mission_id: selectedMissionId,
          repo_path: repoPath,
        });
        updateMissionStatus(selectedMissionId, "running");
        setStartDialogOpen(false);
        setActiveView("workspace");
      } catch (e) {
        setError(String(e));
        setStartDialogOpen(false);
      }
    },
    [selectedMissionId, updateMissionStatus, setActiveView, setError],
  );

  const handleDeleteMission = useCallback(
    async (id: string) => {
      try {
        await commands.deleteMission(id);
        removeMission(id);
      } catch (e) {
        setError(String(e));
      }
    },
    [removeMission, setError],
  );

  const canConfirm =
    selectedMission?.status === "draft" && tasks.length > 0 && !planning;
  const canStart = selectedMission?.status === "planned";

  return (
    <div className={styles.container}>
      <div className={styles.sidebar}>
        <MissionList
          missions={missions}
          selectedId={selectedMissionId}
          onSelect={selectMission}
          onDelete={handleDeleteMission}
        />
      </div>
      <div className={styles.main}>
        <div className={styles.planSection}>
          <PlanInput onPlan={handlePlan} loading={planning} />
          {error && <p className={styles.error}>{error}</p>}
        </div>

        <div className={styles.dagSection}>
          <TaskDAG
            tasks={tasks}
            dependencies={dependencies}
            onEditTask={setEditingTask}
            onDeleteTask={handleDeleteTask}
            onAddTask={() => setAddDialogOpen(true)}
          />
        </div>

        {selectedMission && (canConfirm || canStart) && (
          <div className={styles.actionBar}>
            <Button
              variant="primary"
              size="md"
              onClick={handleConfirmAndStart}
              disabled={!canConfirm && !canStart}
            >
              {canStart ? "Start Mission" : "Confirm & Start"}
            </Button>
          </div>
        )}
      </div>

      <TaskEditDialog
        task={editingTask}
        open={editingTask !== null}
        onClose={() => setEditingTask(null)}
        onSave={handleEditSave}
      />

      <AddTaskDialog
        open={addDialogOpen}
        onClose={() => setAddDialogOpen(false)}
        onAdd={handleAddTask}
        existingTasks={tasks}
      />

      {selectedMissionId && (
        <StartMissionDialog
          open={startDialogOpen}
          missionId={selectedMissionId}
          onClose={() => setStartDialogOpen(false)}
          onStart={handleStartMission}
        />
      )}
    </div>
  );
}
