import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "../ipc/commands";
import { formatBackendError } from "../i18n/format-error";
import type {
  ContractInfo,
  ContractItemInfo,
  PreflightMessageInfo,
  PreflightMode,
  PreflightChoice,
  ConversationPhase,
  PreflightPerfSummary,
} from "../ipc/commands";
import { onPreflightStream, type PreflightStreamPayload } from "../ipc/events";
import { useUiStore } from "../stores/ui-store";
import { useTaskStore } from "../stores/task-store";
import { usePlannerProgressStore } from "../stores/planner-progress-store";
import { PreflightChat } from "../components/preflight/PreflightChat";
import { ContractPanel } from "../components/preflight/ContractPanel";
import { PreflightStatusBar } from "../components/preflight/PreflightStatusBar";
import { useRetryableFlow } from "../hooks/useRetryableFlow";
import {
  finishPreflightUiTurn,
  formatPreflightPerfLog,
  markPreflightFirstVisible,
  startPreflightUiTurn,
  type PreflightTurnSource,
  type PreflightUiTurnState,
} from "../utils/preflight-perf";
import styles from "./PreflightView.module.css";

export function PreflightView() {
  const { t } = useTranslation("preflight");
  const { t: tc } = useTranslation("common");
  const missionId = useUiStore((s) => s.activePreflightMissionId);
  const sessionId = useUiStore((s) => s.activePreflightSessionId);
  const setActivePreflight = useUiStore((s) => s.setActivePreflight);
  const { addMission, setDetail } = useTaskStore();

  const [messages, setMessages] = useState<PreflightMessageInfo[]>([]);
  const [mode, setMode] = useState<PreflightMode>("scenario_walk");
  const [streaming, setStreaming] = useState(false);
  const [streamingText, setStreamingText] = useState("");
  const [streamingReasoning, setStreamingReasoning] = useState("");
  const [contract, setContract] = useState<ContractInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [initialLoading, setInitialLoading] = useState(true);
  const [convergenceScore, setConvergenceScore] = useState(0);
  const [phase, setPhase] = useState<ConversationPhase>("exploring");
  const [statusText, setStatusText] = useState("");

  const sessionIdRef = useRef(sessionId);
  sessionIdRef.current = sessionId;

  const uiTurnRef = useRef<PreflightUiTurnState | null>(null);

  const startUiTurn = useCallback((source: PreflightTurnSource) => {
    uiTurnRef.current = startPreflightUiTurn(source);
  }, []);

  const markFirstVisible = useCallback(() => {
    if (!uiTurnRef.current) return;
    uiTurnRef.current = markPreflightFirstVisible(uiTurnRef.current);
  }, []);

  const finishUiTurn = useCallback((backendPerf: PreflightPerfSummary | null) => {
    if (!uiTurnRef.current) return;
    const summary = finishPreflightUiTurn(uiTurnRef.current, backendPerf);
    uiTurnRef.current = null;
    if (import.meta.env.DEV) {
      console.info("[preflight ui perf]", formatPreflightPerfLog(summary));
    }
  }, []);

  // Load existing session + contract on mount
  useEffect(() => {
    if (!missionId) return;

    commands.getContract(missionId).then(setContract).catch(console.error);

    commands.getPreflightSession(missionId).then((session) => {
      if (session) {
        setMessages(session.messages);
        setMode(session.mode);
        setConvergenceScore(session.convergence_score ?? 0);
        setPhase(session.phase ?? "exploring");
        if (session.messages.length > 0) setInitialLoading(false);
        if (!sessionId) {
          setActivePreflight(missionId, session.id);
        }
      }
    }).catch(console.error);
  }, [missionId, sessionId, setActivePreflight]);

  // Subscribe to preflight stream
  useEffect(() => {
    const unsub = onPreflightStream((payload: PreflightStreamPayload) => {
      if (sessionIdRef.current && payload.session_id !== sessionIdRef.current) return;

      const { kind, content } = payload.chunk;
      if (kind === "start") {
        if (!uiTurnRef.current) {
          startUiTurn("initial");
        }
        setStreaming(true);
        setStreamingText("");
        setStatusText("");
        setInitialLoading(false);
      } else if (kind === "text_delta") {
        markFirstVisible();
        setStreamingText((prev) => prev + content);
        setStatusText("");
        setInitialLoading(false);
      } else if (kind === "reasoning_delta") {
        markFirstVisible();
        setStreamingReasoning((prev) => prev + content);
        setStatusText("");
        setInitialLoading(false);
      } else if (kind === "status") {
        setStatusText(content);
        if (content.trim()) {
          markFirstVisible();
        }
      } else if (kind === "contract_item_added") {
        try {
          const item = JSON.parse(content) as ContractItemInfo;
          setContract((prev) =>
            prev ? { ...prev, items: [...prev.items, { ...item, created_at: new Date().toISOString() }] } : prev,
          );
        } catch { /* ignore parse errors */ }
      } else if (kind === "suggest_sign") {
        // For P0: just log; the LLM's text already suggests signing
        tracing: console.info("[preflight] suggest_sign received:", content);
      } else if (kind === "mode_switched") {
        try {
          const { mode: newMode } = JSON.parse(content);
          setMode(newMode);
        } catch { /* ignore */ }
      } else if (kind === "done") {
        setStreaming(false);
        setStreamingText("");
        setStreamingReasoning("");
        setStatusText("");
        setInitialLoading(false);
        try {
          const parsed = JSON.parse(content);
          const backendPerf = (parsed.perf ?? null) as PreflightPerfSummary | null;
          finishUiTurn(backendPerf);
          setMessages((prev) => [
            ...prev,
            {
              role: "assistant",
              content: parsed.text,
              choices: parsed.choices ?? [],
              mode: parsed.mode ?? undefined,
              reasoning: parsed.reasoning ?? undefined,
            },
          ]);
          if (parsed.convergence_score !== undefined) {
            setConvergenceScore(parsed.convergence_score);
          }
          if (parsed.phase) {
            setPhase(parsed.phase);
          }
        } catch {
          if (missionId) {
            commands.getPreflightSession(missionId).then((session) => {
              if (session) {
                setMessages(session.messages);
                setConvergenceScore(session.convergence_score ?? 0);
                setPhase(session.phase ?? "exploring");
              }
            });
          }
        }
      } else if (kind === "error") {
        setStreaming(false);
        setStreamingText("");
        setStreamingReasoning("");
        setInitialLoading(false);
        setError(content);
        finishUiTurn(null);
        // 后端 Fix A：错误退出前已经把 failed 标记写进 stored_msgs。
        // 这里同步刷新一下 messages，让最后一条 user 气泡立刻出现"重试"按钮，
        // 不依赖用户切换页面再回来才看到。
        if (missionId) {
          commands.getPreflightSession(missionId).then((session) => {
            if (session) setMessages(session.messages);
          }).catch(console.error);
        }
      }
    });

    return () => { unsub.then((fn) => fn()); };
  }, [missionId, startUiTurn, markFirstVisible, finishUiTurn]);

  const handleSend = useCallback(
    (text: string, source: PreflightTurnSource = "free_input") => {
      if (!sessionId || !missionId) return;

      startUiTurn(source);
      setError(null);
      // optimistic insert：立刻显示用户消息（带 mode 让 ChatMessage 一致渲染）；
      // 后端 send_preflight_message 会以同样字段持久化，刷新后不漂移。
      setMessages((prev) => [...prev, { role: "user", content: text, choices: [], mode }]);
      setStreaming(true);
      setStreamingText("");

      commands
        .sendPreflightMessage({
          session_id: sessionId,
          message: text,
          mode,
        })
        .catch((e) => {
          setError(formatBackendError(e));
          setStreaming(false);
        });
    },
    [sessionId, missionId, mode, startUiTurn],
  );

  // 重试最后一条失败的输入：复用 stored_msgs，无需前端重发文本，
  // 也不再 push 新的 user 气泡。后端 retry_preflight_message 会清掉
  // failed 标记并重投同一条消息给 LLM。
  const handleRetry = useCallback(() => {
    if (!sessionId || !missionId) return;
    startUiTurn("retry");
    setError(null);
    setStreaming(true);
    setStreamingText("");

    // 同步把 messages 里最后一条 user 上的 failed 标记去掉，
    // 否则要等 done 事件 + getPreflightSession 才会重置，UI 短暂闪一下。
    setMessages((prev) => {
      const idx = [...prev].reverse().findIndex((m) => m.role === "user");
      if (idx < 0) return prev;
      const realIdx = prev.length - 1 - idx;
      const next = [...prev];
      next[realIdx] = { ...next[realIdx], failed: false, error: undefined };
      return next;
    });

    commands
      .retryPreflightMessage({ session_id: sessionId, mode })
      .catch((e) => {
        setError(formatBackendError(e));
        setStreaming(false);
      });
  }, [sessionId, missionId, mode, startUiTurn]);

  const MODE_LABELS = useMemo<Record<PreflightMode, string>>(
    () => ({
      scenario_walk: t("modeLabel.scenario_walk"),
      devils_advocate: t("modeLabel.devils_advocate"),
      risk_highlighter: t("modeLabel.risk_highlighter"),
    }),
    [t],
  );

  const MODE_OPENERS = useMemo<Record<PreflightMode, string>>(
    () => ({
      scenario_walk: t("modeOpener.scenario_walk"),
      devils_advocate: t("modeOpener.devils_advocate"),
      risk_highlighter: t("modeOpener.risk_highlighter"),
    }),
    [t],
  );

  const handleModeChange = useCallback((newMode: PreflightMode) => {
    if (newMode === mode || !sessionId || !missionId) return;

    startUiTurn("mode_switch");
    setMode(newMode);

    // Insert visual divider as a system message
    const dividerMsg: PreflightMessageInfo = {
      role: "assistant",
      content: t("modeSwitchedDivider", { mode: MODE_LABELS[newMode] }),
      choices: [],
    };
    setMessages((prev) => [...prev, dividerMsg]);

    // Auto-send opener to trigger Agent response in new mode
    setStreaming(true);
    setStreamingText("");

    commands
      .sendPreflightMessage({
        session_id: sessionId,
        message: MODE_OPENERS[newMode],
        mode: newMode,
      })
      .catch((e) => {
        setError(formatBackendError(e));
        setStreaming(false);
      });
  }, [mode, sessionId, missionId, MODE_LABELS, MODE_OPENERS, t, startUiTurn]);

  const handleChoiceSelect = useCallback(
    (choice: PreflightChoice) => {
      if (!missionId) return;

      // Send choice label as user message
      handleSend(choice.label, "choice");

      // Auto-add contract item if the choice has impact
      if (choice.contract_impact) {
        commands
          .addContractItem({
            mission_id: missionId,
            section: choice.contract_impact.section,
            text: choice.contract_impact.text,
          })
          .then((item) => {
            setContract((prev) =>
              prev ? { ...prev, items: [...prev.items, item] } : prev,
            );
          })
          .catch(console.error);
      }
    },
    [missionId, handleSend],
  );

  const handleRemoveItem = useCallback(
    (itemId: string) => {
      if (!missionId) return;
      commands
        .removeContractItem({ mission_id: missionId, item_id: itemId })
        .then(() => {
          setContract((prev) =>
            prev
              ? { ...prev, items: prev.items.filter((i) => i.id !== itemId) }
              : prev,
          );
        })
        .catch(console.error);
    },
    [missionId],
  );

  const handleUpdateConfig = useCallback(
    (field: string, value: number) => {
      if (!missionId) return;
      const update: Record<string, number> = { [field]: value };
      commands
        .updateContractConfig({ mission_id: missionId, ...update })
        .then(() => {
          setContract((prev) =>
            prev ? { ...prev, [field]: value } : prev,
          );
        })
        .catch(console.error);
    },
    [missionId],
  );

  // 流程型操作：sign_contract 内部跑 PlannerEngine（可能超时 / LLM 失败）。
  // 走 useRetryableFlow，失败时统一渲染 banner + 重试按钮，避免用户"会话作废"。
  // 详见 .cursor/rules/retryable-flow.mdc。
  //
  // Planner 浮窗已抽到应用根 `<PlannerProgressOverlay>`：进入流程时 setActive，
  // 用户在 sign 期间切到其它 view 也能看见进度，签约成功后浮窗展示"查看任务图"按钮。
  const plannerLabel = t("plannerLabel");
  const setPlannerActive = usePlannerProgressStore((s) => s.setActive);
  const markPlannerCompleted = usePlannerProgressStore((s) => s.markCompleted);
  const clearPlanner = usePlannerProgressStore((s) => s.clear);
  const signFlow = useRetryableFlow({
    operation: "sign_contract",
    invoke: useCallback(async () => {
      if (!missionId) throw new Error("noActiveSession");
      // 进入 sign 流程：先把浮窗显式装上，让用户即便立即切走也看得见。
      // sessionId 现在拿不到（sign_contract 跑完才返回），PlannerLoopPanel 会
      // 从首个 planner-step 事件自动 discover。
      setPlannerActive({
        kind: "sign_contract",
        missionId,
        label: plannerLabel,
        startedAt: Date.now(),
      });
      try {
        const result = await commands.signContract(missionId);
        const detail = await commands.getMissionDetail(result.mission_id);
        return { result, detail };
      } catch (e) {
        // 失败时清掉浮窗，让 useRetryableFlow 的 failure banner 接管错误展示。
        // 用户点重试会重新进入 invoke 又 setActive，行为对称。
        clearPlanner();
        throw e;
      }
    }, [missionId, setPlannerActive, clearPlanner, plannerLabel]),
    onSuccess: useCallback(
      ({ result, detail }: {
        result: Awaited<ReturnType<typeof commands.signContract>>;
        detail: Awaited<ReturnType<typeof commands.getMissionDetail>>;
      }) => {
        addMission(detail.mission);
        setDetail(detail.tasks, detail.dependencies);
        setActivePreflight(null, null);
        // 转入"完成"态：浮窗显示 ✓ 与"查看任务图"按钮，由用户主动决定何时跳转。
        // 不再自动 selectMission + setActiveView —— 让用户先看见浮窗反馈，
        // 避免"突然跳走又不知道为啥"的体验断层。
        markPlannerCompleted({ missionId: result.mission_id, kind: "sign_contract" });
      },
      [addMission, setDetail, setActivePreflight, markPlannerCompleted],
    ),
  });
  const signing = signFlow.state === "running";
  const handleSign = signFlow.run;

  if (!missionId || !contract) {
    return (
      <div className={styles.container}>
        <div className={styles.errorBanner}>
          {t("noActiveSession")}
        </div>
      </div>
    );
  }

  return (
    <div className={styles.container}>
      {error && (
        <div className={styles.errorBanner}>
          <span>{error}</span>
          <button className={styles.errorClose} onClick={() => setError(null)} title={tc("close")}>×</button>
        </div>
      )}
      {signFlow.failureBanner}
      <div className={styles.main}>
        <PreflightChat
          messages={messages}
          mode={mode}
          streaming={streaming}
          streamingText={streamingText}
          streamingReasoning={streamingReasoning}
          statusText={statusText}
          initialLoading={initialLoading}
          onSend={handleSend}
          onModeChange={handleModeChange}
          onChoiceSelect={handleChoiceSelect}
          onRetry={handleRetry}
        />
        <ContractPanel
          contract={contract}
          sessionId={sessionId ?? null}
          onRemoveItem={handleRemoveItem}
          onUpdateConfig={handleUpdateConfig}
          onSign={handleSign}
          signing={signing}
        />
      </div>
      {/* Issue 4 / Issue 1: Planner 浮窗已抽到 App 根 `<PlannerProgressOverlay>`，
          切换 view 也能继续看到进度。本 view 不再渲染浮窗。 */}
      <PreflightStatusBar
        convergenceScore={convergenceScore}
        phase={phase}
        messageCount={messages.length}
      />
    </div>
  );
}
