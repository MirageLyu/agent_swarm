import { useEffect, useState, useCallback, useRef } from "react";
import { save, open } from "@tauri-apps/plugin-dialog";
import { commands } from "../ipc/commands";
import type { TaskInfo, Complexity } from "../ipc/commands";
import { onPlannerStream, type PlannerStreamPayload } from "../ipc/events";
import { useTaskStore } from "../stores/task-store";
import { useUiStore } from "../stores/ui-store";
import type { MissionAction } from "../components/mission/MissionListItem";
import {
  TaskDAG,
  MissionList,
  TaskEditDialog,
  AddTaskDialog,
  StartMissionDialog,
  DeleteConfirmDialog,
  RestartConfirmDialog,
} from "../components/mission";
import { TaskDetailPanel } from "../components/mission/TaskDetailPanel";
import { PlanMissionDialog } from "../components/mission/PlanMissionDialog";
import {
  PlannerStreamPanel,
  type PlannerStreamState,
} from "../components/mission/PlannerStreamPanel";
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
  const setActivePreflight = useUiStore((s) => s.setActivePreflight);
  const dagSelectedTaskId = useUiStore((s) => s.dagSelectedTaskId);
  const setDagSelectedTaskId = useUiStore((s) => s.setDagSelectedTaskId);

  const [editingTask, setEditingTask] = useState<TaskInfo | null>(null);
  const [addDialogOpen, setAddDialogOpen] = useState(false);
  const [startDialogOpen, setStartDialogOpen] = useState(false);
  const [planDialogOpen, setPlanDialogOpen] = useState(false);

  // FM-08 dialog state
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [deleteTargetId, setDeleteTargetId] = useState<string | null>(null);
  const [restartDialogOpen, setRestartDialogOpen] = useState(false);
  const [restartTargetId, setRestartTargetId] = useState<string | null>(null);
  const [restartMode, setRestartMode] = useState<"full" | "failed_only">("full");

  // Planner stream state (lifted from PlanInput)
  const [stream, setStream] = useState<PlannerStreamState>({
    visible: false,
    text: "",
    tokenCount: 0,
    elapsedMs: 0,
    status: "streaming",
    collapsed: false,
  });
  const streamTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const streamStartRef = useRef<number>(0);
  const streamCancelledRef = useRef(false);

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

  // Start/stop stream timer when planning state changes
  useEffect(() => {
    if (planning) {
      streamCancelledRef.current = false;
      streamStartRef.current = Date.now();
      setStream({
        visible: true,
        text: "",
        tokenCount: 0,
        elapsedMs: 0,
        status: "streaming",
        collapsed: false,
      });
      streamTimerRef.current = setInterval(() => {
        setStream((s) => ({
          ...s,
          elapsedMs: Date.now() - streamStartRef.current,
        }));
      }, 200);
    } else {
      if (streamTimerRef.current) {
        clearInterval(streamTimerRef.current);
        streamTimerRef.current = null;
      }
    }
    return () => {
      if (streamTimerRef.current) clearInterval(streamTimerRef.current);
    };
  }, [planning]);

  // Subscribe to planner stream events
  useEffect(() => {
    const unsub = onPlannerStream((payload: PlannerStreamPayload) => {
      if (streamCancelledRef.current) return;

      if (payload.kind === "reasoning_delta" || payload.kind === "text_delta") {
        setStream((s) => ({
          ...s,
          text: s.text + payload.content,
          tokenCount: s.tokenCount + 1,
        }));
      } else if (payload.kind === "done") {
        setStream((s) => ({
          ...s,
          status: "done",
          elapsedMs: Date.now() - streamStartRef.current,
        }));
      } else if (payload.kind === "error") {
        setStream((s) => ({
          ...s,
          status: "error",
          errorMessage: payload.content,
          elapsedMs: Date.now() - streamStartRef.current,
        }));
      }
    });

    return () => {
      unsub.then((fn) => fn());
    };
  }, []);

  const toggleStreamCollapse = useCallback(() => {
    setStream((s) => ({ ...s, collapsed: !s.collapsed }));
  }, []);

  const planCancelledRef = useRef(false);

  const handlePlan = useCallback(
    async (description: string) => {
      setPlanDialogOpen(false);
      planCancelledRef.current = false;
      setDetail([], []);
      selectMission(null);
      setPlanning(true);
      setError(null);
      try {
        const result = await commands.planMission({ description });
        if (planCancelledRef.current) return;
        const detail = await commands.getMissionDetail(result.mission_id);
        addMission(detail.mission);
        selectMission(result.mission_id);
        setDetail(detail.tasks, detail.dependencies);
      } catch (e) {
        if (!planCancelledRef.current) {
          setError(String(e));
        }
      } finally {
        setPlanning(false);
      }
    },
    [addMission, selectMission, setDetail, setPlanning, setError],
  );

  const handlePlanCancel = useCallback(() => {
    planCancelledRef.current = true;
    streamCancelledRef.current = true;
    setStream((s) => ({
      ...s,
      status: "cancelled",
      elapsedMs: Date.now() - streamStartRef.current,
    }));
    setPlanning(false);
  }, [setPlanning]);

  const handlePreflight = useCallback(
    async (description: string) => {
      setPlanDialogOpen(false);
      setError(null);
      try {
        const result = await commands.startPreflight({ description });
        setActivePreflight(result.mission_id, result.session_id);
        setActiveView("preflight");
      } catch (e) {
        setError(String(e));
      }
    },
    [setActivePreflight, setActiveView, setError],
  );

  const handleEditSave = useCallback(
    async (taskId: string, title: string, description: string, dependsOn: string[]) => {
      updateTaskLocal(taskId, { title, description });
      try {
        await commands.updateTask({ task_id: taskId, title, description });
        await commands.setTaskDependencies({ task_id: taskId, depends_on: dependsOn });
        if (selectedMissionId) {
          const detail = await commands.getMissionDetail(selectedMissionId);
          setDetail(detail.tasks, detail.dependencies);
        }
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

  const handleExportMission = useCallback(
    async (missionId: string) => {
      const mission = missions.find((m) => m.id === missionId);
      const defaultName = (mission?.title ?? "mission")
        .replace(/[^a-zA-Z0-9\u4e00-\u9fff_-]/g, "_")
        .substring(0, 60);

      const filePath = await save({
        title: "Export Mission Template",
        defaultPath: `${defaultName}.mission.yaml`,
        filters: [{ name: "YAML", extensions: ["yaml", "yml"] }],
      });
      if (!filePath) return;

      try {
        await commands.exportMissionTemplate({
          mission_id: missionId,
          file_path: filePath,
        });
      } catch (e) {
        setError(String(e));
      }
    },
    [missions, setError],
  );

  const handleImportMission = useCallback(async () => {
    const selected = await open({
      title: "Import Mission Template",
      multiple: false,
      filters: [{ name: "YAML", extensions: ["yaml", "yml"] }],
    });
    if (!selected) return;

    const filePath = typeof selected === "string" ? selected : selected;
    try {
      const newMission = await commands.importMissionTemplate(filePath);
      addMission(newMission);
      selectMission(newMission.id);
    } catch (e) {
      setError(String(e));
    }
  }, [addMission, selectMission, setError]);

  // FM-08: Mission action handler
  const handleMissionAction = useCallback(
    (id: string, action: MissionAction) => {
      switch (action) {
        case "export":
          handleExportMission(id);
          break;
        case "delete":
          setDeleteTargetId(id);
          setDeleteDialogOpen(true);
          break;
        case "stop":
          commands
            .stopMissionExecution(id)
            .then(() => updateMissionStatus(id, "failed"))
            .catch((e) => setError(String(e)));
          break;
        case "restart_full":
          setRestartTargetId(id);
          setRestartMode("full");
          setRestartDialogOpen(true);
          break;
        case "restart_failed":
          setRestartTargetId(id);
          setRestartMode("failed_only");
          setRestartDialogOpen(true);
          break;
      }
    },
    [handleExportMission, updateMissionStatus, setError],
  );

  const handleDeleteConfirm = useCallback(
    async (cleanWorkspace: boolean) => {
      if (!deleteTargetId) return;
      try {
        await commands.deleteMission({
          mission_id: deleteTargetId,
          clean_workspace: cleanWorkspace,
        });
        removeMission(deleteTargetId);
      } catch (e) {
        setError(String(e));
      } finally {
        setDeleteDialogOpen(false);
        setDeleteTargetId(null);
      }
    },
    [deleteTargetId, removeMission, setError],
  );

  const handleRestartConfirm = useCallback(async () => {
    if (!restartTargetId) return;
    try {
      await commands.restartMission({
        mission_id: restartTargetId,
        mode: restartMode,
      });
      updateMissionStatus(restartTargetId, "planned");
      if (restartTargetId === selectedMissionId) {
        const detail = await commands.getMissionDetail(restartTargetId);
        setDetail(detail.tasks, detail.dependencies);
      }
      setStartDialogOpen(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setRestartDialogOpen(false);
      setRestartTargetId(null);
    }
  }, [
    restartTargetId,
    restartMode,
    selectedMissionId,
    updateMissionStatus,
    setDetail,
    setError,
  ]);

  const canConfirm =
    selectedMission?.status === "draft" && tasks.length > 0 && !planning;
  const canStart = selectedMission?.status === "planned";

  const deleteTarget = missions.find((m) => m.id === deleteTargetId);
  const restartTarget = missions.find((m) => m.id === restartTargetId);
  const failedCount = tasks.filter(
    (t) => t.status === "failed" || t.status === "cancelled",
  ).length;

  const selectedTask = dagSelectedTaskId
    ? (tasks.find((t) => t.id === dagSelectedTaskId) ?? null)
    : null;

  const [focusNodeId, setFocusNodeId] = useState<string | null>(null);
  const handleFocusTask = useCallback((taskId: string) => {
    setDagSelectedTaskId(taskId);
    setFocusNodeId(taskId);
  }, [setDagSelectedTaskId]);

  return (
    <div className={styles.container}>
      <div className={styles.sidebar}>
        <MissionList
          missions={missions}
          selectedId={selectedMissionId}
          onSelect={(id) => {
            const mission = missions.find((m) => m.id === id);
            if (mission?.status === "preflight") {
              setActivePreflight(id, null);
              setActiveView("preflight");
            } else {
              selectMission(id);
            }
          }}
          onAction={handleMissionAction}
          onNewMission={() => setPlanDialogOpen(true)}
          onImport={handleImportMission}
        />
      </div>
      <div className={styles.main}>
        {error && <p className={styles.error}>{error}</p>}

        <div className={styles.contentSection}>
          <div className={styles.dagSection}>
            {planning ? (
              <div className={styles.planningArea}>
                <PlannerStreamPanel
                  state={stream}
                  onToggleCollapse={toggleStreamCollapse}
                  fullHeight
                />
                <div className={styles.cancelBar}>
                  <Button variant="ghost" size="sm" onClick={handlePlanCancel} style={{ color: "var(--color-error)" }}>
                    Cancel Planning
                  </Button>
                </div>
              </div>
            ) : (
              <TaskDAG
                tasks={tasks}
                dependencies={dependencies}
                onEditTask={setEditingTask}
                onDeleteTask={handleDeleteTask}
                onAddTask={() => setAddDialogOpen(true)}
                focusNodeId={focusNodeId}
                onFocusHandled={() => setFocusNodeId(null)}
              />
            )}
          </div>

          {!planning && tasks.length > 0 && (
            <TaskDetailPanel
              task={selectedTask}
              tasks={tasks}
              dependencies={dependencies}
              onClose={() => setDagSelectedTaskId(null)}
              onFocusTask={handleFocusTask}
            />
          )}
        </div>

        {!planning && selectedMission && (canConfirm || canStart) && (
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

      <PlanMissionDialog
        open={planDialogOpen}
        onClose={() => setPlanDialogOpen(false)}
        onPlan={handlePlan}
        onPreflight={handlePreflight}
      />

      <TaskEditDialog
        task={editingTask}
        open={editingTask !== null}
        onClose={() => setEditingTask(null)}
        onSave={handleEditSave}
        allTasks={tasks}
        dependencies={dependencies}
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

      <DeleteConfirmDialog
        open={deleteDialogOpen}
        missionTitle={deleteTarget?.title ?? ""}
        onClose={() => {
          setDeleteDialogOpen(false);
          setDeleteTargetId(null);
        }}
        onConfirm={handleDeleteConfirm}
      />

      <RestartConfirmDialog
        open={restartDialogOpen}
        missionTitle={restartTarget?.title ?? ""}
        mode={restartMode}
        failedCount={failedCount}
        totalCount={tasks.length}
        onClose={() => {
          setRestartDialogOpen(false);
          setRestartTargetId(null);
        }}
        onConfirm={handleRestartConfirm}
      />
    </div>
  );
}
