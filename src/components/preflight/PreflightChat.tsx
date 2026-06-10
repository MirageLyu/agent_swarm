import { useState, useCallback, useRef, useEffect } from "react";
import Markdown from "react-markdown";
import { useTranslation } from "react-i18next";
import type { PreflightMode, PreflightChoice, PreflightMessageInfo } from "../../ipc/commands";
import { PreflightModeSwitch } from "./PreflightModeSwitch";
import { ChatMessage } from "./ChatMessage";
import { ChoiceButtons } from "./ChoiceButtons";
import { ReasoningPanel } from "./ReasoningPanel";
import styles from "./PreflightChat.module.css";

interface PreflightChatProps {
  messages: PreflightMessageInfo[];
  mode: PreflightMode;
  streaming: boolean;
  streamingText: string;
  streamingReasoning?: string;
  statusText?: string;
  initialLoading?: boolean;
  onSend: (text: string) => void;
  onModeChange: (mode: PreflightMode) => void;
  onChoiceSelect: (choice: PreflightChoice) => void;
  /** 用户点击"重试"时调用，复用 stored_msgs 里最后一条失败的 user 消息。 */
  onRetry?: () => void;
}

export function PreflightChat({
  messages,
  mode,
  streaming,
  streamingText,
  streamingReasoning,
  statusText,
  initialLoading,
  onSend,
  onModeChange,
  onChoiceSelect,
  onRetry,
}: PreflightChatProps) {
  const { t } = useTranslation("preflight");
  const [input, setInput] = useState("");
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const streamingStartRef = useRef<number | null>(null);

  const scrollToBottom = useCallback(() => {
    requestAnimationFrame(() => {
      messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
    });
  }, []);

  useEffect(scrollToBottom, [messages, streamingText, streamingReasoning, scrollToBottom]);

  // Track when streaming reasoning starts for the timer
  useEffect(() => {
    if (streamingReasoning && !streamingStartRef.current) {
      streamingStartRef.current = Date.now();
    }
    if (!streamingReasoning) {
      streamingStartRef.current = null;
    }
  }, [streamingReasoning]);

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

  const showTypingIndicator = (streaming && !streamingText && !streamingReasoning) || initialLoading;
  const typingLabel = statusText || t("thinking");

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

          // 失败 + 重试 UI 只挂在"最后一条 user 消息"上：
          // 旧的失败消息可能因为后续重试被覆盖语义，但 stored_msgs 不会回卷
          // failed 标记。所以约定：能点击重试的只有最新的失败消息。
          const isLastUserMessage =
            msg.role === "user" &&
            i === messages.length - 1 &&
            !!msg.failed;

          return (
            <div key={i}>
              {msg.content.trim() && (
                <ChatMessage
                  role={msg.role as "user" | "assistant"}
                  content={msg.content}
                  mode={msg.mode}
                  reasoning={msg.reasoning}
                />
              )}
              {msg.role === "assistant" && msg.choices.length > 0 && (
                <ChoiceButtons
                  choices={msg.choices}
                  onSelect={onChoiceSelect}
                  disabled={i < messages.length - 1}
                />
              )}
              {isLastUserMessage && (
                <div className={styles.failedRow}>
                  <span className={styles.failedHint}>
                    {msg.error || t("messageFailed")}
                  </span>
                  {onRetry && (
                    <button
                      className={styles.retryBtn}
                      onClick={onRetry}
                      disabled={streaming}
                    >
                      {streaming ? t("retrying") : t("retryMessage")}
                    </button>
                  )}
                </div>
              )}
            </div>
          );
        })}

        {streaming && (streamingText || streamingReasoning) && (
          <div className={styles.streamingText}>
            <div className={styles.streamingLabel}>{t("agentLabel")}</div>
            <div className={styles.streamingBubble}>
              {streamingReasoning && (
                <ReasoningPanel
                  reasoning={streamingReasoning}
                  isStreaming
                  streamingStartTime={streamingStartRef.current ?? undefined}
                />
              )}
              {streamingText && (
                <>
                  <Markdown>{streamingText}</Markdown>
                  <span className={styles.streamEllipsis} />
                </>
              )}
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
            placeholder={t("inputPlaceholder")}
            disabled={streaming}
          />
          <button
            className={styles.sendBtn}
            onClick={handleSend}
            disabled={!input.trim() || streaming}
            title={t("sendBtnTitle")}
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
