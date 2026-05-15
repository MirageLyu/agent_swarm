import { memo } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import styles from "./EventLine.module.css";

interface SystemHintLineProps {
  event: AgentEvent;
}

/// 系统注入的提示（"剩 5 步"、"读太多没改过文件"、"idle 重试"等）。
/// 用黄色边框 + 图标突出，避免被淹没在 tool_use 流里。
export const SystemHintLine = memo(function SystemHintLine({
  event,
}: SystemHintLineProps) {
  // 后端拼了 "[System] " 前缀的话去掉，免得视觉重复
  const text = event.content.replace(/^\[System\]\s*/, "");
  return (
    <div className={styles.systemHint}>
      <span className={styles.systemHintLabel}>System Hint</span>
      <span>{text}</span>
    </div>
  );
});
