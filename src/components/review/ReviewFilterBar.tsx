import type { ReviewAction, MissionAgentInfo } from "../../ipc/commands";
import styles from "./ReviewFilterBar.module.css";

export type ReviewFilter = "all" | "needs_review" | "approved";

interface ReviewFilterBarProps {
  filter: ReviewFilter;
  onFilterChange: (filter: ReviewFilter) => void;
  agents: MissionAgentInfo[];
  reviewStatuses: Record<string, ReviewAction | null>;
  totalFiles: number;
  onApproveAll: () => void;
  onMergeAll: () => void;
}

export function ReviewFilterBar({
  filter,
  onFilterChange,
  agents,
  reviewStatuses,
  totalFiles,
  onApproveAll,
  onMergeAll,
}: ReviewFilterBarProps) {
  const allCount = agents.length;
  const approvedCount = agents.filter(
    (a) => reviewStatuses[a.id] === "approved",
  ).length;
  const needsReviewCount = allCount - approvedCount;

  const tabs: { id: ReviewFilter; label: string; count: number }[] = [
    { id: "all", label: "All", count: allCount },
    { id: "needs_review", label: "Needs Review", count: needsReviewCount },
    { id: "approved", label: "Approved", count: approvedCount },
  ];

  return (
    <div className={styles.bar}>
      <div className={styles.tabs}>
        {tabs.map((tab) => (
          <button
            key={tab.id}
            className={`${styles.tab} ${filter === tab.id ? styles.tabActive : ""}`}
            onClick={() => onFilterChange(tab.id)}
            type="button"
          >
            {tab.label}
            <span className={styles.count}>{tab.count}</span>
          </button>
        ))}
      </div>

      <span className={styles.summary}>
        {totalFiles} files changed · {approvedCount} approved · {needsReviewCount} needs review
      </span>

      <div className={styles.actions}>
        <button
          className={styles.approveBtn}
          onClick={onApproveAll}
          disabled={needsReviewCount === 0}
          type="button"
        >
          Approve All
        </button>
        <button
          className={styles.mergeBtn}
          onClick={onMergeAll}
          type="button"
        >
          Merge All
        </button>
      </div>
    </div>
  );
}
