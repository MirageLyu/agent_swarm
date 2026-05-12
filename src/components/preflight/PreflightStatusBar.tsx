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

  // Issue 2: round-pressure 软提示。messageCount 约等于 2-3 倍 round；
  // - >=80 (~30 round) 且 phase 不在 ready_to_sign：催聚焦
  // - >=140 (~50 round)：明确提示对话过长、建议直接签约
  // 同样的引导后端 LLM prompt 也会收到（render_round_pressure_directive），
  // 这里只是给用户的视觉镜像，让用户能"看见"自己被引导的原因。
  const overLength = !isReady && messageCount >= 140;
  const lengthy = !isReady && messageCount >= 80 && messageCount < 140;
  const hint = isReady
    ? t("statusBar.hintReady")
    : overLength
      ? t("statusBar.hintOverLength")
      : lengthy
        ? t("statusBar.hintLengthy")
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
