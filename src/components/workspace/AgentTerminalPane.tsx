import { memo, useEffect, useMemo, useRef, useCallback } from "react";
import { useTranslation } from "react-i18next";
import type { Agent } from "../../stores/agent-store";
import styles from "./AgentTerminalPane.module.css";
import {
  CollapsedReadGroup,
  EventLine,
  groupReadOnlyEvents,
} from "./event-renderers";
import { TodoListPanel } from "./TodoListPanel";
import { ThinkingIndicator } from "./ThinkingIndicator";

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

  // 用户正在拖选/已选中文本时，绝不能 auto-scroll —— 一滚动 selection 就丢，
  // 这是终端"选不了字"的最常见根因（事件 1Hz+ 流入，每次都把选区清掉）。
  // 检测 window.getSelection() 是否落在我们这个容器内且非空。
  const hasActiveSelectionInTerminal = useCallback(() => {
    const el = scrollRef.current;
    if (!el || typeof window === "undefined") return false;
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0 || sel.isCollapsed) return false;
    if (!sel.toString().trim()) return false;
    // anchorNode 落在终端 DOM 内才算
    const anchor = sel.anchorNode;
    return anchor !== null && el.contains(anchor);
  }, []);

  useEffect(() => {
    if (
      isAtBottomRef.current &&
      scrollRef.current &&
      !hasActiveSelectionInTerminal()
    ) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [
    agent.events.length,
    agent.streamBuffer,
    agent.shellBuffer,
    agent.reasoningStartedAt,
    agent.reasoningBuffer.length,
    hasActiveSelectionInTerminal,
  ]);

  const trimmedEvents = useMemo(
    () =>
      agent.events.length > MAX_LINES
        ? agent.events.slice(-MAX_LINES)
        : agent.events,
    [agent.events],
  );

  // A3 collapseReadSearch: 把连续 ≥2 个只读 ops 折叠成单行 group
  const eventGroups = useMemo(
    () => groupReadOnlyEvents(trimmedEvents),
    [trimmedEvents],
  );

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
        {trimmedEvents.length === 0 &&
        !agent.streamBuffer &&
        !agent.shellBuffer &&
        agent.reasoningStartedAt === null ? (
          <div className={styles.empty}>
            {isRunning ? t("terminal.waitingForOutput") : t("terminal.noEventsYet")}
          </div>
        ) : (
          <>
            {eventGroups.map((g, i) => {
              const isLastGroup = i === eventGroups.length - 1;
              if (g.kind === "group") {
                // 折叠组：用第一个 event 的 id 做 key（稳定且唯一）
                return (
                  <CollapsedReadGroup
                    key={`grp-${g.events[0].id}`}
                    events={g.events}
                  />
                );
              }
              return (
                <EventLine
                  key={g.event.id}
                  event={g.event}
                  isLast={
                    isLastGroup &&
                    !agent.streamBuffer &&
                    !agent.shellBuffer &&
                    agent.reasoningStartedAt === null
                  }
                  isRunning={isRunning}
                />
              );
            })}
            {/* 推理模型 thinking 占位：放在 streamBuffer 之前——thinking 永远先于
                正式 output 抵达，第一个 text_delta 来时 store 自动清空 reasoning。*/}
            {agent.reasoningStartedAt !== null && (
              <ThinkingIndicator
                startedAt={agent.reasoningStartedAt}
                chars={agent.reasoningBuffer.length}
              />
            )}
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
