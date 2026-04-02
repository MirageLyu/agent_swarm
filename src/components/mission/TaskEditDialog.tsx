import { useState, useEffect } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import type { TaskInfo, Complexity } from "../../ipc/commands";
import { Button } from "../ui";
import styles from "./TaskEditDialog.module.css";

interface TaskEditDialogProps {
  task: TaskInfo | null;
  open: boolean;
  onClose: () => void;
  onSave: (taskId: string, title: string, description: string) => void;
}

export function TaskEditDialog({
  task,
  open,
  onClose,
  onSave,
}: TaskEditDialogProps) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");

  useEffect(() => {
    if (task) {
      setTitle(task.title);
      setDescription(task.description);
    }
  }, [task]);

  const handleSave = () => {
    if (!task || !title.trim()) return;
    onSave(task.id, title.trim(), description.trim());
    onClose();
  };

  return (
    <Dialog.Root open={open} onOpenChange={(v) => !v && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>Edit Task</Dialog.Title>
          <div className={styles.field}>
            <label className={styles.label}>Title</label>
            <input
              className={styles.input}
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              autoFocus
            />
          </div>
          <div className={styles.field}>
            <label className={styles.label}>Description</label>
            <textarea
              className={styles.textarea}
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={4}
            />
          </div>
          <div className={styles.actions}>
            <Button variant="ghost" size="sm" onClick={onClose}>
              Cancel
            </Button>
            <Button
              variant="primary"
              size="sm"
              onClick={handleSave}
              disabled={!title.trim()}
            >
              Save
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

// --- Add Task dialog ---

interface AddTaskDialogProps {
  open: boolean;
  onClose: () => void;
  onAdd: (title: string, description: string, complexity: Complexity, dependsOn: string[]) => void;
  existingTasks: TaskInfo[];
}

export function AddTaskDialog({
  open,
  onClose,
  onAdd,
  existingTasks,
}: AddTaskDialogProps) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [complexity, setComplexity] = useState<Complexity>("medium");
  const [selectedDeps, setSelectedDeps] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (open) {
      setTitle("");
      setDescription("");
      setComplexity("medium");
      setSelectedDeps(new Set());
    }
  }, [open]);

  const toggleDep = (id: string) => {
    setSelectedDeps((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const handleAdd = () => {
    if (!title.trim()) return;
    onAdd(title.trim(), description.trim(), complexity, [...selectedDeps]);
    onClose();
  };

  return (
    <Dialog.Root open={open} onOpenChange={(v) => !v && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className={styles.overlay} />
        <Dialog.Content className={styles.content}>
          <Dialog.Title className={styles.title}>Add Task</Dialog.Title>
          <div className={styles.field}>
            <label className={styles.label}>Title</label>
            <input
              className={styles.input}
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              autoFocus
            />
          </div>
          <div className={styles.field}>
            <label className={styles.label}>Description</label>
            <textarea
              className={styles.textarea}
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={3}
            />
          </div>
          <div className={styles.field}>
            <label className={styles.label}>Complexity</label>
            <div className={styles.complexityGroup}>
              {(["low", "medium", "high"] as Complexity[]).map((c) => (
                <button
                  key={c}
                  className={`${styles.complexityBtn} ${complexity === c ? styles.complexityActive : ""}`}
                  data-complexity={c}
                  onClick={() => setComplexity(c)}
                  type="button"
                >
                  {c}
                </button>
              ))}
            </div>
          </div>
          {existingTasks.length > 0 && (
            <div className={styles.field}>
              <label className={styles.label}>Depends on</label>
              <div className={styles.depList}>
                {existingTasks.map((t) => (
                  <label key={t.id} className={styles.depItem}>
                    <input
                      type="checkbox"
                      checked={selectedDeps.has(t.id)}
                      onChange={() => toggleDep(t.id)}
                    />
                    <span>{t.title}</span>
                  </label>
                ))}
              </div>
            </div>
          )}
          <div className={styles.actions}>
            <Button variant="ghost" size="sm" onClick={onClose}>
              Cancel
            </Button>
            <Button
              variant="primary"
              size="sm"
              onClick={handleAdd}
              disabled={!title.trim()}
            >
              Add
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
