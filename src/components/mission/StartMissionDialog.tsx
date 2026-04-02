import { useState, useEffect, useCallback, type WheelEvent } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { open } from "@tauri-apps/plugin-dialog";
import { commands } from "../../ipc/commands";
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
      title: "Select Workspace Directory",
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
      setError(String(e));
      setLoading(false);
    }
  };

  return (
    <Dialog.Root open={isOpen} onOpenChange={(v) => !v && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>Start Mission</Dialog.Title>
          <p className={styles.subtitle}>
            Choose a workspace directory for agent execution. A new Git repo will be initialized automatically if needed.
          </p>

          <div className={styles.section}>
            <label className={styles.label}>Workspace Path</label>
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
                Browse
              </Button>
            </div>
            <p className={styles.hint}>
              Directory will be created automatically. Non-git directories will be initialized with git init.
            </p>
          </div>

          {error && <p className={styles.errorMsg}>{error}</p>}

          <div className={styles.actions}>
            <Button variant="ghost" size="sm" onClick={onClose}>
              Cancel
            </Button>
            <Button
              variant="primary"
              size="md"
              onClick={handleStart}
              disabled={!workspacePath.trim() || loading}
            >
              {loading ? "Starting..." : "Start"}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
