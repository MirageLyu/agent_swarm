import type { EvaluationResult } from "../../ipc";
import { FileScore } from "./FileScore";
import styles from "./EvalSummaryBar.module.css";

interface EvalSummaryBarProps {
  result: EvaluationResult | null;
  evaluating?: boolean;
  onTrigger?: () => void;
  canTrigger?: boolean;
}

export function EvalSummaryBar({ result, evaluating, onTrigger, canTrigger }: EvalSummaryBarProps) {
  if (evaluating) {
    return (
      <div className={styles.bar}>
        <div className={styles.left}>
          <span className={styles.label}>Evaluator</span>
          <span className={styles.spinner} />
          <span className={styles.evaluatingText}>Evaluating…</span>
        </div>
      </div>
    );
  }

  if (!result && canTrigger) {
    return (
      <div className={styles.bar}>
        <div className={styles.left}>
          <span className={styles.label}>Evaluator</span>
          <span className={styles.noResult}>No evaluation yet</span>
        </div>
        <div className={styles.right}>
          <button className={styles.triggerBtn} onClick={onTrigger}>
            Run Evaluation
          </button>
        </div>
      </div>
    );
  }

  if (!result) return null;

  return (
    <div className={styles.bar}>
      <div className={styles.left}>
        <span className={styles.label}>Evaluator</span>
        <FileScore score={result.overall_score} />
      </div>
      <div className={styles.right}>
        <span className={styles.stat}>
          <span className={styles.count}>{result.annotation_count}</span>
          {result.annotation_count === 1 ? " issue" : " issues"}
        </span>
        {result.auto_fixed_count > 0 && (
          <>
            <span className={styles.sep}>·</span>
            <span className={styles.fixed}>
              {result.auto_fixed_count} auto-fixed
            </span>
          </>
        )}
        {result.needs_review_count > 0 && (
          <>
            <span className={styles.sep}>·</span>
            <span className={styles.review}>
              {result.needs_review_count} needs review
            </span>
          </>
        )}
      </div>
    </div>
  );
}
