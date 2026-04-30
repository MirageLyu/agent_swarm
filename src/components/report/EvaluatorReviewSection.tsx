import { useTranslation } from "react-i18next";
import type { MissionReportEvaluatorReview } from "../../ipc/commands";
import styles from "./EvaluatorReviewSection.module.css";

interface Props {
  review: MissionReportEvaluatorReview;
}

/**
 * FM-12 FR-05: Evaluator Review 节
 * 时间线展示每轮评审：score / issues / auto-fixed + 摘要
 */
export function EvaluatorReviewSection({ review }: Props) {
  const { t } = useTranslation("report");
  return (
    <div className={styles.container}>
      <div className={styles.headerStats}>
        <Stat label={t("evaluatorStatReviews")} value={String(review.rounds.length)} />
        <Stat label={t("evaluatorStatTotalIssues")} value={String(review.total_issues)} />
        <Stat
          label={t("evaluatorStatAutoFixed")}
          value={String(review.auto_fixed)}
          tone={review.auto_fixed > 0 ? "ok" : undefined}
        />
      </div>

      <ol className={styles.timeline}>
        {review.rounds.map((r, i) => (
          <li key={`${r.agent_id}-${i}`} className={styles.round}>
            <div className={styles.bullet} aria-hidden>
              <ScoreBadge score={r.score} />
            </div>
            <div className={styles.body}>
              <div className={styles.title}>
                {r.task_title}
                <span className={styles.agentName}> · {r.agent_name}</span>
              </div>
              <div className={styles.statsLine}>
                <span>{t("evaluatorIssues", { count: r.issues })}</span>
                {r.auto_fixed > 0 && (
                  <span className={styles.autoFixedTag}>
                    {t("evaluatorAutoFixed", { count: r.auto_fixed })}
                  </span>
                )}
                <time className={styles.time}>{r.created_at}</time>
              </div>
              {r.summary && <p className={styles.summary}>{r.summary}</p>}
            </div>
          </li>
        ))}
      </ol>
    </div>
  );
}

function Stat({ label, value, tone }: { label: string; value: string; tone?: "ok" }) {
  return (
    <div className={styles.stat}>
      <span className={styles.statLabel}>{label}</span>
      <span
        className={`${styles.statValue} ${tone === "ok" ? styles.statValueOk : ""}`}
      >
        {value}
      </span>
    </div>
  );
}

function ScoreBadge({ score }: { score: number }) {
  const tone = score >= 7.5 ? "ok" : score >= 5 ? "warn" : "bad";
  return (
    <span className={`${styles.scoreBadge} ${styles[`score_${tone}`]}`}>
      {score.toFixed(1)}
    </span>
  );
}
