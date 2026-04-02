# FM-07: Mission Planning UX Enhancements

> 版本: v1.0 | 日期: 2026-04-02  
> 优先级: P1 | 预估周期: 2-3 天  
> 依赖: FM-01 | 被依赖: 无  
> 性质: FM-01 的体验增补，不修改 FM-01 已有需求

---

## IR — Initial Requirements

### IR-01: 用户故事

**US-01**: 作为开发者，我希望 Task DAG 画布支持自由缩放和拖拽平移，并能用 MacBook 触控板进行捏合缩放和双指拖拽，就像使用 Figma 或 Apple 原生应用一样丝滑，这样在任务数量多时我仍然能高效浏览和操作。

**US-02**: 作为开发者，我希望在 Plan Mission 的过程中能看到 LLM 的实时思考输出（流式文本），而不是只看到一个静态的 "Planning..." 动画，这样我知道系统没有卡住，也能提前预览任务拆解的方向。

### IR-02: 业务价值

- **DAG 画布交互**：目前 DAG 仅支持滚动条横向滚动，节点多时（>10）浏览体验差，无法全局预览。缩放+平移是 DAG 可视化工具的基本能力。
- **Planning 流式输出**：当前 Planner 调用推理模型（qwen3.5-plus），一次 planning 可能需要 20-60 秒。无进度反馈的静态 loading 让用户难以判断是在思考还是已挂掉，严重损害信任感。

### IR-03: 高层验收标准

1. DAG 画布支持鼠标滚轮缩放、触控板捏合缩放、鼠标拖拽平移、触控板双指滑动平移
2. 缩放范围限制在 25%-200%，有"Fit to View"重置按钮
3. 所有变换动画流畅，≥ 60fps
4. Planning 过程中实时显示 LLM 流式文本输出
5. 流式输出面板在 DAG 结果出来后自动收起

---

## SR — Software Requirements

### 功能需求

#### FR-01: DAG 画布缩放

- **FR-01.1**: DAG viewport 支持通过鼠标滚轮（`wheel` 事件 + `Ctrl/Cmd`）进行缩放，以鼠标位置为缩放中心
- **FR-01.2**: 支持触控板捏合手势（`gesturestart/change/end` 或 `wheel` 事件的 `ctrlKey` 判定）进行缩放
- **FR-01.3**: 缩放范围限制在 `[0.25, 2.0]`，步长平滑连续
- **FR-01.4**: 缩放变换应用于 SVG 外层的 CSS `transform: scale()`，不重新计算节点布局
- **FR-01.5**: 缩放过程中保持鼠标/手指所在的 DAG 内容位置不变（zoom-to-point）

#### FR-02: DAG 画布平移

- **FR-02.1**: 支持鼠标左键拖拽画布进行平移（当不在节点上时）
- **FR-02.2**: 支持触控板双指滑动进行平移（无需按键修饰）
- **FR-02.3**: 平移范围限制在内容边界 ± 100px，防止画布滑出视野
- **FR-02.4**: 拖拽时光标变为 `grab`/`grabbing`

#### FR-03: 画布控制栏

- **FR-03.1**: DAG 右下角提供浮动控制栏，包含：缩放百分比显示、+/- 按钮、"Fit" 按钮
- **FR-03.2**: "Fit" 按钮点击后自动计算使所有节点适配视口的缩放比例和偏移量，带 300ms 过渡动画
- **FR-03.3**: +/- 按钮每次步进 ±10%
- **FR-03.4**: 控制栏半透明背景 + 毛玻璃效果，不遮挡内容时降低不透明度

#### FR-04: Planner 流式输出

- **FR-04.1**: 后端 `plan_mission` command 改用流式 LLM 调用（`stream_chat`），通过 Tauri event 实时推送文本片段到前端
- **FR-04.2**: 新增 Tauri event `planner-stream`，payload 为 `{ kind: "text_delta" | "done" | "error", content: string }`
- **FR-04.3**: 前端 PlanInput 下方展示一个可折叠的流式输出面板，实时显示 LLM 产出的文本（包括 reasoning 思考过程）
- **FR-04.4**: 流式输出面板最大高度 200px，超出后自动滚动到底部
- **FR-04.5**: LLM 完成后（收到 `done` 事件），解析完整文本为 JSON，走原有的校验和入库流程
- **FR-04.6**: 流式输出面板在 DAG 成功渲染后自动折叠（可手动展开查看历史）
- **FR-04.7**: 若 LLM 返回包含 `reasoning_content` 字段（推理模型），优先展示 `reasoning_content`；若无则展示 `content` 的 delta

#### FR-05: 流式进度指示

- **FR-05.1**: Planning 过程中 "Planning..." 按钮旁增加已接收 token 数和已用时间的实时计数
- **FR-05.2**: 若 30 秒内无任何 delta 到达，显示 "Connection may be slow..." 提示
- **FR-05.3**: 用户可在流式输出过程中点击 "Cancel" 取消 planning（前端丢弃结果，后端忽略该请求的后续 event）

### 非功能需求

