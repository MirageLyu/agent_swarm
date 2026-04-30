import { memo, useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { commands } from "../../ipc";
import { formatBackendError } from "../../i18n/format-error";
import styles from "./MissionNoteBar.module.css";

interface MissionNoteBarProps {
  missionId: string;
  hasRunningAgents: boolean;
}

export const MissionNoteBar = memo(function MissionNoteBar({
  missionId,
  hasRunningAgents,
}: MissionNoteBarProps) {
  const { t } = useTranslation("workspace");
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [feedback, setFeedback] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const handleSend = useCallback(async () => {
    const text = draft.trim();
    if (!text) return;

    setSending(true);
    setFeedback(null);

    try {
      const result = await commands.injectMissionNote({
        mission_id: missionId,
        note: text,
      });
      setDraft("");
      setFeedback(t("noteBar.sentSummary", { count: result.agent_count }));
      inputRef.current?.focus();
      setTimeout(() => setFeedback(null), 3000);
    } catch (e) {
      setFeedback(formatBackendError(e));
    } finally {
      setSending(false);
    }
  }, [draft, missionId, t]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter" && e.metaKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [handleSend],
  );

  return (
    <div className={styles.bar}>
      <input
        ref={inputRef}
        className={styles.input}
        type="text"
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder={
          hasRunningAgents
            ? t("noteBar.broadcastPlaceholder")
            : t("noteBar.noRunningAgents")
        }
        disabled={!hasRunningAgents || sending}
      />
      <Button
        variant="primary"
        size="sm"
        onClick={handleSend}
        disabled={!hasRunningAgents || sending || !draft.trim()}
      >
        {sending ? t("noteBar.sending") : t("noteBar.broadcast")}
      </Button>
      {feedback && (
        <span className={styles.feedback}>{feedback}</span>
      )}
    </div>
  );
});
