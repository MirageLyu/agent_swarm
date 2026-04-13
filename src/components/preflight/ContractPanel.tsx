import { useMemo } from "react";
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
  const readOnly = contract.status === "signed";
  const scopeCount = useMemo(
    () => contract.items.filter((i) => i.section === "scope").length,
    [contract.items],
  );

  const canSign = scopeCount > 0 && !readOnly && !signing;

  return (
    <div className={styles.panel}>
      <div className={styles.header}>
        <div className={styles.title}>Mission Contract</div>
        <div className={styles.subtitle}>
          {readOnly ? "已签署 — 只读" : "实时构建中 — 随对话更新"}
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
                  签署中…
                </>
              ) : (
                <>
                  <span>✓</span>
                  签署合同并启动 Swarm
                </>
              )}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
