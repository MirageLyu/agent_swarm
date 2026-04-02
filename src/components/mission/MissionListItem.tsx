import type { MissionInfo } from "../../ipc/commands";
import { Badge } from "../ui";
import styles from "./MissionListItem.module.css";

interface MissionListItemProps {
  mission: MissionInfo;
  selected: boolean;
  onSelect: () => void;
  onDelete: () => void;
}

const STATUS_VARIANT: Record<string, "default" | "success" | "warning" | "error" | "info"> = {
  draft: "default",
  planned: "info",
  running: "warning",
  completed: "success",
  failed: "error",
};

export function MissionListItem({
  mission,
  selected,
  onSelect,
  onDelete,
}: MissionListItemProps) {
  const canDelete = mission.status === "draft";

  return (
    <div
      className={`${styles.item} ${selected ? styles.selected : ""}`}
      onClick={onSelect}
    >
      <div className={styles.top}>
        <span className={styles.title}>{mission.title}</span>
        <Badge variant={STATUS_VARIANT[mission.status] ?? "default"}>
          {mission.status}
        </Badge>
      </div>
      <div className={styles.bottom}>
        <span className={styles.meta}>
          {mission.completed_count}/{mission.task_count} tasks
        </span>
        <span className={styles.meta}>
          {new Date(mission.created_at + "Z").toLocaleDateString()}
        </span>
        {canDelete && (
          <button
            className={styles.deleteBtn}
            onClick={(e) => {
              e.stopPropagation();
              onDelete();
            }}
            title="Delete mission"
          >
            &times;
          </button>
        )}
      </div>
    </div>
  );
}
