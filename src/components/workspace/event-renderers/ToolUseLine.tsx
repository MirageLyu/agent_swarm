import { memo, useMemo, useState } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import styles from "./EventLine.module.css";

interface ToolUseMeta {
  tool: string;
  tool_use_id?: string;
  input?: unknown;
}

function isToolUseMeta(value: unknown): value is ToolUseMeta {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as ToolUseMeta).tool === "string"
  );
}

/// 把工具入参压缩成 1 行可读 summary。优先展示用户最关心的字段：
///   - read_file / write_file / edit_file / list_files: path
///   - shell_exec: command 首行
///   - grep (alias: search_files) / glob: pattern
///   - 其它: 紧凑 JSON
/// 故意截短到 ~80 字，超过部分点击展开按钮看完整 input。
function summarizeInput(tool: string, input: unknown): string {
  if (input === null || input === undefined) return "";
  if (typeof input !== "object") return String(input);
  const obj = input as Record<string, unknown>;

  // Sentinel：后端检测到 LLM 漏写 / 非法 JSON 时塞这两个 key。
  // 前端给个明显标记，让用户一眼看到"模型这里偷工减料了"。
  if (typeof obj.__arg_parse_error__ === "string") {
    return `⚠ no/invalid arguments — ${obj.__arg_parse_error__}`;
  }

  if (typeof obj.path === "string") return obj.path;
  if (typeof obj.command === "string") {
    const firstLine = obj.command.split("\n", 1)[0] ?? obj.command;
    return firstLine.length > 80 ? `${firstLine.slice(0, 77)}…` : firstLine;
  }
  if (typeof obj.pattern === "string") {
    const tail = typeof obj.path === "string" ? ` in ${obj.path}` : "";
    return `${obj.pattern}${tail}`;
  }
  if (tool === "task_complete" && typeof obj.summary === "string") {
    return obj.summary.slice(0, 80);
  }

  try {
    const json = JSON.stringify(obj);
    return json.length > 80 ? `${json.slice(0, 77)}…` : json;
  } catch {
    return "";
  }
}

const TOOL_ICON: Record<string, string> = {
  read_file: "📖",
  write_file: "✏️",
  edit_file: "✏️",
  list_files: "📂",
  grep: "🔍",
  search_files: "🔍",
  glob: "🔎",
  shell_exec: "⚡",
  publish_artifact: "📦",
  task_complete: "✓",
  todo_write: "✓",
  propose_followup_mission: "🚀",
};

interface ToolUseLineProps {
  event: AgentEvent;
}

export const ToolUseLine = memo(function ToolUseLine({ event }: ToolUseLineProps) {
  const [expanded, setExpanded] = useState(false);
  const meta = isToolUseMeta(event.meta) ? event.meta : null;
  const toolName = meta?.tool ?? extractToolNameFromContent(event.content) ?? "tool";
  const summary = useMemo(
    () => (meta ? summarizeInput(toolName, meta.input) : event.content),
    [meta, toolName, event.content],
  );
  const icon = TOOL_ICON[toolName] ?? "▶";

  const fullInput = useMemo(() => {
    if (!meta || meta.input === undefined) return null;
    try {
      return JSON.stringify(meta.input, null, 2);
    } catch {
      return null;
    }
  }, [meta]);

  return (
    <div className={styles.row}>
      <span className={`${styles.icon} ${styles.iconAccent}`}>{icon}</span>
      <div className={styles.body}>
        <div className={styles.headLine}>
          <span className={styles.toolName}>{toolName}</span>
          <span className={styles.params}>{summary}</span>
        </div>
        {fullInput && expanded && (
          <pre className={`${styles.preview} ${styles.previewExpanded}`}>
            {fullInput}
          </pre>
        )}
        {fullInput && (
          <button
            className={styles.expandBtn}
            type="button"
            onClick={() => setExpanded((v) => !v)}
          >
            {expanded ? "Collapse" : "Show input"}
          </button>
        )}
      </div>
    </div>
  );
});

/// Fallback：老事件没有 meta 时从 "tool_name(...)" 这种内容里挖出工具名，
/// 至少图标和颜色还能正确——不至于让旧记录看起来比新记录还原始。
function extractToolNameFromContent(content: string): string | null {
  const match = content.match(/^([a-z_][a-z0-9_]*)\(/);
  return match ? match[1] : null;
}
