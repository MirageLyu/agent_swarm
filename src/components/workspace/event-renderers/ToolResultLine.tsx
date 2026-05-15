import { memo, useMemo, useState } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import styles from "./EventLine.module.css";

interface ToolResultMeta {
  tool?: string;
  tool_use_id?: string;
  is_error?: boolean;
  duration_ms?: number;
  size_chars?: number;
}

function isToolResultMeta(value: unknown): value is ToolResultMeta {
  return typeof value === "object" && value !== null;
}

/// 工具结果默认折到 8 行；点击 "Show full" 展开。
/// JSON 错误（来自 ToolOutput::error）解析后只展示 message 字段，更人类。
const PREVIEW_LINES = 8;

interface ToolResultLineProps {
  event: AgentEvent;
}

export const ToolResultLine = memo(function ToolResultLine({ event }: ToolResultLineProps) {
  const [expanded, setExpanded] = useState(false);
  const meta = isToolResultMeta(event.meta) ? event.meta : null;
  const isError = meta?.is_error ?? event.kind === "error";

  const { previewText, totalLines, fullText } = useMemo(() => {
    const friendly = friendlyContent(event.content, isError);
    const lines = friendly.split("\n");
    const head = lines.slice(0, PREVIEW_LINES).join("\n");
    return { previewText: head, totalLines: lines.length, fullText: friendly };
  }, [event.content, isError]);

  const truncated = totalLines > PREVIEW_LINES;
  const tool = meta?.tool ?? "";
  const duration = meta?.duration_ms;

  return (
    <div className={styles.row}>
      <span
        className={`${styles.icon} ${isError ? styles.iconError : styles.iconOk}`}
      >
        {isError ? "✗" : "✓"}
      </span>
      <div className={styles.body}>
        <div className={styles.headLine}>
          <span
            className={`${styles.badge} ${
              isError ? styles.badgeError : styles.badgeOk
            }`}
          >
            {isError ? "error" : "result"}
          </span>
          {tool && <span className={styles.toolName}>{tool}</span>}
          {meta?.size_chars !== undefined && (
            <span className={styles.params}>
              {meta.size_chars.toLocaleString()} chars
            </span>
          )}
          {typeof duration === "number" && (
            <span className={styles.duration}>
              {duration < 1000
                ? `${duration}ms`
                : `${(duration / 1000).toFixed(1)}s`}
            </span>
          )}
        </div>
        <pre
          className={`${styles.preview} ${
            expanded ? styles.previewExpanded : truncated ? styles.previewFade : ""
          }`}
        >
          {expanded ? fullText : previewText}
        </pre>
        {truncated && (
          <button
            className={styles.expandBtn}
            type="button"
            onClick={() => setExpanded((v) => !v)}
          >
            {expanded ? "Collapse" : `Show full (${totalLines} lines)`}
          </button>
        )}
      </div>
    </div>
  );
});

/// 后端错误 ToolOutput::error 的 content 是 `{"error": "...", "message": "..."}` JSON，
/// 直接吐成原文用户看不懂。解析成功就只显示 message 字段——和 Claude Code 一致。
function friendlyContent(content: string, isError: boolean): string {
  if (!isError) return content;
  if (!content.startsWith("{")) return content;
  try {
    const obj = JSON.parse(content) as { error?: string; message?: string };
    if (obj.message && obj.error) {
      return `[${obj.error}] ${obj.message}`;
    }
    if (obj.message) return obj.message;
  } catch {
    // not JSON — fall through to raw
  }
  return content;
}
