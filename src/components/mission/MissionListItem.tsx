import { useState, useCallback } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import type { MissionInfo, MissionStatus } from "../../ipc/commands";
import { Badge, Button } from "../ui";
import styles from "./MissionListItem.module.css";

export type MissionAction =
  | "delete"
  | "stop"
  | "restart_full"
  | "restart_failed"
  | "export"
  | "view_report";

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
      return ["export", "stop"];
    case "completed":
      return ["view_report", "export", "restart_full", "delete"];
    case "failed":
      return ["view_report", "export", "restart_full", "restart_failed", "delete"];
    case "draft":
    case "planned":
      return ["export", "delete"];
    default:
      return [];
  }
}

const ACTION_LABELS: Record<MissionAction, string> = {
  delete: "Delete",
  stop: "Stop",
  restart_full: "Re-run (Full)",
  restart_failed: "Re-run (Failed Only)",
  export: "Export Template",
  view_report: "View Report",
};

export function MissionListItem({
  mission,
  selected,
  onSelect,
  onAction,
}: MissionListItemProps) {
  const actions = getAvailableActions(mission.status);
  const [promptOpen, setPromptOpen] = useState(false);
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(mission.description).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    }).catch(() => {});
  }, [mission.description]);

  const progressText = `${mission.completed_count}/${mission.task_count} tasks completed`;

  return (
    <>
      <div
        className={`${styles.item} ${selected ? styles.selected : ""}`}
        onClick={onSelect}
        title={mission.title}
      >
        <div className={styles.top}>
          <span className={styles.title}>{mission.title}</span>
          <Badge variant={STATUS_VARIANT[mission.status] ?? "default"}>
            {mission.status}
          </Badge>
        </div>
        <div className={styles.bottom}>
          <span className={styles.meta}>{progressText}</span>
          <span className={styles.meta}>
            {new Date(mission.created_at + "Z").toLocaleDateString()}
          </span>
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
                <DropdownMenu.Item
                  className={styles.menuItem}
                  onSelect={(e) => {
                    e.stopPropagation();
                    setPromptOpen(true);
                  }}
                >
                  Show Prompt
                </DropdownMenu.Item>

                {actions.length > 0 && (
                  <DropdownMenu.Separator className={styles.menuSeparator} />
                )}

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
        </div>
      </div>

      <Dialog.Root open={promptOpen} onOpenChange={setPromptOpen}>
        <Dialog.Portal>
          <Dialog.Overlay className={styles.promptOverlay} />
          <Dialog.Content className={styles.promptContent} onClick={(e) => e.stopPropagation()}>
            <Dialog.Title className={styles.promptTitle}>Mission Prompt</Dialog.Title>
            <pre className={styles.promptText}>
              {mission.description || "无描述"}
            </pre>
            <div className={styles.promptActions}>
              <Button variant="ghost" size="sm" onClick={() => setPromptOpen(false)}>
                Close
              </Button>
              <Button variant="primary" size="sm" onClick={handleCopy}>
                {copied ? "Copied!" : "Copy"}
              </Button>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </>
  );
}
