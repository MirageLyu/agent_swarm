import { useEffect, useState, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import { save, open } from "@tauri-apps/plugin-dialog";
import { commands } from "../ipc/commands";
import { formatBackendError } from "../i18n";
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
import { useRetryableFlow } from "../hooks/useRetryableFlow";
import styles from "./MissionsView.module.css";

export function MissionsView() {
  const { t } = useTranslation("mission");
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
  // Quick Plan 失败后用户点"重试"时复用第一次创建出来的 mission（draft 状态依然在 DB 里）。
  // 见 .cursor/rules/retryable-flow.mdc：失败时上一次成功的状态必须保留。
  const lastPlanCreatedRef = useRef<CreateMissionResponse | null>(null);

  const planFlow = useRetryableFlow({
    operation: "plan_mission",
    invoke: useCallback(async () => {
      const created = lastPlanCreatedRef.current;
      if (!created) {
        throw new Error("planFlow.invoke called without prior createMission");
      }

      // 显式重置 planner stream（首次和重试都走这里），避免上一轮失败的 thinking 残留。
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

      try {
        const result = await commands.planMission({ mission_id: created.id });
        if (result.planner_session_id) {
          setLivePlannerSessionId(result.planner_session_id);
        }
        const detail = await commands.getMissionDetail(result.mission_id);
        return { result, detail };
      } finally {
        setPlanning(false);
      }
    }, [setStream, setLivePlannerSessionId, setPlanning]),
    onSuccess: useCallback(
      ({
        result,
        detail,
      }: {
        result: Awaited<ReturnType<typeof commands.planMission>>;
        detail: Awaited<ReturnType<typeof commands.getMissionDetail>>;
      }) => {
        // 用户已显式 cancel 的话不再拉用户视角——但 mission 已在 DB 里，
        // 用户回头能在列表看到它（draft 状态）。
        if (planCancelledRef.current) return;
        addMission(detail.mission);
        selectMission(result.mission_id);
        setDetail(detail.tasks, detail.dependencies);
      },
      [addMission, selectMission, setDetail],
    ),
    onAbandon: useCallback(() => {
      // 用户点"忽略"：不再保留 planner stream 区域，避免视觉残留。
      setStream((s) => ({
        ...s,
        status: "cancelled",
        elapsedMs: s.startTime ? Date.now() - s.startTime : s.elapsedMs,
      }));
    }, [setStream]),
  });

  const handlePlanReady = useCallback(
    (created: CreateMissionResponse) => {
      setPlanDialogOpen(false);
      planCancelledRef.current = false;

      // 立刻把 draft mission 插入列表 + 选中，让用户感知到 mission 已存在。
      addMission(created);
      selectMission(created.id);
      setDetail([], []);
      setError(null);

      lastPlanCreatedRef.current = created;
      planFlow.run();
    },
    [addMission, selectMission, setDetail, setError, planFlow],
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
    // 同时把 retryable banner 也收掉（如果有），避免"取消了还显示重试"。
    planFlow.reset();
  }, [setPlanning, setStream, planFlow]);

  const startPreflightFlow = useRetryableFlow({
    operation: "start_preflight",
    invoke: useCallback(async () => {
      const created = lastPlanCreatedRef.current;
      if (!created) {
        throw new Error("startPreflightFlow.invoke called without prior createMission");
      }
      return commands.startPreflight({ mission_id: created.id });
    }, []),
    onSuccess: useCallback(
      (result: Awaited<ReturnType<typeof commands.startPreflight>>) => {
        setActivePreflight(result.mission_id, result.session_id);
        setActiveView("preflight");
      },
      [setActivePreflight, setActiveView],
    ),
  });

  const handlePreflightReady = useCallback(
    (created: CreateMissionResponse) => {
      setPlanDialogOpen(false);
      setError(null);
      addMission(created);
      lastPlanCreatedRef.current = created;
      startPreflightFlow.run();
    },
    [addMission, setError, startPreflightFlow],
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
        setError(formatBackendError(e));
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
      setError(formatBackendError(e));
    }
  }, [selectedMissionId, selectedMission?.status, updateMissionStatus, setError]);

  const lastStartArgsRef = useRef<{ missionId: string; repoPath: string } | null>(null);
  const startMissionFlow = useRetryableFlow({
    operation: "start_mission_execution",
    invoke: useCallback(async () => {
      const args = lastStartArgsRef.current;
      if (!args) throw new Error("startMissionFlow.invoke called without prior args");
      await commands.startMissionExecution({
        mission_id: args.missionId,
        repo_path: args.repoPath,
      });
      return args;
    }, []),
    onSuccess: useCallback(
      (args: { missionId: string; repoPath: string }) => {
        updateMissionStatus(args.missionId, "running");
        setActiveView("workspace");
      },
      [updateMissionStatus, setActiveView],
    ),
  });

  const handleStartMission = useCallback(
    (repoPath: string) => {
      if (!selectedMissionId) return;
      lastStartArgsRef.current = { missionId: selectedMissionId, repoPath };
      setStartDialogOpen(false);
      startMissionFlow.run();
    },
    [selectedMissionId, startMissionFlow],
  );

  const handleExportMission = useCallback(
    async (missionId: string) => {
      const mission = missions.find((m) => m.id === missionId);
      const defaultName = (mission?.title ?? "mission")
        .replace(/[^a-zA-Z0-9\u4e00-\u9fff_-]/g, "_")
        .substring(0, 60);

      const filePath = await save({
        title: t("exportTemplate"),
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
        setError(formatBackendError(e));
      }
    },
    [missions, setError, t],
  );

  const handleImportMission = useCallback(async () => {
    const selected = await open({
      title: t("importMission"),
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
      setError(formatBackendError(e));
    }
  }, [addMission, selectMission, setError, t]);

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
            .catch((e) => setError(formatBackendError(e)));
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
        case "view_report":
          // FM-12: 跳到 ReportView，store 内部会切 activeView 并设 missionId
          useUiStore.getState().openMissionReport(id);
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
        setError(formatBackendError(e));
      } finally {
        setDeleteDialogOpen(false);
        setDeleteTargetId(null);
      }
    },
    [deleteTargetId, removeMission, setError],
  );

  const lastRestartArgsRef = useRef<{
    missionId: string;
    mode: "full" | "failed_only";
  } | null>(null);
  const restartFlow = useRetryableFlow({
    operation: "restart_mission",
    invoke: useCallback(async () => {
      const args = lastRestartArgsRef.current;
      if (!args) throw new Error("restartFlow.invoke called without prior args");
      // 优先尝试 auto_start 复用上次 repo_path 一键重跑；后端没记 repo_path 时
      // 会返回 auto_started=false，前端再 fallback 到工作区选择对话框。
      const result = await commands.restartMission({
        mission_id: args.missionId,
        mode: args.mode,
        auto_start: true,
      });
      const detail =
        args.missionId === selectedMissionId
          ? await commands.getMissionDetail(args.missionId)
          : null;
      return { args, result, detail };
    }, [selectedMissionId]),
    onSuccess: useCallback(
      ({
        args,
        result,
        detail,
      }: {
        args: { missionId: string; mode: "full" | "failed_only" };
        result: Awaited<ReturnType<typeof commands.restartMission>>;
        detail: Awaited<ReturnType<typeof commands.getMissionDetail>> | null;
      }) => {
        updateMissionStatus(
          args.missionId,
          result.auto_started ? "running" : "planned",
        );
        if (detail) {
          setDetail(detail.tasks, detail.dependencies);
        }
        if (!result.auto_started) {
          setStartDialogOpen(true);
        }
      },
      [updateMissionStatus, setDetail],
    ),
  });

  const handleRestartConfirm = useCallback(() => {
    if (!restartTargetId) return;
    lastRestartArgsRef.current = { missionId: restartTargetId, mode: restartMode };
    setRestartDialogOpen(false);
    setRestartTargetId(null);
    restartFlow.run();
  }, [
    restartTargetId,
    restartMode,
    restartFlow,
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
        <ApiKeyBanner />
        {error && <p className={styles.error}>{error}</p>}
        {planFlow.failureBanner}
        {startPreflightFlow.failureBanner}
        {startMissionFlow.failureBanner}
        {restartFlow.failureBanner}

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
                    {t("cancelPlanning")}
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
              {canStart ? t("startMission") : t("confirmAndStart")}
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

/**
 * MVP onboarding banner：当用户尚未配置 API key 时，在 MissionsView 顶部显眼提示。
 * - apiKeyConfigured=null（启动时还没探测）→ 不显示，避免闪烁
 * - apiKeyConfigured=true → 不显示
 * - apiKeyConfigured=false → 显示，附 "Open Settings" 按钮一键跳转
 *
 * 设计取舍：不做模态弹窗，因为用户可能想先浏览 demo mission；
 * banner 比 modal 干扰小，且 plan/start 会被 backend 自然拒绝（有错误反馈）。
 */
function ApiKeyBanner() {
  const { t } = useTranslation("mission");
  const apiKeyConfigured = useUiStore((s) => s.apiKeyConfigured);
  const setActiveView = useUiStore((s) => s.setActiveView);

  if (apiKeyConfigured !== false) {
    return null;
  }

  return (
    <div className={styles.onboardingBanner}>
      <div className={styles.onboardingText}>
        <strong>{t("apiKeyBannerTitle")}</strong>
        <span> {t("apiKeyBannerBody")}</span>
      </div>
      <Button
        variant="primary"
        size="sm"
        onClick={() => setActiveView("settings")}
      >
        {t("apiKeyBannerCta")}
      </Button>
    </div>
  );
}
