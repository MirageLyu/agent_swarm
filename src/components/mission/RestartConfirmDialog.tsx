import { useTranslation, Trans } from "react-i18next";
import * as Dialog from "@radix-ui/react-dialog";
import { Button } from "../ui";
import styles from "./ConfirmDialog.module.css";

interface RestartConfirmDialogProps {
  open: boolean;
  missionTitle: string;
  mode: "full" | "failed_only";
  failedCount?: number;
  totalCount?: number;
  onClose: () => void;
  onConfirm: () => void;
}

export function RestartConfirmDialog({
  open,
  missionTitle,
  mode,
  failedCount,
  totalCount,
  onClose,
  onConfirm,
}: RestartConfirmDialogProps) {
  const { t } = useTranslation("mission");
  const { t: tc } = useTranslation("common");
  const isFullRestart = mode === "full";

  return (
    <Dialog.Root open={open} onOpenChange={(v) => !v && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>
            {isFullRestart ? t("restartFullTitle") : t("restartFailedTitle")}
          </Dialog.Title>
          <Dialog.Description className={styles.description}>
            {isFullRestart ? (
              <Trans
                i18nKey="mission:restartFullDescBody"
                values={{ total: totalCount ?? 0, name: missionTitle }}
                components={{ strong: <strong /> }}
              />
            ) : (
              <Trans
                i18nKey="mission:restartFailedDescBody"
                values={{ count: failedCount ?? 0, name: missionTitle }}
                components={{ strong: <strong /> }}
              />
            )}
          </Dialog.Description>
          <p className={styles.info}>{t("restartHint")}</p>
          <div className={styles.actions}>
            <Button variant="secondary" size="sm" onClick={onClose}>
              {tc("cancel")}
            </Button>
            <Button variant="primary" size="sm" onClick={onConfirm}>
              {isFullRestart ? t("restartAllBtn") : t("restartFailedBtn")}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
