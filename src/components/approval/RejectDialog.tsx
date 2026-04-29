/**
 * FM-14: Reject 备注弹窗。
 *
 * 用户在 ApprovalCard 点击 "Reject" 时弹出，要求输入"为什么"。备注最终以
 * `[user reject] <note>` 的形式被注入到 agent 上下文（参见 backend
 * `inject_agent_note`），所以哪怕只填一行也比留空有用。
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import * as Dialog from "@radix-ui/react-dialog";
import { Button } from "../ui";
import styles from "./RejectDialog.module.css";

interface RejectDialogProps {
  open: boolean;
  approvalTitle: string;
  /** 提交后调用；返回 promise 让按钮显示 loading */
  onConfirm: (note: string) => Promise<void> | void;
  onCancel: () => void;
}

export function RejectDialog({ open, approvalTitle, onConfirm, onCancel }: RejectDialogProps) {
  const { t } = useTranslation("approval");
  const { t: tc } = useTranslation("common");
  const [note, setNote] = useState("");
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (!open) {
      setNote("");
      setSubmitting(false);
    }
  }, [open]);

  const handleConfirm = async () => {
    setSubmitting(true);
    try {
      await onConfirm(note.trim());
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog.Root open={open} onOpenChange={(v) => !v && !submitting && onCancel()}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>{t("rejectDialogTitle")}</Dialog.Title>
          <Dialog.Description className={styles.subtitle}>{approvalTitle}</Dialog.Description>

          <label className={styles.label} htmlFor="reject-note">
            {t("rejectReasonLabel")}
          </label>
          <textarea
            id="reject-note"
            className={styles.textarea}
            value={note}
            onChange={(e) => setNote(e.target.value)}
            placeholder={t("rejectReasonPlaceholder")}
            disabled={submitting}
            autoFocus
          />
          <p className={styles.hint}>{t("rejectReasonHint")}</p>

          <div className={styles.actions}>
            <Button variant="secondary" size="sm" onClick={onCancel} disabled={submitting}>
              {tc("cancel")}
            </Button>
            <Button variant="primary" size="sm" onClick={handleConfirm} disabled={submitting}>
              {submitting ? t("rejecting") : t("reject")}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
