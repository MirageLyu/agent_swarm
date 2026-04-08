import type { PreflightMode } from "../../ipc/commands";
import styles from "./PreflightModeSwitch.module.css";

interface PreflightModeSwitchProps {
  mode: PreflightMode;
  onModeChange: (mode: PreflightMode) => void;
}

const MODES: { value: PreflightMode; label: string }[] = [
  { value: "scenario_walk", label: "场景走查" },
  { value: "devils_advocate", label: "魔鬼代言人" },
  { value: "risk_highlighter", label: "风险标记" },
];

export function PreflightModeSwitch({ mode, onModeChange }: PreflightModeSwitchProps) {
  return (
    <div className={styles.header}>
      <div className={styles.label}>澄清模式</div>
      <div className={styles.segmented}>
        {MODES.map((m) => (
          <button
            key={m.value}
            className={`${styles.segBtn} ${mode === m.value ? styles.active : ""}`}
            onClick={() => onModeChange(m.value)}
          >
            {m.label}
          </button>
        ))}
      </div>
    </div>
  );
}
