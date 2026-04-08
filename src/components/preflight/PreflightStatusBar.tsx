import styles from "./PreflightStatusBar.module.css";

interface PreflightStatusBarProps {
  messageCount: number;
  maxMessages?: number;
}

export function PreflightStatusBar({
  messageCount,
  maxMessages = 15,
}: PreflightStatusBarProps) {
  const progress = Math.min(100, Math.round((messageCount / maxMessages) * 100));
  const isComplete = progress >= 100;

  return (
    <div className={styles.bar}>
      <div className={styles.statusItem}>
        <div className={styles.statusDot} />
        <span>Pre-flight 进行中</span>
      </div>
      <div className={styles.progress}>
        <div className={styles.progressBg}>
          <div className={styles.progressFill} style={{ width: `${progress}%` }} />
        </div>
        <span>{progress}%</span>
      </div>
      <div className={styles.spacer} />
      <div className={styles.hint}>
        <span>💡</span>
        <span>
          {isComplete
            ? "澄清完成，可签署 Contract"
            : "5 分钟澄清 → 节省 ~$50 错误方向成本"}
        </span>
      </div>
    </div>
  );
}
