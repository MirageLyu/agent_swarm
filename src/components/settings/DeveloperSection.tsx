/**
 * Settings → Developer：面向 agent 循环内部调试的开关合集。
 *
 * 当前内容：
 * - **Show silent recovery events** (P0-3)：把后端 emit 的 `recovery_attempt`
 *   / `recovery_succeeded` 这类 silent 事件渲染到 timeline。默认关闭遵循
 *   "agent 自救成功的事件不打扰用户"原则，仅 developer debug 时打开。
 * - **Allow command hooks** (P2-1 Phase C)：开启后 agent 会执行 workspace 的
 *   `.miragenty/hooks.json` 中定义的 shell 命令。**RCE 风险**——默认禁用。
 * - **Enable explicit merge node** (Merge v1)：多 parent 汇合点用独立 Merge agent
 *   显式合并；默认关闭，开启需新建 mission 才生效。
 *
 * 前端偏好（show silent recovery）走 `useUiStore` + localStorage；
 * 影响 agent 真实行为的开关走后端 AppConfig（必须 persist 到 server）。
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
  const [enableExplicitMerge, setEnableExplicitMerge] = useState<boolean | null>(null);
  // verify_command 是文本输入，需 onBlur 才提交（避免每个 keystroke 都打后端）
  const [mergeVerifyCommand, setMergeVerifyCommand] = useState<string>("");
  const [mergeVerifyDirty, setMergeVerifyDirty] = useState(false);
  useEffect(() => {
    commands
      .getConfig()
      .then((c) => {
        setAllowCommandHooks(c.allow_command_hooks);
        setEnableExplicitMerge(c.enable_explicit_merge_node);
        setMergeVerifyCommand(c.merge_verify_command);
      })
      .catch(() => {
        setAllowCommandHooks(false);
        setEnableExplicitMerge(false);
      });
  }, []);
  const updateAllowCommandHooks = async (next: boolean) => {
    setAllowCommandHooks(next);
    try {
      await commands.updateConfig({ allow_command_hooks: next });
    } catch (e) {
      console.error("Failed to persist allow_command_hooks:", e);
      setAllowCommandHooks(!next);
    }
  };
  const updateEnableExplicitMerge = async (next: boolean) => {
    setEnableExplicitMerge(next);
    try {
      await commands.updateConfig({ enable_explicit_merge_node: next });
    } catch (e) {
      console.error("Failed to persist enable_explicit_merge_node:", e);
      setEnableExplicitMerge(!next);
    }
  };
  const flushMergeVerifyCommand = async () => {
    if (!mergeVerifyDirty) return;
    setMergeVerifyDirty(false);
    try {
      await commands.updateConfig({ merge_verify_command: mergeVerifyCommand });
    } catch (e) {
      console.error("Failed to persist merge_verify_command:", e);
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

      <div className={styles.field}>
        <label className={styles.label}>{t("explicitMergeNodeLabel")}</label>
        <div className={styles.segment} role="radiogroup">
          <button
            role="radio"
            aria-checked={enableExplicitMerge === false}
            className={`${styles.option} ${enableExplicitMerge === false ? styles.optionActive : ""}`}
            onClick={() => void updateEnableExplicitMerge(false)}
            disabled={enableExplicitMerge === null}
          >
            {t("explicitMergeNodeOff")}
          </button>
          <button
            role="radio"
            aria-checked={enableExplicitMerge === true}
            className={`${styles.option} ${enableExplicitMerge === true ? styles.optionActive : ""}`}
            onClick={() => void updateEnableExplicitMerge(true)}
            disabled={enableExplicitMerge === null}
          >
            {t("explicitMergeNodeOn")}
          </button>
        </div>
        <p className={styles.intro} style={{ marginTop: "var(--space-2)" }}>
          {t("explicitMergeNodeHint")}
        </p>
      </div>

      <div className={styles.field}>
        <label className={styles.label}>{t("mergeVerifyCommandLabel")}</label>
        <input
          type="text"
          value={mergeVerifyCommand}
          placeholder={t("mergeVerifyCommandPlaceholder")}
          onChange={(e) => {
            setMergeVerifyCommand(e.target.value);
            setMergeVerifyDirty(true);
          }}
          onBlur={() => void flushMergeVerifyCommand()}
          disabled={enableExplicitMerge !== true}
          style={{
            width: "100%",
            padding: "var(--space-2) var(--space-3)",
            background: "var(--color-surface-2)",
            border: "1px solid var(--color-border)",
            borderRadius: "var(--radius-md)",
            color: "var(--color-text-primary)",
            fontSize: "var(--text-sm)",
            fontFamily: "var(--font-mono)",
          }}
        />
        <p className={styles.intro} style={{ marginTop: "var(--space-2)" }}>
          {t("mergeVerifyCommandHint")}
        </p>
      </div>
    </div>
  );
}
