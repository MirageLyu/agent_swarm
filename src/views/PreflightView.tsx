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
} from "../ipc/commands";
import { onPreflightStream, type PreflightStreamPayload } from "../ipc/events";
import { useUiStore } from "../stores/ui-store";
import { useTaskStore } from "../stores/task-store";
import { PreflightChat } from "../components/preflight/PreflightChat";
import { ContractPanel } from "../components/preflight/ContractPanel";
import { PreflightStatusBar } from "../components/preflight/PreflightStatusBar";
import { PlannerLoopPanel } from "../components/mission";
import { useRetryableFlow } from "../hooks/useRetryableFlow";
import styles from "./PreflightView.module.css";

export function PreflightView() {
  const { t } = useTranslation("preflight");
  const { t: tc } = useTranslation("common");
  const missionId = useUiStore((s) => s.activePreflightMissionId);
  const sessionId = useUiStore((s) => s.activePreflightSessionId);
  const setActivePreflight = useUiStore((s) => s.setActivePreflight);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const { addMission, selectMission, setDetail } = useTaskStore();

  const [messages, setMessages] = useState<PreflightMessageInfo[]>([]);
  const [mode, setMode] = useState<PreflightMode>("scenario_walk");
  const [streaming, setStreaming] = useState(false);
  const [streamingText, setStreamingText] = useState("");
  const [contract, setContract] = useState<ContractInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [initialLoading, setInitialLoading] = useState(true);
  const [convergenceScore, setConvergenceScore] = useState(0);
  const [phase, setPhase] = useState<ConversationPhase>("exploring");
  const [statusText, setStatusText] = useState("");

  const sessionIdRef = useRef(sessionId);
  sessionIdRef.current = sessionId;

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
        setStreaming(true);
        setStreamingText("");
        setStatusText("");
        setInitialLoading(false);
      } else if (kind === "text_delta") {
        setStreamingText((prev) => prev + content);
        setStatusText("");
        setInitialLoading(false);
      } else if (kind === "status") {
        setStatusText(content);
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
        setStatusText("");
        setInitialLoading(false);
        try {
          const parsed = JSON.parse(content);
          setMessages((prev) => [
            ...prev,
            {
              role: "assistant",
              content: parsed.text,
              choices: parsed.choices ?? [],
              mode: parsed.mode ?? undefined,
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
        setInitialLoading(false);
        setError(content);
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
  }, [missionId]);

  const handleSend = useCallback(
    (text: string) => {
      if (!sessionId || !missionId) return;

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
    [sessionId, missionId, mode],
  );

  // 重试最后一条失败的输入：复用 stored_msgs，无需前端重发文本，
  // 也不再 push 新的 user 气泡。后端 retry_preflight_message 会清掉
  // failed 标记并重投同一条消息给 LLM。
  const handleRetry = useCallback(() => {
    if (!sessionId || !missionId) return;
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
  }, [sessionId, missionId, mode]);

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
  }, [mode, sessionId, missionId, MODE_LABELS, MODE_OPENERS, t]);

  const handleChoiceSelect = useCallback(
    (choice: PreflightChoice) => {
      if (!missionId) return;

      // Send choice label as user message
      handleSend(choice.label);

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
  const signFlow = useRetryableFlow({
    operation: "sign_contract",
    invoke: useCallback(async () => {
      if (!missionId) throw new Error("noActiveSession");
      const result = await commands.signContract(missionId);
      const detail = await commands.getMissionDetail(result.mission_id);
      return { result, detail };
    }, [missionId]),
    onSuccess: useCallback(
      ({ result, detail }: {
        result: Awaited<ReturnType<typeof commands.signContract>>;
        detail: Awaited<ReturnType<typeof commands.getMissionDetail>>;
      }) => {
        addMission(detail.mission);
        selectMission(result.mission_id);
        setDetail(detail.tasks, detail.dependencies);
        setActivePreflight(null, null);
        setActiveView("missions");
      },
      [addMission, selectMission, setDetail, setActivePreflight, setActiveView],
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
      {signing && (
        // Issue 4: 用 floating 模式，避免下方 PlannerLoopPanel 把上面的对话/合同
        // 面板挤成滚动条。Portal 到 body，固定右下角，可折叠。
        <PlannerLoopPanel label={t("plannerLabel")} isLive floating />
      )}
      <PreflightStatusBar
        convergenceScore={convergenceScore}
        phase={phase}
        messageCount={messages.length}
      />
    </div>
  );
}
