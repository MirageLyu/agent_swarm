import { memo, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import { commands } from "../../ipc";
import { formatBackendError } from "../../i18n/format-error";
import type { AgentNoteRecord, NoteStatus } from "../../ipc/commands";
import styles from "./InterventionPanel.module.css";

interface InterventionPanelProps {
  agentId: string;
  agentStatus: string;
}

const STATUS_BADGE: Record<NoteStatus, "info" | "success" | "default"> = {
  queued: "info",
  applied: "success",
  expired: "default",
};

function formatTime(ts: string): string {
  const d = new Date(ts);
  if (isNaN(d.getTime())) return ts;
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

export const InterventionPanel = memo(function InterventionPanel({
  agentId,
  agentStatus,
}: InterventionPanelProps) {
  const { t } = useTranslation("workspace");
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notes, setNotes] = useState<AgentNoteRecord[]>([]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const isRunning = agentStatus === "running";

  const fetchNotes = useCallback(() => {
    commands.listAgentNotes(agentId).then(setNotes).catch(() => {});
  }, [agentId]);

  useEffect(() => {
    fetchNotes();
  }, [fetchNotes]);

  // Refresh notes periodically while agent is running
  useEffect(() => {
    if (!isRunning) return;
    const interval = setInterval(fetchNotes, 3000);
    return () => clearInterval(interval);
  }, [isRunning, fetchNotes]);

  const handleSend = useCallback(async () => {
    const text = draft.trim();
    if (!text) return;

    setSending(true);
    setError(null);

    try {
      await commands.injectAgentNote({ agent_id: agentId, note: text });
      setDraft("");
      fetchNotes();
      textareaRef.current?.focus();
    } catch (e) {
      setError(formatBackendError(e));
    } finally {
      setSending(false);
    }
  }, [draft, agentId, fetchNotes]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Enter" && e.metaKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [handleSend],
  );

  return (
    <div className={styles.panel}>
      {notes.length > 0 && (
        <>
          <span className={styles.sectionLabel}>{t("intervention.notes")}</span>
          <div className={styles.noteList}>
            {notes.map((n) => (
              <div key={n.id} className={styles.noteItem}>
                <Badge variant={STATUS_BADGE[n.status]}>{n.status}</Badge>
                {n.mission_id && (
                  <span className={styles.scopeBadge}>{t("intervention.missionScope")}</span>
                )}
                <span className={styles.noteContent} title={n.content}>
                  {n.content}
                </span>
                <span className={styles.noteTime}>{formatTime(n.created_at)}</span>
              </div>
            ))}
          </div>
        </>
      )}

      <div className={styles.inputRow}>
        <textarea
          ref={textareaRef}
          className={styles.textarea}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={isRunning ? t("intervention.sendPlaceholder") : t("intervention.agentNotRunning")}
          disabled={!isRunning || sending}
          rows={1}
        />
        <Button
          variant="primary"
          size="sm"
          onClick={handleSend}
          disabled={!isRunning || sending || !draft.trim()}
        >
          {sending ? t("intervention.sending") : t("intervention.send")}
        </Button>
      </div>

      {error && <span className={styles.error}>{error}</span>}
    </div>
  );
});
