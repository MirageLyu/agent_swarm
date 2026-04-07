import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import type { MissionInfo, MissionStatus } from "../../ipc/commands";
import { Badge } from "../ui";
import styles from "./MissionListItem.module.css";

export type MissionAction = "delete" | "stop" | "restart_full" | "restart_failed";

interface MissionListItemProps {
  mission: MissionInfo;
  selected: boolean;
  onSelect: () => void;
  onAction: (action: MissionAction) => void;
}

const STATUS_VARIANT: Record<string, "default" | "success" | "warning" | "error" | "info"> = {
  draft: "default",
  planned: "info",
  running: "warning",
  completed: "success",
  failed: "error",
};

function getAvailableActions(status: MissionStatus): MissionAction[] {
  switch (status) {
    case "running":
      return ["stop"];
    case "completed":
      return ["restart_full", "delete"];
    case "failed":
      return ["restart_full", "restart_failed", "delete"];
    case "draft":
    case "planned":
      return ["delete"];
    default:
      return [];
  }
}

const ACTION_LABELS: Record<MissionAction, string> = {
  delete: "Delete",
  stop: "Stop",
  restart_full: "Re-run (Full)",
  restart_failed: "Re-run (Failed Only)",
};

export function MissionListItem({
  mission,
  selected,
  onSelect,
  onAction,
}: MissionListItemProps) {
  const actions = getAvailableActions(mission.status);

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
        {actions.length > 0 && (
          <DropdownMenu.Root>
            <DropdownMenu.Trigger asChild>
              <button
                className={styles.menuTrigger}
                onClick={(e) => e.stopPropagation()}
                title="Actions"
              >
                ···
              </button>
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content className={styles.menuContent} sideOffset={4} align="end">
                {actions.map((action) => (
                  <DropdownMenu.Item
                    key={action}
                    className={`${styles.menuItem} ${action === "delete" ? styles.menuDanger : ""}`}
                    onSelect={(e) => {
                      e.stopPropagation();
                      onAction(action);
                    }}
                  >
                    {ACTION_LABELS[action]}
                  </DropdownMenu.Item>
                ))}
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>
        )}
      </div>
    </div>
  );
}
