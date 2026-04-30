import Markdown from "react-markdown";
import { useTranslation } from "react-i18next";
import type { PreflightMode } from "../../ipc/commands";
import styles from "./ChatMessage.module.css";

interface ChatMessageProps {
  role: "user" | "assistant";
  content: string;
  mode?: PreflightMode;
}

const MODE_STYLE: Record<string, string> = {
  scenario_walk: styles.modeScenario,
  devils_advocate: styles.modeDevil,
  risk_highlighter: styles.modeRisk,
};

export function ChatMessage({ role, content, mode }: ChatMessageProps) {
  const { t } = useTranslation("preflight");
  const isUser = role === "user";
  const modeClass = !isUser && mode ? (MODE_STYLE[mode] ?? "") : "";
  const className = `${styles.message} ${isUser ? styles.user : styles.agent} ${modeClass}`;

  return (
    <div className={className}>
      <div className={styles.label}>
        {isUser ? t("userLabel") : t("agentLabel")}
        {!isUser && mode && mode !== "scenario_walk" && (
          <span className={styles.modeBadge}>{t(`modeLabel.${mode}`)}</span>
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
