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
            描述你的任务目标，选择启动方式。
          </p>

          <textarea
            className={styles.textarea}
            value={text}
            onChange={(e) => setText(e.target.value.slice(0, MAX_CHARS))}
            onKeyDown={handleKeyDown}
            placeholder="e.g. 实现用户认证系统，包含注册、登录和密码重置"
            rows={5}
            autoFocus
          />

          <div className={styles.modeSection}>
            <div className={styles.modeLabel}>选择启动方式</div>
            <div className={styles.modeCards}>
              {onPreflight && (
                <button
                  className={`${styles.modeCard} ${styles.preflightCard}`}
                  onClick={handlePreflight}
                  disabled={!canSubmit}
                >
                  <div className={styles.cardBadge}>推荐</div>
                  <div className={styles.cardIcon}>💬</div>
                  <div className={styles.cardTitle}>Pre-flight 澄清</div>
                  <div className={styles.cardDesc}>
                    与 AI 多轮对话，逐步澄清需求边界、排除歧义，生成结构化 Contract 后再规划
                  </div>
                  <div className={styles.cardMeta}>约 3-5 分钟 · 更高质量</div>
                </button>
              )}
              <button
                className={`${styles.modeCard} ${styles.quickCard}`}
                onClick={handleSubmit}
                disabled={!canSubmit}
              >
                <div className={styles.cardIcon}>⚡</div>
                <div className={styles.cardTitle}>Quick Plan</div>
                <div className={styles.cardDesc}>
                  AI 直接分解为 Task DAG，跳过需求澄清，适合目标已明确的简单任务
                </div>
                <div className={styles.cardMeta}>约 10 秒 · 快速启动</div>
              </button>
            </div>
          </div>

          <div className={styles.footer}>
            <span className={styles.charCount}>
              {text.length}/{MAX_CHARS}
            </span>
            <Button variant="ghost" size="sm" onClick={() => handleOpenChange(false)}>
              取消
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
