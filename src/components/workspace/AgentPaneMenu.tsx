import { useState, useCallback } from "react";
import { useTranslation } from "react-i18next";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { commands } from "../../ipc";
import type { Agent } from "../../stores/agent-store";
import styles from "./AgentPaneMenu.module.css";

interface AgentPaneMenuProps {
  agent: Agent;
}

export function AgentPaneMenu({ agent }: AgentPaneMenuProps) {
  const { t } = useTranslation("workspace");
  const { t: tc } = useTranslation("common");
  const [noteOpen, setNoteOpen] = useState(false);
  const [noteText, setNoteText] = useState("");
  const [confirmKill, setConfirmKill] = useState(false);

  const isRunning = agent.status === "running";

  const handleSendNote = useCallback(async () => {
    if (!noteText.trim()) return;
    try {
      await commands.injectAgentNote({ agent_id: agent.id, note: noteText.trim() });
      setNoteText("");
      setNoteOpen(false);
    } catch {}
  }, [agent.id, noteText]);

  const handlePause = useCallback(async () => {
    try { await commands.stopAgent(agent.id); } catch {}
  }, [agent.id]);

  const handleKill = useCallback(async () => {
    if (!confirmKill) {
      setConfirmKill(true);
      return;
    }
    try { await commands.stopAgent(agent.id); } catch {}
    setConfirmKill(false);
  }, [agent.id, confirmKill]);

  return (
    <>
      <DropdownMenu.Root onOpenChange={() => setConfirmKill(false)}>
        <DropdownMenu.Trigger asChild>
          <button className={styles.trigger} type="button" title={t("paneMenu.actionsTitle")}>⋮</button>
        </DropdownMenu.Trigger>
        <DropdownMenu.Portal>
          <DropdownMenu.Content className={styles.content} sideOffset={4} align="end">
            <DropdownMenu.Item
              className={styles.item}
              onSelect={() => setNoteOpen(true)}
            >
              {t("paneMenu.sendNote")}
            </DropdownMenu.Item>
            <DropdownMenu.Item
              className={styles.item}
              disabled={!isRunning}
              onSelect={handlePause}
            >
              {t("paneMenu.pause")}
            </DropdownMenu.Item>
            <DropdownMenu.Separator className={styles.separator} />
            <DropdownMenu.Item
              className={`${styles.item} ${styles.danger}`}
              disabled={!isRunning}
              onSelect={handleKill}
            >
              {confirmKill ? t("paneMenu.confirmKill") : t("paneMenu.killRestart")}
            </DropdownMenu.Item>
          </DropdownMenu.Content>
        </DropdownMenu.Portal>
      </DropdownMenu.Root>

      {noteOpen && (
        <div className={styles.noteOverlay} onClick={() => setNoteOpen(false)}>
          <div className={styles.noteDialog} onClick={(e) => e.stopPropagation()}>
            <div className={styles.noteTitle}>
              {t("paneMenu.noteDialogTitle", { name: agent.name })}
            </div>
            <textarea
              className={styles.noteInput}
              value={noteText}
              onChange={(e) => setNoteText(e.target.value)}
              placeholder={t("paneMenu.notePlaceholder")}
              rows={3}
              autoFocus
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) handleSendNote();
              }}
            />
            <div className={styles.noteActions}>
              <button className={styles.noteCancel} onClick={() => setNoteOpen(false)} type="button">
                {tc("cancel")}
              </button>
              <button className={styles.noteSend} onClick={handleSendNote} type="button">
                {t("paneMenu.send")}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
