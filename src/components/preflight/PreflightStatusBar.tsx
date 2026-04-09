import type { ConversationPhase } from "../../ipc/commands";
import styles from "./PreflightStatusBar.module.css";

interface PreflightStatusBarProps {
  convergenceScore: number;
  phase: ConversationPhase;
  messageCount: number;
}

const PHASE_CONFIG: Record<
  ConversationPhase,
  { label: string; color: string }
> = {
  exploring: { label: "探索", color: "#007AFF" },
  narrowing: { label: "收窄", color: "#AF52DE" },
  confirming: { label: "确认", color: "#34C759" },
  ready_to_sign: { label: "就绪", color: "#FF9500" },
};

export function PreflightStatusBar({
  convergenceScore,
  phase,
  messageCount,
}: PreflightStatusBarProps) {
  const percent = Math.round(convergenceScore * 100);
  const config = PHASE_CONFIG[phase] ?? PHASE_CONFIG.exploring;
  const isReady = phase === "ready_to_sign";

  const hint = isReady
    ? "澄清完成，可签署 Contract"
    : percent >= 60
      ? "即将完成，再确认几个关键点"
      : messageCount === 0
        ? "与 AI 多轮对话，逐步澄清需求边界"
        : "5 分钟澄清 → 节省 ~$50 错误方向成本";

  return (
    <div className={styles.bar}>
      <div className={styles.statusItem}>
        <div
          className={`${styles.statusDot} ${isReady ? styles.statusDotComplete : ""}`}
          style={{ background: config.color }}
        />
        <span>{isReady ? "Pre-flight 就绪" : "Pre-flight 进行中"}</span>
      </div>
      <div className={styles.phaseLabel} style={{ color: config.color }}>
        {config.label}
      </div>
      <div className={styles.progress}>
        <div className={styles.progressBg}>
          <div
            className={styles.progressFill}
            style={{ width: `${percent}%`, background: config.color }}
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
