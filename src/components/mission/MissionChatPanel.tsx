import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type ChatMessageInfo, type ChatTurnSummary } from "../../ipc/commands";
import { formatBackendError } from "../../i18n/format-error";
import {
  onChatStream,
  onFollowupProposed,
  type ChatStreamPayload,
  type FollowupProposedPayload,
} from "../../ipc/events";
import { Button } from "../ui";
import styles from "./MissionChatPanel.module.css";

interface MissionChatPanelProps {
  missionId: string;
  /** Mission 必须 completed/failed/running 才允许 chat。父层判断后传入 enabled。 */
  enabled: boolean;
  /** 用户确认升级后用此回调拿到子 mission id（让父层做后续 plan_mission 跳转）。 */
  onFollowupCreated?: (childMissionId: string) => void;
}

type PendingProposal = FollowupProposedPayload;

export function MissionChatPanel({ missionId, enabled, onFollowupCreated }: MissionChatPanelProps) {
  const { t } = useTranslation("mission");
  const [history, setHistory] = useState<ChatMessageInfo[]>([]);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [forceDirect, setForceDirect] = useState(false);
  const [streamingByMsg, setStreamingByMsg] = useState<Record<string, string>>({});
  const [activeAssistantMsgId, setActiveAssistantMsgId] = useState<string | null>(null);
  const [pendingProposal, setPendingProposal] = useState<PendingProposal | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  // 加载历史
  useEffect(() => {
    if (!missionId) return;
    let cancelled = false;
    commands
      .listChatMessages(missionId)
      .then((rows) => {
        if (!cancelled) setHistory(rows);
      })
      .catch((err) => {
        if (!cancelled) setError(formatBackendError(err));
      });
    return () => {
      cancelled = true;
    };
  }, [missionId]);

  // 订阅 chat-stream
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    onChatStream((payload: ChatStreamPayload) => {
      if (payload.missionId !== missionId) return;
      setStreamingByMsg((prev) => {
        const next = { ...prev };
        next[payload.messageId] = (next[payload.messageId] ?? "") + payload.content;
        return next;
      });
      setActiveAssistantMsgId(payload.messageId);
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => {});
    return () => {
      if (unlisten) unlisten();
    };
  }, [missionId]);

  // 订阅 followup-proposed
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    onFollowupProposed((payload) => {
      if (payload.missionId !== missionId) return;
      setPendingProposal(payload);
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => {});
    return () => {
      if (unlisten) unlisten();
    };
  }, [missionId]);

  // 滚动到底部
  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [history, streamingByMsg]);

  const refreshHistory = useCallback(async () => {
    try {
      const rows = await commands.listChatMessages(missionId);
      setHistory(rows);
      // 历史里的消息已经定型，清掉对应的流式增量
      setStreamingByMsg((prev) => {
        const next = { ...prev };
        for (const r of rows) delete next[r.id];
        return next;
      });
    } catch (err) {
      setError(formatBackendError(err));
    }
  }, [missionId]);

  const handleSend = useCallback(
    async (overrideDirect?: boolean) => {
      const trimmed = draft.trim();
      if (!trimmed) return;
      setBusy(true);
      setError(null);
      setActiveAssistantMsgId(null);
      try {
        const summary: ChatTurnSummary = await commands.sendChatMessage({
          mission_id: missionId,
          content: trimmed,
          force_direct: overrideDirect ?? forceDirect,
        });
        setDraft("");
        // 用户已发出过一次 force_direct = true 后，重置为 false（下条消息默认重新允许 propose）
        if (overrideDirect ?? forceDirect) setForceDirect(false);
        await refreshHistory();
        if (summary.status === "rejected_oversize") {
          setError(summary.error ?? t("chat.errorOversize"));
        }
      } catch (err) {
        setError(formatBackendError(err));
      } finally {
        setBusy(false);
      }
    },
    [draft, forceDirect, missionId, refreshHistory, t],
  );

  const handleConfirmProposal = useCallback(
    async (proposal: PendingProposal) => {
      setBusy(true);
      try {
        const resp = await commands.confirmFollowupProposal({
          parent_mission_id: missionId,
          title: proposal.title,
          request_summary: proposal.requestSummary,
        });
        setPendingProposal(null);
        await refreshHistory();
        onFollowupCreated?.(resp.child_mission_id);
      } catch (err) {
        setError(formatBackendError(err));
      } finally {
        setBusy(false);
      }
    },
    [missionId, onFollowupCreated, refreshHistory],
  );

  const handleRejectProposal = useCallback(async () => {
    setBusy(true);
    try {
      await commands.rejectFollowupProposal(missionId);
      setPendingProposal(null);
      setForceDirect(true);
      await refreshHistory();
    } catch (err) {
      setError(formatBackendError(err));
    } finally {
      setBusy(false);
    }
  }, [missionId, refreshHistory]);

  const renderedMessages = useMemo(() => {
    return history.map((msg) => {
      const streamed = streamingByMsg[msg.id];
      // streamed 仅作回退兜底（极快返回时 history 已经包含完整内容；偶有竞态时显示流式文本）
      const text = streamed && !msg.content ? streamed : msg.content;
      return { ...msg, displayText: text };
    });
  }, [history, streamingByMsg]);

  // 正在生成但尚未落库的 assistant 消息（active id 不在 history 中）
  const activeStream = useMemo(() => {
    if (!activeAssistantMsgId) return null;
    if (history.some((m) => m.id === activeAssistantMsgId)) return null;
    return streamingByMsg[activeAssistantMsgId] ?? "";
  }, [activeAssistantMsgId, history, streamingByMsg]);

  if (!enabled) {
    return (
      <div className={styles.disabled}>
        {t("chat.disabled")}
      </div>
    );
  }

  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <span className={styles.title}>{t("chat.title")}</span>
        {forceDirect ? (
          <span className={styles.badge}>{t("chat.directMode")}</span>
        ) : null}
      </div>

      <div className={styles.thread} ref={scrollRef}>
        {renderedMessages.length === 0 && !activeStream ? (
          <div className={styles.empty}>
            {t("chat.empty")}
          </div>
        ) : null}

        {renderedMessages.map((msg) => (
          <div
            key={msg.id}
            className={`${styles.bubble} ${
              msg.role === "user"
                ? styles.bubbleUser
                : msg.role === "system"
                  ? styles.bubbleSystem
                  : styles.bubbleAssistant
            }`}
          >
            <div className={styles.bubbleMeta}>
              <span className={styles.role}>{t(`chat.rolePill.${msg.role}`, { defaultValue: msg.role })}</span>
              <span className={styles.timestamp}>{formatTs(msg.created_at)}</span>
            </div>
            <pre className={styles.content}>{msg.displayText}</pre>
            {msg.artifact_refs ? (
              <div className={styles.artifacts}>
                {parseArtifactRefs(msg.artifact_refs).map((p) => (
                  <span key={p} className={styles.artifactChip}>
                    {p}
                  </span>
                ))}
              </div>
            ) : null}
          </div>
        ))}

        {activeStream ? (
          <div className={`${styles.bubble} ${styles.bubbleAssistant} ${styles.streaming}`}>
            <div className={styles.bubbleMeta}>
              <span className={styles.role}>{t("chat.rolePill.assistant")}</span>
              <span className={styles.timestamp}>{t("chat.streaming")}</span>
            </div>
            <pre className={styles.content}>{activeStream}</pre>
          </div>
        ) : null}
      </div>

      {pendingProposal ? (
        <div className={styles.proposalCard}>
          <div className={styles.proposalTitle}>
            {t("chat.proposalTitle")}
          </div>
          <div className={styles.proposalLine}>
            <strong>{t("chat.proposalLabel.title")}:</strong> {pendingProposal.title}
          </div>
          <div className={styles.proposalLine}>
            <strong>{t("chat.proposalLabel.why")}:</strong> {pendingProposal.rationale}
          </div>
          <div className={styles.proposalLine}>
            <strong>{t("chat.proposalLabel.estimatedTasks")}:</strong> {pendingProposal.estimatedTasks}
          </div>
          <div className={styles.proposalActions}>
            <Button
              variant="primary"
              size="sm"
              onClick={() => handleConfirmProposal(pendingProposal)}
              disabled={busy}
            >
              {t("chat.proposalAccept")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              onClick={handleRejectProposal}
              disabled={busy}
            >
              {t("chat.proposalReject")}
            </Button>
          </div>
        </div>
      ) : null}

      {error ? <div className={styles.error}>{error}</div> : null}

      <div className={styles.composer}>
        <textarea
          className={styles.input}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder={
            pendingProposal
              ? t("chat.placeholder.pendingProposal")
              : forceDirect
                ? t("chat.placeholder.forceDirect")
                : t("chat.placeholder.default")
          }
          rows={3}
          disabled={busy || pendingProposal !== null}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              void handleSend();
            }
          }}
        />
        <div className={styles.composerActions}>
          <span className={styles.hint}>{t("chat.hintShortcut")}</span>
          <Button
            variant="primary"
            size="sm"
            onClick={() => handleSend()}
            disabled={busy || draft.trim().length === 0 || pendingProposal !== null}
          >
            {busy ? t("chat.sending") : t("chat.send")}
          </Button>
        </div>
      </div>
    </div>
  );
}

function formatTs(ts: string): string {
  // SQLite UTC 时间到本地短格式
  try {
    const d = new Date(ts.replace(" ", "T") + "Z");
    if (Number.isNaN(d.getTime())) return ts;
    return d.toLocaleTimeString();
  } catch {
    return ts;
  }
}

function parseArtifactRefs(raw: string): string[] {
  try {
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return parsed.filter((x): x is string => typeof x === "string");
  } catch {
    // ignore
  }
  return [];
}
