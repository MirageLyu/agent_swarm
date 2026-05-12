/**
 * 应用级 Planner 进度浮窗。
 *
 * 由 App 根挂载，监听 `planner-progress-store`。三态：
 * 1. `active` 非空 → 渲染 PlannerLoopPanel(floating)，事件流持续。
 * 2. `completed` 非空 → 渲染一个极简成功卡片，提供"查看任务图"按钮跳 DAG。
 * 3. 都为空 → 不渲染（return null）。
 *
 * 切 view 时本组件不会 unmount（位于 App 根而非 ActiveView 子树），
 * 解决了之前"切走浮窗消失"的体验问题。
 */
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { useUiStore } from "../../stores/ui-store";
import { useTaskStore } from "../../stores/task-store";
import { usePlannerProgressStore } from "../../stores/planner-progress-store";
import { PlannerLoopPanel } from "../mission/PlannerLoopPanel";
import styles from "./PlannerProgressOverlay.module.css";

export function PlannerProgressOverlay() {
  const { t } = useTranslation("preflight");
  const active = usePlannerProgressStore((s) => s.active);
  const completed = usePlannerProgressStore((s) => s.completed);
  const clear = usePlannerProgressStore((s) => s.clear);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const selectMission = useTaskStore((s) => s.selectMission);

  if (active) {
    return (
      <PlannerLoopPanel
        sessionId={active.sessionId}
        label={active.label}
        floating
        isLive
      />
    );
  }

  if (completed) {
    const handleGoToDag = () => {
      selectMission(completed.missionId);
      setActiveView("missions");
      clear();
    };

    return createPortal(
      <div
        className={styles.successCard}
        role="status"
        aria-live="polite"
      >
        <div className={styles.successHeader}>
          <span className={styles.successIcon} aria-hidden>✓</span>
          <span className={styles.successTitle}>{t("plannerOverlay.completedTitle")}</span>
          <button
            className={styles.dismissBtn}
            onClick={clear}
            aria-label={t("plannerOverlay.dismiss")}
            title={t("plannerOverlay.dismiss")}
          >
            ×
          </button>
        </div>
        <div className={styles.successBody}>
          {t("plannerOverlay.completedBody")}
        </div>
        <button className={styles.primaryBtn} onClick={handleGoToDag}>
          {t("plannerOverlay.viewDag")}
        </button>
      </div>,
      document.body,
    );
  }

  return null;
}
