import { useState, useCallback } from "react";
import { useTranslation } from "react-i18next";
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
    case "preflight":
      return ["delete"];
    default:
      return [];
  }
}

const ACTION_LABEL_KEYS: Record<MissionAction, string> = {
  delete: "deleteMission",
  stop: "stopMission",
  restart_full: "restartFull",
  restart_failed: "restartFailed",
  export: "exportTemplate",
  view_report: "viewReport",
};

export function MissionListItem({
  mission,
  selected,
  onSelect,
  onAction,
}: MissionListItemProps) {
  const { t } = useTranslation("mission");
  const { t: tc } = useTranslation("common");
  const actions = getAvailableActions(mission.status);
  const [promptOpen, setPromptOpen] = useState(false);
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(mission.description).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    }).catch(() => {});
  }, [mission.description]);

  const progressText = t("tasksProgress", {
    done: mission.completed_count,
    total: mission.task_count,
  });

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
                title={t("actionsMenu")}
              >
                ···
              </button>
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              {/* React 合成事件沿组件树冒泡，会穿过 Portal 抵达父 onClick={onSelect}。
                  必须在 Content 上 stopPropagation，否则点菜单项会同时触发 row 选中
                  （preflight 状态尤其明显——会跳转到 Preflight 视图）。 */}
              <DropdownMenu.Content
                className={styles.menuContent}
                sideOffset={4}
                align="end"
                onClick={(e) => e.stopPropagation()}
              >
                <DropdownMenu.Item
                  className={styles.menuItem}
                  onSelect={() => setPromptOpen(true)}
                >
                  {t("showPrompt")}
                </DropdownMenu.Item>

                {actions.length > 0 && (
                  <DropdownMenu.Separator className={styles.menuSeparator} />
                )}

                {actions.map((action) => (
                  <DropdownMenu.Item
                    key={action}
                    className={`${styles.menuItem} ${action === "delete" ? styles.menuDanger : ""}`}
                    onSelect={() => onAction(action)}
                  >
                    {t(ACTION_LABEL_KEYS[action])}
                  </DropdownMenu.Item>
                ))}
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>
        </div>
      </div>

      <Dialog.Root open={promptOpen} onOpenChange={setPromptOpen}>
        <Dialog.Portal>
          {/* 同 DropdownMenu.Portal：React 合成事件会穿 Portal 冒泡，
              overlay/content 都要拦截 onClick 防止误触发 row 选中。 */}
          <Dialog.Overlay className={styles.promptOverlay} onClick={(e) => e.stopPropagation()} />
          <Dialog.Content className={styles.promptContent} onClick={(e) => e.stopPropagation()}>
            <Dialog.Title className={styles.promptTitle}>{t("missionPrompt")}</Dialog.Title>
            <pre className={styles.promptText}>
              {mission.description || t("noDescription")}
            </pre>
            <div className={styles.promptActions}>
              <Button variant="ghost" size="sm" onClick={() => setPromptOpen(false)}>
                {tc("close")}
              </Button>
              <Button variant="primary" size="sm" onClick={handleCopy}>
                {copied ? tc("copied") : tc("copy")}
              </Button>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </>
  );
}
