/**
 * Settings → Developer：面向 agent 循环内部调试的开关合集。
 *
 * 当前内容：
 * - **Show silent recovery events** (P0-3)：把后端 emit 的 `recovery_attempt`
 *   / `recovery_succeeded` 这类 silent 事件渲染到 timeline。默认关闭遵循
 *   "agent 自救成功的事件不打扰用户"原则，仅 developer debug 时打开。
 * - **Allow command hooks** (P2-1 Phase C)：开启后 agent 会执行 workspace 的
 *   `.miragenty/hooks.json` 中定义的 shell 命令。**RCE 风险**——默认禁用。
 *
 * Silent recovery toggle 走 `useUiStore` + localStorage（纯前端偏好），
 * allow_command_hooks 走后端 AppConfig（影响 agent 真实行为，必须 persist 到 server）。
 */
import { useTranslation } from "react-i18next";
import { useEffect, useState } from "react";
import { useUiStore } from "../../stores/ui-store";
import { commands } from "../../ipc";
import styles from "./LanguageSection.module.css";

export function DeveloperSection() {
  const { t } = useTranslation("settings");
  const showSilentRecoveryEvents = useUiStore((s) => s.showSilentRecoveryEvents);
  const setShowSilentRecoveryEvents = useUiStore((s) => s.setShowSilentRecoveryEvents);

  // allow_command_hooks 状态：本地缓存 + 启动时拉取。保存即生效（safety 关键开关
  // 不引入 dirty + save 二段式 UX——避免用户改了但忘 save 实际未生效的危险幻觉）。
  const [allowCommandHooks, setAllowCommandHooks] = useState<boolean | null>(null);
  useEffect(() => {
    commands
      .getConfig()
      .then((c) => setAllowCommandHooks(c.allow_command_hooks))
      .catch(() => setAllowCommandHooks(false));
  }, []);
  const updateAllowCommandHooks = async (next: boolean) => {
    setAllowCommandHooks(next);
    try {
      await commands.updateConfig({ allow_command_hooks: next });
    } catch (e) {
      // 后端写失败 → 回滚 UI，避免显示 false success
      // eslint-disable-next-line no-console
      console.error("Failed to persist allow_command_hooks:", e);
      setAllowCommandHooks(!next);
    }
  };

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

      <div className={styles.field}>
        <label className={styles.label}>{t("allowCommandHooksLabel")}</label>
        <div className={styles.segment} role="radiogroup">
          <button
            role="radio"
            aria-checked={allowCommandHooks === false}
            className={`${styles.option} ${allowCommandHooks === false ? styles.optionActive : ""}`}
            onClick={() => void updateAllowCommandHooks(false)}
            disabled={allowCommandHooks === null}
          >
            {t("allowCommandHooksOff")}
          </button>
          <button
            role="radio"
            aria-checked={allowCommandHooks === true}
            className={`${styles.option} ${allowCommandHooks === true ? styles.optionActive : ""}`}
            onClick={() => void updateAllowCommandHooks(true)}
            disabled={allowCommandHooks === null}
          >
            {t("allowCommandHooksOn")}
          </button>
        </div>
        <p className={styles.intro} style={{ marginTop: "var(--space-2)" }}>
          {t("allowCommandHooksHint")}
        </p>
      </div>
    </div>
  );
}
