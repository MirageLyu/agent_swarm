import { useState, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import styles from "./ContractConfigCards.module.css";

interface ContractConfigCardsProps {
  budgetUsd: number | null;
  qualityThreshold: number | null;
  maxDurationHours: number | null;
  onUpdate: (field: string, value: number) => void;
  readOnly?: boolean;
}

function ConfigCard({
  label,
  prefix,
  suffix,
  value,
  inputWidth,
  onCommit,
  readOnly,
}: {
  label: string;
  prefix?: string;
  suffix?: string;
  value: number | null;
  inputWidth?: number;
  onCommit: (val: number) => void;
  readOnly?: boolean;
}) {
  const [localVal, setLocalVal] = useState(String(value ?? ""));
  const timerRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const v = e.target.value;
      setLocalVal(v);
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = setTimeout(() => {
        const num = parseFloat(v);
        if (!isNaN(num) && num >= 0) onCommit(num);
      }, 600);
    },
    [onCommit],
  );

  return (
    <div className={styles.card}>
      <div className={styles.cardLabel}>{label}</div>
      <div className={styles.cardValue}>
        {prefix && <span>{prefix}</span>}
        <input
          className={styles.input}
          type="text"
          value={localVal}
          onChange={handleChange}
          style={{ width: inputWidth ?? 60 }}
          readOnly={readOnly}
        />
        {suffix && <span className={styles.unit}>{suffix}</span>}
      </div>
    </div>
  );
}

export function ContractConfigCards({
  budgetUsd,
  qualityThreshold,
  maxDurationHours,
  onUpdate,
  readOnly,
}: ContractConfigCardsProps) {
  const { t } = useTranslation("preflight");
  return (
    <div className={styles.container}>
      <ConfigCard
        label={t("config.budget")}
        prefix="$"
        value={budgetUsd}
        onCommit={(v) => onUpdate("budget_usd", v)}
        readOnly={readOnly}
      />
      <ConfigCard
        label={t("config.quality")}
        suffix="/10"
        value={qualityThreshold}
        inputWidth={30}
        onCommit={(v) => onUpdate("quality_threshold", v)}
        readOnly={readOnly}
      />
      <ConfigCard
        label={t("config.maxDuration")}
        suffix={t("config.hours")}
        value={maxDurationHours}
        inputWidth={30}
        onCommit={(v) => onUpdate("max_duration_hours", v)}
        readOnly={readOnly}
      />
    </div>
  );
}
