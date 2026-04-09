import Markdown from "react-markdown";
import type { PreflightMode } from "../../ipc/commands";
import styles from "./ChatMessage.module.css";

interface ChatMessageProps {
  role: "user" | "assistant";
  content: string;
  mode?: PreflightMode;
}

const MODE_LABEL: Record<string, string> = {
  scenario_walk: "场景走查",
  devils_advocate: "魔鬼代言人",
  risk_highlighter: "风险标记",
};

const MODE_STYLE: Record<string, string> = {
  scenario_walk: styles.modeScenario,
  devils_advocate: styles.modeDevil,
  risk_highlighter: styles.modeRisk,
};

export function ChatMessage({ role, content, mode }: ChatMessageProps) {
  const isUser = role === "user";
  const modeClass = !isUser && mode ? (MODE_STYLE[mode] ?? "") : "";
  const className = `${styles.message} ${isUser ? styles.user : styles.agent} ${modeClass}`;

  return (
    <div className={className}>
      <div className={styles.label}>
        {isUser ? "你" : "Swarm Agent"}
        {!isUser && mode && mode !== "scenario_walk" && (
          <span className={styles.modeBadge}>{MODE_LABEL[mode]}</span>
        )}
      </div>
      <div className={styles.bubble}>
        {isUser ? content : (
          <div className={styles.markdown}>
            <Markdown>{content}</Markdown>
          </div>
        )}
      </div>
    </div>
  );
}
