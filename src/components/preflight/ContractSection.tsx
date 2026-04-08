import { useEffect, useState, useRef } from "react";
import type { ContractItemInfo, ContractSection as SectionType } from "../../ipc/commands";
import styles from "./ContractSection.module.css";

interface ContractSectionProps {
  section: SectionType;
  items: ContractItemInfo[];
  onRemove: (itemId: string) => void;
  readOnly?: boolean;
}

const SECTION_CONFIG: Record<SectionType, {
  label: string;
  icon: string;
  dotClass: string;
  iconClass: string;
  emptyText: string;
}> = {
  scope: {
    label: "用户明确要求的",
    icon: "✓",
    dotClass: styles.dotScope,
    iconClass: styles.iconScope,
    emptyText: "等待对话确认…",
  },
  constraints: {
    label: "Agent 自主决策",
    icon: "◆",
    dotClass: styles.dotConstraints,
    iconClass: styles.iconConstraints,
    emptyText: "当你选择「你决定」时填充",
  },
  exclusions: {
    label: "明确不做的",
    icon: "✕",
    dotClass: styles.dotExclusions,
    iconClass: styles.iconExclusions,
    emptyText: "排除范围待确认…",
  },
  assumptions: {
    label: "已确认的环境前提",
    icon: "○",
    dotClass: styles.dotAssumptions,
    iconClass: styles.iconAssumptions,
    emptyText: "环境信息待确认…",
  },
};

function ItemWithNew({
  item,
  config,
  onRemove,
  readOnly,
  isNew,
}: {
  item: ContractItemInfo;
  config: typeof SECTION_CONFIG.scope;
  onRemove: (id: string) => void;
  readOnly?: boolean;
  isNew: boolean;
}) {
  const [showTag, setShowTag] = useState(isNew);

  useEffect(() => {
    if (!isNew) return;
    const timer = setTimeout(() => setShowTag(false), 2000);
    return () => clearTimeout(timer);
  }, [isNew]);

  return (
    <div className={styles.item}>
      <span className={`${styles.itemIcon} ${config.iconClass}`}>{config.icon}</span>
      <span className={styles.itemText}>{item.text}</span>
      {showTag && <span className={styles.itemTag}>NEW</span>}
      {!readOnly && (
        <button className={styles.removeBtn} onClick={() => onRemove(item.id)} title="移除">
          ×
        </button>
      )}
    </div>
  );
}

export function ContractSection({ section, items, onRemove, readOnly }: ContractSectionProps) {
  const config = SECTION_CONFIG[section];
  const prevCountRef = useRef(items.length);
  const [newItemIds, setNewItemIds] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (items.length > prevCountRef.current) {
      const existingIds = new Set(items.slice(0, prevCountRef.current).map((i) => i.id));
      const freshIds = items.filter((i) => !existingIds.has(i.id)).map((i) => i.id);
      setNewItemIds((prev) => new Set([...prev, ...freshIds]));

      setTimeout(() => {
        setNewItemIds((prev) => {
          const next = new Set(prev);
          freshIds.forEach((id) => next.delete(id));
          return next;
        });
      }, 2500);
    }
    prevCountRef.current = items.length;
  }, [items]);

  return (
    <div className={styles.section}>
      <div className={styles.sectionHeader}>
        <div className={`${styles.dot} ${config.dotClass}`} />
        <div className={styles.sectionLabel}>{config.label}</div>
        <div className={styles.count}>{items.length}</div>
      </div>
      {items.length === 0 ? (
        <div className={styles.empty}>{config.emptyText}</div>
      ) : (
        <div className={styles.items}>
          {items.map((item) => (
            <ItemWithNew
              key={item.id}
              item={item}
              config={config}
              onRemove={onRemove}
              readOnly={readOnly}
              isNew={newItemIds.has(item.id)}
            />
          ))}
        </div>
      )}
    </div>
  );
}
