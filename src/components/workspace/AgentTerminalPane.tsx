import { memo, useEffect, useRef, useCallback } from "react";
import { useTranslation } from "react-i18next";
import type { Agent } from "../../stores/agent-store";
import styles from "./AgentTerminalPane.module.css";
import { EventLine } from "./event-renderers";
import { TodoListPanel } from "./TodoListPanel";

const MAX_LINES = 1000;

interface AgentTerminalPaneProps {
  agent: Agent;
  taskTitle?: string;
  onFocus?: (agentId: string) => void;
  menuSlot?: React.ReactNode;
  compact?: boolean;
}

export const AgentTerminalPane = memo(function AgentTerminalPane({
  agent,
  taskTitle,
  onFocus,
  menuSlot,
  compact,
}: AgentTerminalPaneProps) {
  const { t } = useTranslation("workspace");
  const scrollRef = useRef<HTMLDivElement>(null);
  const isAtBottomRef = useRef(true);

  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const threshold = 40;
    isAtBottomRef.current =
      el.scrollHeight - el.scrollTop - el.clientHeight < threshold;
  }, []);

  useEffect(() => {
    if (isAtBottomRef.current && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [agent.events.length, agent.streamBuffer, agent.shellBuffer]);

  const trimmedEvents =
    agent.events.length > MAX_LINES
      ? agent.events.slice(-MAX_LINES)
      : agent.events;

  const isRunning = agent.status === "running";

  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <span className={styles.statusDot} data-status={agent.status} />
        <span className={styles.agentName}>{taskTitle || agent.name}</span>
        <span className={styles.taskName}>{agent.id.substring(0, 8)}</span>
        <span className={styles.statusBadge} data-status={agent.status}>
          {agent.status}
        </span>
        <div className={styles.headerActions}>
          {!compact && onFocus && (
            <button
              className={styles.focusBtn}
              onClick={() => onFocus(agent.id)}
              type="button"
            >
              ⌃F
            </button>
          )}
          {menuSlot}
        </div>
      </div>
      {/* Single-Agent Uplift Phase 1.2: TodoListPanel 渲染在 header 与事件流之间，
          固定不滚动；Agent 没用过 TodoWriteTool 时返回 null 不占空间。 */}
      <TodoListPanel todos={agent.todos} />
      <div
        className={styles.terminal}
        ref={scrollRef}
        onScroll={handleScroll}
      >
        {trimmedEvents.length === 0 && !agent.streamBuffer && !agent.shellBuffer ? (
          <div className={styles.empty}>
            {isRunning ? t("terminal.waitingForOutput") : t("terminal.noEventsYet")}
          </div>
        ) : (
          <>
            {trimmedEvents.map((evt, i) => (
              <EventLine
                key={evt.id}
                event={evt}
                isLast={
                  i === trimmedEvents.length - 1 &&
                  !agent.streamBuffer &&
                  !agent.shellBuffer
                }
                isRunning={isRunning}
              />
            ))}
            {agent.streamBuffer && (
              <div
                className={`${styles.streamBlock} ${
                  isRunning && !agent.shellBuffer ? styles.cursor : ""
                }`}
              >
                {agent.streamBuffer}
              </div>
            )}
            {agent.shellBuffer && (
              <div className={`${styles.shellBlock} ${isRunning ? styles.cursor : ""}`}>
                <div className={styles.shellBlockHeader}>shell</div>
                <pre className={styles.shellBlockBody}>{agent.shellBuffer}</pre>
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
});
