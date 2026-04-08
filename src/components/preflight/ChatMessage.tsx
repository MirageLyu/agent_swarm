import styles from "./ChatMessage.module.css";

interface ChatMessageProps {
  role: "user" | "assistant";
  content: string;
}

export function ChatMessage({ role, content }: ChatMessageProps) {
  const isUser = role === "user";
  const className = `${styles.message} ${isUser ? styles.user : styles.agent}`;

  return (
    <div className={className}>
      <div className={styles.label}>{isUser ? "你" : "Swarm Agent"}</div>
      <div className={styles.bubble}>{content}</div>
    </div>
  );
}
