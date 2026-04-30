import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { MissionReportTaskRow } from "../../ipc/commands";
import styles from "./TaskMatrixSection.module.css";

interface Props {
  rows: MissionReportTaskRow[];
}

/**
 * FM-12 FR-06: Task Completion Matrix 节
 * 表格按 score 升序排（低分置顶高亮），未评分的排末尾。
 */
export function TaskMatrixSection({ rows }: Props) {
  const { t } = useTranslation("report");
  const sorted = useMemo(() => {
    const copy = [...rows];
    copy.sort((a, b) => {
      // null score → 排末尾
      if (a.score === null && b.score === null) return 0;
      if (a.score === null) return 1;
      if (b.score === null) return -1;
      return a.score - b.score;
    });
    return copy;
  }, [rows]);

  return (
    <div className={styles.tableWrap}>
      <table className={styles.table}>
        <thead>
          <tr>
            <th className={styles.colTask}>{t("taskMatrixCol.task")}</th>
            <th className={styles.colAgent}>{t("taskMatrixCol.agent")}</th>
            <th className={styles.colScore}>{t("taskMatrixCol.score")}</th>
            <th className={styles.colCost}>{t("taskMatrixCol.cost")}</th>
            <th className={styles.colDuration}>{t("taskMatrixCol.duration")}</th>
            <th className={styles.colStatus}>{t("taskMatrixCol.status")}</th>
          </tr>
        </thead>
        <tbody>
          {sorted.map((r) => (
            <tr
              key={r.task_id}
              className={
                r.score !== null && r.score < 5
                  ? styles.rowLowScore
                  : undefined
              }
            >
              <td className={styles.cellTask} title={r.title}>
                {r.title}
              </td>
              <td className={styles.cellAgent}>{r.agent_name ?? "—"}</td>
              <td className={styles.cellScore}>
                {r.score !== null ? r.score.toFixed(1) : "—"}
              </td>
              <td className={styles.cellCost}>${r.cost_usd.toFixed(4)}</td>
              <td className={styles.cellDuration}>
                {r.duration_seconds !== null ? formatDuration(r.duration_seconds) : "—"}
              </td>
              <td className={styles.cellStatus}>
                <StatusPill status={r.status} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function StatusPill({ status }: { status: string }) {
  const tone =
    status === "completed"
      ? "ok"
      : status === "failed" || status === "cancelled"
        ? "bad"
        : status === "running"
          ? "warn"
          : "neutral";
  return (
    <span className={`${styles.pill} ${styles[`pill_${tone}`]}`}>{status}</span>
  );
}

function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  return `${mins}m ${secs}s`;
}
