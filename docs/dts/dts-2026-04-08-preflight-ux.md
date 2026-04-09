# DTS: Pre-flight UX 问题汇总

> ID: dts-2026-04-08-preflight-ux
> 模块: FM-10 Pre-flight & Mission Contract
> 状态: Fixed
> 创建日期: 2026-04-08

---

## Issue #1: Pre-flight 入口不够醒目

**现状**: PlanMissionDialog 中 "Pre-flight" 按钮使用 `variant="ghost"` 样式，与 "Cancel" 按钮视觉层级相同，用户不容易注意到，也不理解它的用途。

**期望**: 重新设计用户入口，让 Pre-flight 选项更醒目、功能更直观，用户能理解选择它的价值。

**涉及文件**:
- `src/components/mission/PlanMissionDialog.tsx`
- `src/components/mission/PlanMissionDialog.module.css`

---

## Issue #2: Pre-flight 首轮对话等待缺少加载状态

**现状**: 点击 "Pre-flight" 后，`start_preflight` 异步调用 LLM 生成第一条 Agent 消息，此期间 PreflightView 已渲染但聊天区域空白，用户无法感知系统正在工作。

**期望**: 在 LLM 返回第一条消息前，显示 loading 状态（进度条、typing indicator 或骨架屏），让用户知道系统正在准备。

**涉及文件**:
- `src/views/PreflightView.tsx`
- `src/components/preflight/PreflightChat.tsx`

---

## Issue #3: 对话气泡缺少 Markdown 渲染

**现状**: Agent 回复中包含大量 Markdown 格式（`**加粗**`、`\n`、`<code>` 等），但 `ChatMessage` 组件使用纯文本渲染，导致用户看到的是原始 Markdown 符号。

**期望**: Agent 消息气泡支持 Markdown 渲染（加粗、斜体、代码块、列表等），用户消息保持纯文本即可。

**涉及文件**:
- `src/components/preflight/ChatMessage.tsx`
- `src/components/preflight/ChatMessage.module.css`（可能需要增加 markdown 排版样式）

---

## Issue #4: choices 解析过于脆弱，多问题场景下选项无法渲染

**现状**: 当前方案要求 LLM 在回复末尾严格附加 `---CHOICES---\n[JSON]` 分隔符，后端 `parse_preflight_response()` 仅做单次 split 解析。实际使用中存在两个问题：

1. **LLM 经常不遵守分隔符约定** — 当回复内容较长（包含多个子问题）时，LLM 倾向于直接在 Markdown 中内联选项（如 `- **A. xxx**`、`- **B. yyy**`），不附加 `---CHOICES---` 块，导致前端收到空 choices 数组，不渲染任何按钮。

2. **多问题只渲染一组选项** — 即使 LLM 返回了结构化 choices，一条 Agent 消息中包含多个独立问题（如"1. 支持哪些平台？""2. 数据管理方式？""3. 账号绑定策略？"）时，前端只渲染一组 ChoiceButtons，无法区分和分别回答各个子问题。

**期望**:

- **后端 prompt 优化**: 强化 system prompt，明确要求 LLM 每条消息只聚焦一个决策点（一次一问），避免单条消息塞多个问题；
- **后端 fallback 解析**: `parse_preflight_response()` 在未找到 `---CHOICES---` 时，尝试从 Markdown 内容中提取 `- **A.` / `- **B.` 等模式作为 fallback choices；
- **前端多问题支持**（可选增强）: 如果后端仍可能返回多问题，考虑拆分为多组 ChoiceButtons 或顺序引导用户逐个回答。

**涉及文件**:
- `src-tauri/src/agent/planner.rs`（system prompt + `parse_preflight_response()`）
- `src/components/preflight/ChoiceButtons.tsx`
- `src/components/preflight/PreflightChat.tsx`

---

## Issue #5: 切换澄清模式（魔鬼代言人 / 风险标记）后页面无变化

**现状**: 点击模式页签后，只更新了前端本地 `mode` 状态。该 mode 值仅在用户下一次手动发消息时作为 `send_preflight_message` 的参数传给后端，影响 system prompt 选择。但切换本身不触发任何 Agent 行为，也没有视觉反馈（分隔线、模式切换提示、Agent 开场白等），用户以为功能坏了。

**期望**:

切换模式时应该：
1. 在聊天区插入一条模式切换分隔线（如 `── 切换到 魔鬼代言人 模式 ──`）；
2. 自动发送一条系统消息触发 Agent 按新模式发出开场提问（如"我现在以魔鬼代言人角度重新审视你的需求…"）；
3. 模式切换不清空对话历史和 Contract 条目（已有需求 BT-04）。

**涉及文件**:
- `src/views/PreflightView.tsx`（`handleModeChange` 需触发 `sendPreflightMessage`）
- `src/components/preflight/PreflightChat.tsx`（可能需要渲染分隔线组件）
- `src/components/preflight/PreflightChat.module.css`（分隔线样式）
