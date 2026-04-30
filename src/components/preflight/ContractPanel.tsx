import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { ContractInfo, ContractSection as SectionType } from "../../ipc/commands";
import { ContractSection } from "./ContractSection";
import { ContractConfigCards } from "./ContractConfigCards";
import { DecisionTimeline } from "./DecisionTimeline";
import styles from "./ContractPanel.module.css";

interface ContractPanelProps {
  contract: ContractInfo;
  sessionId: string | null;
  onRemoveItem: (itemId: string) => void;
  onUpdateConfig: (field: string, value: number) => void;
  onSign: () => void;
  signing: boolean;
}

const SECTIONS: SectionType[] = ["scope", "constraints", "exclusions", "assumptions"];

export function ContractPanel({
  contract,
  sessionId,
  onRemoveItem,
  onUpdateConfig,
  onSign,
  signing,
}: ContractPanelProps) {
  const { t } = useTranslation("preflight");
  const readOnly = contract.status === "signed";
  const scopeCount = useMemo(
    () => contract.items.filter((i) => i.section === "scope").length,
    [contract.items],
  );

  const canSign = scopeCount > 0 && !readOnly && !signing;

  return (
    <div className={styles.panel}>
      <div className={styles.header}>
        <div className={styles.title}>{t("missionContract")}</div>
        <div className={styles.subtitle}>
          {readOnly ? t("contractReadOnly") : t("contractLive")}
        </div>
      </div>

      <div className={styles.body}>
        {SECTIONS.map((section) => (
          <ContractSection
            key={section}
            section={section}
            items={contract.items.filter((i) => i.section === section)}
            onRemove={onRemoveItem}
            readOnly={readOnly}
          />
        ))}
      </div>

      <div className={styles.footer}>
        <DecisionTimeline sessionId={sessionId} />
        <ContractConfigCards
          budgetUsd={contract.budget_usd}
          qualityThreshold={contract.quality_threshold}
          maxDurationHours={contract.max_duration_hours}
          onUpdate={onUpdateConfig}
          readOnly={readOnly}
        />
        {!readOnly && (
          <div className={styles.confirmArea}>
            <button
              className={styles.confirmBtn}
              onClick={onSign}
              disabled={!canSign}
            >
              {signing ? (
                <>
                  <span className={styles.spinner} />
                  {t("signing")}
                </>
              ) : (
                <>
                  <span>✓</span>
                  {t("signContractCta")}
                </>
              )}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
