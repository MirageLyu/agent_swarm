/**
 * Language switcher in Settings.
 *
 * 语言切换的 UX 决策：
 * - 单选 segmented，不用下拉（语言数 ≤ 5 时 segmented 视觉成本最低）
 * - 选择即应用，不需要二次 Save 按钮（避免"我以为切了但没切"的困惑）
 * - 切换后 backend persist 失败时仅 console.warn，不弹错误：UI 已经切了，
 *   下次启动还原成 backend 旧值即可（一次性体验损失，不是数据损失）
 */
import { useTranslation } from "react-i18next";
import {
  SUPPORTED_LANGUAGES,
  changeLanguage,
  type SupportedLanguage,
} from "../../i18n";
import { commands } from "../../ipc/commands";
import styles from "./LanguageSection.module.css";

export function LanguageSection() {
  const { t, i18n } = useTranslation("settings");
  const current = i18n.language as SupportedLanguage;

  const handleSwitch = async (lng: SupportedLanguage) => {
    if (lng === current) return;
    await changeLanguage(lng, async (l) => {
      await commands.updateConfig({ language: l });
    });
  };

  return (
    <div className={styles.section}>
      <h3 className={styles.title}>{t("languageHeader")}</h3>
      <p className={styles.intro}>{t("languageIntro")}</p>
      <div className={styles.field}>
        <label className={styles.label}>{t("languageLabel")}</label>
        <div className={styles.segment} role="radiogroup">
          {SUPPORTED_LANGUAGES.map((lang) => {
            const active = lang.code === current;
            return (
              <button
                key={lang.code}
                role="radio"
                aria-checked={active}
                className={`${styles.option} ${active ? styles.optionActive : ""}`}
                onClick={() => void handleSwitch(lang.code)}
              >
                {lang.label}
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
