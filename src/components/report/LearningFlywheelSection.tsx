import type { MissionReportLearningFlywheel } from "../../ipc/commands";
import styles from "./LearningFlywheelSection.module.css";

interface Props {
  data: MissionReportLearningFlywheel;
}

/**
 * FM-12 FR-09: Learning Flywheel 节
 * 当前是占位实现：past_decision_patterns 来自历史投票聚合，
 * MVP 阶段单 mission 无积累，主要展示 insight 文字。FM-13 完成后会拓展。
 */
export function LearningFlywheelSection({ data }: Props) {
  return (
    <div className={styles.container}>
      <div className={styles.insight}>
        <span className={styles.icon} aria-hidden>
          ↻
        </span>
        <p className={styles.insightText}>{data.insight}</p>
      </div>

      {data.past_decision_patterns.length > 0 && (
        <div className={styles.patterns}>
          <h4 className={styles.patternsTitle}>Past Decision Patterns</h4>
          <ul className={styles.patternsList}>
            {data.past_decision_patterns.map((p, i) => (
              <li key={i}>{p}</li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
