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
  const isFullRestart = mode === "full";

  return (
    <Dialog.Root open={open} onOpenChange={(v) => !v && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>
            {isFullRestart ? "Re-run Mission (Full)" : "Re-run Failed Tasks"}
          </Dialog.Title>
          <Dialog.Description className={styles.description}>
            {isFullRestart ? (
              <>
                This will reset <strong>all {totalCount} tasks</strong> in{" "}
                <strong>{missionTitle}</strong> and delete associated agent data.
              </>
            ) : (
              <>
                This will reset <strong>{failedCount} failed task(s)</strong> in{" "}
                <strong>{missionTitle}</strong>. Completed tasks will be preserved.
              </>
            )}
          </Dialog.Description>
          <p className={styles.info}>
            After restart, you will need to select a workspace and click Start.
          </p>
          <div className={styles.actions}>
            <Button variant="secondary" size="sm" onClick={onClose}>
              Cancel
            </Button>
            <Button variant="primary" size="sm" onClick={onConfirm}>
              {isFullRestart ? "Re-run All" : "Re-run Failed"}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
