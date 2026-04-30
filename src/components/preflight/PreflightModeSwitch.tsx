import { useTranslation } from "react-i18next";
import type { PreflightMode } from "../../ipc/commands";
import styles from "./PreflightModeSwitch.module.css";

interface PreflightModeSwitchProps {
  mode: PreflightMode;
  onModeChange: (mode: PreflightMode) => void;
}

const MODE_VALUES: PreflightMode[] = [
  "scenario_walk",
  "devils_advocate",
  "risk_highlighter",
];

export function PreflightModeSwitch({ mode, onModeChange }: PreflightModeSwitchProps) {
  const { t } = useTranslation("preflight");
  return (
    <div className={styles.header}>
      <div className={styles.label}>{t("modeSwitchLabel")}</div>
      <div className={styles.segmented}>
        {MODE_VALUES.map((value) => (
          <button
            key={value}
            className={`${styles.segBtn} ${mode === value ? styles.active : ""}`}
            onClick={() => onModeChange(value)}
          >
            {t(`modeLabel.${value}`)}
          </button>
        ))}
      </div>
    </div>
  );
}
