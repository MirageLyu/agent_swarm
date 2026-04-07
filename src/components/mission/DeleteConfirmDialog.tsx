import { useState } from "react";
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
  const [cleanWorkspace, setCleanWorkspace] = useState(true);

  return (
    <Dialog.Root open={open} onOpenChange={(v) => !v && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>Delete Mission</Dialog.Title>
          <Dialog.Description className={styles.description}>
            Are you sure you want to delete <strong>{missionTitle}</strong>?
          </Dialog.Description>
          <p className={styles.warning}>This action cannot be undone.</p>
          <label className={styles.checkLabel}>
            <input
              type="checkbox"
              checked={cleanWorkspace}
              onChange={(e) => setCleanWorkspace(e.target.checked)}
              className={styles.checkbox}
            />
            <span>Also clean workspace directory</span>
          </label>
          <div className={styles.actions}>
            <Button variant="secondary" size="sm" onClick={onClose}>
              Cancel
            </Button>
            <Button
              variant="danger"
              size="sm"
              onClick={() => onConfirm(cleanWorkspace)}
            >
              Delete
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
