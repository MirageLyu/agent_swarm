import type { AgentEvent } from "../../../stores/agent-store";

/**
 * Single-Agent Uplift A3 (collapseReadSearch) — 分组算法。
 *
 * 把 events 分成两类：
 *   - `single`：直接渲染（write/shell/llm_call/error/...）
 *   - `group`：连续只读探查（≥2 个 tool_use），由 CollapsedReadGroup 折叠展示
 *
 * 折叠规则：
 *   - 只读工具：read_file / search_files / glob / list_files
 *   - 单个 tool_use 后必须紧跟它的 tool_result（或 error），中间不能插非 readonly 的事件
 *   - 连续 ≥2 个完整的 (tool_use, tool_result) 才折叠；单一 op 不值得折叠
 *   - 任一 tool_result 是 is_error 仍可参与折叠（搜不到/找不到不算严重失败）
 *   - 但 kind=='error' 且没有对应 tool_use 时不参与
 *
 * **重要边界**：尾部"未完成的 tool_use（无 tool_result）"——LLM 还没拿到结果——
 * 不能放进 group，否则展开折叠会丢动画。所以扫到尾部 dangling tool_use 单独输出。
 */
export type EventGroup =
  | { kind: "single"; event: AgentEvent }
  | { kind: "group"; events: AgentEvent[] };

const READONLY_TOOLS = new Set(["read_file", "search_files", "glob", "list_files"]);

function getToolName(event: AgentEvent): string | null {
  if (
    event.meta &&
    typeof event.meta === "object" &&
    typeof (event.meta as { tool?: unknown }).tool === "string"
  ) {
    return (event.meta as { tool: string }).tool;
  }
  return null;
}

function isReadOnlyToolUse(event: AgentEvent): boolean {
  if (event.kind !== "tool_use") return false;
  const tool = getToolName(event);
  return tool !== null && READONLY_TOOLS.has(tool);
}

function isReadOnlyToolResult(event: AgentEvent): boolean {
  if (event.kind !== "tool_result" && event.kind !== "error") return false;
  const tool = getToolName(event);
  return tool !== null && READONLY_TOOLS.has(tool);
}

export function groupReadOnlyEvents(events: AgentEvent[]): EventGroup[] {
  const out: EventGroup[] = [];
  let buffer: AgentEvent[] = [];
  let bufferOpCount = 0;

  const flushBuffer = () => {
    if (buffer.length === 0) return;
    if (bufferOpCount >= 2) {
      out.push({ kind: "group", events: buffer });
    } else {
      // 不达折叠门槛：原样输出
      for (const e of buffer) out.push({ kind: "single", event: e });
    }
    buffer = [];
    bufferOpCount = 0;
  };

  let i = 0;
  while (i < events.length) {
    const evt = events[i];
    if (isReadOnlyToolUse(evt)) {
      // 看下一个事件是不是它的配对结果
      const next = events[i + 1];
      if (next && isReadOnlyToolResult(next)) {
        buffer.push(evt);
        buffer.push(next);
        bufferOpCount += 1;
        i += 2;
        continue;
      }
      // dangling tool_use：当前是 stream 中实时观察到的最新一条，下一个 tool_result 还没到
      flushBuffer();
      out.push({ kind: "single", event: evt });
      i += 1;
      continue;
    }
    flushBuffer();
    out.push({ kind: "single", event: evt });
    i += 1;
  }
  flushBuffer();
  return out;
}
