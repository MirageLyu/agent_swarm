import { useState, useCallback } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Button } from "../ui";
import styles from "./PlanMissionDialog.module.css";

interface PlanMissionDialogProps {
  open: boolean;
  onClose: () => void;
  onPlan: (description: string) => void;
  onPreflight?: (description: string) => void;
}

const MAX_CHARS = 2000;

export function PlanMissionDialog({
  open: isOpen,
  onClose,
  onPlan,
  onPreflight,
}: PlanMissionDialogProps) {
  const [text, setText] = useState("");

  const canSubmit = text.trim().length > 0;

  const handleSubmit = useCallback(() => {
    if (!canSubmit) return;
    const description = text.trim();
    setText("");
    onPlan(description);
  }, [text, canSubmit, onPlan]);

  const handlePreflight = useCallback(() => {
    if (!canSubmit || !onPreflight) return;
    const description = text.trim();
    setText("");
    onPreflight(description);
  }, [text, canSubmit, onPreflight]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      handleSubmit();
    }
  };

  const handleOpenChange = (v: boolean) => {
    if (!v) {
      setText("");
      onClose();
    }
  };

  return (
    <Dialog.Root open={isOpen} onOpenChange={handleOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>New Mission</Dialog.Title>
          <p className={styles.subtitle}>
            描述你的任务目标，AI 将自动分解为可执行的 Task DAG。
          </p>

          <textarea
            className={styles.textarea}
            value={text}
            onChange={(e) => setText(e.target.value.slice(0, MAX_CHARS))}
            onKeyDown={handleKeyDown}
            placeholder="e.g. Build a user authentication system with login, registration, and password reset"
            rows={5}
            autoFocus
          />

          <div className={styles.footer}>
            <span className={styles.charCount}>
              {text.length}/{MAX_CHARS}
            </span>
            <div className={styles.actions}>
              <Button variant="ghost" size="sm" onClick={() => handleOpenChange(false)}>
                Cancel
              </Button>
              {onPreflight && (
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={handlePreflight}
                  disabled={!canSubmit}
                >
                  Pre-flight
                </Button>
              )}
              <Button
                variant="primary"
                size="sm"
                onClick={handleSubmit}
                disabled={!canSubmit}
              >
                Quick Plan
              </Button>
            </div>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
