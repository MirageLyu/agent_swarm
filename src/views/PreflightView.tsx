import { useState, useEffect, useCallback, useRef } from "react";
import { commands } from "../ipc/commands";
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
import styles from "./PreflightView.module.css";

export function PreflightView() {
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
  const [signing, setSigning] = useState(false);
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
      }
    });

    return () => { unsub.then((fn) => fn()); };
  }, [missionId]);

  const handleSend = useCallback(
    (text: string) => {
      if (!sessionId || !missionId) return;

      setError(null);
      setMessages((prev) => [...prev, { role: "user", content: text, choices: [] }]);
      setStreaming(true);
      setStreamingText("");

      commands
        .sendPreflightMessage({
          session_id: sessionId,
          message: text,
          mode,
        })
        .catch((e) => {
          setError(String(e));
          setStreaming(false);
        });
    },
    [sessionId, missionId, mode],
  );

  const MODE_LABELS: Record<PreflightMode, string> = {
    scenario_walk: "场景走查",
    devils_advocate: "魔鬼代言人",
    risk_highlighter: "风险标记",
  };

  const MODE_OPENERS: Record<PreflightMode, string> = {
    scenario_walk: "请以场景走查模式继续审视之前的需求讨论，从下一个关键决策点开始提问。",
    devils_advocate: "请以魔鬼代言人的角度重新审视当前需求，找出最关键的一个漏洞或隐含假设。",
    risk_highlighter: "请以风险分析师的角度审视当前需求，找出最高影响的一个技术或安全风险。",
  };

  const handleModeChange = useCallback((newMode: PreflightMode) => {
    if (newMode === mode || !sessionId || !missionId) return;

    setMode(newMode);

    // Insert visual divider as a system message
    const dividerMsg: PreflightMessageInfo = {
      role: "assistant",
      content: `── 切换到「${MODE_LABELS[newMode]}」模式 ──`,
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
        setError(String(e));
        setStreaming(false);
      });
  }, [mode, sessionId, missionId]);

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

  const handleSign = useCallback(async () => {
    if (!missionId) return;
    setSigning(true);
    setError(null);

    try {
      const result = await commands.signContract(missionId);
      const detail = await commands.getMissionDetail(result.mission_id);
      addMission(detail.mission);
      selectMission(result.mission_id);
      setDetail(detail.tasks, detail.dependencies);
      setActivePreflight(null, null);
      setActiveView("missions");
    } catch (e) {
      setError(String(e));
    } finally {
      setSigning(false);
    }
  }, [missionId, addMission, selectMission, setDetail, setActivePreflight, setActiveView]);

  if (!missionId || !contract) {
    return (
      <div className={styles.container}>
        <div className={styles.errorBanner}>
          No active pre-flight session. Please start one from the Missions view.
        </div>
      </div>
    );
  }

  return (
    <div className={styles.container}>
      {error && (
        <div className={styles.errorBanner}>
          <span>{error}</span>
          <button className={styles.errorClose} onClick={() => setError(null)} title="关闭">×</button>
        </div>
      )}
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
        />
        <ContractPanel
          contract={contract}
          onRemoveItem={handleRemoveItem}
          onUpdateConfig={handleUpdateConfig}
          onSign={handleSign}
          signing={signing}
        />
      </div>
      <PreflightStatusBar
        convergenceScore={convergenceScore}
        phase={phase}
        messageCount={messages.length}
      />
    </div>
  );
}
