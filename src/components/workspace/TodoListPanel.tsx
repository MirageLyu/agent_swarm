import { memo, useMemo } from "react";
import type { AgentTodo } from "../../stores/agent-store";
import styles from "./TodoListPanel.module.css";

interface TodoListPanelProps {
  todos: AgentTodo[];
}

/// Single-Agent Uplift Phase 1.2: TodoListPanel
///
/// 渲染 agent 自维护的待办清单，对应 Cursor / Claude Code 的"正在做的事情"面板。
/// - 折叠在 AgentTerminalPane 顶部，不与事件流混排（混排时一闪即过）
/// - todos 为空时不渲染——避免给从来没用过 TodoWriteTool 的 agent 留空 panel
/// - 计数 + 进度条让用户一眼看到完成度
const STATUS_ORDER = {
  in_progress: 0,
  pending: 1,
  completed: 2,
  cancelled: 3,
} as const;

export const TodoListPanel = memo(function TodoListPanel({ todos }: TodoListPanelProps) {
  const sorted = useMemo(() => {
    // 保留原顺序，但 in_progress 上浮到第一项让用户一眼看到 "现在在干啥"
    const copy = [...todos];
    copy.sort((a, b) => STATUS_ORDER[a.status] - STATUS_ORDER[b.status]);
    return copy;
  }, [todos]);

  const counts = useMemo(() => {
    let completed = 0;
    let inProgress = 0;
    let pending = 0;
    for (const t of todos) {
      if (t.status === "completed") completed++;
      else if (t.status === "in_progress") inProgress++;
      else if (t.status === "pending") pending++;
    }
    return { completed, inProgress, pending, total: todos.length };
  }, [todos]);

  if (todos.length === 0) return null;

  const progress = counts.total > 0 ? (counts.completed / counts.total) * 100 : 0;

  return (
    <div className={styles.panel}>
      <div className={styles.header}>
        <span className={styles.title}>To-dos</span>
        <span className={styles.summary}>
          {counts.completed} / {counts.total}
        </span>
        <div className={styles.bar}>
          <div className={styles.barFill} style={{ width: `${progress}%` }} />
        </div>
      </div>
      <ul className={styles.list}>
        {sorted.map((t) => (
          <li key={t.id} className={styles.item} data-status={t.status}>
            <span className={styles.icon}>{statusIcon(t.status)}</span>
            <span className={styles.content}>{t.content}</span>
          </li>
        ))}
      </ul>
    </div>
  );
});

function statusIcon(status: AgentTodo["status"]): string {
  switch (status) {
    case "completed":
      return "✓";
    case "in_progress":
      return "▶";
    case "cancelled":
      return "−";
    case "pending":
    default:
      return "○";
  }
}
