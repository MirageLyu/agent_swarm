/**
 * FM-14: 全局审批中心。
 *
 * 由 App 根挂一份；自身负责：
 * 1. 应用启动时一次性 init 订阅事件流；
 * 2. 一个浮在右上角的圆形按钮（带 pending count badge）；点击展开侧边 drawer；
 * 3. drawer 内列出当前所有 pending 审批，按 created_at 升序——下方旧、上方新。
 *
 * 任何 view 都可以通过 useApprovalStore 获取数据，但 ApprovalCenter 是唯一
 * 负责"事件订阅"的组件，避免重复订阅 / 内存泄露。
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApprovalStore } from "../../stores/approval-store";
import { ApprovalCard } from "./ApprovalCard";
import styles from "./ApprovalCenter.module.css";

export function ApprovalCenter() {
  const { t } = useTranslation("approval");
  const { t: tn } = useTranslation("nav");
  const { t: tc } = useTranslation("common");
  const { init, dispose, items, error } = useApprovalStore();
  const [open, setOpen] = useState(false);

  useEffect(() => {
    void init();
    return () => {
      void dispose();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const count = items.length;

  return (
    <>
      <button
        className={`${styles.toggle} ${open ? styles.active : ""}`}
        onClick={() => setOpen((v) => !v)}
        aria-label={`${tn("approvals")} (${tn("approvalsBadge", { count })})`}
        title={`${tn("approvals")} — ${tn("approvalsBadge", { count })}`}
      >
        <BellIcon />
        {count > 0 && (
          <span className={styles.badgePill}>{count > 99 ? "99+" : count}</span>
        )}
      </button>

      {open && (
        <>
          <div className={styles.overlay} onClick={() => setOpen(false)} />
          <aside className={styles.drawer} role="dialog" aria-label={t("centerTitle")}>
            <div className={styles.drawerHeader}>
              <h2 className={styles.drawerTitle}>{t("centerTitle")}</h2>
              <button
                className={styles.drawerClose}
                onClick={() => setOpen(false)}
                aria-label={tc("close")}
              >
                ×
              </button>
            </div>
            <div className={styles.drawerBody}>
              {error && (
                <p className={styles.errorBanner}>{tc("errorPrefix", { message: error })}</p>
              )}
              {items.length === 0 ? (
                <p className={styles.empty}>{t("centerEmpty")}</p>
              ) : (
                items.map((it) => <ApprovalCard key={it.id} approval={it} />)
              )}
            </div>
          </aside>
        </>
      )}
    </>
  );
}

function BellIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 18 18" fill="none">
      <path
        d="M9 2v.6M5 7.5a4 4 0 0 1 8 0v3l1.2 2.2H3.8L5 10.5v-3z"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinejoin="round"
      />
      <path
        d="M7.2 14a1.8 1.8 0 0 0 3.6 0"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}
