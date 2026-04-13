import { useState, useCallback, useRef, useEffect } from "react";
import Markdown from "react-markdown";
import type { PreflightMode, PreflightChoice, PreflightMessageInfo } from "../../ipc/commands";
import { PreflightModeSwitch } from "./PreflightModeSwitch";
import { ChatMessage } from "./ChatMessage";
import { ChoiceButtons } from "./ChoiceButtons";
import styles from "./PreflightChat.module.css";

interface PreflightChatProps {
  messages: PreflightMessageInfo[];
  mode: PreflightMode;
  streaming: boolean;
  streamingText: string;
  statusText?: string;
  initialLoading?: boolean;
  onSend: (text: string) => void;
  onModeChange: (mode: PreflightMode) => void;
  onChoiceSelect: (choice: PreflightChoice) => void;
}

export function PreflightChat({
  messages,
  mode,
  streaming,
  streamingText,
  statusText,
  initialLoading,
  onSend,
  onModeChange,
  onChoiceSelect,
}: PreflightChatProps) {
  const [input, setInput] = useState("");
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const scrollToBottom = useCallback(() => {
    requestAnimationFrame(() => {
      messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
    });
  }, []);

  useEffect(scrollToBottom, [messages, streamingText, scrollToBottom]);

  const handleSend = useCallback(() => {
    const text = input.trim();
    if (!text || streaming) return;
    onSend(text);
    setInput("");
  }, [input, streaming, onSend]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [handleSend],
  );

  const showTypingIndicator = (streaming && !streamingText) || initialLoading;
  const typingLabel = statusText || "思考中…";

  return (
    <div className={styles.panel}>
      <PreflightModeSwitch mode={mode} onModeChange={onModeChange} />

      <div className={styles.messages}>
        {messages.map((msg, i) => {
          const isDivider = msg.role === "assistant"
            && msg.content.startsWith("──")
            && msg.content.endsWith("──");

          if (isDivider) {
            return (
              <div key={i} className={styles.divider}>
                <span>{msg.content}</span>
              </div>
            );
          }

          if (msg.role === "assistant" && !msg.content.trim() && msg.choices.length === 0) {
            return null;
          }

          return (
            <div key={i}>
              {msg.content.trim() && (
                <ChatMessage
                  role={msg.role as "user" | "assistant"}
                  content={msg.content}
                  mode={msg.mode}
                />
              )}
              {msg.role === "assistant" && msg.choices.length > 0 && (
                <ChoiceButtons
                  choices={msg.choices}
                  onSelect={onChoiceSelect}
                  disabled={i < messages.length - 1}
                />
              )}
            </div>
          );
        })}

        {streaming && streamingText && (
          <div className={styles.streamingText}>
            <div className={styles.streamingLabel}>Swarm Agent</div>
            <div className={styles.streamingBubble}>
              <Markdown>{streamingText}</Markdown>
              <span className={styles.streamEllipsis} />
            </div>
          </div>
        )}

        {showTypingIndicator ? (
          <div className={styles.typing}>
            <div className={styles.typingDots}>
              <div className={styles.typingDot} />
              <div className={styles.typingDot} />
              <div className={styles.typingDot} />
            </div>
            <span className={styles.typingLabel}>{typingLabel}</span>
          </div>
        ) : null}

        <div ref={messagesEndRef} />
      </div>

      <div className={styles.inputArea}>
        <div className={styles.inputWrapper}>
          <input
            className={styles.input}
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="输入回复，或点击选项…"
            disabled={streaming}
          />
          <button
            className={styles.sendBtn}
            onClick={handleSend}
            disabled={!input.trim() || streaming}
            title="发送"
          >
            <svg viewBox="0 0 20 20" fill="currentColor">
              <path d="M3.105 2.29a.75.75 0 0 1 .814-.12l13.5 6.5a.75.75 0 0 1 0 1.36l-13.5 6.5a.75.75 0 0 1-1.06-.86L4.87 10.5H10a.5.5 0 0 0 0-1H4.87L2.86 3.17a.75.75 0 0 1 .246-.88Z" />
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
}
