/**
 * FM-15 v2.2 (S4): 任务角色徽章。
 *
 * 在 DAG 节点和任务详情侧栏复用：
 * - 紧凑模式 (`compact`)：仅显示 emoji 图标 + 1-2 个字符的角色缩写，DAG 节点用。
 * - 完整模式：emoji + Display Name，详情面板用。
 *
 * 颜色完全跟随后端 RoleRegistry 的 `ui_color`，保持视觉一致。
 */

import { getRoleMeta } from "./task-meta";
import styles from "./RoleBadge.module.css";

interface RoleBadgeProps {
  role?: string | null;
  /** 紧凑模式：仅 emoji + 大写首字母。默认 false。 */
  compact?: boolean;
  /** 自定义额外 className。 */
  className?: string;
}

export function RoleBadge({ role, compact = false, className }: RoleBadgeProps) {
  const meta = getRoleMeta(role);
  const cls = [styles.badge, compact ? styles.compact : "", className ?? ""]
    .filter(Boolean)
    .join(" ");
  return (
    <span
      className={cls}
      style={{
        // 半透明色板：背景用 12% alpha，文字/边框用纯色
        backgroundColor: `${meta.uiColor}1f`,
        borderColor: `${meta.uiColor}80`,
        color: meta.uiColor,
      }}
      title={meta.displayName}
      data-role={meta.id}
    >
      <span className={styles.icon} aria-hidden>
        {meta.uiIcon}
      </span>
      {!compact && <span className={styles.label}>{meta.displayName}</span>}
    </span>
  );
}
