import { useTranslation } from "react-i18next";
import type { ConversationPhase } from "../../ipc/commands";
import styles from "./PreflightStatusBar.module.css";

interface PreflightStatusBarProps {
  convergenceScore: number;
  phase: ConversationPhase;
  messageCount: number;
}

const PHASE_COLOR: Record<ConversationPhase, string> = {
  exploring: "#007AFF",
  narrowing: "#AF52DE",
  confirming: "#34C759",
  ready_to_sign: "#FF9500",
};

export function PreflightStatusBar({
  convergenceScore,
  phase,
  messageCount,
}: PreflightStatusBarProps) {
  const { t } = useTranslation("preflight");
  const percent = Math.round(convergenceScore * 100);
  const color = PHASE_COLOR[phase] ?? PHASE_COLOR.exploring;
  const isReady = phase === "ready_to_sign";

  const hint = isReady
    ? t("statusBar.hintReady")
    : percent >= 60
      ? t("statusBar.hintAlmost")
      : messageCount === 0
        ? t("statusBar.hintEmpty")
        : t("statusBar.hintGeneric");

  return (
    <div className={styles.bar}>
      <div className={styles.statusItem}>
        <div
          className={`${styles.statusDot} ${isReady ? styles.statusDotComplete : ""}`}
          style={{ background: color }}
        />
        <span>{isReady ? t("statusBar.ready") : t("statusBar.inProgress")}</span>
      </div>
      <div className={styles.phaseLabel} style={{ color }}>
        {t(`statusBar.phase.${phase}`)}
      </div>
      <div className={styles.progress}>
        <div className={styles.progressBg}>
          <div
            className={styles.progressFill}
            style={{ width: `${percent}%`, background: color }}
          />
        </div>
        <span>{percent}%</span>
      </div>
      <div className={styles.spacer} />
      <div className={styles.hint}>
        <span>💡</span>
        <span>{hint}</span>
      </div>
    </div>
  );
}
