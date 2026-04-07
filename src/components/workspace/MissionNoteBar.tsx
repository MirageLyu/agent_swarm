import { memo, useCallback, useRef, useState } from "react";
import { Button } from "../ui/Button";
import { commands } from "../../ipc";
import styles from "./MissionNoteBar.module.css";

interface MissionNoteBarProps {
  missionId: string;
  hasRunningAgents: boolean;
}

export const MissionNoteBar = memo(function MissionNoteBar({
  missionId,
  hasRunningAgents,
}: MissionNoteBarProps) {
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
      setFeedback(`Sent to ${result.agent_count} agent${result.agent_count !== 1 ? "s" : ""}`);
      inputRef.current?.focus();
      setTimeout(() => setFeedback(null), 3000);
    } catch (e) {
      setFeedback(String(e));
    } finally {
      setSending(false);
    }
  }, [draft, missionId]);

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
            ? "Broadcast a note to all running agents… (⌘↵)"
            : "No running agents"
        }
        disabled={!hasRunningAgents || sending}
      />
      <Button
        variant="primary"
        size="sm"
        onClick={handleSend}
        disabled={!hasRunningAgents || sending || !draft.trim()}
      >
        {sending ? "Sending…" : "Broadcast"}
      </Button>
      {feedback && (
        <span className={styles.feedback}>{feedback}</span>
      )}
    </div>
  );
});
