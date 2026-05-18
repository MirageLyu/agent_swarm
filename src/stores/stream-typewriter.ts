/// 永远打字机：所有 stream chunk 都按 RAF 节奏渐进刷到前端 streamBuffer。
///
/// # 为什么需要这层
///
/// 后端 forwarder 每收到一个 LLM chunk 立即 `app_handle.emit("agent-stream", ...)`。
/// 直连真流式 provider（OpenAI / Anthropic）时这是 token 级 delta，~10-30 字符；
/// 但走 reseller / 假流式 endpoint（DeepSeek-V4 / SiliconFlow Qwen 部分路由）时，
/// 单个 chunk 可能是整段 8-16KB，用户感受到的是 "沉默 30s 后一次性砸出一大坨"。
///
/// 直接 `appendStream(content)` 会把这一坨一帧之内塞到 React state，UI 一次性 reflow，
/// 既视觉突兀又没有 Cursor 那种"文字滑过"的丝滑感。
///
/// # 策略：always 模式
///
/// **所有 chunk 都进 RAF 队列**，按统一节奏吐——一致丝滑感优先于零延迟。
/// 代价：真流式 provider 的小 chunk 也会被引入 ~16ms RAF 延迟，
/// 但每秒 ~3600 字符的吐字速度对人眼来说仍然是"几乎跟得上输入"的体感。
///
/// - **基础速率 60 chars/frame**（≈ 3600 chars/sec）：Cursor 式打字机感的核心节奏。
/// - **大 chunk 温和上限保护**：pending > 4KB 时按"目标 1.5s 内消化完"自适应加速，
///   防止假流式 reseller 一坨 16KB 砸来时打字机刷 4+ 秒，体验反而崩。
/// - **紧急 flush**：`flushStreamNow(agentId)` 给"agent 进入终态"等场景立即吐光。
///
/// # 测试性
///
/// `requestAnimationFrame` 在 jsdom 没有原生实现；fallback 到 setTimeout(16)。

import { useAgentStore } from "./agent-store";

interface PendingState {
  /// 待消费的字符（按到达顺序拼接）。
  buffer: string;
  /// 是否已经有一次 RAF 在排队，避免重复 schedule。
  scheduled: boolean;
}

const PENDING: Map<string, PendingState> = new Map();

/// 基础速率：每帧吐多少字符（≈ 60 fps × 60 = 3600 字符/秒）。
const BASE_CHARS_PER_FRAME = 60;
/// 仅当 pending 超过此阈值时启用"目标 1.5s 消化完"的加速保护，
/// 避免大坨砸来卡住 4+ 秒。低于阈值始终按 BASE 速率打字机。
const ADAPTIVE_BACKLOG_THRESHOLD = 4 * 1024;
/// 加速保护的目标消化时间（秒）。
const ADAPTIVE_TARGET_DRAIN_SECS = 1.5;
/// 假设 60fps。
const FPS_ASSUMPTION = 60;

const raf: (cb: () => void) => number =
  typeof requestAnimationFrame === "function"
    ? (cb) => requestAnimationFrame(cb)
    : (cb) => setTimeout(cb, 16) as unknown as number;

function appendToStore(agentId: string, content: string) {
  if (!content) return;
  useAgentStore.getState().appendStream(agentId, content);
}

function scheduleDrain(agentId: string) {
  const state = PENDING.get(agentId);
  if (!state || state.scheduled) return;
  state.scheduled = true;
  raf(() => drainOnce(agentId));
}

function drainOnce(agentId: string) {
  const state = PENDING.get(agentId);
  if (!state) return;
  state.scheduled = false;

  if (!state.buffer) return;

  // 默认按基础速率；仅当 backlog 超阈值才加速保护，避免假流式大坨刷太久。
  const total = state.buffer.length;
  let charsThisFrame = BASE_CHARS_PER_FRAME;
  if (total > ADAPTIVE_BACKLOG_THRESHOLD) {
    const minByTarget = Math.ceil(
      total / (ADAPTIVE_TARGET_DRAIN_SECS * FPS_ASSUMPTION),
    );
    charsThisFrame = Math.max(BASE_CHARS_PER_FRAME, minByTarget);
  }

  const chunk = state.buffer.slice(0, charsThisFrame);
  state.buffer = state.buffer.slice(charsThisFrame);
  appendToStore(agentId, chunk);

  if (state.buffer) {
    scheduleDrain(agentId);
  }
}

/// 入队一段 chunk。**所有 chunk 都进 RAF 队列**——always 打字机模式。
/// 真流式 token-level delta 也走 RAF（~16ms 延迟），换取一致的"文字滑过"节奏。
export function enqueueStreamChunk(agentId: string, content: string) {
  if (!content) return;

  let state = PENDING.get(agentId);
  if (!state) {
    state = { buffer: "", scheduled: false };
    PENDING.set(agentId, state);
  }

  state.buffer += content;
  scheduleDrain(agentId);
}

/// 立即吐光某 agent 所有 pending。用于：
/// - agent 进入终态（completed/failed/cancelled）：避免遗留打字机半截
/// - 切走窗口前 flush，防止用户回来看到半句话
export function flushStreamNow(agentId: string) {
  const state = PENDING.get(agentId);
  if (!state) return;
  if (state.buffer) {
    appendToStore(agentId, state.buffer);
    state.buffer = "";
  }
  state.scheduled = false;
}

/// 清空 pending（不 flush）。agent 被 store 删除 / clearStream 时调。
export function dropStreamQueue(agentId: string) {
  PENDING.delete(agentId);
}
