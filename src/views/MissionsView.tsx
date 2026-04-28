import { useEffect, useState, useCallback, useRef } from "react";
import { save, open } from "@tauri-apps/plugin-dialog";
import { commands } from "../ipc/commands";
import type { TaskInfo, Complexity, CreateMissionResponse } from "../ipc/commands";
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
import { PlannerStreamPanel } from "../components/mission/PlannerStreamPanel";
import { PlannerLoopPanel } from "../components/mission/PlannerLoopPanel";
import { MissionDeliveryPanel } from "../components/mission/MissionDeliveryPanel";
import { MissionChatPanel } from "../components/mission/MissionChatPanel";
import { onMissionDelivered, type MissionDeliveredPayload } from "../ipc/events";
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
    livePlannerSessionId,
    setLivePlannerSessionId,
    plannerStream: stream,
    setPlannerStream: setStream,
  } = useTaskStore();

  const setActiveView = useUiStore((s) => s.setActiveView);
  const setActivePreflight = useUiStore((s) => s.setActivePreflight);
  const dagSelectedTaskId = useUiStore((s) => s.dagSelectedTaskId);
  const setDagSelectedTaskId = useUiStore((s) => s.setDagSelectedTaskId);

  const [editingTask, setEditingTask] = useState<TaskInfo | null>(null);
  const [addDialogOpen, setAddDialogOpen] = useState(false);
  const [startDialogOpen, setStartDialogOpen] = useState(false);
  const [planDialogOpen, setPlanDialogOpen] = useState(false);

  // FM-15 v2.2 P4-S4: mission-delivered payload 缓存，按 mission_id 索引。
  // 在 mission 完成时显示交付面板，切换 mission 后保留——便于回头查看。
  const [deliveredPayloads, setDeliveredPayloads] = useState<
    Record<string, MissionDeliveredPayload>
  >({});

  // FM-08 dialog state
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [deleteTargetId, setDeleteTargetId] = useState<string | null>(null);
  const [restartDialogOpen, setRestartDialogOpen] = useState(false);
  const [restartTargetId, setRestartTargetId] = useState<string | null>(null);
  const [restartMode, setRestartMode] = useState<"full" | "failed_only">("full");

  // FM-15 v2.2: planner stream state 已提到 task-store + planner 事件订阅
  // 已提到 App 级 usePlannerEventSync。这里只保留计时器（驱动 elapsedMs 更新）。
  const streamTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

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

  // FM-15 v2.2 P4-S4: 监听 mission-delivered 事件并存进缓存，
  // 切 view / 切 mission 不丢；多个 mission 各自独立缓存。
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    onMissionDelivered((payload) => {
      setDeliveredPayloads((prev) => ({ ...prev, [payload.missionId]: payload }));
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch((err) => {
        console.warn("[MissionsView] failed to subscribe mission-delivered", err);
      });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // FM-15 v2.2: 计时器跟随 store 的 planning + startTime。
  // 事件订阅在 App 级 usePlannerEventSync，这里不再处理。
  useEffect(() => {
    if (!planning) {
      if (streamTimerRef.current) {
        clearInterval(streamTimerRef.current);
        streamTimerRef.current = null;
      }
      return;
    }
    streamTimerRef.current = setInterval(() => {
      setStream((s) => ({
        ...s,
        elapsedMs: s.startTime ? Date.now() - s.startTime : s.elapsedMs,
      }));
    }, 200);
    return () => {
      if (streamTimerRef.current) clearInterval(streamTimerRef.current);
      streamTimerRef.current = null;
    };
  }, [planning, setStream]);

  const toggleStreamCollapse = useCallback(() => {
    setStream((s) => ({ ...s, collapsed: !s.collapsed }));
  }, []);

  const planCancelledRef = useRef(false);

  const handlePlanReady = useCallback(
    async (created: CreateMissionResponse) => {
      // FM-15 v2.2 (S2-3): mission 已在对话框 Step 1 创建。这里只负责跑 PlannerEngine。
      setPlanDialogOpen(false);
      planCancelledRef.current = false;

      // 立刻把 draft mission 插入列表 + 选中，让用户感知到 mission 已存在。
      addMission(created);
      selectMission(created.id);
      setDetail([], []);

      // 显式重置 planner stream——这是"开始一次新 plan"的明确动作，
      // 不能放在 effect 里（否则切 view 回来 effect 重跑会清空已有 text）。
      const now = Date.now();
      setStream({
        visible: true,
        text: "",
        tokenCount: 0,
        startTime: now,
        elapsedMs: 0,
        status: "streaming",
        collapsed: false,
      });
      setLivePlannerSessionId(null);
      setPlanning(true);
      setError(null);
      try {
        const result = await commands.planMission({ mission_id: created.id });
        if (planCancelledRef.current) return;
        if (result.planner_session_id) {
          setLivePlannerSessionId(result.planner_session_id);
        }
        const detail = await commands.getMissionDetail(result.mission_id);
        // 用 plan 后的 detail 覆盖（title 会被 planner 改写）
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
    [
      addMission,
      selectMission,
      setDetail,
      setPlanning,
      setError,
      setStream,
      setLivePlannerSessionId,
    ],
  );

  const handlePlanCancel = useCallback(() => {
    planCancelledRef.current = true;
    // status="cancelled" 让全局事件订阅在收到后续 done/error 时不再覆盖回 done/error。
    setStream((s) => ({
      ...s,
      status: "cancelled",
      elapsedMs: s.startTime ? Date.now() - s.startTime : s.elapsedMs,
    }));
    setPlanning(false);
  }, [setPlanning, setStream]);

  const handlePreflightReady = useCallback(
    async (created: CreateMissionResponse) => {
      // FM-15 v2.2 (S2-3): mission 已存在，这里只 startPreflight。
      setPlanDialogOpen(false);
      setError(null);

      addMission(created);

      try {
        const result = await commands.startPreflight({ mission_id: created.id });
        setActivePreflight(result.mission_id, result.session_id);
        setActiveView("preflight");
      } catch (e) {
        setError(String(e));
      }
    },
    [addMission, setActivePreflight, setActiveView, setError],
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
      // 优先尝试 auto_start 复用上次 repo_path 一键重跑；后端没记 repo_path 时
      // 会返回 auto_started=false，前端再 fallback 到工作区选择对话框。
      const result = await commands.restartMission({
        mission_id: restartTargetId,
        mode: restartMode,
        auto_start: true,
      });
      updateMissionStatus(
        restartTargetId,
        result.auto_started ? "running" : "planned",
      );
      if (restartTargetId === selectedMissionId) {
        const detail = await commands.getMissionDetail(restartTargetId);
        setDetail(detail.tasks, detail.dependencies);
      }
      if (!result.auto_started) {
        setStartDialogOpen(true);
      }
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
                {/* FM-15 v2.2 (S3-6): 即便 plan_mission 还没返回 session_id，
                    PlannerLoopPanel 也会从首个 planner-step 事件里自动发现并挂上
                    后续步骤——避免几十秒"什么都看不见"的体感。 */}
                <PlannerLoopPanel
                  sessionId={livePlannerSessionId ?? undefined}
                  isLive
                />
                
                <div className={styles.cancelBar}>
                  <Button variant="ghost" size="sm" onClick={handlePlanCancel} style={{ color: "var(--color-error)" }}>
                    Cancel Planning
                  </Button>
                </div>
              </div>
            ) : (
              <>
                {selectedMission &&
                selectedMission.status === "completed" &&
                deliveredPayloads[selectedMission.id] ? (
                  <div className={styles.deliverySection}>
                    <MissionDeliveryPanel
                      payload={deliveredPayloads[selectedMission.id]}
                    />
                  </div>
                ) : null}
                <TaskDAG
                  tasks={tasks}
                  dependencies={dependencies}
                  onEditTask={setEditingTask}
                  onDeleteTask={handleDeleteTask}
                  onAddTask={() => setAddDialogOpen(true)}
                  focusNodeId={focusNodeId}
                  onFocusHandled={() => setFocusNodeId(null)}
                />
                {selectedMission &&
                (selectedMission.status === "completed" ||
                  selectedMission.status === "failed") ? (
                  <div className={styles.chatSection}>
                    <MissionChatPanel
                      missionId={selectedMission.id}
                      enabled
                      onFollowupCreated={(childId) => {
                        // 跳到子 mission 并自动启动 planner
                        selectMission(childId);
                      }}
                    />
                  </div>
                ) : null}
              </>
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
        onPlanReady={handlePlanReady}
        onPreflightReady={handlePreflightReady}
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
