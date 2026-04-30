import { useTranslation } from "react-i18next";
import type { TaskInfo, DependencyInfo } from "../../ipc/commands";
import styles from "./TaskDetailPanel.module.css";

interface TaskDetailPanelProps {
  task: TaskInfo | null;
  tasks: TaskInfo[];
  dependencies: DependencyInfo[];
  onClose: () => void;
  onFocusTask?: (taskId: string) => void;
}

export function TaskDetailPanel({
  task,
  tasks,
  dependencies,
  onClose,
  onFocusTask,
}: TaskDetailPanelProps) {
  const { t } = useTranslation("mission");
  if (!task) {
    return (
      <div className={styles.panel}>
        <div className={styles.empty}>
          <svg className={styles.emptyIcon} viewBox="0 0 32 32" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round">
            <rect x="4" y="4" width="24" height="24" rx="4" />
            <line x1="10" y1="12" x2="22" y2="12" />
            <line x1="10" y1="17" x2="18" y2="17" />
          </svg>
          <span className={styles.emptyText}>{t("taskDetail.emptyHint")}</span>
        </div>
      </div>
    );
  }

  const upstream = dependencies
    .filter((d) => d.task_id === task.id)
    .map((d) => tasks.find((t) => t.id === d.depends_on))
    .filter(Boolean) as TaskInfo[];

  const downstream = dependencies
    .filter((d) => d.depends_on === task.id)
    .map((d) => tasks.find((t) => t.id === d.task_id))
    .filter(Boolean) as TaskInfo[];

  return (
    <div className={styles.panel}>
      <div className={styles.body}>
        <div className={styles.section}>
          <div className={styles.nameRow}>
            <div className={styles.taskName}>{task.title}</div>
            {onFocusTask && (
              <button
                className={styles.focusBtn}
                onClick={() => onFocusTask(task.id)}
                type="button"
                title={t("taskDetail.focusTitle")}
              >
                <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
                  <circle cx="8" cy="8" r="5.5" />
                  <circle cx="8" cy="8" r="1.5" />
                  <line x1="8" y1="0.5" x2="8" y2="2.5" />
                  <line x1="8" y1="13.5" x2="8" y2="15.5" />
                  <line x1="0.5" y1="8" x2="2.5" y2="8" />
                  <line x1="13.5" y1="8" x2="15.5" y2="8" />
                </svg>
              </button>
            )}
          </div>
          <span className={styles.statusBadge} data-status={task.status}>
            <span className={styles.statusDot} />
            {t(`taskDetail.status.${task.status}`, { defaultValue: task.status })}
          </span>
        </div>

        {task.description && (
          <div className={styles.section}>
            <div className={styles.taskDesc}>{task.description}</div>
          </div>
        )}

        {task.last_error && (
          <div className={styles.section}>
            <div className={styles.sectionTitle}>{t("taskDetail.failureCause")}</div>
            <pre className={styles.errorBlock}>{task.last_error}</pre>
            {task.last_failed_at && (
              <div className={styles.errorTime}>
                {new Date(task.last_failed_at + "Z").toLocaleString()}
              </div>
            )}
          </div>
        )}

        <div className={styles.section}>
          <div className={styles.detailRow}>
            <span className={styles.detailLabel}>{t("taskDetail.complexity")}</span>
            <span className={styles.detailValue}>{task.complexity}</span>
          </div>
          {task.assigned_agent_id && (
            <div className={styles.detailRow}>
              <span className={styles.detailLabel}>{t("taskDetail.agent")}</span>
              <span className={styles.detailValue}>{task.assigned_agent_id}</span>
            </div>
          )}
        </div>

        {upstream.length > 0 && (
          <div className={styles.section}>
            <div className={styles.sectionTitle}>{t("taskDetail.upstream")}</div>
            <div className={styles.depList}>
              {upstream.map((t) => (
                <button
                  key={t.id}
                  className={styles.depItem}
                  onClick={() => onFocusTask?.(t.id)}
                  type="button"
                >
                  <span className={styles.depDot} data-status={t.status} />
                  {t.title}
                </button>
              ))}
            </div>
          </div>
        )}

        {downstream.length > 0 && (
          <div className={styles.section}>
            <div className={styles.sectionTitle}>{t("taskDetail.downstream")}</div>
            <div className={styles.depList}>
              {downstream.map((t) => (
                <button
                  key={t.id}
                  className={styles.depItem}
                  onClick={() => onFocusTask?.(t.id)}
                  type="button"
                >
                  <span className={styles.depDot} data-status={t.status} />
                  {t.title}
                </button>
              ))}
            </div>
          </div>
        )}

        <button className={styles.closeBtn} onClick={onClose} type="button">×</button>
      </div>
    </div>
  );
}
