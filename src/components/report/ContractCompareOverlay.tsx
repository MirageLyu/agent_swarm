import type { MissionReportContract } from "../../ipc/commands";
import styles from "./ContractCompareOverlay.module.css";

interface Props {
  contract: MissionReportContract | null;
  onClose: () => void;
}

/**
 * FM-12 FR-10: Contract 对照面板
 *
 * 右侧 320px overlay，列出 Contract 各 section 的条目 + 达成状态。
 * achieved 字段由后端 collect_contract 启发式生成（completed → scope/constraints true）；
 * 未来 FM-11 evaluator 可注入条目级证据。
 */
export function ContractCompareOverlay({ contract, onClose }: Props) {
  if (!contract) {
    return (
      <aside className={styles.overlay}>
        <Header onClose={onClose} />
        <div className={styles.empty}>
          This mission was created without going through Pre-flight, so there
          is no signed Contract to compare against.
        </div>
      </aside>
    );
  }

  // 按 section 分组
  const sections = ["scope", "constraints", "exclusions", "assumptions"] as const;
  const grouped = new Map<string, typeof contract.items>();
  for (const it of contract.items) {
    const list = grouped.get(it.section) ?? [];
    list.push(it);
    grouped.set(it.section, list);
  }

  return (
    <aside className={styles.overlay}>
      <Header onClose={onClose} />

      <div className={styles.metaRow}>
        <span className={styles.metaTag}>{contract.status}</span>
        {contract.budget_usd !== null && (
          <span className={styles.metaItem}>${contract.budget_usd.toFixed(2)} budget</span>
        )}
        {contract.quality_threshold !== null && (
          <span className={styles.metaItem}>≥{contract.quality_threshold.toFixed(1)} quality</span>
        )}
        {contract.max_duration_hours !== null && (
          <span className={styles.metaItem}>≤{contract.max_duration_hours}h</span>
        )}
      </div>

      <div className={styles.sections}>
        {sections.map((section) => {
          const items = grouped.get(section) ?? [];
          if (items.length === 0) return null;
          return (
            <div key={section} className={styles.section}>
              <h4 className={styles.sectionTitle}>{section}</h4>
              <ul className={styles.itemList}>
                {items.map((it, i) => (
                  <li
                    key={`${section}-${i}`}
                    className={`${styles.item} ${
                      it.achieved ? styles.itemOk : styles.itemMissing
                    }`}
                  >
                    <span className={styles.mark} aria-hidden>
                      {it.achieved ? "✓" : "✗"}
                    </span>
                    <span className={styles.itemText}>{it.text}</span>
                  </li>
                ))}
              </ul>
            </div>
          );
        })}
      </div>
    </aside>
  );
}

function Header({ onClose }: { onClose: () => void }) {
  return (
    <div className={styles.header}>
      <h3 className={styles.title}>Contract</h3>
      <button
        type="button"
        className={styles.closeBtn}
        onClick={onClose}
        title="Close contract panel"
        aria-label="Close"
      >
        ×
      </button>
    </div>
  );
}
