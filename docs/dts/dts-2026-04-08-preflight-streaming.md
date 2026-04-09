# DTS: Pre-flight 流式对话错误处理

> ID: dts-2026-04-08-preflight-streaming
> 模块: FM-10 Pre-flight & Mission Contract
> 状态: resolved
> 创建日期: 2026-04-08

---

## Issue #1: stream_chat 流解码错误无重试 — error decoding response body

**现状**: `openai_compat.rs` 的 `stream_chat()` 在遍历 `resp.bytes_stream()` 时，如果网络不稳定或代理中断，`chunk?` 直接返回 `reqwest::Error("error decoding response body")`，错误一路传播到 `preflight_chat()` → `commands::preflight` 的 `tokio::spawn` 内。此时：

1. 后端日志输出 `ERROR miragenty_lib::commands::preflight: Preflight chat failed: LLM call failed: error decoding response body`
2. 通过 `emit_preflight_event_pub` 发射 `kind="error"` 事件给前端
3. 前端 `setError(content)` 将错误消息设入状态
4. **但没有任何重试机制**：LLM 调用已返回部分 token 后中断时，用户被迫手动重新发送消息

**期望**:

- 后端 `stream_chat()` 在流解码出错时尝试至少 1 次重试（仅对网络错误，不对 4xx/5xx 重试）
- 如果重试仍失败，返回更清晰的错误提示（如「网络连接中断，请检查网络后重试」），而非 reqwest 原始技术信息
- 前端错误消息需要用户可理解的中文

**涉及文件**:
- `src-tauri/src/llm/openai_compat.rs`（`stream_chat()` 流遍历部分）
- `src-tauri/src/commands/preflight.rs`（`send_preflight_message` 错误处理）
- `src-tauri/src/agent/planner.rs`（`preflight_chat()` 错误转换）

---

## Issue #2: 错误横幅（Error Banner）无法关闭

**现状**: `PreflightView.tsx` 中 `{error && <div className={styles.errorBanner}>{error}</div>}` 渲染了一条错误横幅。一旦 `error` 被设置（来自 LLM 调用失败或其他异常），横幅会永久悬浮在页面顶部，没有关闭按钮或自动消失机制。用户在错误发生后继续发消息时横幅仍在，遮挡视线。

**期望**:

1. 错误横幅增加一个关闭按钮（`×`），用户可以手动关闭
2. 用户发送下一条消息时自动清除 error 状态（`setError(null)`）
3. 可选：非致命错误可添加 auto-dismiss（如 8 秒后自动消失）

**涉及文件**:
- `src/views/PreflightView.tsx`（error 状态管理 + banner 渲染）
- `src/views/PreflightView.module.css`（关闭按钮样式）

---

## Issue #3: 进度条 100% 后 Agent 仍不断提问 — 缺少收敛机制

**现状**: 两个问题叠加：

1. **进度条语义错误** — `PreflightStatusBar` 用 `messageCount / maxMessages(15)` 线性计算进度。消息条数并不反映"澄清是否充分"，15 条消息很容易到达（用户 + Agent 各 7-8 条就满），但 Scope 可能才刚开始澄清。进度条 100% 后提示"澄清完成，可签署 Contract"，但 Agent 继续问新问题，体验矛盾。

2. **Agent 无收敛意识** — 三组 system prompt 都只指导 LLM 逐步提问，但没有：
   - 告知 LLM 当前是第几轮对话（缺少上下文位置感）
   - 要求 LLM 在 5-8 轮后主动收敛并建议签署
   - 在 Scope 已有足够条目时切换到确认模式而非继续发散

**期望**:

- **进度条改为基于 Contract 完成度**：根据 Contract 各 section 已有条目数量计算（例如 scope ≥ 1 贡献 40%，constraints ≥ 1 贡献 20%，exclusions ≥ 1 贡献 20%，assumptions ≥ 1 贡献 20%），而非消息条数
- **System prompt 增加收敛指令**：在对话历史传入 LLM 时，附加当前轮次信息（如"This is round 8 of clarification"），并指示 LLM 在 6-8 轮后主动总结已有决策、建议签署
- **进度条 100% 时输入框提示文案变化**：如 placeholder 改为"澄清已充分，您可以签署 Contract 或继续提问"

**涉及文件**:
- `src/components/preflight/PreflightStatusBar.tsx`（进度算法重构）
- `src/views/PreflightView.tsx`（传递 contract 信息给 StatusBar）
- `src-tauri/src/agent/planner.rs`（system prompt 收敛指令）
- `src-tauri/src/commands/preflight.rs`（在 history 中注入轮次信息）

---

## Issue #4: 首轮消息渲染完成后 typing indicator 仍在显示

**现状**: 进入 Pre-flight 对话后，第一条 Agent 消息及其选项按钮正确渲染，但消息下方仍然显示三点跳动加载动画，不会消失。

**根因分析**: 存在事件监听注册的竞态条件（race condition）。时序如下：

1. `MissionsView` 调用 `start_preflight` → 后端立即开始 `tokio::spawn` 执行 `preflight_chat`
2. 前端导航到 `PreflightView`，组件挂载，`initialLoading = true`
3. `useEffect` 中调用 `onPreflightStream()`（底层是 `listen()` 异步注册 Tauri 事件监听器）
4. **在监听器注册完成前**，后端已发射 `kind="start"` 事件 → **前端错过该事件**
5. 监听器注册完成后，后续的 `text_delta` 和 `done` 事件正常接收
6. `done` 事件处理：`setStreaming(false)`，消息被添加到 `messages`
7. **但 `initialLoading` 仍为 `true`**（只有 `kind="start"` 的处理器会设 `setInitialLoading(false)`，而这个事件被错过了）
8. 渲染条件 `(streaming && !streamingText) || initialLoading` = `(false && true) || true` = **`true`** → typing indicator 持续显示

**期望**:

- `done` 事件处理器中也调用 `setInitialLoading(false)` 作为兜底
- `text_delta` 事件处理器中也调用 `setInitialLoading(false)` 作为兜底（收到任何实质内容即表示已不在 initial loading）
- 或者重构为：将 `initialLoading` 的清除逻辑改为"只要收到任何 preflight-stream 事件就清除"

**涉及文件**:
- `src/views/PreflightView.tsx`（stream event handler 中的 `initialLoading` 管理）
