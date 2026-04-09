import { useCallback, useEffect, useRef, useState } from "react";
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

  const itemsRef = useRef<HTMLDivElement>(null);
  const rafRef = useRef(0);
  const [hiddenAbove, setHiddenAbove] = useState(0);
  const [hiddenBelow, setHiddenBelow] = useState(0);

  // Track newly added items for the NEW badge
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

  // Count items hidden above / below the visible scroll area
  const updateHiddenCounts = useCallback(() => {
    const el = itemsRef.current;
    if (!el || items.length === 0) {
      setHiddenAbove(0);
      setHiddenBelow(0);
      return;
    }

    const rect = el.getBoundingClientRect();
    let above = 0;
    let below = 0;

    for (let i = 0; i < el.children.length; i++) {
      const childRect = (el.children[i] as HTMLElement).getBoundingClientRect();
      if (childRect.bottom <= rect.top + 1) above++;
      else if (childRect.top >= rect.bottom - 1) below++;
    }

    setHiddenAbove(above);
    setHiddenBelow(below);
  }, [items.length]);

  const handleScroll = useCallback(() => {
    cancelAnimationFrame(rafRef.current);
    rafRef.current = requestAnimationFrame(updateHiddenCounts);
  }, [updateHiddenCounts]);

  // Recalculate when items change (additions / removals)
  useEffect(() => {
    requestAnimationFrame(updateHiddenCounts);
  }, [items.length, updateHiddenCounts]);

  useEffect(() => {
    return () => cancelAnimationFrame(rafRef.current);
  }, []);

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
        <>
          {hiddenAbove > 0 && (
            <div className={`${styles.moreIndicator} ${styles.moreAbove}`}>
              ↑ {hiddenAbove} more {hiddenAbove === 1 ? "item" : "items"}
            </div>
          )}

          <div ref={itemsRef} className={styles.items} onScroll={handleScroll}>
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

          {hiddenBelow > 0 && (
            <div className={`${styles.moreIndicator} ${styles.moreBelow}`}>
              ↓ {hiddenBelow} more {hiddenBelow === 1 ? "item" : "items"}
            </div>
          )}
        </>
      )}
    </div>
  );
}