- **NFR-01**: 缩放/平移变换必须在 GPU 层完成（CSS transform），不触发 JS 布局重算，保证 ≥ 60fps
- **NFR-02**: 触控板手势识别准确率 ≥ 95%，不与浏览器默认滚动冲突
- **NFR-03**: 流式输出面板渲染不阻塞 UI 主线程，500 token 以内流畅
- **NFR-04**: Planner 流式调用超时 90 秒，与 FR-02.5（重试机制）兼容

### 接口需求

**变更的 Tauri Commands：**

| Command | 变更说明 |
|---------|---------|
| `plan_mission` | 内部改用 `stream_chat`；结果仍通过 command 返回值返回，但过程中通过 event 推送 delta |

**新增 Tauri Events：**

| Event | Payload | 说明 |
|-------|---------|------|
| `planner-stream` | `{ kind: "text_delta" \| "reasoning_delta" \| "done" \| "error", content: string }` | Planner LLM 流式输出 |

### 数据需求

无新增数据表或字段变更。流式输出为瞬态数据，不持久化。

---

## AR — Architecture Requirements

### 组件设计

```
TaskDAG (变更)
  └─ DAGViewport (新)
       ├─ transform state: { scale, translateX, translateY }
       ├─ 鼠标/触控板事件监听
       └─ <div style="transform: ...">
            └─ <svg> (原有 DAG SVG)

  └─ ViewportControls (新)
       ├─ Zoom +/- 按钮
       ├─ 缩放百分比
       └─ Fit 按钮

PlanInput (变更)
  └─ PlannerStreamPanel (新)
       ├─ 流式文本显示区
       ├─ Token/时间计数
       └─ 折叠/展开控制
```

### 前端组件清单

| 组件 | 路径 | 职责 |
|------|------|------|
| `DAGViewport` | `src/components/mission/DAGViewport.tsx` | 包裹 SVG 的可缩放/可平移容器 |
| `ViewportControls` | `src/components/mission/ViewportControls.tsx` | 浮动缩放控制栏 |
| `PlannerStreamPanel` | `src/components/mission/PlannerStreamPanel.tsx` | 流式输出展示面板 |

### 后端模块变更

| 文件 | 变更 |
|------|------|
| `agent/planner.rs` | `call_planner` 改用 `stream_chat`，通过 `AppHandle` 发送 `planner-stream` event |
| `commands/mission.rs` | `plan_mission` 传入 `AppHandle` 给 planner，用于 event 推送 |

### DAG 画布变换模型

```
视口坐标系:
  screen_x = (dag_x + translateX) * scale
  screen_y = (dag_y + translateY) * scale

Zoom-to-point 算法:
  1. 记录鼠标在 viewport 中的位置 (mx, my)
  2. 计算鼠标在 DAG 坐标系中的位置: dagX = mx / oldScale - oldTx, dagY = my / oldScale - oldTy
  3. 更新 scale
  4. 反推新 translate: newTx = mx / newScale - dagX, newTy = my / newScale - dagY

Fit-to-View 算法:
  1. 获取 viewport 尺寸 (vw, vh) 和 DAG 内容尺寸 (dw, dh)
  2. scale = min(vw / dw, vh / dh, 1.0) × 0.9 (留 10% 边距)
  3. translateX = (vw / scale - dw) / 2
  4. translateY = (vh / scale - dh) / 2
```

### Planner 流式输出时序

```
前端                     后端 plan_mission            LLM Provider
 │                           │                            │
 │── invoke plan_mission ──► │                            │
 │                           │── stream_chat ────────────►│
 │                           │                            │
 │◄── planner-stream ───────│◄── text_delta ─────────────│
 │    (reasoning_delta)      │                            │
 │◄── planner-stream ───────│◄── text_delta ─────────────│
 │    (text_delta)           │                            │
 │   ...                     │   ...                      │
 │                           │◄── message_stop ──────────│
 │◄── planner-stream ───────│                            │
 │    (done + full_text)     │                            │
 │                           │── parse & validate JSON    │
 │                           │── write to DB              │
 │◄── plan_mission result ──│                            │
```

### 流式输出 DashScope/Qwen 适配说明

qwen3.5-plus 是推理模型，流式响应中：
- `delta.reasoning_content` 字段包含思考过程（大量 token）
- `delta.content` 字段包含最终输出

后端应将两者分别以 `reasoning_delta` 和 `text_delta` 两种 kind 推送，前端可选择展示策略（默认展示 reasoning_delta 以显示思考过程；若无 reasoning 则展示 text_delta）。

### 现有代码关键入口

| 说明 | 文件路径 |
|------|---------|
| DAG 视口容器 | `src/components/mission/TaskDAG.tsx` 中的 `.viewport` div |
| DAG 布局算法 | `src/components/mission/dag-layout.ts` |
| PlanInput 组件 | `src/components/mission/PlanInput.tsx` |
| Planner 调用（当前非流式） | `src-tauri/src/agent/planner.rs` → `call_planner()` |
| OpenAI 兼容流式实现 | `src-tauri/src/llm/openai_compat.rs` → `stream_chat()` |
| 前端事件系统 | `src/ipc/events.ts` |
| MissionsView 容器 | `src/views/MissionsView.tsx` |
