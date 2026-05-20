/**
 * Settings → Developer：面向 agent 循环内部调试的开关合集。
 *
 * 当前内容：
 * - **Show silent recovery events** (P0-3)：把后端 emit 的 `recovery_attempt`
 *   / `recovery_succeeded` 这类 silent 事件渲染到 timeline。默认关闭遵循
 *   "agent 自救成功的事件不打扰用户"原则，仅 developer debug 时打开。
 *
 * Toggle 状态走 `useUiStore.showSilentRecoveryEvents` + localStorage 持久化，
 * 不走后端 AppConfig —— 这是纯前端渲染偏好，没必要上 IPC + DB migration。
 */
import { useTranslation } from "react-i18next";
import { useUiStore } from "../../stores/ui-store";
import styles from "./LanguageSection.module.css";

export function DeveloperSection() {
  const { t } = useTranslation("settings");
  const showSilentRecoveryEvents = useUiStore((s) => s.showSilentRecoveryEvents);
  const setShowSilentRecoveryEvents = useUiStore((s) => s.setShowSilentRecoveryEvents);

  return (
    <div className={styles.section}>
      <h3 className={styles.title}>{t("developerHeader")}</h3>
      <p className={styles.intro}>{t("developerIntro")}</p>
      <div className={styles.field}>
        <label className={styles.label}>{t("showSilentRecoveryLabel")}</label>
        <div className={styles.segment} role="radiogroup">
          <button
            role="radio"
            aria-checked={!showSilentRecoveryEvents}
            className={`${styles.option} ${!showSilentRecoveryEvents ? styles.optionActive : ""}`}
            onClick={() => setShowSilentRecoveryEvents(false)}
          >
            Off
          </button>
          <button
            role="radio"
            aria-checked={showSilentRecoveryEvents}
            className={`${styles.option} ${showSilentRecoveryEvents ? styles.optionActive : ""}`}
            onClick={() => setShowSilentRecoveryEvents(true)}
          >
            On
          </button>
        </div>
        <p className={styles.intro} style={{ marginTop: "var(--space-2)" }}>
          {t("showSilentRecoveryHint")}
        </p>
      </div>
    </div>
  );
}
