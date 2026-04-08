import { useState, useEffect, useCallback, useRef } from "react";
import { commands } from "../ipc/commands";
import type {
  ContractInfo,
  PreflightMessageInfo,
  PreflightMode,
  PreflightChoice,
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
      } else if (kind === "text_delta") {
        setStreamingText((prev) => prev + content);
      } else if (kind === "done") {
        setStreaming(false);
        setStreamingText("");
        // Parse the done payload for the full response
        try {
          const parsed = JSON.parse(content);
          setMessages((prev) => [
            ...prev,
            {
              role: "assistant",
              content: parsed.text,
              choices: parsed.choices ?? [],
            },
          ]);
        } catch {
          // Fallback: reload session from backend
          if (missionId) {
            commands.getPreflightSession(missionId).then((session) => {
              if (session) setMessages(session.messages);
            });
          }
        }
      } else if (kind === "error") {
        setStreaming(false);
        setStreamingText("");
        setError(content);
      }
    });

    return () => { unsub.then((fn) => fn()); };
  }, [missionId]);

  const handleSend = useCallback(
    (text: string) => {
      if (!sessionId || !missionId) return;

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

  const handleModeChange = useCallback((newMode: PreflightMode) => {
    setMode(newMode);
  }, []);

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
      {error && <div className={styles.errorBanner}>{error}</div>}
      <div className={styles.main}>
        <PreflightChat
          messages={messages}
          mode={mode}
          streaming={streaming}
          streamingText={streamingText}
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
      <PreflightStatusBar messageCount={messages.length} />
    </div>
  );
}
