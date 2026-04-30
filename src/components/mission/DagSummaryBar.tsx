import { useTranslation } from "react-i18next";
import type { TaskInfo, TaskStatus } from "../../ipc/commands";
import styles from "./DagSummaryBar.module.css";

interface DagSummaryBarProps {
  tasks: TaskInfo[];
}

const STATUS_ORDER: TaskStatus[] = ["completed", "running", "pending", "ready", "failed", "cancelled"];

const STATUS_CLASS: Record<string, string> = {
  completed: styles.completed,
  running: styles.running,
  pending: styles.pending,
  ready: styles.pending,
  failed: styles.failed,
  cancelled: styles.failed,
};

export function DagSummaryBar({ tasks }: DagSummaryBarProps) {
  const { t } = useTranslation("mission");
  if (tasks.length === 0) return null;

  const counts: Record<string, number> = {};
  for (const t of tasks) {
    counts[t.status] = (counts[t.status] || 0) + 1;
  }

  const parts: { status: string; count: number }[] = [];
  for (const s of STATUS_ORDER) {
    if (counts[s]) parts.push({ status: s, count: counts[s] });
  }

  return (
    <div className={styles.bar}>
      <span>{t("dagSummary.tasksLabel", { count: tasks.length })}</span>
      <div className={styles.stats}>
        {parts.map((p, i) => (
          <span key={p.status}>
            {i > 0 && <span className={styles.separator}> · </span>}
            <span className={STATUS_CLASS[p.status]}>
              {p.count} {t(`dagSummary.status.${p.status}`, { defaultValue: p.status })}
            </span>
          </span>
        ))}
      </div>
    </div>
  );
}
