import { memo, useMemo, useState } from "react";
import type { AgentEvent } from "../../../stores/agent-store";
import styles from "./EventLine.module.css";
import { EventLine } from "./EventLine";

interface CollapsedReadGroupProps {
  events: AgentEvent[];
}

/**
 * Single-Agent Uplift A3 (collapseReadSearch).
 *
 * 把"连续只读探查"——read_file / grep / glob / list_files 的
 * tool_use + tool_result 配对——折叠成一行，带 chevron 可展开。
 *
 * 设计动机：
 *   - LLM 在动手改之前往往先连读 5-10 个文件 / 跑几次 grep。这些事件本身没有错，
 *     但每条都占两行，把真正动手做事的 write_file/edit_file/shell_exec 推到屏外。
 *   - 折叠后用户的 cognitive 焦点回到"它到底干了啥"。
 *   - 仍然可以一键展开看每一步细节，所以信息没丢。
 *
 * 折叠条件由父组件保证：传进来的 events 至少包含 2 个 tool_use（统计在 header 上）。
 */
export const CollapsedReadGroup = memo(function CollapsedReadGroup({
  events,
}: CollapsedReadGroupProps) {
  const [expanded, setExpanded] = useState(false);

  const summary = useMemo(() => buildSummary(events), [events]);
  const opCount = useMemo(
    () => events.filter((e) => e.kind === "tool_use").length,
    [events],
  );

  return (
    <div className={styles.readGroup}>
      <div
        className={styles.readGroupHeader}
        role="button"
        tabIndex={0}
        onClick={() => setExpanded((v) => !v)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            setExpanded((v) => !v);
          }
        }}
      >
        <span className={styles.readGroupChevron}>{expanded ? "▾" : "▸"}</span>
        <span className={styles.readGroupIcon}>📖</span>
        <span className={styles.readGroupSummary}>{summary}</span>
        <span className={styles.readGroupCount}>{opCount} ops</span>
      </div>
      {expanded && (
        <div className={styles.readGroupBody}>
          {events.map((evt) => (
            <EventLine key={evt.id} event={evt} isLast={false} isRunning={false} />
          ))}
        </div>
      )}
    </div>
  );
});

interface ToolUseMetaShape {
  tool: string;
  input?: unknown;
}

function isToolUseMeta(value: unknown): value is ToolUseMetaShape {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as ToolUseMetaShape).tool === "string"
  );
}

/**
 * Header 一行 summary：把所有 tool_use 的关键参数串起来。
 * 示例：`read_file src/foo.rs · grep "needle" · glob **\/*.rs`
 * 长度上限交给 CSS ellipsis 处理，这里只做语义压缩。
 */
function buildSummary(events: AgentEvent[]): string {
  const parts: string[] = [];
  for (const evt of events) {
    if (evt.kind !== "tool_use") continue;
    if (!isToolUseMeta(evt.meta)) continue;
    const { tool, input } = evt.meta;
    const arg = extractKeyArg(tool, input);
    parts.push(arg ? `${tool} ${arg}` : tool);
  }
  // 重复路径合并：连续多次同 tool 同 path 显示一次（罕见但发生过）
  const deduped: string[] = [];
  for (const p of parts) {
    if (deduped[deduped.length - 1] !== p) deduped.push(p);
  }
  return deduped.join(" · ");
}

function extractKeyArg(tool: string, input: unknown): string | null {
  if (input === null || typeof input !== "object") return null;
  const obj = input as Record<string, unknown>;
  if (typeof obj.path === "string") return obj.path;
  if (typeof obj.pattern === "string") {
    // grep（主名）+ search_files（alias）走引号包裹格式；glob 直接展示 pattern。
    const isGrep = tool === "grep" || tool === "search_files";
    return isGrep ? `"${obj.pattern}"` : obj.pattern;
  }
  return null;
}
