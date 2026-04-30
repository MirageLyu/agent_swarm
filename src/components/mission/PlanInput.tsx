import { useState, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui";
import styles from "./PlanInput.module.css";

interface PlanInputProps {
  onPlan: (description: string) => void;
  onCancel?: () => void;
  loading: boolean;
}

const MAX_CHARS = 2000;

export function PlanInput({ onPlan, onCancel, loading }: PlanInputProps) {
  const { t } = useTranslation("mission");
  const { t: tc } = useTranslation("common");
  const [text, setText] = useState("");

  const canSubmit = text.trim().length > 0 && !loading;

  const handleSubmit = useCallback(() => {
    if (!canSubmit) return;
    onPlan(text.trim());
  }, [text, canSubmit, onPlan]);

  const handleCancel = useCallback(() => {
    onCancel?.();
  }, [onCancel]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      handleSubmit();
    }
  };

  return (
    <div className={styles.container}>
      <textarea
        className={styles.textarea}
        value={text}
        onChange={(e) => setText(e.target.value.slice(0, MAX_CHARS))}
        onKeyDown={handleKeyDown}
        placeholder={t("planInput.placeholder")}
        disabled={loading}
        rows={3}
      />
      <div className={styles.footer}>
        <span className={styles.charCount}>
          {text.length}/{MAX_CHARS}
        </span>
        <div className={styles.actions}>
          <span className={styles.hint}>
            <kbd className={styles.kbd}>{navigator.platform?.includes("Mac") ? "\u2318" : "Ctrl"}</kbd>
            <kbd className={styles.kbd}>Enter</kbd>
          </span>
          {loading ? (
            <Button variant="secondary" size="sm" onClick={handleCancel}>
              {tc("cancel")}
            </Button>
          ) : null}
          <Button
            variant="primary"
            size="sm"
            onClick={handleSubmit}
            disabled={!canSubmit}
          >
            {loading ? t("planInput.planning") : t("planInput.planMission")}
          </Button>
        </div>
      </div>
    </div>
  );
}
