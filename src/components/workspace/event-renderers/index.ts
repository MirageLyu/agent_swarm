/// Single-Agent Uplift Phase 0.3: per-kind event renderer registry.
///
/// 设计原则：
/// 1. **每个 kind 一个组件**——不要在一个大 switch 里塞 200 行 JSX。
/// 2. **renderer 自己解析 meta**——上层只传 event，renderer 决定怎么用 meta（缺
///    失或 schema 错就 fallback 到 content 文本，不要让单个事件能炸 UI）。
/// 3. **对接 AgentTerminalPane 的 cursor 行为**——把 isLast/isRunning 透传，
///    最后一行还在跑就显示 blink cursor。
///
/// 加新 kind 的步骤：
///   ① 后端 emit_event_with_meta 用新 kind（migration 025 已经预留枚举值）
///   ② 加渲染组件文件，export 一个 `XxxLine`
///   ③ 在 `EventLine` 的 switch 里登记
///   ④ 同步 src/stores/agent-store.ts 的 AgentEventKind 联合类型

export { EventLine } from "./EventLine";
export { ToolUseLine } from "./ToolUseLine";
export { ToolResultLine } from "./ToolResultLine";
export { SystemHintLine } from "./SystemHintLine";
export { GuardrailLine } from "./GuardrailLine";
export { ToolProgressLine } from "./ToolProgressLine";
export { NoteAppliedLine } from "./NoteAppliedLine";
