import type { MissionReportMission, MissionReportSummary } from "../../ipc/commands";
import styles from "./ExecSummarySection.module.css";

interface Props {
  mission: MissionReportMission;
  summary: MissionReportSummary;
}

/**
 * FM-12 FR-03: Executive Summary 节
 * 顶部 metric 卡片 + LLM/降级摘要文字。
 */
export function ExecSummarySection({ mission, summary }: Props) {
  const m = summary.metrics;

  const metrics: { label: string; value: string; tone?: "ok" | "warn" | "bad" }[] = [
    {
      label: "Duration",
      value: formatDuration(m.duration_seconds),
    },
    {
      label: "Cost",
      value: `$${m.total_cost_usd.toFixed(4)}`,
    },
    {
      label: "Quality",
      value: m.avg_quality_score !== null ? m.avg_quality_score.toFixed(2) : "—",
      tone: scoreTone(m.avg_quality_score),
    },
    {
      label: "Auto-fixes",
      value: String(m.auto_fixes),
    },
    {
      label: "Tasks",
      value: `${m.tasks_completed}/${m.tasks_total}`,
      tone: m.tasks_failed > 0 ? "warn" : undefined,
    },
    {
      label: "Reduction Rate",
      value:
        m.review_reduction_rate !== null
          ? `${(m.review_reduction_rate * 100).toFixed(0)}%`
          : "—",
    },
  ];

  return (
    <div className={styles.container}>
      <div className={styles.statusRow}>
        <StatusBadge status={mission.status} />
        {mission.main_branch && (
          <span className={styles.branch}>on {mission.main_branch}</span>
        )}
      </div>

      <div className={styles.metricsGrid}>
        {metrics.map((mt) => (
          <div key={mt.label} className={styles.metric}>
            <div className={styles.metricLabel}>{mt.label}</div>
            <div
              className={`${styles.metricValue} ${
                mt.tone ? styles[`metricValue_${mt.tone}`] : ""
              }`}
            >
              {mt.value}
            </div>
          </div>
        ))}
      </div>

      {summary.executive && (
        <div className={styles.summaryText}>{summary.executive}</div>
      )}
    </div>
  );
}

function StatusBadge({ status }: { status: string }) {
  const tone =
    status === "completed" ? "ok" : status === "failed" ? "bad" : "neutral";
  return (
    <span className={`${styles.statusBadge} ${styles[`status_${tone}`]}`}>
      {status}
    </span>
  );
}

function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  if (mins < 60) return `${mins}m ${secs}s`;
  const hours = Math.floor(mins / 60);
  const m = mins % 60;
  return `${hours}h ${m}m`;
}

function scoreTone(score: number | null): "ok" | "warn" | "bad" | undefined {
  if (score === null) return undefined;
  if (score >= 7.5) return "ok";
  if (score >= 5) return "warn";
  return "bad";
}
