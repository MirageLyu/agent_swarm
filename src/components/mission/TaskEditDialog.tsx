import { useState, useEffect } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import type { TaskInfo, Complexity, DependencyInfo } from "../../ipc/commands";
import { Button } from "../ui";
import styles from "./TaskEditDialog.module.css";

interface TaskEditDialogProps {
  task: TaskInfo | null;
  open: boolean;
  onClose: () => void;
  onSave: (taskId: string, title: string, description: string, dependsOn: string[]) => void;
  allTasks?: TaskInfo[];
  dependencies?: DependencyInfo[];
}

export function TaskEditDialog({
  task,
  open,
  onClose,
  onSave,
  allTasks = [],
  dependencies = [],
}: TaskEditDialogProps) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [selectedDeps, setSelectedDeps] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (task) {
      setTitle(task.title);
      setDescription(task.description);
      const currentDeps = dependencies
        .filter((d) => d.task_id === task.id)
        .map((d) => d.depends_on);
      setSelectedDeps(new Set(currentDeps));
    }
  }, [task, dependencies]);

  const toggleDep = (id: string) => {
    setSelectedDeps((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const handleSave = () => {
    if (!task || !title.trim()) return;
    onSave(task.id, title.trim(), description.trim(), [...selectedDeps]);
    onClose();
  };

  const otherTasks = allTasks.filter((t) => t.id !== task?.id);

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
          {otherTasks.length > 0 && (
            <div className={styles.field}>
              <label className={styles.label}>Depends on</label>
              <div className={styles.depList}>
                {otherTasks.map((t) => (
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
            <label className={styles.label}>
              Complexity
              <span className={styles.hintWrap}>
                <span className={styles.hint}>?</span>
                <span className={styles.hintTip}>仅供展示，不影响调度与执行流程</span>
              </span>
            </label>
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
