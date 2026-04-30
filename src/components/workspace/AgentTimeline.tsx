import { memo, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Badge } from "../ui/Badge";
import { InterventionPanel } from "./InterventionPanel";
import type { Agent, AgentEvent } from "../../stores/agent-store";
import styles from "./AgentTimeline.module.css";

interface AgentTimelineProps {
  agent: Agent;
  onBack: () => void;
}

const KIND_BADGE_VARIANT = {
  error: "error",
  checkpoint: "warning",
  tool_use: "info",
  tool_result: "info",
  status_change: "default",
  llm_call: "default",
  message: "success",
} as const;

const ERROR_KINDS = new Set(["error"]);
const HIGHLIGHT_KINDS = new Set(["error", "status_change"]);

function formatTimestamp(ts: string): string {
  const d = new Date(ts);
  if (isNaN(d.getTime())) return ts;
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function parseCheckpointTokens(content: string): { input: number; output: number } | null {
  const match = content.match(/(\d+)\s*in\s*\/\s*(\d+)\s*out/i)
    || content.match(/input[:\s]*(\d+).*?output[:\s]*(\d+)/i);
  if (!match) return null;
  return { input: parseInt(match[1], 10), output: parseInt(match[2], 10) };
}

const TimelineEvent = memo(function TimelineEvent({ event }: { event: AgentEvent }) {
  const isError = ERROR_KINDS.has(event.kind);
  const isHighlight = HIGHLIGHT_KINDS.has(event.kind);
  const tokens = event.kind === "checkpoint" ? parseCheckpointTokens(event.content) : null;

  return (
    <div
      className={`${styles.event} ${isError ? styles.eventError : ""} ${isHighlight ? styles.eventHighlight : ""}`}
    >
      <div className={styles.eventDot} data-kind={event.kind} />
      <div className={styles.eventBody}>
        <div className={styles.eventHeader}>
          <Badge variant={KIND_BADGE_VARIANT[event.kind] ?? "default"}>
            Step {event.step} · {event.kind}
          </Badge>
          <span className={styles.eventTime}>{formatTimestamp(event.timestamp)}</span>
          {tokens && (
            <span className={styles.eventTokens}>
              {tokens.input}↓ / {tokens.output}↑
            </span>
          )}
        </div>
        <pre className={styles.eventContent}>{event.content}</pre>
      </div>
    </div>
  );
});

export const AgentTimeline = memo(function AgentTimeline({
  agent,
  onBack,
}: AgentTimelineProps) {
  const { t } = useTranslation("workspace");
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [agent.events.length, agent.streamBuffer]);

  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <button className={styles.backButton} onClick={onBack} type="button">
          ← {t("timeline.back")}
        </button>
        <div className={styles.agentMeta}>
          <span className={styles.statusDot} data-status={agent.status} />
          <span className={styles.agentName}>{agent.name}</span>
          <Badge
            variant={
              agent.status === "completed"
                ? "success"
                : agent.status === "failed"
                  ? "error"
                  : agent.status === "cancelled"
                    ? "warning"
                    : "info"
            }
          >
            {agent.status}
          </Badge>
        </div>
        <div className={styles.agentStats}>
          <span className={styles.stat}>{t("timeline.stat.step", { n: agent.currentStep })}</span>
          <span className={styles.stat}>{t("timeline.stat.tokens", { count: agent.tokensUsed })}</span>
          <span className={styles.stat}>${agent.costUsd.toFixed(4)}</span>
        </div>
      </div>

      <div className={styles.timeline} ref={scrollRef}>
        {agent.events.map((evt) => (
          <TimelineEvent key={evt.id} event={evt} />
        ))}

        {agent.streamBuffer && (
          <div className={styles.streamingBlock}>
            <pre className={styles.streamText}>{agent.streamBuffer}</pre>
            <span className={styles.cursor} />
          </div>
        )}

        {agent.events.length === 0 && !agent.streamBuffer && (
          <div className={styles.empty}>{t("timeline.noEvents")}</div>
        )}
      </div>

      <InterventionPanel agentId={agent.id} agentStatus={agent.status} />
    </div>
  );
});
