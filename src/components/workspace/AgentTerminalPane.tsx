import { memo, useEffect, useRef, useCallback } from "react";
import type { Agent, AgentEvent } from "../../stores/agent-store";
import styles from "./AgentTerminalPane.module.css";

const MAX_LINES = 1000;

const KIND_CLASS: Record<string, string> = {
  llm_call: styles.lineLlmCall,
  tool_use: styles.lineToolUse,
  tool_result: styles.lineToolResult,
  message: styles.lineMessage,
  error: styles.lineError,
  checkpoint: styles.lineCheckpoint,
  status_change: styles.lineStatusChange,
  note_applied: styles.lineNoteApplied,
};

interface AgentTerminalPaneProps {
  agent: Agent;
  taskTitle?: string;
  onFocus?: (agentId: string) => void;
  menuSlot?: React.ReactNode;
  compact?: boolean;
}

const TerminalLine = memo(function TerminalLine({
  event,
  isLast,
  isRunning,
}: {
  event: AgentEvent;
  isLast: boolean;
  isRunning: boolean;
}) {
  const cls = KIND_CLASS[event.kind] ?? styles.lineMessage;
  const cursorCls = isLast && isRunning ? ` ${styles.cursor}` : "";

  return (
    <div className={`${styles.line} ${cls}${cursorCls}`}>
      {event.content}
    </div>
  );
});

export const AgentTerminalPane = memo(function AgentTerminalPane({
  agent,
  taskTitle,
  onFocus,
  menuSlot,
  compact,
}: AgentTerminalPaneProps) {
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
      <div
        className={styles.terminal}
        ref={scrollRef}
        onScroll={handleScroll}
      >
        {trimmedEvents.length === 0 && !agent.streamBuffer && !agent.shellBuffer ? (
          <div className={styles.empty}>
            {isRunning ? "Waiting for output…" : "No events yet"}
          </div>
        ) : (
          <>
            {trimmedEvents.map((evt, i) => (
              <TerminalLine
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
