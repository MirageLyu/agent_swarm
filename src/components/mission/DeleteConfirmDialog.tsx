import { useState } from "react";
import { useTranslation, Trans } from "react-i18next";
import * as Dialog from "@radix-ui/react-dialog";
import { Button } from "../ui";
import styles from "./ConfirmDialog.module.css";

interface DeleteConfirmDialogProps {
  open: boolean;
  missionTitle: string;
  onClose: () => void;
  onConfirm: (cleanWorkspace: boolean) => void;
}

export function DeleteConfirmDialog({
  open,
  missionTitle,
  onClose,
  onConfirm,
}: DeleteConfirmDialogProps) {
  const { t } = useTranslation("mission");
  const { t: tc } = useTranslation("common");
  const [cleanWorkspace, setCleanWorkspace] = useState(true);

  return (
    <Dialog.Root open={open} onOpenChange={(v) => !v && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>{t("deleteConfirmTitle")}</Dialog.Title>
          <Dialog.Description className={styles.description}>
            <Trans
              i18nKey="mission:deleteConfirmBodyWithName"
              values={{ name: missionTitle }}
              components={{ strong: <strong /> }}
            />
          </Dialog.Description>
          <p className={styles.warning}>{t("deleteConfirmBody")}</p>
          <label className={styles.checkLabel}>
            <input
              type="checkbox"
              checked={cleanWorkspace}
              onChange={(e) => setCleanWorkspace(e.target.checked)}
              className={styles.checkbox}
            />
            <span>{t("deleteWithWorkspace")}</span>
          </label>
          <div className={styles.actions}>
            <Button variant="secondary" size="sm" onClick={onClose}>
              {tc("cancel")}
            </Button>
            <Button
              variant="danger"
              size="sm"
              onClick={() => onConfirm(cleanWorkspace)}
            >
              {tc("delete")}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
