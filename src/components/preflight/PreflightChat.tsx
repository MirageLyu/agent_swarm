import { useState, useCallback, useRef, useEffect } from "react";
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
  onSend: (text: string) => void;
  onModeChange: (mode: PreflightMode) => void;
  onChoiceSelect: (choice: PreflightChoice) => void;
}

export function PreflightChat({
  messages,
  mode,
  streaming,
  streamingText,
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

  return (
    <div className={styles.panel}>
      <PreflightModeSwitch mode={mode} onModeChange={onModeChange} />

      <div className={styles.messages}>
        {messages.map((msg, i) => (
          <div key={i}>
            <ChatMessage role={msg.role as "user" | "assistant"} content={msg.content} />
            {msg.role === "assistant" && msg.choices.length > 0 && (
              <ChoiceButtons
                choices={msg.choices}
                onSelect={onChoiceSelect}
                disabled={i < messages.length - 1}
              />
            )}
          </div>
        ))}

        {streaming && streamingText && (
          <div className={styles.streamingText}>
            <div className={styles.streamingLabel}>Swarm Agent</div>
            <div className={styles.streamingBubble}>
              {streamingText}
              <span className={styles.streamCursor} />
            </div>
          </div>
        )}

        {streaming && !streamingText && (
          <div className={styles.typing}>
            <div className={styles.typingDot} />
            <div className={styles.typingDot} />
            <div className={styles.typingDot} />
          </div>
        )}

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
