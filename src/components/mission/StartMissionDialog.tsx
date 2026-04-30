import { useState, useEffect, useCallback, type WheelEvent } from "react";
import { useTranslation } from "react-i18next";
import * as Dialog from "@radix-ui/react-dialog";
import { open } from "@tauri-apps/plugin-dialog";
import { commands } from "../../ipc/commands";
import { formatBackendError } from "../../i18n/format-error";
import { Button } from "../ui";
import styles from "./StartMissionDialog.module.css";

interface StartMissionDialogProps {
  open: boolean;
  missionId: string;
  onClose: () => void;
  onStart: (repoPath: string) => void;
}

export function StartMissionDialog({
  open: isOpen,
  missionId,
  onClose,
  onStart,
}: StartMissionDialogProps) {
  const { t } = useTranslation("mission");
  const { t: tc } = useTranslation("common");
  const [workspacePath, setWorkspacePath] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (isOpen && missionId) {
      setError(null);
      setLoading(false);
      commands
        .getDefaultWorkspacePath(missionId)
        .then((res) => setWorkspacePath(res.path))
        .catch(() => setWorkspacePath(""));
    }
  }, [isOpen, missionId]);

  const handleBrowse = async () => {
    const selected = await open({
      directory: true,
      title: t("startDialog.browseDialogTitle"),
    });
    if (selected) {
      setWorkspacePath(selected);
      setError(null);
    }
  };

  const handlePathWheel = useCallback((e: WheelEvent<HTMLInputElement>) => {
    const el = e.currentTarget;
    const delta = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
    el.scrollLeft += delta;
  }, []);

  const handleStart = async () => {
    if (!workspacePath.trim()) return;
    setLoading(true);
    setError(null);
    try {
      onStart(workspacePath.trim());
    } catch (e) {
      setError(formatBackendError(e));
      setLoading(false);
    }
  };

  return (
    <Dialog.Root open={isOpen} onOpenChange={(v) => !v && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>{t("startDialog.title")}</Dialog.Title>
          <p className={styles.subtitle}>
            {t("startDialog.subtitle")}
          </p>

          <div className={styles.section}>
            <label className={styles.label}>{t("startDialog.workspacePath")}</label>
            <div className={styles.pathRow}>
              <input
                className={styles.pathInput}
                value={workspacePath}
                onChange={(e) => {
                  setWorkspacePath(e.target.value);
                  setError(null);
                }}
                onWheel={handlePathWheel}
                placeholder="~/miragenty-workspaces/..."
              />
              <Button variant="secondary" size="sm" onClick={handleBrowse}>
                {t("startDialog.browse")}
              </Button>
            </div>
            <p className={styles.hint}>
              {t("startDialog.hint")}
            </p>
          </div>

          {error && <p className={styles.errorMsg}>{error}</p>}

          <div className={styles.actions}>
            <Button variant="ghost" size="sm" onClick={onClose}>
              {tc("cancel")}
            </Button>
            <Button
              variant="primary"
              size="md"
              onClick={handleStart}
              disabled={!workspacePath.trim() || loading}
            >
              {loading ? t("startDialog.starting") : t("startDialog.start")}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
